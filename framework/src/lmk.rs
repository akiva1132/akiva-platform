//! Low-Memory Killer policy (Phase 12.3).
//!
//! Decides which package to terminate when system memory pressure
//! crosses a threshold. Priority tiers mirror Android's process-state
//! model: foreground apps are protected, background apps are
//! killable, already-trapped apps are eligible (their workers are
//! dead anyway and their `ActivityRecord`s are sitting on the 30 s
//! reap timer from P0 #3).
//!
//! ## What this module does NOT do
//!
//! - **Termination.** This module is pure policy: given the
//!   accounting state from [`crate::memory_manager`] and
//!   [`crate::activity_manager`], it returns *which* package should
//!   die. The kernel performs the actual `worker.terminate()` via
//!   the `hal_app_terminate` HAL once the framework hands back a
//!   victim.
//! - **Pressure detection.** Today the caller passes a static
//!   `threshold_bytes` (e.g. 256 MiB) and we compare against
//!   `MemoryManager::total_current()`. A future refinement could
//!   fold in `performance.memory.usedJSHeapSize` or a
//!   `MemoryPressureObserver` signal from the browser.
//! - **CPU-based killing.** Phase 12.2's `CpuBudget` data is
//!   available here for future tie-breakers (kill the heaviest
//!   memory user OR the heaviest CPU hog), but v1 ranks on memory
//!   alone for predictability.
//!
//! ## Selection rule (v1)
//!
//! 1. If `total_current_memory < threshold_bytes`: no victim.
//! 2. Otherwise scan every tracked package, compute its [`Priority`]
//!    and current memory usage. Drop foreground (we never kill
//!    what the user is actively looking at).
//! 3. Among the remaining, pick the lowest priority. Ties broken
//!    by highest memory usage (kill the biggest offender first).
//! 4. If no candidate exists (only foreground apps), no victim —
//!    log "memory pressure with no killable peer" elsewhere.
//!
//! The result is one package_id per call; if the kill doesn't
//! relieve pressure, the next frame's call yields the next victim.
//! Killing in waves rather than all-at-once gives the watchdog/trap
//! pipeline a chance to settle and avoids cascading failures.

use crate::activity_manager::{ActivityManager, ActivityId, LifecycleState};
use crate::memory_manager::MemoryManager;

/// Where a package sits in the LMK pecking order. Higher value =
/// more important to keep alive. Foreground is protected; everything
/// else is killable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Priority {
    /// Already trapped — at least one of its activity records is in
    /// `LifecycleState::Trapped`. Eligible for early reap; the 30 s
    /// `TRAPPED_GRACE_MS` window is a fallback for the *normal* case
    /// (debugger attached, tap-to-restart UI). Under memory pressure
    /// we don't have to wait.
    Trapped = 0,
    /// Has live activity records but none on the foreground task
    /// stack — the user is not actively looking at this package.
    Background = 1,
    /// At least one of this package's activities sits at the top of
    /// the foreground task. Never killed by LMK; the only kills
    /// targeting Foreground come from explicit user intent
    /// (`finish`) or hard crashes (Trapped via `worker.terminate`).
    Foreground = 2,
}

/// One row in the LMK ranking table. Exported so consumers (debug
/// overlays, future Phase 12.4 suspension policy) can introspect
/// the same data the killer uses.
#[derive(Debug, Clone)]
pub struct Candidate {
    pub package_id: String,
    pub priority: Priority,
    pub memory_bytes: u64,
}

/// Selection inputs the kernel hands the framework. Bundled in a
/// struct so future signals (CPU pressure, browser MemoryPressureObserver
/// state) can be added without churning every call site.
#[derive(Debug, Clone, Copy)]
pub struct PressureContext {
    /// Threshold to compare `MemoryManager::total_current()` against.
    /// Values below this leave the LMK idle.
    pub threshold_bytes: u64,
}

