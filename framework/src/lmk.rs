//! Low-Memory Killer (Phase 12.3).
//!
//! Pure decision logic: given the current [`MemoryManager`] +
//! [`ActivityManager`] state, decide which packages to evict. The
//! caller is responsible for the actual side effects (worker.terminate
//! on the JS host, `destroy_all_for_package` on the activity manager,
//! `mark_package_trapped` on the framework).
//!
//! The policy is Android-inspired: every tracked package gets a
//! [`Priority`] tier derived from the lifecycle state of its activities
//! and whether any of them sits on the foreground task. When aggregate
//! memory crosses [`LmkPolicy::threshold_bytes`] we kill the
//! lowest-priority package first; ties are broken by largest
//! `current_bytes` (kicking the biggest pig out of the room reclaims
//! memory fastest). Killing continues until aggregate falls below
//! [`LmkPolicy::recovery_bytes`] or [`LmkPolicy::max_kills_per_check`]
//! is reached, whichever comes first.
//!
//! ## Why not kill the highest-priority package even when it's huge?
//!
//! A foreground app the user is actively interacting with is the
//! single thing they care about; killing it is roughly indistinguishable
//! from the OS itself crashing, from the user's perspective. We prefer
//! to evict cached background apps even if doing so reclaims less memory.
//! When the foreground app *itself* is the one over-budget, Phase 12.5
//! handles it via manifest enforcement — not LMK.
//!
//! ## Protected packages
//!
//! The shell is never killed. Callers pass it (and any other system
//! package) through `protected` — the policy filters those out before
//! ranking. There is no per-priority kill veto otherwise: an EMPTY
//! shell would still be skipped via `protected`, never via priority.

use std::collections::HashSet;

use crate::activity_manager::{ActivityManager, LifecycleState};
use crate::memory_manager::MemoryManager;

/// Lifecycle-derived priority. Ordering is intentional —
/// `Empty < Background < Paused < Visible < Foreground` — so the
/// kill-ranking code can `.sort_by(|a, b| a.cmp(b))` and pop from the
/// front. `#[repr(u8)]` is documented (not enforced) for HAL crossover
/// in Phase 12.4, where suspended apps will report their last priority.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Priority {
    /// No live activity instances, or only `Trapped`/`Destroyed` ones.
    /// First to die.
    Empty,
    /// At least one `Stopped` or `Created` activity. The user is unlikely
    /// to revisit this app soon; saved-state on disk covers them anyway.
    Background,
    /// At least one `Paused` activity. The app was visible recently but
    /// isn't right now — it can probably re-create cleanly.
    Paused,
    /// At least one `Started`/`Resumed` activity that isn't on the
    /// foreground task. Used for split-screen / picture-in-picture in
    /// Android; for us today this is the surface that's painting but
    /// isn't currently focused.
    Visible,
    /// At least one `Resumed` activity on the foreground task. The
    /// thing the user is actively driving. Last to die.
    Foreground,
}

/// Tunable thresholds for the policy. Defaults are conservative and
/// chosen for desktop Chrome with 4-8GB RAM headroom; mobile tuning
/// arrives with the Phase-12 mobile baseline.
#[derive(Debug, Clone, Copy)]
pub struct LmkPolicy {
    /// Aggregate memory across tracked packages above which we start
    /// killing.
    pub threshold_bytes: u64,
    /// Killing continues until aggregate falls below this. Hysteresis —
    /// must be `<= threshold_bytes` so a single check can settle.
    pub recovery_bytes: u64,
    /// Hard cap on victims per call. Prevents a runaway estimate from
    /// killing every app when memory accounting is briefly skewed.
    pub max_kills_per_check: u32,
}

impl LmkPolicy {
    pub const DEFAULT_THRESHOLD: u64 = 256 * 1024 * 1024;
    pub const DEFAULT_RECOVERY: u64 = 192 * 1024 * 1024;
    pub const DEFAULT_MAX_KILLS: u32 = 4;
}

impl Default for LmkPolicy {
    fn default() -> Self {
        Self {
            threshold_bytes: Self::DEFAULT_THRESHOLD,
            recovery_bytes: Self::DEFAULT_RECOVERY,
            max_kills_per_check: Self::DEFAULT_MAX_KILLS,
        }
    }
}

/// One package the policy elected to evict, plus diagnostics for
/// observability/tracing. `priority` is recorded at decision time —
/// by the time the caller acts, the activity may have transitioned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KillRequest {
    pub package_id: String,
    pub priority: Priority,
    pub current_bytes: u64,
    pub reason: KillReason,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KillReason {
    /// Aggregate memory crossed [`LmkPolicy::threshold_bytes`].
    SystemPressure,
    /// Browser surfaced a memory-pressure event (currently mapped from
    /// the Chromium-only `PerformanceObserver` API). Caller decides
    /// when to use this; the policy treats it identically to
    /// `SystemPressure` but the reason is preserved for logging.
    HostPressure,
}

