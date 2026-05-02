//! Activity manager.
//!
//! Owns the activity lifecycle state machine documented in
//! docs/activity-model.md and the task stack. An `ActivityRecord` pairs a
//! boxed `dyn Activity` impl with its current lifecycle state; the manager
//! is the only thing that mutates that state, and it enforces the legal
//! transitions.
//!
//! MVP scope:
//!   - Multiple tasks on a single display. One task is `foreground_task`
//!     at a time; the Framework is responsible for driving lifecycle
//!     callbacks when the foreground switches.
//!   - Launch modes: all four (`Standard`, `SingleTop`, `SingleTask`,
//!     `SingleInstance`). Standard / SingleTop / SingleTask fresh-create
//!     lands in the current foreground task (task-by-affinity routing
//!     is a later optimisation — with a single display the visible
//!     behaviour is identical). `SingleInstance` gets a dedicated,
//!     system-wide-unique task: reuse scans *every* task, fresh-create
//!     allocates a new task just for that activity.
//!   - Activities are system-side only (registered by the framework at
//!     boot). User-app wasm activities come through the wasm activity
//!     proxy on the same path.

use std::collections::{HashMap, HashSet};

use crate::activity::Activity;

pub type ActivityId = u64;
pub type TaskId = u64;

/// Lifecycle states, ordered by "liveness": higher variants imply all
/// preceding callbacks have fired.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LifecycleState {
    /// Activity instance exists but `on_create` hasn't run yet. Only used
    /// for the brief window inside `launch()`.
    Initial,
    Created,
    Started,
    Resumed,
    Paused,
    Stopped,
    Destroyed,
    /// Stone 11.5 — the worker that owned this activity trapped (panic,
    /// loop {} caught by the watchdog, OOM, JS-side `worker.onerror`).
    /// The `Activity` instance still exists but the framework refuses
    /// every dispatch into it; only edge out is `Destroyed` (driven by
    /// the user's tap-to-restart or by package uninstall).
    Trapped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchMode {
    Standard,
    SingleTop,
    SingleTask,
    SingleInstance,
}

#[derive(Debug)]
pub enum LaunchError {
    UnknownActivity { package_id: String, class: String },
}

/// Outcome of a [`ActivityManager::launch`] call. Richer than a bare
/// `ActivityId` because SingleTask / SingleInstance reuse may require the
/// caller (Framework) to destroy activities that were stacked above the
/// returned target.
///
/// `popped` is in **top-first** order: the caller must drive destroy on
/// `popped[0]` first (it was the current top) then walk downward. For
/// fresh-create and top-reuse paths, `popped` is empty.
#[derive(Debug)]
pub struct LaunchOutcome {
    pub id: ActivityId,
    /// `true` iff an existing instance was returned (SingleTop top match,
    /// or SingleTask / SingleInstance finding a live instance). `false`
    /// for every fresh-create path.
    pub reused: bool,
    /// Activities stacked above `id` in its task that the caller must
    /// destroy to complete the reuse. Top-first order. Empty unless
    /// SingleTask / SingleInstance reused a non-top instance.
    pub popped: Vec<ActivityId>,
}

/// A lifecycle/input/render dispatch the host is still working on.
///
/// The framework records one of these on `ActivityRecord` whenever a
/// `WasmHost` method returns a non-zero `future_id` — the call hasn't
/// finished yet (Stone 11.4c onwards: the host had to round-trip to
/// the package's worker). Until the framework polls the future and
/// gets a `Some(...)` back, no further dispatch may target the same
/// activity. When the future resolves the framework drains it,
/// advances `state` to `target`, and clears the slot.
///
/// In Stone 11.4a the slot is wired up but never populated — every
/// host method still resolves synchronously and returns `0`. The
/// scaffolding ships now so the deferred path can be exercised and
/// tested before 11.4c flips real workers on.
#[derive(Debug, Clone)]
pub struct PendingTransition {
    /// State the activity should reach once the future resolves.
    pub target: LifecycleState,
    /// `WasmHost::poll_future` key. Non-zero by construction — `0` is
    /// the "resolved synchronously" sentinel from `WasmHost` calls.
    pub future_id: u64,
    /// Frame timestamp at which the future was opened. The watchdog in
    /// Stone 11.5 compares this against `now` to decide when to mark
    /// the activity `Trapped`.
    pub started_at_ms: f64,
}

/// Metadata + lifecycle slot for a single activity instance. The
/// `Activity` trait object is stored in a sibling map so the record can
/// remain `Debug` and trivially-`Clone`-less.
#[derive(Debug)]
pub struct ActivityRecord {
    pub id: ActivityId,
    pub task: TaskId,
    pub package_id: String,
    pub class: String,
    pub state: LifecycleState,
    pub launch_mode: LaunchMode,
    /// Set when the most recent dispatch into the host hasn't resolved
    /// yet (Stone 11.4a scaffolding; populated by 11.4c). The framework
    /// polls and drains this each frame via
    /// [`Framework::drive_pending_transitions`] (Stone 11.4a), and
    /// declines to fire fresh dispatches at this activity until it
    /// clears.
    pub pending: Option<PendingTransition>,
    /// P0 #3 — frame timestamp at which the activity was marked
    /// `Trapped`. `None` for activities that haven't trapped (the
    /// common case). [`Framework::reap_trapped`] compares this against
    /// `now_ms` and drops records whose age exceeds the configured
    /// grace window — a real Android-equivalent of "the OS gives a
    /// crashed process some time to be re-launched manually before
    /// recycling its slot".
    pub trapped_at_ms: Option<f64>,
}

#[derive(Debug)]
pub struct Task {
    pub id: TaskId,
    pub affinity: String,
    /// Bottom-to-top stack of activity IDs. The last element is the top.
    pub stack: Vec<ActivityId>,
}

/// A factory produces a fresh instance of a system activity. Registered
/// at framework boot so `launch()` can stamp out new instances without
/// reflection or dynamic dispatch on construction.
type ActivityFactory = Box<dyn Fn() -> Box<dyn Activity> + 'static>;

/// Key into the registry: `(package_id, class)`.
type ActivityKey = (String, String);

pub struct ActivityManager {
    records: HashMap<ActivityId, ActivityRecord>,
    instances: HashMap<ActivityId, Box<dyn Activity>>,
    tasks: HashMap<TaskId, Task>,
    registry: HashMap<ActivityKey, (LaunchMode, ActivityFactory)>,
    foreground_task: Option<TaskId>,
    next_activity_id: ActivityId,
    next_task_id: TaskId,
}

impl ActivityManager {
    pub fn new() -> Self {
        Self {
            records: HashMap::new(),
            instances: HashMap::new(),
            tasks: HashMap::new(),
            registry: HashMap::new(),
            foreground_task: None,
            next_activity_id: 1,
            next_task_id: 1,
        }
    }

    /// Register a system activity. Must be called during framework boot —
    /// before any `launch()` that might reference it.
    pub fn register<F>(&mut self, package_id: &str, class: &str, launch_mode: LaunchMode, factory: F)
    where
        F: Fn() -> Box<dyn Activity> + 'static,
    {
        self.registry.insert(
            (package_id.to_string(), class.to_string()),
            (launch_mode, Box::new(factory)),
        );
    }

    // ── Launch ───────────────────────────────────────────────────────────