/// Compute the priority for `package_id` from the activity manager's
/// state. The foreground package is the one owning `foreground_top_id`
/// (top of the foreground stack). Trapped wins over Background if any
/// of the package's records are Trapped.
fn priority_for(
    package_id: &str,
    activities: &ActivityManager,
    foreground_pkg: Option<&str>,
) -> Priority {
    if foreground_pkg == Some(package_id) {
        return Priority::Foreground;
    }
    let mut any_trapped = false;
    let mut any_live = false;
    for rec in activities.records() {
        if rec.package_id != package_id {
            continue;
        }
        any_live = true;
        if rec.state == LifecycleState::Trapped {
            any_trapped = true;
        }
    }
    // Edge case: package has no activity records at all (installed
    // but never launched, or every activity destroyed). Treat as
    // Background — its worker may still be alive holding wasm
    // memory, and killing it is harmless.
    let _ = any_live;
    if any_trapped {
        Priority::Trapped
    } else {
        Priority::Background
    }
}

/// Build the full ranking table — one row per package the
/// `MemoryManager` knows about. Sorted by priority ascending then
/// memory descending so callers can `into_iter().next()` to get the
/// LMK victim.
pub fn rank_candidates(
    memory: &MemoryManager,
    activities: &ActivityManager,
    foreground_pkg: Option<&str>,
) -> Vec<Candidate> {
    let mut candidates: Vec<Candidate> = memory
        .iter()
        .map(|(pkg, current, _peak)| Candidate {
            package_id: pkg.to_string(),
            priority: priority_for(pkg, activities, foreground_pkg),
            memory_bytes: current,
        })
        .collect();
    // Sort: lowest priority first (kill earliest), tied by highest
    // memory first (kill the heaviest in each tier). `sort_by`
    // builds an unstable order across equal keys but the priority
    // and memory together break every tie of practical interest.
    candidates.sort_by(|a, b| {
        a.priority
            .cmp(&b.priority)
            .then(b.memory_bytes.cmp(&a.memory_bytes))
    });
    candidates
}