/// Compute the priority of `package_id` from its activities' lifecycle
/// states and the position of their tasks. `Priority::Empty` if no
/// activity records exist for the package.
pub fn priority_of(package_id: &str, activities: &ActivityManager) -> Priority {
    let fg_task = activities.foreground_task();
    let mut best = Priority::Empty;

    for id in activities.ids_for_package(package_id) {
        let Some(record) = activities.record(id) else { continue };
        let on_fg = fg_task == Some(record.task);

        let p = match (record.state, on_fg) {
            (LifecycleState::Resumed, true) => Priority::Foreground,
            (LifecycleState::Resumed, false) => Priority::Visible,
            (LifecycleState::Started, _) => Priority::Visible,
            (LifecycleState::Paused, _) => Priority::Paused,
            (LifecycleState::Stopped, _) => Priority::Background,
            (LifecycleState::Created, _) => Priority::Background,
            (LifecycleState::Initial, _)
            | (LifecycleState::Destroyed, _)
            | (LifecycleState::Trapped, _)
            // Stone 12.4 — Suspended packages have already been evicted
            // by definition; the LMK should never re-pick them. They
            // also have memory.forget()-ed accounting, so they almost
            // never reach this match in practice. Mapped to Empty for
            // correctness in the rare case a Suspended record outlives
            // its memory_manager entry.
            | (LifecycleState::Suspended, _) => Priority::Empty,
        };
        if p > best {
            best = p;
        }
    }
    best
}