    /// Launch an activity. Returns a [`LaunchOutcome`] describing whether
    /// the caller is getting a fresh instance or reusing an existing one,
    /// plus any activities stacked above the reused target that the
    /// caller must destroy to complete the reuse (SingleTask /
    /// SingleInstance only; empty otherwise).
    ///
    /// This method never mutates the lifecycle state of popped
    /// activities — it only identifies them. The Framework is what owns
    /// the callback dispatch, so it must walk `popped` and call
    /// `destroy()` on each in order. Task-stack bookkeeping happens as a
    /// side effect of those `destroy()` calls (via `task.stack.retain`),
    /// so a caller that ignores `popped` leaves the system in an
    /// inconsistent state. That's why [`LaunchOutcome`] is a struct —
    /// ignoring the popped list should be a conscious decision, not a
    /// silent one.
    pub fn launch(&mut self, package_id: &str, class: &str) -> Result<LaunchOutcome, LaunchError> {
        let key = (package_id.to_string(), class.to_string());
        let (launch_mode, factory) = self.registry.get(&key).ok_or_else(|| {
            LaunchError::UnknownActivity { package_id: package_id.into(), class: class.into() }
        })?;
        let launch_mode = *launch_mode;

        // SingleTop: reuse only when the current top exactly matches.
        // No pop — anything below stays put. `on_new_intent` delivery is
        // the Framework's job via the reused=true path.
        if launch_mode == LaunchMode::SingleTop {
            if let Some(top_id) = self.foreground_top() {
                let rec = &self.records[&top_id];
                if rec.package_id == package_id && rec.class == class {
                    return Ok(LaunchOutcome { id: top_id, reused: true, popped: Vec::new() });
                }
            }
        }

        // SingleTask: find an existing instance in the foreground task.
        // If found, the caller will destroy everything above it
        // (top-first) and resume the target. If not found, fall through
        // to fresh-create.
        if launch_mode == LaunchMode::SingleTask {
            if let Some(task_id) = self.foreground_task {
                let task_stack = &self.tasks[&task_id].stack;
                let target_pos = task_stack.iter().position(|id| {
                    let r = &self.records[id];
                    r.package_id == package_id && r.class == class
                });
                if let Some(pos) = target_pos {
                    let target_id = task_stack[pos];
                    let popped: Vec<ActivityId> = task_stack[pos + 1..].iter().rev().copied().collect();
                    return Ok(LaunchOutcome { id: target_id, reused: true, popped });
                }
            }
        }

        // SingleInstance: system-wide uniqueness. Scan every task (not
        // just foreground) for a matching (pkg, class). A live instance
        // means we reuse it; whichever task it lives in becomes the
        // caller's responsibility to bring to the foreground. The
        // dedicated-task invariant guarantees popped is empty — nothing
        // else is ever in that task.
        if launch_mode == LaunchMode::SingleInstance {
            let hit = self.tasks.values().find_map(|t| {
                t.stack.iter().copied().find(|id| {
                    let r = &self.records[id];
                    r.package_id == package_id && r.class == class
                })
            });
            if let Some(target_id) = hit {
                return Ok(LaunchOutcome { id: target_id, reused: true, popped: Vec::new() });
            }
        }

        // Fresh-create path — shared by Standard, SingleTop-no-match,
        // SingleTask-not-found, and SingleInstance-not-found.
        let instance = (factory)();
        let id = self.next_activity_id;
        self.next_activity_id += 1;

        // SingleInstance lands in a freshly-allocated dedicated task;
        // every other mode shares the foreground task. The Framework
        // is responsible for calling `bring_task_to_front` on the new
        // task after `launch` returns — `launch` doesn't flip foreground
        // itself so the caller can pause the old top first.
        let task_id = if launch_mode == LaunchMode::SingleInstance {
            self.allocate_task(&format!("__singleInstance__:{}:{}", package_id, class))
        } else {
            self.ensure_foreground_task(package_id)
        };
        self.tasks.get_mut(&task_id).unwrap().stack.push(id);

        self.records.insert(id, ActivityRecord {
            id,
            task: task_id,
            package_id: package_id.into(),
            class: class.into(),
            state: LifecycleState::Initial,
            launch_mode,
            pending: None,
            trapped_at_ms: None,
        });
        self.instances.insert(id, instance);
        Ok(LaunchOutcome { id, reused: false, popped: Vec::new() })
    }

    fn ensure_foreground_task(&mut self, affinity: &str) -> TaskId {
        if let Some(t) = self.foreground_task {
            return t;
        }
        let id = self.allocate_task(affinity);
        self.foreground_task = Some(id);
        id
    }

    /// Allocate a new task without touching `foreground_task`. Used for
    /// the SingleInstance dedicated-task path, where the Framework owns
    /// the decision to bring the new task forward (so it can pause the
    /// old foreground top first).
    fn allocate_task(&mut self, affinity: &str) -> TaskId {
        let id = self.next_task_id;
        self.next_task_id += 1;
        self.tasks.insert(id, Task { id, affinity: affinity.into(), stack: Vec::new() });
        id
    }

    /// Move `task_id` to the foreground, returning the previous
    /// foreground task. Returns `None` if the task doesn't exist OR if
    /// it was already foreground (the caller reads this as "nothing to
    /// do"). The state machine of the activities themselves is not
    /// touched — the Framework is responsible for pausing the old top
    /// and resuming the new one.
    pub fn bring_task_to_front(&mut self, task_id: TaskId) -> Option<TaskId> {
        if !self.tasks.contains_key(&task_id) { return None; }
        let prev = self.foreground_task;
        if prev == Some(task_id) { return None; }
        self.foreground_task = Some(task_id);
        prev
    }

    // ── Introspection ────────────────────────────────────────────────────

    pub fn foreground_task(&self) -> Option<TaskId> { self.foreground_task }

    pub fn foreground_top(&self) -> Option<ActivityId> {
        let t = self.tasks.get(&self.foreground_task?)?;
        t.stack.last().copied()
    }

    /// Bottom-to-top stack of activity ids in `task_id`, or `None` if
    /// the task doesn't exist. Used by the Framework to sync its own
    /// `stack` mirror when the foreground switches.
    pub fn task_stack(&self, task_id: TaskId) -> Option<&[ActivityId]> {
        self.tasks.get(&task_id).map(|t| t.stack.as_slice())
    }

    pub fn state_of(&self, id: ActivityId) -> Option<LifecycleState> {
        self.records.get(&id).map(|r| r.state)
    }

    pub fn record(&self, id: ActivityId) -> Option<&ActivityRecord> {
        self.records.get(&id)
    }