/// Pick the package to kill, if any. Returns `None` when total
/// memory is under threshold OR the only candidates are Foreground.
pub fn select_victim(
    memory: &MemoryManager,
    activities: &ActivityManager,
    foreground_pkg: Option<&str>,
    pressure: PressureContext,
) -> Option<String> {
    if memory.total_current() < pressure.threshold_bytes {
        return None;
    }
    let candidates = rank_candidates(memory, activities, foreground_pkg);
    candidates
        .into_iter()
        .find(|c| c.priority != Priority::Foreground)
        .map(|c| c.package_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::activity_manager::{LaunchMode, LifecycleState};

    /// Helper: register a package + class with `ActivityManager`,
    /// launch one activity, optionally drive it to a target state,
    /// optionally mark it Trapped. Returns the activity_id.
    fn install_and_launch(
        am: &mut ActivityManager,
        package_id: &str,
        class: &str,
        target_state: LifecycleState,
    ) -> ActivityId {
        am.register(package_id, class, LaunchMode::Standard, || {
            #[derive(Default)]
            struct Stub;
            impl crate::activity::Activity for Stub {
                fn render(
                    &mut self,
                    _: &mut crate::activity::ActivityContext<'_>,
                    _: &mut crate::compositor::FrameCommands,
                ) {}
            }
            Box::new(Stub)
        });
        let id = am.launch(package_id, class).unwrap().id;
        // Crude state forcing for tests — production callers go
        // through `advance_through_create` / `mark_trapped` etc.
        // We only need state observation, not lifecycle correctness.
        if target_state == LifecycleState::Trapped {
            am.mark_trapped(id, 0.0);
        }
        id
    }

    fn fresh() -> (MemoryManager, ActivityManager) {
        (MemoryManager::new(), ActivityManager::new())
    }

    #[test]
    fn no_victim_when_total_memory_is_under_threshold() {
        let (mut mem, am) = fresh();
        mem.record("a", 50_000_000);  // 50 MB
        mem.record("b", 30_000_000);  // 30 MB
        let v = select_victim(&mem, &am, None, PressureContext {
            threshold_bytes: 256 * 1024 * 1024,
        });
        assert_eq!(v, None);
    }

    #[test]
    fn no_victim_when_only_foreground_packages_track_memory() {
        let (mut mem, mut am) = fresh();
        let _id = install_and_launch(&mut am, "com.app.foreground", "Main", LifecycleState::Initial);
        mem.record("com.app.foreground", 300_000_000);
        // Threshold tripped, but the only candidate is the foreground
        // package — we refuse to kill what the user is looking at.
        let v = select_victim(&mem, &am, Some("com.app.foreground"), PressureContext {
            threshold_bytes: 256 * 1024 * 1024,
        });
        assert_eq!(v, None);
    }

    #[test]
    fn picks_background_over_foreground_when_threshold_tripped() {
        let (mut mem, mut am) = fresh();
        let _bg = install_and_launch(&mut am, "com.app.bg", "Main", LifecycleState::Initial);
        let _fg = install_and_launch(&mut am, "com.app.fg", "Main", LifecycleState::Initial);
        mem.record("com.app.fg", 200_000_000);
        mem.record("com.app.bg", 100_000_000);
        // Total = 300 MB > 256 MB threshold → must pick the background app.
        let v = select_victim(&mem, &am, Some("com.app.fg"), PressureContext {
            threshold_bytes: 256 * 1024 * 1024,
        });
        assert_eq!(v, Some("com.app.bg".to_string()));
    }

    #[test]
    fn prefers_trapped_package_over_background() {
        let (mut mem, mut am) = fresh();
        let _trapped = install_and_launch(&mut am, "com.app.dead", "Main", LifecycleState::Trapped);
        let _bg = install_and_launch(&mut am, "com.app.bg", "Main", LifecycleState::Initial);
        mem.record("com.app.dead", 50_000_000);
        mem.record("com.app.bg", 200_000_000);
        // Threshold tripped (250 MB > 256-megs sounds wrong — let me re-check)
        let v = select_victim(&mem, &am, None, PressureContext {
            threshold_bytes: 200 * 1024 * 1024,
        });
        // 250 MB total > 200 MB threshold → kill the trapped one
        // first even though it's smaller, because it's already dead
        // and reclaiming costs nothing observable to the user.
        assert_eq!(v, Some("com.app.dead".to_string()));
    }

    #[test]
    fn within_a_tier_picks_the_heaviest_user() {
        // Two background apps, both eligible — kill the bigger one.
        let (mut mem, mut am) = fresh();
        let _a = install_and_launch(&mut am, "com.app.a", "Main", LifecycleState::Initial);
        let _b = install_and_launch(&mut am, "com.app.b", "Main", LifecycleState::Initial);
        mem.record("com.app.a", 60_000_000);
        mem.record("com.app.b", 240_000_000);
        let v = select_victim(&mem, &am, None, PressureContext {
            threshold_bytes: 256 * 1024 * 1024,
        });
        assert_eq!(v, Some("com.app.b".to_string()));
    }

    #[test]
    fn ranks_packages_with_no_records_as_background() {
        // A package may exist in the memory tracker without any live
        // activity records (worker spawned, all activities destroyed,
        // worker still alive holding wasm memory). Should be treated
        // as Background — killable.
        let (mut mem, am) = fresh();
        mem.record("com.app.zombie", 300_000_000);
        let v = select_victim(&mem, &am, None, PressureContext {
            threshold_bytes: 200 * 1024 * 1024,
        });
        assert_eq!(v, Some("com.app.zombie".to_string()));
    }

    #[test]
    fn rank_candidates_returns_full_table_sorted() {
        let (mut mem, mut am) = fresh();
        let _t = install_and_launch(&mut am, "trapped", "Main", LifecycleState::Trapped);
        let _b = install_and_launch(&mut am, "bg", "Main", LifecycleState::Initial);
        let _f = install_and_launch(&mut am, "fg", "Main", LifecycleState::Initial);
        mem.record("trapped", 30_000_000);
        mem.record("bg", 70_000_000);
        mem.record("fg", 200_000_000);
        let table = rank_candidates(&mem, &am, Some("fg"));
        assert_eq!(table.len(), 3);
        // Sorted: Trapped first, then Background, then Foreground last.
        assert_eq!(table[0].package_id, "trapped");
        assert_eq!(table[0].priority, Priority::Trapped);
        assert_eq!(table[1].package_id, "bg");
        assert_eq!(table[1].priority, Priority::Background);
        assert_eq!(table[2].package_id, "fg");
        assert_eq!(table[2].priority, Priority::Foreground);
    }
}