/// Decide who to kill given current state. Returns an empty vec if the
/// system is below threshold. The ordering of the returned vec is the
/// order the caller should kill in (lowest-priority and largest first).
///
/// The function is pure — it never mutates `memory` or `activities`.
/// The caller is expected to invoke the side-effecting kill path once
/// per [`KillRequest`].
pub fn pick_victims(
    policy: &LmkPolicy,
    memory: &MemoryManager,
    activities: &ActivityManager,
    protected: &HashSet<&str>,
    reason: KillReason,
) -> Vec<KillRequest> {
    let total = memory.total_current();
    if total <= policy.threshold_bytes {
        return Vec::new();
    }
    if policy.recovery_bytes > policy.threshold_bytes {
        // Misconfigured policy — treat as "do nothing" rather than
        // panicking on an asymmetric threshold/recovery pair.
        return Vec::new();
    }

    let mut candidates: Vec<(Priority, u64, String)> = memory
        .iter()
        .filter(|(id, _, _)| !protected.contains(id))
        .map(|(id, current, _peak)| {
            let prio = priority_of(id, activities);
            (prio, current, id.to_string())
        })
        .collect();

    // Lowest priority first; within same tier, largest bytes first.
    // `.then` chains the secondary key with the same Ordering shape.
    candidates.sort_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)));

    let mut victims = Vec::new();
    let mut running = total;
    for (prio, bytes, id) in candidates {
        if running <= policy.recovery_bytes {
            break;
        }
        if victims.len() as u32 >= policy.max_kills_per_check {
            break;
        }
        victims.push(KillRequest {
            package_id: id,
            priority: prio,
            current_bytes: bytes,
            reason,
        });
        running = running.saturating_sub(bytes);
    }
    victims
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::activity::Activity;
    use crate::activity_manager::{ActivityManager, LaunchMode};
    use std::collections::HashSet;

    fn protected_set<'a>(items: &'a [&'a str]) -> HashSet<&'a str> {
        items.iter().copied().collect()
    }

    /// Empty no-op activity for tests — we only ever read state from
    /// the record, never dispatch into it.
    #[derive(Default)]
    struct StubActivity;
    impl Activity for StubActivity {
        fn render(
            &mut self,
            _ctx: &mut crate::activity::ActivityContext<'_>,
            _out: &mut crate::compositor::FrameCommands,
        ) {
        }
    }

    fn am_with_one_activity(package: &str, class: &str, state: LifecycleState) -> ActivityManager {
        let mut am = ActivityManager::new();
        am.register(package, class, LaunchMode::Standard, || Box::new(StubActivity));
        let outcome = am.launch(package, class).unwrap();
        // Force the lifecycle by calling the appropriate transition.
        // We bypass async dispatch and write the state directly via a
        // helper — tests in the activity_manager use the same trick.
        am.force_state_for_test(outcome.id, state);
        am.set_foreground_task_for_test(am.record(outcome.id).unwrap().task);
        am
    }

    #[test]
    fn no_kill_when_total_under_threshold() {
        let mut mm = MemoryManager::new();
        mm.record("a", 10);
        mm.record("b", 20);
        let am = ActivityManager::new();
        let policy = LmkPolicy { threshold_bytes: 1000, recovery_bytes: 800, max_kills_per_check: 4 };
        let v = pick_victims(&policy, &mm, &am, &HashSet::new(), KillReason::SystemPressure);
        assert!(v.is_empty());
    }

    #[test]
    fn priority_empty_when_package_has_no_activities() {
        let am = ActivityManager::new();
        assert_eq!(priority_of("never.installed", &am), Priority::Empty);
    }

    #[test]
    fn priority_foreground_for_resumed_on_foreground_task() {
        let am = am_with_one_activity("com.test.front", "Main", LifecycleState::Resumed);
        assert_eq!(priority_of("com.test.front", &am), Priority::Foreground);
    }

    #[test]
    fn priority_paused_for_paused_activity() {
        let am = am_with_one_activity("com.test.paused", "Main", LifecycleState::Paused);
        assert_eq!(priority_of("com.test.paused", &am), Priority::Paused);
    }

    #[test]
    fn priority_background_for_stopped_activity() {
        let am = am_with_one_activity("com.test.stop", "Main", LifecycleState::Stopped);
        assert_eq!(priority_of("com.test.stop", &am), Priority::Background);
    }

    #[test]
    fn priority_empty_for_trapped_activity() {
        let am = am_with_one_activity("com.test.dead", "Main", LifecycleState::Trapped);
        assert_eq!(priority_of("com.test.dead", &am), Priority::Empty);
    }

    #[test]
    fn over_threshold_picks_lowest_priority_first() {
        let mut am = ActivityManager::new();
        am.register("fg", "M", LaunchMode::Standard, || Box::new(StubActivity));
        am.register("bg", "M", LaunchMode::Standard, || Box::new(StubActivity));
        let fg = am.launch("fg", "M").unwrap();
        let bg = am.launch("bg", "M").unwrap();
        am.force_state_for_test(fg.id, LifecycleState::Resumed);
        am.force_state_for_test(bg.id, LifecycleState::Stopped);
        am.set_foreground_task_for_test(am.record(fg.id).unwrap().task);

        let mut mm = MemoryManager::new();
        mm.record("fg", 100 * 1024 * 1024);
        mm.record("bg", 80 * 1024 * 1024);
        let policy = LmkPolicy {
            threshold_bytes: 150 * 1024 * 1024,
            recovery_bytes: 100 * 1024 * 1024,
            max_kills_per_check: 4,
        };
        let v = pick_victims(&policy, &mm, &am, &HashSet::new(), KillReason::SystemPressure);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].package_id, "bg");
        assert_eq!(v[0].priority, Priority::Background);
    }

    #[test]
    fn ties_broken_by_largest_bytes() {
        // Two paused apps over threshold — the larger one gets killed.
        let mut am = ActivityManager::new();
        am.register("small", "M", LaunchMode::Standard, || Box::new(StubActivity));
        am.register("big", "M", LaunchMode::Standard, || Box::new(StubActivity));
        let s = am.launch("small", "M").unwrap();
        let b = am.launch("big", "M").unwrap();
        am.force_state_for_test(s.id, LifecycleState::Paused);
        am.force_state_for_test(b.id, LifecycleState::Paused);

        let mut mm = MemoryManager::new();
        mm.record("small", 50 * 1024 * 1024);
        mm.record("big", 200 * 1024 * 1024);
        let policy = LmkPolicy {
            threshold_bytes: 100 * 1024 * 1024,
            recovery_bytes: 80 * 1024 * 1024,
            max_kills_per_check: 4,
        };
        let v = pick_victims(&policy, &mm, &am, &HashSet::new(), KillReason::SystemPressure);
        assert_eq!(v[0].package_id, "big");
    }

    #[test]
    fn protected_package_never_picked() {
        let am = am_with_one_activity("sys.shell", "Main", LifecycleState::Stopped);
        let mut mm = MemoryManager::new();
        // Shell is the only thing tracked AND it's huge — but protected.
        mm.record("sys.shell", 500 * 1024 * 1024);
        let policy = LmkPolicy {
            threshold_bytes: 100 * 1024 * 1024,
            recovery_bytes: 50 * 1024 * 1024,
            max_kills_per_check: 4,
        };
        let v = pick_victims(&policy, &mm, &am, &protected_set(&["sys.shell"]), KillReason::SystemPressure);
        assert!(v.is_empty(), "shell must never be evicted, even at Empty/Background priority");
    }

    #[test]
    fn killing_stops_when_recovery_reached() {
        // Three paused apps; killing the largest brings us below recovery —
        // stop, even though the policy allows up to max_kills_per_check.
        let mut am = ActivityManager::new();
        for id in &["a", "b", "c"] {
            am.register(id, "M", LaunchMode::Standard, || Box::new(StubActivity));
            let o = am.launch(id, "M").unwrap();
            am.force_state_for_test(o.id, LifecycleState::Paused);
        }
        let mut mm = MemoryManager::new();
        mm.record("a", 200 * 1024 * 1024);
        mm.record("b", 60 * 1024 * 1024);
        mm.record("c", 60 * 1024 * 1024);
        let policy = LmkPolicy {
            threshold_bytes: 250 * 1024 * 1024,
            recovery_bytes: 150 * 1024 * 1024,
            max_kills_per_check: 10,
        };
        let v = pick_victims(&policy, &mm, &am, &HashSet::new(), KillReason::SystemPressure);
        assert_eq!(v.len(), 1, "killing 'a' (200MB) drops total to 120MB ≤ recovery (150MB)");
        assert_eq!(v[0].package_id, "a");
    }

    #[test]
    fn max_kills_caps_evictions() {
        // Five tiny background apps, all over threshold even after
        // killing one. max_kills_per_check=2 caps the response.
        let mut am = ActivityManager::new();
        let mut mm = MemoryManager::new();
        for id in &["a", "b", "c", "d", "e"] {
            am.register(id, "M", LaunchMode::Standard, || Box::new(StubActivity));
            let o = am.launch(id, "M").unwrap();
            am.force_state_for_test(o.id, LifecycleState::Stopped);
            mm.record(id, 100 * 1024 * 1024);
        }
        let policy = LmkPolicy {
            threshold_bytes: 50 * 1024 * 1024,
            recovery_bytes: 10 * 1024 * 1024,
            max_kills_per_check: 2,
        };
        let v = pick_victims(&policy, &mm, &am, &HashSet::new(), KillReason::SystemPressure);
        assert_eq!(v.len(), 2);
    }

    #[test]
    fn unranked_package_treated_as_empty_priority() {
        // Memory tracked for a package that has no activity records
        // (e.g. partially-uninstalled). Should be killed first.
        let mut am = ActivityManager::new();
        am.register("live", "M", LaunchMode::Standard, || Box::new(StubActivity));
        let l = am.launch("live", "M").unwrap();
        am.force_state_for_test(l.id, LifecycleState::Paused);

        let mut mm = MemoryManager::new();
        mm.record("ghost", 50 * 1024 * 1024);
        mm.record("live", 80 * 1024 * 1024);
        let policy = LmkPolicy {
            threshold_bytes: 100 * 1024 * 1024,
            recovery_bytes: 60 * 1024 * 1024,
            max_kills_per_check: 4,
        };
        let v = pick_victims(&policy, &mm, &am, &HashSet::new(), KillReason::SystemPressure);
        assert_eq!(v[0].package_id, "ghost", "ghost is Empty priority — killed first");
    }

    #[test]
    fn host_pressure_reason_is_preserved() {
        let am = am_with_one_activity("p", "M", LifecycleState::Stopped);
        let mut mm = MemoryManager::new();
        mm.record("p", 200 * 1024 * 1024);
        let policy = LmkPolicy {
            threshold_bytes: 100 * 1024 * 1024,
            recovery_bytes: 50 * 1024 * 1024,
            max_kills_per_check: 4,
        };
        let v = pick_victims(&policy, &mm, &am, &HashSet::new(), KillReason::HostPressure);
        assert_eq!(v[0].reason, KillReason::HostPressure);
    }

    #[test]
    fn priority_ordering_is_total_and_correct() {
        // Sanity: Empty < Background < Paused < Visible < Foreground
        assert!(Priority::Empty < Priority::Background);
        assert!(Priority::Background < Priority::Paused);
        assert!(Priority::Paused < Priority::Visible);
        assert!(Priority::Visible < Priority::Foreground);
    }

    #[test]
    fn misconfigured_recovery_above_threshold_returns_no_victims() {
        let mut mm = MemoryManager::new();
        mm.record("a", 500);
        let am = ActivityManager::new();
        let policy = LmkPolicy {
            threshold_bytes: 100,
            recovery_bytes: 200,
            max_kills_per_check: 4,
        };
        let v = pick_victims(&policy, &mm, &am, &HashSet::new(), KillReason::SystemPressure);
        assert!(v.is_empty());
    }
}