    /// Iterator over every live `ActivityRecord` regardless of state.
    /// Order is HashMap-iteration (i.e. unspecified). Used by Phase
    /// 12.3 (LMK) to derive per-package priority — "does this
    /// package have any non-Trapped activity?" — without exposing
    /// the records HashMap directly.
    pub fn records(&self) -> impl Iterator<Item = &ActivityRecord> + '_ {
        self.records.values()
    }

    // ── Lifecycle drivers ────────────────────────────────────────────────
    //
    // Each driver takes `&mut self` *and* an `activity_access` closure that
    // receives the instance + its record, so callers can pass the context.
    // This keeps the borrow checker happy despite the `HashMap` split
    // between `records` and `instances`.

    /// Drive an activity up to `Resumed`. Fires every missing callback in
    /// order — works equally well from `Initial` (bring-up chain) and from
    /// `Paused` (just the single `Paused → Resumed` edge). No-op if already
    /// at `Resumed`. A `None` from `advance_to` means "that edge isn't
    /// legal from the current state", so we skip to the next target rather
    /// than bail — otherwise calling `resume()` on a paused activity would
    /// stop at the first illegal `Paused → Created` check.
    pub fn resume(
        &mut self,
        id: ActivityId,
        mut cb: impl FnMut(&mut dyn Activity, LifecycleState),
    ) {
        for target in [
            LifecycleState::Created,
            LifecycleState::Started,
            LifecycleState::Resumed,
        ] {
            let _ = self.advance_to(id, target, &mut cb);
        }
    }

    /// Single-step advance. Exposed so the framework can pause the state
    /// machine partway through bring-up — e.g. to inject a saved-state
    /// restore between `Created` and `Started`. Returns `Some(prev)` if
    /// the edge fired, `None` if the transition wasn't legal.
    pub fn advance(
        &mut self,
        id: ActivityId,
        target: LifecycleState,
        mut cb: impl FnMut(&mut dyn Activity, LifecycleState),
    ) -> Option<LifecycleState> {
        self.advance_to(id, target, &mut cb)
    }

    /// Drive an activity down to `Paused` only. Used when a new activity
    /// is foregrounded on top of this one — the underlying activity must
    /// pause (so it stops receiving input) but must *not* be destroyed.
    /// No-op if already at or past `Paused`.
    pub fn pause(
        &mut self,
        id: ActivityId,
        mut cb: impl FnMut(&mut dyn Activity, LifecycleState),
    ) {
        let _ = self.advance_to(id, LifecycleState::Paused, &mut cb);
    }

    /// Drive an activity down to `Destroyed`. Fires pause → stop →
    /// destroy in order, skipping any edge that isn't currently legal
    /// from the activity's state (so `destroy` from `Paused` fires
    /// just stop+destroy; from `Stopped` just destroy).
    ///
    /// Implementation note: we intentionally ignore `None` from
    /// `advance_to` and continue trying the next target, rather than
    /// breaking out of the loop. The previous behaviour broke out on
    /// the first rejected edge, which meant destroying a `Paused`
    /// activity silently skipped `stop` *and* `destroy` because
    /// `Paused → Paused` isn't a legal self-loop. Same `for _ in …
    /// { let _ = … }` shape as `resume`.
    pub fn destroy(
        &mut self,
        id: ActivityId,
        mut cb: impl FnMut(&mut dyn Activity, LifecycleState),
    ) {
        for target in [
            LifecycleState::Paused,
            LifecycleState::Stopped,
            LifecycleState::Destroyed,
        ] {
            let _ = self.advance_to(id, target, &mut cb);
        }
        // Only tear down the instance + stack entry once we actually reached
        // `Destroyed`. An out-of-order call (e.g. `destroy()` on an `Initial`
        // activity — no transitions legal, no callbacks fired) would
        // otherwise silently drop the instance and leave the record stuck at
        // `Initial`. Leaking a record is better than inventing an `on_destroy`
        // that never had a matching `on_create`.
        if self.records.get(&id).map(|r| r.state) != Some(LifecycleState::Destroyed) {
            return;
        }
        self.instances.remove(&id);
        // Keep the record so observers can see `Destroyed`; the next GC
        // pass (Phase 2b) will prune it.
        if let Some(t) = self.records.get(&id).map(|r| r.task) {
            if let Some(task) = self.tasks.get_mut(&t) {
                task.stack.retain(|x| *x != id);
            }
        }
    }

    /// Advance `id` to exactly `target`. Returns `Some(prev)` if we moved,
    /// `None` if we couldn't (unknown id, missing instance, illegal
    /// transition, or already past). The callback is invoked once per
    /// transition so the framework can call the corresponding trait method
    /// with a fresh context.
    ///
    /// Implementation note: we check `instances` *before* flipping
    /// `record.state`. An instance-less record is treated as "nothing to
    /// advance" and the record's state is left untouched — otherwise a
    /// caller who saw `None` would still face a silently-mutated state.
    /// In practice `instances` is only drained at the end of `destroy()`
    /// (when the state is already `Destroyed`, blocking every transition
    /// anyway) so this guard is defensive — but cheap, and it keeps
    /// "state flipped" and "cb fired" atomic from the outside.
    fn advance_to(
        &mut self,
        id: ActivityId,
        target: LifecycleState,
        cb: &mut dyn FnMut(&mut dyn Activity, LifecycleState),
    ) -> Option<LifecycleState> {
        let prev = self.records.get(&id)?.state;
        if !legal_transition(prev, target) { return None; }
        if !self.instances.contains_key(&id) { return None; }
        self.records.get_mut(&id).expect("record checked above").state = target;
        let inst = self.instances.get_mut(&id).expect("instance checked above");
        cb(inst.as_mut(), target);
        Some(prev)
    }

    /// Run an arbitrary closure against a live activity instance (not a
    /// lifecycle transition — used for input dispatch, render, etc.).
    pub fn with_instance(
        &mut self,
        id: ActivityId,
        f: impl FnOnce(&mut dyn Activity),
    ) -> bool {
        match self.instances.get_mut(&id) {
            Some(inst) => { f(inst.as_mut()); true }
            None => false,
        }
    }

    /// Like [`Self::with_instance`] but returns the closure's value (or
    /// `None` if the instance isn't live). Used by Framework helpers
    /// that need to read a method's return value out of the borrow —
    /// e.g. `dispatch_save_state_async` returning a future_id the
    /// framework records for later polling.
    pub fn with_instance_ret<R>(
        &mut self,
        id: ActivityId,
        f: impl FnOnce(&mut dyn Activity) -> R,
    ) -> Option<R> {
        self.instances.get_mut(&id).map(|inst| f(inst.as_mut()))
    }

    // ── Stone 11.4a-5: deferred lifecycle plumbing ───────────────────────
    //
    // `pending` slots are populated by the deferred dispatch path (Stone
    // 11.4c WorkerHost) when a host call returns a non-zero `future_id`
    // instead of resolving inline. The frame loop calls
    // `drive_pending_transitions` to poll the host and flip the record's
    // state once the future resolves.
    //
    // In Stone 11.4a these methods are exercised only by tests — the
    // production `WasmActivity` path is still sync (every dispatch returns
    // 0). The plumbing ships now so 11.4c can wire it up to a real Worker
    // without churning the public API again.

    /// Mark `id` as awaiting a host-side completion. Returns `true` if the
    /// slot was opened, `false` if the activity is unknown, missing its
    /// instance, already has a pending slot, or `target` isn't a legal
    /// next state. `started_at_ms` is the frame timestamp when the
    /// dispatch fired — Stone 11.5's watchdog uses it for the deadline.
    pub fn open_pending(
        &mut self,
        id: ActivityId,
        target: LifecycleState,
        future_id: u64,
        started_at_ms: f64,
    ) -> bool {
        let Some(rec) = self.records.get_mut(&id) else { return false; };
        if rec.pending.is_some() { return false; }
        if !self.instances.contains_key(&id) { return false; }
        if !legal_transition(rec.state, target) { return false; }
        rec.pending = Some(PendingTransition { target, future_id, started_at_ms });
        true
    }

    /// Read-only view of an activity's pending slot. Used by the
    /// Framework wrapper to drive the poll loop without holding a `&mut`
    /// across the borrow.
    pub fn pending_of(&self, id: ActivityId) -> Option<&PendingTransition> {
        self.records.get(&id).and_then(|r| r.pending.as_ref())
    }

    /// Resolve a pending transition: clear the slot and flip the record's
    /// state to the target. The caller must have observed the host's
    /// future as resolved before calling. No callback fires — the work
    /// already ran in the worker; this only updates framework
    /// bookkeeping.
    ///
    /// Returns `Some(prev)` on success, `None` if the slot wasn't open,
    /// the id is unknown, or the recorded transition isn't legal anymore
    /// (the latter shouldn't happen — `open_pending` already checked,
    /// and nothing else mutates state once a slot is open).
    pub fn resolve_pending(&mut self, id: ActivityId) -> Option<LifecycleState> {
        let rec = self.records.get_mut(&id)?;
        let p = rec.pending.take()?;
        let prev = rec.state;
        if !legal_transition(prev, p.target) { return None; }
        rec.state = p.target;
        Some(prev)
    }

    /// Cancel a pending slot without advancing state. Used when the host
    /// reports a trap (Stone 11.5 will follow up with a `Trapped` mark);
    /// today the slot just clears and the activity stays at its previous
    /// state.
    pub fn cancel_pending(&mut self, id: ActivityId) -> Option<PendingTransition> {
        self.records.get_mut(&id)?.pending.take()
    }

    /// All ids with an open pending slot, in arbitrary order. Snapshot —
    /// callers may mutate `pending` while iterating their own copy of
    /// the result.
    pub fn pending_ids(&self) -> Vec<ActivityId> {
        self.records.iter()
            .filter(|(_, r)| r.pending.is_some())
            .map(|(id, _)| *id)
            .collect()
    }

    // ── Stone 11.5: trap handling ─────────────────────────────────────────

    /// Mark `id` as Trapped — the worker that owned it panicked, hung,
    /// or got terminated by the watchdog. Returns `Some(prev)` on success
    /// (the activity transitioned from a live state to `Trapped`) or
    /// `None` if the id is unknown, the instance is gone, or the activity
    /// was already Trapped/Destroyed (idempotent — re-trap is a no-op).
    /// Any open pending slot clears: the future will never resolve.
    ///
    /// `now_ms` is the frame timestamp at which the trap was observed —
    /// stored on the record so [`Framework::reap_trapped`] can age the
    /// entry out after the configured grace window.
    pub fn mark_trapped(&mut self, id: ActivityId, now_ms: f64) -> Option<LifecycleState> {
        let rec = self.records.get_mut(&id)?;
        if !legal_transition(rec.state, LifecycleState::Trapped) {
            return None;
        }
        if !self.instances.contains_key(&id) {
            return None;
        }
        let prev = rec.state;
        rec.state = LifecycleState::Trapped;
        rec.pending = None;
        rec.trapped_at_ms = Some(now_ms);
        Some(prev)
    }

    /// All activity ids belonging to `package_id`. Used by the watchdog
    /// to mark every activity of a terminated worker as Trapped in one
    /// pass without iterating records externally.
    pub fn ids_for_package(&self, package_id: &str) -> Vec<ActivityId> {
        self.records.iter()
            .filter(|(_, r)| r.package_id == package_id)
            .map(|(id, _)| *id)
            .collect()
    }

    /// All activity ids in `package_id` whose state is currently `Trapped`.
    /// Used by `Framework::mark_package_trapped` to auto-pop dead
    /// activities from their task stacks.
    pub fn trapped_ids_for_package(&self, package_id: &str) -> Vec<ActivityId> {
        self.records.iter()
            .filter(|(_, r)|
                r.package_id == package_id
                && matches!(r.state, LifecycleState::Trapped))
            .map(|(id, _)| *id)
            .collect()
    }

    /// Strip a set of activity ids out of every task's stack. Used by
    /// `Framework::mark_package_trapped` to remove dead activities
    /// from the foreground without driving the legal destroy chain
    /// (which would land dispatches in a dead worker).
    pub fn evict_from_task_stacks(&mut self, ids: &[ActivityId]) {
        if ids.is_empty() { return; }
        let set: HashSet<ActivityId> = ids.iter().copied().collect();
        for task in self.tasks.values_mut() {
            task.stack.retain(|id| !set.contains(id));
        }
    }

    // ── P0 #3: Trapped recycle ────────────────────────────────────────────

    /// Trapped activity ids whose `trapped_at_ms` is older than
    /// `now_ms - max_age_ms`. Records that landed in `Trapped` without
    /// a timestamp (legacy callers from before P0 #3 wired the
    /// parameter through) are considered ageless and won't be reaped —
    /// the safe choice when in doubt.
    pub fn reap_trapped_ids(&self, now_ms: f64, max_age_ms: f64) -> Vec<ActivityId> {
        self.records.iter()
            .filter(|(_, r)| {
                matches!(r.state, LifecycleState::Trapped)
                    && r.trapped_at_ms.map_or(false, |t| now_ms - t > max_age_ms)
            })
            .map(|(id, _)| *id)
            .collect()
    }

    /// Recycle a Trapped record: drop the instance and remove the
    /// record entirely. No lifecycle callbacks fire — the worker that
    /// owned the activity is dead, and dispatching `on_destroy` would
    /// either time out or land in a recycled handle slot.
    ///
    /// Returns `Some(())` on success, `None` if the id is unknown or
    /// the record isn't actually `Trapped` (e.g. concurrent recovery
    /// flipped it to `Destroyed` first via tap-to-restart). Idempotent
    /// against the second call: the second `reap` returns `None`.
    ///
    /// The caller is responsible for purging any task-stack references
    /// to `id` first via [`Self::evict_from_task_stacks`] —
    /// `Framework::mark_package_trapped` already does this at trap
    /// time so reap normally finds the stack already clean.
    pub fn reap(&mut self, id: ActivityId) -> Option<()> {
        let rec = self.records.get(&id)?;
        if !matches!(rec.state, LifecycleState::Trapped) {
            return None;
        }
        self.records.remove(&id);
        self.instances.remove(&id);
        Some(())
    }
}

