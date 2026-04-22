//! Activity manager.
//!
//! Owns the activity lifecycle state machine documented in
//! docs/activity-model.md and the task stack. An `ActivityRecord` pairs a
//! boxed `dyn Activity` impl with its current lifecycle state; the manager
//! is the only thing that mutates that state, and it enforces the legal
//! transitions.
//!
//! MVP scope (Phase 2a):
//!   - Single foreground task, single display.
//!   - Launch modes: `Standard` and `SingleTop`. `SingleTask` and
//!     `SingleInstance` return `LaunchError::Unsupported` for now.
//!   - Activities are system-side only (registered by the framework at
//!     boot). User-app wasm activities land in Phase 2b.

use std::collections::HashMap;

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
    Unsupported(&'static str),
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

    /// Launch an activity. Returns the ID of the instance that now sits at
    /// the top of the foreground task (which may be a pre-existing
    /// instance in `SingleTop` mode).
    pub fn launch(&mut self, package_id: &str, class: &str) -> Result<ActivityId, LaunchError> {
        let key = (package_id.to_string(), class.to_string());
        let (launch_mode, factory) = self.registry.get(&key).ok_or_else(|| {
            LaunchError::UnknownActivity { package_id: package_id.into(), class: class.into() }
        })?;
        let launch_mode = *launch_mode;

        match launch_mode {
            LaunchMode::Standard | LaunchMode::SingleTop => {}
            LaunchMode::SingleTask | LaunchMode::SingleInstance => {
                return Err(LaunchError::Unsupported("singleTask/singleInstance: Phase 2b"));
            }
        }

        // SingleTop: if the current top already matches, reuse it. We do
        // NOT fire on_new_intent yet — that's a Phase 2b API; for now
        // reuse just means "no new instance, no lifecycle churn".
        if launch_mode == LaunchMode::SingleTop {
            if let Some(top_id) = self.foreground_top() {
                let rec = &self.records[&top_id];
                if rec.package_id == package_id && rec.class == class {
                    return Ok(top_id);
                }
            }
        }

        let instance = (factory)();
        let id = self.next_activity_id;
        self.next_activity_id += 1;

        let task_id = self.ensure_foreground_task(package_id);
        self.tasks.get_mut(&task_id).unwrap().stack.push(id);

        self.records.insert(id, ActivityRecord {
            id,
            task: task_id,
            package_id: package_id.into(),
            class: class.into(),
            state: LifecycleState::Initial,
            launch_mode,
        });
        self.instances.insert(id, instance);
        Ok(id)
    }

    fn ensure_foreground_task(&mut self, affinity: &str) -> TaskId {
        if let Some(t) = self.foreground_task {
            return t;
        }
        let id = self.next_task_id;
        self.next_task_id += 1;
        self.tasks.insert(id, Task { id, affinity: affinity.into(), stack: Vec::new() });
        self.foreground_task = Some(id);
        id
    }

    // ── Introspection ────────────────────────────────────────────────────

    pub fn foreground_task(&self) -> Option<TaskId> { self.foreground_task }

    pub fn foreground_top(&self) -> Option<ActivityId> {
        let t = self.tasks.get(&self.foreground_task?)?;
        t.stack.last().copied()
    }

    pub fn state_of(&self, id: ActivityId) -> Option<LifecycleState> {
        self.records.get(&id).map(|r| r.state)
    }

    pub fn record(&self, id: ActivityId) -> Option<&ActivityRecord> {
        self.records.get(&id)
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
    /// destroy in order, skipping any that have already run.
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
            if self.advance_to(id, target, &mut cb).is_none() { break; }
        }
        // Clean up once destroyed.
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
    /// `None` if we couldn't (unknown id, illegal transition, or already
    /// past). The callback is invoked once per transition so the framework
    /// can call the corresponding trait method with a fresh context.
    fn advance_to(
        &mut self,
        id: ActivityId,
        target: LifecycleState,
        cb: &mut dyn FnMut(&mut dyn Activity, LifecycleState),
    ) -> Option<LifecycleState> {
        let record = self.records.get_mut(&id)?;
        let prev = record.state;
        if !legal_transition(prev, target) { return None; }
        record.state = target;

        let inst = self.instances.get_mut(&id)?;
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
}

/// Decide whether `from → to` is one of the legal one-step transitions in
/// docs/activity-model.md §2. Treats `from == to` as illegal (idempotent
/// resume/pause is the caller's job to short-circuit).
fn legal_transition(from: LifecycleState, to: LifecycleState) -> bool {
    use LifecycleState::*;
    matches!(
        (from, to),
        (Initial, Created)
        | (Created, Started)
        | (Started, Resumed)
        | (Resumed, Paused)
        | (Paused, Resumed)
        | (Paused, Stopped)
        | (Stopped, Started)   // onRestart → onStart
        | (Stopped, Destroyed)
        | (Paused, Destroyed)  // allowed when activity finishes from Paused
    )
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