/// Decide whether `from → to` is one of the legal one-step transitions in
/// docs/activity-model.md §2. Treats `from == to` as illegal (idempotent
/// resume/pause is the caller's job to short-circuit).
///
/// `Trapped` is special: every live state can edge into it (a worker can
/// trap at any moment), and the only edge OUT is to `Destroyed` (which
/// the user triggers via the Shell's tap-to-restart UI in Stone 11.5d).
fn legal_transition(from: LifecycleState, to: LifecycleState) -> bool {
    use LifecycleState::*;
    if matches!(to, Trapped) {
        // Any live state can trap — `loop {}` in `on_pointer`, panic in
        // `on_resume`, OOM mid-render. Only Destroyed/Trapped can't
        // re-trap (already terminal).
        return !matches!(from, Destroyed | Trapped);
    }
    if matches!(from, Trapped) {
        // The single recovery edge: tear the activity down so the user
        // can restart with a fresh instance. Stone 11.5d wires the UX.
        return matches!(to, Destroyed);
    }
    matches!(
        (from, to),
        (Initial, Created)
        | (Created, Started)
        | (Started, Resumed)
        | (Resumed, Paused)
        | (Paused, Resumed)
        | (Paused, Stopped)
        | (Stopped, Started)    // onRestart → onStart
        | (Stopped, Destroyed)
        | (Paused, Destroyed)   // allowed when activity finishes from Paused
        | (Created, Destroyed)  // finish() called from on_create — skip Start/Resume entirely, Android parity
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::activity::{ActivityContext, PointerEvent, SurfaceMetrics};
    use crate::compositor::FrameCommands;
    use std::cell::RefCell;
    use std::rc::Rc;

    #[test]
    fn legal_path_create_to_destroy() {
        use LifecycleState::*;
        for (from, to) in [
            (Initial, Created),
            (Created, Started),
            (Started, Resumed),
            (Resumed, Paused),
            (Paused, Stopped),
            (Stopped, Destroyed),
        ] {
            assert!(legal_transition(from, to), "{:?} → {:?}", from, to);
        }
    }

    #[test]
    fn illegal_skips_rejected() {
        use LifecycleState::*;
        assert!(!legal_transition(Initial, Resumed));
        assert!(!legal_transition(Created, Resumed));
        assert!(!legal_transition(Resumed, Stopped));
        assert!(!legal_transition(Destroyed, Created));
    }

    #[test]
    fn created_to_destroyed_shortcut_is_legal() {
        // Stone 9.3 — added to match Android's finish()-in-on_create
        // behaviour. Lock this edge in so a future matrix cleanup
        // doesn't quietly drop it.
        assert!(legal_transition(LifecycleState::Created, LifecycleState::Destroyed));
    }

    // ── Stone 11.5: Trapped transition matrix ────────────────────────────

    #[test]
    fn every_live_state_can_trap() {
        use LifecycleState::*;
        for from in [Initial, Created, Started, Resumed, Paused, Stopped] {
            assert!(legal_transition(from, Trapped),
                "{:?} → Trapped should be legal — workers can panic from any state",
                from);
        }
    }

    #[test]
    fn already_dead_states_cannot_re_trap() {
        use LifecycleState::*;
        assert!(!legal_transition(Destroyed, Trapped),
            "Destroyed activities have no instance; trapping them would be a wiring bug");
        assert!(!legal_transition(Trapped, Trapped),
            "Re-trap is a no-op — `mark_trapped` is idempotent on already-trapped activities");
    }

    #[test]
    fn trapped_only_recovers_via_destroy() {
        use LifecycleState::*;
        for to in [Initial, Created, Started, Resumed, Paused, Stopped] {
            assert!(!legal_transition(Trapped, to),
                "Trapped → {:?} should be illegal; only Destroyed is the recovery edge",
                to);
        }
        assert!(legal_transition(Trapped, Destroyed),
            "Tap-to-restart routes Trapped → Destroyed before re-launching the activity fresh");
    }

    // ── Driver tests ─────────────────────────────────────────────────────
    //
    // The legality matrix is validated above in isolation; these tests
    // exercise the `resume` / `pause` / `destroy` drivers end-to-end. The
    // state machine is cheap to get wrong in subtle ways (silently skipping
    // an edge, firing the callback before flipping state, double-firing on
    // idempotent calls), and the framework's bring-up / tear-down paths all
    // route through these drivers — so a regression here would land in every
    // running app at once.
    //
    // `RecordingActivity` logs every callback it receives, along with the
    // state label, into a shared `Rc<RefCell<Vec<&'static str>>>`. Tests
    // assert on the exact sequence rather than just counts so out-of-order
    // firing (which would still yield the same count) doesn't slip through.

    /// Mock Activity that appends a label to a shared log every time a
    /// lifecycle callback fires. Cheap to construct and register.
    struct RecordingActivity {
        log: Rc<RefCell<Vec<&'static str>>>,
    }

    impl crate::activity::Activity for RecordingActivity {
        fn on_create(&mut self, _: &mut ActivityContext<'_>)  { self.log.borrow_mut().push("create"); }
        fn on_start(&mut self, _: &mut ActivityContext<'_>)   { self.log.borrow_mut().push("start"); }
        fn on_resume(&mut self, _: &mut ActivityContext<'_>)  { self.log.borrow_mut().push("resume"); }
        fn on_pause(&mut self, _: &mut ActivityContext<'_>)   { self.log.borrow_mut().push("pause"); }
        fn on_stop(&mut self, _: &mut ActivityContext<'_>)    { self.log.borrow_mut().push("stop"); }
        fn on_destroy(&mut self, _: &mut ActivityContext<'_>) { self.log.borrow_mut().push("destroy"); }
        fn render(&mut self, _: &mut ActivityContext<'_>, _: &mut FrameCommands) {}
    }

    /// Invoke an [`ActivityManager`] driver with a closure that builds a
    /// throwaway [`ActivityContext`] and forwards each state to the
    /// corresponding trait method. This mirrors the dispatch shape used by
    /// [`crate::Framework`] so a test passing here means a real bring-up
    /// would see the same callback ordering.
    fn fire(activity: &mut dyn Activity, state: LifecycleState) {
        let mut finish = false;
        let mut launch: Option<crate::intent::Intent> = None;
        let mut launch_for_result: Option<(crate::intent::Intent, u32)> = None;
        let mut set_result: Option<crate::result::ActivityResult> = None;
        let mut ctx = ActivityContext {
            metrics: SurfaceMetrics::default(),
            timestamp_ms: 0.0,
            pointer: None::<PointerEvent>,
            finish_requested: &mut finish,
            launch_requested: &mut launch,
            start_for_result_requested: &mut launch_for_result,
            set_result_requested: &mut set_result,
        };
        match state {
            LifecycleState::Created  => activity.on_create(&mut ctx),
            LifecycleState::Started  => activity.on_start(&mut ctx),
            LifecycleState::Resumed  => activity.on_resume(&mut ctx),
            LifecycleState::Paused   => activity.on_pause(&mut ctx),
            LifecycleState::Stopped  => activity.on_stop(&mut ctx),
            LifecycleState::Destroyed => activity.on_destroy(&mut ctx),
            // Stone 11.5 — Trapped never fires through this path; the
            // framework refuses dispatches into trapped activities and
            // only the Trapped → Destroyed recovery edge ever lands
            // here (which routes via the on_destroy arm above before
            // mark_trapped flips the state). Initial is the launch-
            // window state with no callback to run.
            LifecycleState::Initial | LifecycleState::Trapped => {}
        }
    }

    /// Build an [`ActivityManager`] with a single registered `RecordingActivity`
    /// and return `(manager, log)` ready for launch.
    fn harness() -> (ActivityManager, Rc<RefCell<Vec<&'static str>>>) {
        let log = Rc::new(RefCell::new(Vec::new()));
        let mut am = ActivityManager::new();
        let log_for_factory = log.clone();
        am.register(
            "test.pkg",
            "RecMain",
            LaunchMode::Standard,
            move || Box::new(RecordingActivity { log: log_for_factory.clone() }),
        );
        (am, log)
    }

    // ── resume() ─────────────────────────────────────────────────────────

    #[test]
    fn resume_from_initial_fires_create_start_resume() {
        let (mut am, log) = harness();
        let id = am.launch("test.pkg", "RecMain").unwrap().id;
        am.resume(id, fire);
        assert_eq!(&*log.borrow(), &["create", "start", "resume"]);
        assert_eq!(am.state_of(id), Some(LifecycleState::Resumed));
    }

    #[test]
    fn resume_from_paused_fires_only_resume() {
        // Paused → Resumed is the "user comes back to the app" edge.
        // Must not re-fire create/start — that would confuse state apps
        // that treat on_create as a one-time init.
        let (mut am, log) = harness();
        let id = am.launch("test.pkg", "RecMain").unwrap().id;
        am.resume(id, fire);
        am.pause(id, fire);
        log.borrow_mut().clear();

        am.resume(id, fire);
        assert_eq!(&*log.borrow(), &["resume"]);
        assert_eq!(am.state_of(id), Some(LifecycleState::Resumed));
    }

    #[test]
    fn resume_from_stopped_fires_start_then_resume() {
        // Stopped → Started → Resumed is Android's "onRestart" path.
        // Our enum doesn't have a distinct Restart state — the state
        // machine just allows Stopped → Started directly, and the app
        // sees on_start fire again.
        let (mut am, log) = harness();
        let id = am.launch("test.pkg", "RecMain").unwrap().id;
        am.resume(id, fire);
        am.pause(id, fire);
        am.advance(id, LifecycleState::Stopped, fire);
        log.borrow_mut().clear();

        am.resume(id, fire);
        assert_eq!(&*log.borrow(), &["start", "resume"]);
        assert_eq!(am.state_of(id), Some(LifecycleState::Resumed));
    }

    #[test]
    fn resume_idempotent_when_already_resumed() {
        let (mut am, log) = harness();
        let id = am.launch("test.pkg", "RecMain").unwrap().id;
        am.resume(id, fire);
        log.borrow_mut().clear();

        am.resume(id, fire);
        assert!(log.borrow().is_empty(), "already-Resumed resume must be a no-op");
    }

    // ── pause() ──────────────────────────────────────────────────────────

    #[test]
    fn pause_from_resumed_fires_pause() {
        let (mut am, log) = harness();
        let id = am.launch("test.pkg", "RecMain").unwrap().id;
        am.resume(id, fire);
        log.borrow_mut().clear();

        am.pause(id, fire);
        assert_eq!(&*log.borrow(), &["pause"]);
        assert_eq!(am.state_of(id), Some(LifecycleState::Paused));
    }

    #[test]
    fn pause_idempotent_when_already_paused() {
        let (mut am, log) = harness();
        let id = am.launch("test.pkg", "RecMain").unwrap().id;
        am.resume(id, fire);
        am.pause(id, fire);
        log.borrow_mut().clear();

        am.pause(id, fire);
        assert!(log.borrow().is_empty());
        assert_eq!(am.state_of(id), Some(LifecycleState::Paused));
    }

    #[test]
    fn pause_from_stopped_is_noop() {
        // Stopped → Paused isn't a legal edge (would be a backwards
        // transition). The driver must silently do nothing rather than
        // flip state to Paused and fire on_pause out of order.
        let (mut am, log) = harness();
        let id = am.launch("test.pkg", "RecMain").unwrap().id;
        am.resume(id, fire);
        am.pause(id, fire);
        am.advance(id, LifecycleState::Stopped, fire);
        log.borrow_mut().clear();

        am.pause(id, fire);
        assert!(log.borrow().is_empty());
        assert_eq!(am.state_of(id), Some(LifecycleState::Stopped));
    }

    // ── destroy() ────────────────────────────────────────────────────────

    #[test]
    fn destroy_from_resumed_fires_pause_stop_destroy() {
        let (mut am, log) = harness();
        let id = am.launch("test.pkg", "RecMain").unwrap().id;
        am.resume(id, fire);
        log.borrow_mut().clear();

        am.destroy(id, fire);
        assert_eq!(&*log.borrow(), &["pause", "stop", "destroy"]);
        assert_eq!(am.state_of(id), Some(LifecycleState::Destroyed));
    }

    #[test]
    fn destroy_from_paused_fires_stop_and_destroy() {
        let (mut am, log) = harness();
        let id = am.launch("test.pkg", "RecMain").unwrap().id;
        am.resume(id, fire);
        am.pause(id, fire);
        log.borrow_mut().clear();

        am.destroy(id, fire);
        assert_eq!(&*log.borrow(), &["stop", "destroy"]);
        assert_eq!(am.state_of(id), Some(LifecycleState::Destroyed));
    }

    #[test]
    fn destroy_from_created_fires_only_destroy() {
        // Stone 9.3 — the `Created → Destroyed` shortcut. Android parity:
        // finish() called from inside on_create skips Start/Resume/Pause/
        // Stop entirely. on_destroy fires with state flipped straight
        // from Created to Destroyed. Before 9.3 this would've been a
        // full no-op (every target illegal from Created).
        let (mut am, log) = harness();
        let id = am.launch("test.pkg", "RecMain").unwrap().id;
        am.advance(id, LifecycleState::Created, fire);
        log.borrow_mut().clear();

        am.destroy(id, fire);
        assert_eq!(&*log.borrow(), &["destroy"],
            "Created -> Destroyed shortcut must fire only on_destroy");
        assert_eq!(am.state_of(id), Some(LifecycleState::Destroyed));
        assert!(am.with_instance(id, |_| {}) == false,
            "instance must be cleaned up after the shortcut, same as the long path");
    }

    #[test]
    fn destroy_from_stopped_fires_only_destroy() {
        let (mut am, log) = harness();
        let id = am.launch("test.pkg", "RecMain").unwrap().id;
        am.resume(id, fire);
        am.pause(id, fire);
        am.advance(id, LifecycleState::Stopped, fire);
        log.borrow_mut().clear();

        am.destroy(id, fire);
        assert_eq!(&*log.borrow(), &["destroy"]);
        assert_eq!(am.state_of(id), Some(LifecycleState::Destroyed));
    }

    #[test]
    fn destroy_removes_instance_but_keeps_record() {
        // The framework keeps the record after Destroyed so observers
        // (like the Shell's task switcher when it arrives) can see a
        // just-destroyed activity for one frame. The instance itself is
        // dropped — no more callbacks.
        let (mut am, _log) = harness();
        let id = am.launch("test.pkg", "RecMain").unwrap().id;
        am.resume(id, fire);
        am.destroy(id, fire);
        assert_eq!(am.state_of(id), Some(LifecycleState::Destroyed));
        assert!(am.with_instance(id, |_| {}) == false, "instance must be dropped after destroy");
    }

    // ── advance() ────────────────────────────────────────────────────────

    #[test]
    fn advance_legal_edge_returns_prev_state() {
        let (mut am, log) = harness();
        let id = am.launch("test.pkg", "RecMain").unwrap().id;
        let prev = am.advance(id, LifecycleState::Created, fire);
        assert_eq!(prev, Some(LifecycleState::Initial));
        assert_eq!(&*log.borrow(), &["create"]);
    }

    #[test]
    fn advance_illegal_edge_returns_none_and_skips_callback() {
        // Initial → Resumed isn't legal — must stay Initial and not
        // fire any callback. Returning None is how callers learn to
        // retry via an intermediate state.
        let (mut am, log) = harness();
        let id = am.launch("test.pkg", "RecMain").unwrap().id;
        let prev = am.advance(id, LifecycleState::Resumed, fire);
        assert_eq!(prev, None);
        assert!(log.borrow().is_empty());
        assert_eq!(am.state_of(id), Some(LifecycleState::Initial));
    }

    // ── launch() ─────────────────────────────────────────────────────────

    #[test]
    fn launch_unknown_activity_errors() {
        let mut am = ActivityManager::new();
        let err = am.launch("nope", "Nope").unwrap_err();
        assert!(matches!(err, LaunchError::UnknownActivity { .. }));
    }

    #[test]
    fn launch_standard_creates_fresh_instance_every_time() {
        // Two Standard launches → two distinct ids, two stacked in the
        // foreground task. Contrast with SingleTop which reuses below.
        let (mut am, _log) = harness();
        let a = am.launch("test.pkg", "RecMain").unwrap().id;
        let b = am.launch("test.pkg", "RecMain").unwrap().id;
        assert_ne!(a, b);
        let task = am.foreground_task().expect("task exists");
        assert_eq!(am.tasks[&task].stack, vec![a, b]);
    }

    #[test]
    fn launch_singletop_reuses_top_when_ids_match() {
        let log = Rc::new(RefCell::new(Vec::new()));
        let mut am = ActivityManager::new();
        let log_for_factory = log.clone();
        am.register(
            "test.pkg",
            "TopMain",
            LaunchMode::SingleTop,
            move || Box::new(RecordingActivity { log: log_for_factory.clone() }),
        );
        let first = am.launch("test.pkg", "TopMain").unwrap().id;
        let second = am.launch("test.pkg", "TopMain").unwrap().id;
        assert_eq!(first, second, "SingleTop must reuse when already on top");
        // And the task stack hasn't grown.
        let task = am.foreground_task().expect("task exists");
        assert_eq!(am.tasks[&task].stack, vec![first]);
    }

    // ── SingleTask / SingleInstance (Stone 9.2) ──────────────────────────

    /// Build an [`ActivityManager`] with three registered classes: a
    /// `Standard` filler (used for stack-padding), a `SingleTask` target,
    /// and a `SingleInstance` target. All three share the same log so
    /// ordering assertions span the whole set.
    fn harness_9_2() -> (ActivityManager, Rc<RefCell<Vec<&'static str>>>) {
        let log = Rc::new(RefCell::new(Vec::new()));
        let mut am = ActivityManager::new();
        for (class, mode) in [
            ("Filler", LaunchMode::Standard),
            ("TaskMain", LaunchMode::SingleTask),
            ("InstMain", LaunchMode::SingleInstance),
        ] {
            let log_c = log.clone();
            am.register("test.pkg", class, mode, move || {
                Box::new(RecordingActivity { log: log_c.clone() })
            });
        }
        (am, log)
    }

    #[test]
    fn launch_singletask_on_empty_task_creates_fresh() {
        let (mut am, _log) = harness_9_2();
        let outcome = am.launch("test.pkg", "TaskMain").unwrap();
        assert!(!outcome.reused, "empty task must create fresh, not reuse");
        assert!(outcome.popped.is_empty());
        assert_eq!(am.state_of(outcome.id), Some(LifecycleState::Initial));
    }

    #[test]
    fn launch_singletask_reuses_when_target_is_already_top() {
        // SingleTask on top behaves like SingleTop: no pop, no new
        // instance, just the reuse signal to the caller.
        let (mut am, _log) = harness_9_2();
        let first = am.launch("test.pkg", "TaskMain").unwrap().id;
        let outcome = am.launch("test.pkg", "TaskMain").unwrap();
        assert!(outcome.reused);
        assert!(outcome.popped.is_empty());
        assert_eq!(outcome.id, first);
    }

    #[test]
    fn launch_singletask_pops_above_and_reuses_when_target_is_below_top() {
        // Stack: [TaskMain, Filler1, Filler2]. Relaunching TaskMain must
        // return reused=true with popped = [Filler2, Filler1] (top-first)
        // so the caller knows exactly which ids to destroy, in order.
        let (mut am, _log) = harness_9_2();
        let task_id = am.launch("test.pkg", "TaskMain").unwrap().id;
        let filler1 = am.launch("test.pkg", "Filler").unwrap().id;
        let filler2 = am.launch("test.pkg", "Filler").unwrap().id;

        let outcome = am.launch("test.pkg", "TaskMain").unwrap();
        assert!(outcome.reused);
        assert_eq!(outcome.id, task_id);
        assert_eq!(outcome.popped, vec![filler2, filler1],
            "popped must be top-first so the caller destroys in correct order");
    }

    #[test]
    fn launch_singleinstance_lives_in_its_own_task() {
        // Stone 9.6 — SingleInstance gets a dedicated, system-wide-unique
        // task. A later launch of any other activity (here: Filler via
        // Standard) lands in a different task and never shares stack
        // with the SingleInstance.
        let (mut am, _log) = harness_9_2();
        let inst = am.launch("test.pkg", "InstMain").unwrap();
        let filler = am.launch("test.pkg", "Filler").unwrap().id;

        let inst_task = am.record(inst.id).unwrap().task;
        let filler_task = am.record(filler).unwrap().task;
        assert_ne!(inst_task, filler_task,
            "SingleInstance must not share a task with other activities");
        assert_eq!(am.task_stack(inst_task).unwrap(), &[inst.id],
            "SingleInstance's dedicated task must contain exactly one activity");
    }

    #[test]
    fn launch_singleinstance_reuse_is_system_wide() {
        // Reuse scan crosses task boundaries: the second launch finds
        // the instance in its dedicated task even though the foreground
        // task is someone else's. `popped` is always empty on
        // SingleInstance reuse because the dedicated task has no room
        // above the instance to stack anything.
        let (mut am, _log) = harness_9_2();
        let first = am.launch("test.pkg", "InstMain").unwrap().id;
        // Filler ends up in the foreground task, not the SingleInstance's.
        let _filler = am.launch("test.pkg", "Filler").unwrap().id;

        let outcome = am.launch("test.pkg", "InstMain").unwrap();
        assert!(outcome.reused);
        assert_eq!(outcome.id, first);
        assert!(outcome.popped.is_empty(),
            "SingleInstance reuse never has a popped set");
    }

    // ── bring_task_to_front / task_stack (Stone 9.6) ─────────────────────

    #[test]
    fn bring_task_to_front_switches_foreground_and_returns_prev() {
        // Two SingleInstance activities → two dedicated tasks, plus
        // whatever foreground task the first implicit Filler created.
        // `bring_task_to_front` flips foreground and hands back the
        // previous id so the Framework can pause its top.
        let (mut am, _log) = harness_9_2();
        let _filler = am.launch("test.pkg", "Filler").unwrap().id;
        let fg_before = am.foreground_task().expect("Filler set foreground");
        let inst = am.launch("test.pkg", "InstMain").unwrap().id;
        let inst_task = am.record(inst).unwrap().task;
        assert_ne!(inst_task, fg_before,
            "SingleInstance dedicated task must not be foreground yet");

        let prev = am.bring_task_to_front(inst_task);
        assert_eq!(prev, Some(fg_before));
        assert_eq!(am.foreground_task(), Some(inst_task));

        // Idempotent: same task twice → None.
        let again = am.bring_task_to_front(inst_task);
        assert_eq!(again, None, "bringing already-foreground task must be a no-op signal");

        // Unknown task → None, foreground unchanged.
        let unknown = am.bring_task_to_front(999);
        assert_eq!(unknown, None);
        assert_eq!(am.foreground_task(), Some(inst_task));
    }

    #[test]
    fn task_stack_returns_bottom_to_top_slice() {
        let (mut am, _log) = harness_9_2();
        let a = am.launch("test.pkg", "Filler").unwrap().id;
        let b = am.launch("test.pkg", "Filler").unwrap().id;
        let fg = am.foreground_task().expect("task exists");
        assert_eq!(am.task_stack(fg), Some(&[a, b][..]));
        assert_eq!(am.task_stack(999), None);
    }

    #[test]
    fn launch_singletask_does_not_mutate_popped_state_until_caller_destroys() {
        // Contract: launch() only *identifies* popped activities. Their
        // lifecycle state stays untouched until the Framework drives
        // destroy on each. If this ever regresses — e.g. a premature
        // state flip inside launch() — every caller would see stale
        // states and fire on_pause twice.
        let (mut am, _log) = harness_9_2();
        let task_id = am.launch("test.pkg", "TaskMain").unwrap().id;
        am.resume(task_id, fire);
        am.pause(task_id, fire);  // covered by the filler below
        let filler = am.launch("test.pkg", "Filler").unwrap().id;
        am.resume(filler, fire);
        assert_eq!(am.state_of(filler), Some(LifecycleState::Resumed));

        let _ = am.launch("test.pkg", "TaskMain").unwrap();
        assert_eq!(am.state_of(filler), Some(LifecycleState::Resumed),
            "popped activity's state must not flip inside launch()");
        assert_eq!(am.state_of(task_id), Some(LifecycleState::Paused),
            "target's state must not flip inside launch() either");
    }

    struct NullActivity;
    impl crate::activity::Activity for NullActivity {
        fn render(&mut self, _: &mut ActivityContext<'_>, _: &mut FrameCommands) {}
    }

    // ── Audit regressions ────────────────────────────────────────────────
    //
    // These tests pin down edges the post-9.1 state-machine audit turned
    // up: out-of-order driver calls, destroyed-id reuse, and the two
    // guards added to keep the state machine self-consistent.

    #[test]
    fn destroy_from_initial_is_full_noop() {
        // Calling destroy() before on_create ran must not drop the
        // instance or rewrite the stack — every transition is illegal
        // from Initial, so no callback fires, and the cleanup branch
        // refuses to act on a record that never reached Destroyed.
        // Previous behaviour wiped instances/stack silently, leaving a
        // record stuck at `Initial` with no matching on_destroy.
        let (mut am, log) = harness();
        let id = am.launch("test.pkg", "RecMain").unwrap().id;
        assert_eq!(am.state_of(id), Some(LifecycleState::Initial));

        am.destroy(id, fire);
        assert!(log.borrow().is_empty(), "no callbacks must fire from Initial");
        assert_eq!(am.state_of(id), Some(LifecycleState::Initial),
            "state must not flip when no edge was taken");
        assert!(am.with_instance(id, |_| {}), "instance must still be alive");
        let task = am.foreground_task().expect("task exists");
        assert_eq!(am.tasks[&task].stack, vec![id],
            "stack entry must still be present");
    }

    #[test]
    fn advance_to_on_destroyed_record_is_rejected() {
        // After destroy(), the record remains at Destroyed while the
        // instance is dropped. Any further advance_to must return None
        // and leave the state untouched (legal_transition blocks every
        // edge out of Destroyed). Regression guard: if someone ever adds
        // an edge out of Destroyed, the instance-missing check becomes
        // the second line of defence.
        let (mut am, _log) = harness();
        let id = am.launch("test.pkg", "RecMain").unwrap().id;
        am.resume(id, fire);
        am.destroy(id, fire);
        assert_eq!(am.state_of(id), Some(LifecycleState::Destroyed));

        for target in [
            LifecycleState::Created,
            LifecycleState::Started,
            LifecycleState::Resumed,
            LifecycleState::Paused,
            LifecycleState::Stopped,
        ] {
            assert_eq!(am.advance(id, target, fire), None,
                "advance {:?} -> {:?} must be rejected", LifecycleState::Destroyed, target);
            assert_eq!(am.state_of(id), Some(LifecycleState::Destroyed),
                "state must remain Destroyed after rejected advance");
        }
    }

    #[test]
    fn singletop_reuse_returns_current_top_id() {
        // The reuse path trusts that the foreground top is in a sane
        // state when launch() is called on its (pkg, class). In practice
        // it's always Resumed (the only time stack.last() isn't Resumed
        // is mid-driver, and drivers are synchronous). This test pins
        // that invariant: a fresh SingleTop launch → resume → relaunch
        // yields the same id with state still Resumed, no lifecycle
        // churn, stack unchanged.
        let log = Rc::new(RefCell::new(Vec::new()));
        let mut am = ActivityManager::new();
        let log_for_factory = log.clone();
        am.register(
            "test.pkg",
            "TopMain",
            LaunchMode::SingleTop,
            move || Box::new(RecordingActivity { log: log_for_factory.clone() }),
        );
        let first = am.launch("test.pkg", "TopMain").unwrap().id;
        am.resume(first, fire);
        log.borrow_mut().clear();

        let second = am.launch("test.pkg", "TopMain").unwrap().id;
        assert_eq!(first, second);
        assert_eq!(am.state_of(second), Some(LifecycleState::Resumed),
            "reused top must still be Resumed");
        assert!(log.borrow().is_empty(), "reuse must not fire any callback");
    }

    // ── Stone 11.4a-5: pending-transition slot ──────────────────────────
    //
    // These tests pin the slot mutators (`open_pending`, `resolve_pending`,
    // `cancel_pending`, `pending_ids`). The end-to-end driver loop —
    // which polls a real `WasmHost` and calls these in sequence — lives
    // in [`crate::tests::deferred_lifecycle_drives_through_two_polls`]
    // because it needs a Framework + WasmHost.

    #[test]
    fn open_pending_records_target_and_future_id() {
        let (mut am, _log) = harness();
        let id = am.launch("test.pkg", "RecMain").unwrap().id;
        // Initial → Created is the first legal edge; opening the slot
        // means "the host is working on creating this activity".
        assert!(am.open_pending(id, LifecycleState::Created, 0xCAFE, 100.0));
        let p = am.pending_of(id).expect("slot must be set");
        assert_eq!(p.target, LifecycleState::Created);
        assert_eq!(p.future_id, 0xCAFE);
        assert_eq!(p.started_at_ms, 100.0);
        // State stays at Initial — the work is still in flight.
        assert_eq!(am.state_of(id), Some(LifecycleState::Initial));
    }

    #[test]
    fn open_pending_rejects_when_slot_already_open() {
        // Two host calls in flight against the same activity is a
        // dispatcher bug — the framework promises one outstanding op
        // per activity. Verify the second open is rejected so the bug
        // surfaces here instead of silently overwriting.
        let (mut am, _log) = harness();
        let id = am.launch("test.pkg", "RecMain").unwrap().id;
        assert!(am.open_pending(id, LifecycleState::Created, 1, 0.0));
        assert!(!am.open_pending(id, LifecycleState::Created, 2, 0.0));
        assert_eq!(am.pending_of(id).unwrap().future_id, 1);
    }

    #[test]
    fn open_pending_rejects_illegal_transition() {
        let (mut am, _log) = harness();
        let id = am.launch("test.pkg", "RecMain").unwrap().id;
        // Initial → Resumed isn't a legal one-step edge; guard it the
        // same way `advance_to` does.
        assert!(!am.open_pending(id, LifecycleState::Resumed, 1, 0.0));
        assert!(am.pending_of(id).is_none());
    }

    #[test]
    fn resolve_pending_flips_state_and_clears_slot() {
        let (mut am, log) = harness();
        let id = am.launch("test.pkg", "RecMain").unwrap().id;
        assert!(am.open_pending(id, LifecycleState::Created, 1, 0.0));
        let prev = am.resolve_pending(id);
        assert_eq!(prev, Some(LifecycleState::Initial));
        assert_eq!(am.state_of(id), Some(LifecycleState::Created));
        assert!(am.pending_of(id).is_none());
        // Critical: no Activity callback fires from resolve. The wasm
        // app's on_create ran inside the worker — the framework just
        // updates its bookkeeping. The RecordingActivity log stays
        // empty for that reason.
        assert!(log.borrow().is_empty(),
            "resolve_pending must not fire trait callbacks; got {:?}", *log.borrow());
    }

    #[test]
    fn cancel_pending_clears_slot_without_advancing_state() {
        let (mut am, _log) = harness();
        let id = am.launch("test.pkg", "RecMain").unwrap().id;
        assert!(am.open_pending(id, LifecycleState::Created, 1, 0.0));
        let dropped = am.cancel_pending(id);
        assert_eq!(dropped.unwrap().future_id, 1);
        // State stayed at Initial — the cancel doesn't promote.
        assert_eq!(am.state_of(id), Some(LifecycleState::Initial));
        assert!(am.pending_of(id).is_none());
    }

    #[test]
    fn pending_ids_lists_only_open_slots() {
        let (mut am, _log) = harness();
        let log_for_factory = Rc::new(RefCell::new(Vec::new()));
        // Need a second registered class so we can launch two distinct
        // ids with a Standard launch mode.
        let log_b = log_for_factory.clone();
        am.register("test.pkg", "RecMain2", LaunchMode::Standard,
            move || Box::new(RecordingActivity { log: log_b.clone() }));
        let a = am.launch("test.pkg", "RecMain").unwrap().id;
        let b = am.launch("test.pkg", "RecMain2").unwrap().id;
        assert!(am.pending_ids().is_empty());

        assert!(am.open_pending(a, LifecycleState::Created, 7, 0.0));
        assert_eq!(am.pending_ids(), vec![a]);

        assert!(am.open_pending(b, LifecycleState::Created, 8, 0.0));
        let mut ids = am.pending_ids();
        ids.sort();
        let mut expected = vec![a, b]; expected.sort();
        assert_eq!(ids, expected);

        am.resolve_pending(a);
        assert_eq!(am.pending_ids(), vec![b]);
    }

    // ── Stone 11.5: mark_trapped driver ──────────────────────────────────

    #[test]
    fn mark_trapped_flips_state_and_returns_prev() {
        let (mut am, _log) = harness();
        let id = am.launch("test.pkg", "RecMain").unwrap().id;
        // Drive it to Resumed via the existing `resume` driver so we
        // have a non-trivial prev to assert against. (`advance` directly
        // would also work but the closure inference around the `cb`
        // generic is fussy; the driver wraps the `fire` helper for us.)
        am.resume(id, fire);
        let prev = am.mark_trapped(id, 1000.0);
        assert_eq!(prev, Some(LifecycleState::Resumed));
        assert_eq!(am.state_of(id), Some(LifecycleState::Trapped));
        assert_eq!(am.record(id).unwrap().trapped_at_ms, Some(1000.0),
            "P0 #3 — mark_trapped stamps the timestamp for reap aging");
    }

    #[test]
    fn mark_trapped_clears_pending_slot() {
        let (mut am, _log) = harness();
        let id = am.launch("test.pkg", "RecMain").unwrap().id;
        assert!(am.open_pending(id, LifecycleState::Created, 99, 0.0));
        am.mark_trapped(id, 0.0).unwrap();
        // The future is gone — the worker won't ever resolve it.
        assert!(am.pending_of(id).is_none());
        assert!(am.pending_ids().is_empty());
    }

    #[test]
    fn mark_trapped_is_idempotent() {
        let (mut am, _log) = harness();
        let id = am.launch("test.pkg", "RecMain").unwrap().id;
        assert!(am.mark_trapped(id, 0.0).is_some());
        // Second call: state is already Trapped, no legal edge.
        assert!(am.mark_trapped(id, 5000.0).is_none());
        assert_eq!(am.state_of(id), Some(LifecycleState::Trapped));
    }

    // ── P0 #3: Trapped recycle ─────────────────────────────────────────────

    #[test]
    fn reap_trapped_ids_returns_only_aged_records() {
        let (mut am, _log) = harness();
        let young = am.launch("test.pkg", "RecMain").unwrap().id;
        let old = am.launch("test.pkg", "RecMain").unwrap().id;
        am.mark_trapped(young, 9_000.0).unwrap();
        am.mark_trapped(old, 0.0).unwrap();

        // now_ms=10_000, max_age=5_000 → age threshold is anything older
        // than ts=5_000. `young` (9_000 → age 1_000) stays; `old`
        // (0 → age 10_000) is ripe.
        let ids = am.reap_trapped_ids(10_000.0, 5_000.0);
        assert_eq!(ids, vec![old]);
    }

    #[test]
    fn reap_drops_record_and_instance_for_trapped() {
        let (mut am, _log) = harness();
        let id = am.launch("test.pkg", "RecMain").unwrap().id;
        am.mark_trapped(id, 0.0).unwrap();
        assert!(am.reap(id).is_some(), "first reap succeeds");
        assert!(am.record(id).is_none(), "record gone");
        assert!(!am.with_instance(id, |_| {}), "instance gone");

        // Second reap is a no-op (idempotent — record is gone).
        assert!(am.reap(id).is_none());
    }

    #[test]
    fn reap_refuses_non_trapped_records() {
        // Reaping a Resumed/Initial activity would be a state-machine
        // violation — only Trapped records are recyclable without
        // firing on_destroy. Defensive check so a buggy caller can't
        // silently drop a live activity.
        let (mut am, _log) = harness();
        let id = am.launch("test.pkg", "RecMain").unwrap().id;
        am.resume(id, fire);
        assert!(am.reap(id).is_none(),
            "live (Resumed) activity must not be reapable");
        assert!(am.record(id).is_some(), "record stayed");
    }

    #[test]
    fn reap_trapped_ids_skips_records_without_timestamp() {
        // Defensive: a hypothetical legacy record that landed in
        // Trapped without a timestamp (e.g. constructed by hand in a
        // test fixture) is treated as ageless and must not be reaped
        // — the safe default when we don't know how old the entry is.
        let (mut am, _log) = harness();
        let id = am.launch("test.pkg", "RecMain").unwrap().id;
        am.mark_trapped(id, 0.0).unwrap();
        // Manually clear the stamp to simulate the legacy shape.
        am.records.get_mut(&id).unwrap().trapped_at_ms = None;

        let ids = am.reap_trapped_ids(10_000_000.0, 0.0);
        assert!(ids.is_empty(),
            "missing trapped_at_ms must be treated as ageless, not stale");
    }

    #[test]
    fn ids_for_package_filters_correctly() {
        let (mut am, _log) = harness();
        let log_for_factory = Rc::new(RefCell::new(Vec::new()));
        let log_b = log_for_factory.clone();
        am.register("other.pkg", "OtherMain", LaunchMode::Standard,
            move || Box::new(RecordingActivity { log: log_b.clone() }));
        let a = am.launch("test.pkg", "RecMain").unwrap().id;
        let b = am.launch("other.pkg", "OtherMain").unwrap().id;
        let mut got = am.ids_for_package("test.pkg");
        got.sort();
        assert_eq!(got, vec![a]);
        let mut got_other = am.ids_for_package("other.pkg");
        got_other.sort();
        assert_eq!(got_other, vec![b]);
    }
}
