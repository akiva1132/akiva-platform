//! Activity manager.
//!
//! Owns the activity lifecycle state machine documented in
//! docs/activity-model.md and the task stack. An `ActivityRecord` pairs a
//! boxed `dyn Activity` impl with its current lifecycle state; the manager
//! is the only thing that mutates that state, and it enforces the legal
//! transitions.
//!
//! MVP scope:
//!   - Single foreground task, single display.
//!   - Launch modes: all four (`Standard`, `SingleTop`, `SingleTask`,
//!     `SingleInstance`). `SingleInstance`'s "dedicated task" isolation
//!     is flattened to the single foreground task for MVP — the reuse
//!     semantics (system-wide uniqueness + pop-above + `on_new_intent`)
//!     are faithful; full task isolation lands with Stone 9.6.
//!   - Activities are system-side only (registered by the framework at
//!     boot). User-app wasm activities come through the wasm activity
//!     proxy on the same path.

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

        // SingleTask / SingleInstance: find an existing instance with the
        // matching (pkg, class) anywhere in the foreground task. If found,
        // the caller will destroy everything above it (top-first) and
        // resume the target. If not found, fall through to fresh-create.
        //
        // In MVP's single-foreground-task world SingleInstance reduces to
        // SingleTask semantically — full dedicated-task isolation is
        // Stone 9.6, and `launch()` is the wrong seam for that anyway
        // (the foreground-switch decision lives in the Framework).
        if matches!(launch_mode, LaunchMode::SingleTask | LaunchMode::SingleInstance) {
            if let Some(task_id) = self.foreground_task {
                let task_stack = &self.tasks[&task_id].stack;
                let target_pos = task_stack.iter().position(|id| {
                    let r = &self.records[id];
                    r.package_id == package_id && r.class == class
                });
                if let Some(pos) = target_pos {
                    let target_id = task_stack[pos];
                    // Activities above the target, top-first. Cloned so
                    // the caller can iterate while we've released the
                    // borrow.
                    let popped: Vec<ActivityId> = task_stack[pos + 1..].iter().rev().copied().collect();
                    return Ok(LaunchOutcome { id: target_id, reused: true, popped });
                }
            }
        }

        // Fresh-create path — shared by Standard, SingleTop-no-match, and
        // SingleTask/SingleInstance-not-found.
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
        Ok(LaunchOutcome { id, reused: false, popped: Vec::new() })
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
        | (Stopped, Started)    // onRestart → onStart
        | (Stopped, Destroyed)
        | (Paused, Destroyed)   // allowed when activity finishes from Paused
        | (Created, Destroyed)  // finish() called from on_create — skip Start/Resume entirely, Android parity (research-android-lifecycle.md §1)
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
        // behaviour (research-android-lifecycle.md §1). Lock this edge
        // in so a future matrix cleanup doesn't quietly drop it.
        assert!(legal_transition(LifecycleState::Created, LifecycleState::Destroyed));
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
        let mut ctx = ActivityContext {
            metrics: SurfaceMetrics::default(),
            timestamp_ms: 0.0,
            pointer: None::<PointerEvent>,
            finish_requested: &mut finish,
            launch_requested: &mut launch,
        };
        match state {
            LifecycleState::Created  => activity.on_create(&mut ctx),
            LifecycleState::Started  => activity.on_start(&mut ctx),
            LifecycleState::Resumed  => activity.on_resume(&mut ctx),
            LifecycleState::Paused   => activity.on_pause(&mut ctx),
            LifecycleState::Stopped  => activity.on_stop(&mut ctx),
            LifecycleState::Destroyed => activity.on_destroy(&mut ctx),
            LifecycleState::Initial => {}  // nothing fires for Initial
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
    fn launch_singleinstance_matches_singletask_in_single_foreground_task() {
        // MVP collapse: with only one foreground task, SingleInstance's
        // "dedicated task" isolation doesn't manifest — it behaves
        // identically to SingleTask. This test locks that equivalence so
        // 9.6's multi-task foreground work can break it deliberately.
        let (mut am, _log) = harness_9_2();
        let inst_id = am.launch("test.pkg", "InstMain").unwrap().id;
        let filler = am.launch("test.pkg", "Filler").unwrap().id;

        let outcome = am.launch("test.pkg", "InstMain").unwrap();
        assert!(outcome.reused);
        assert_eq!(outcome.id, inst_id);
        assert_eq!(outcome.popped, vec![filler]);
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

    // ── Audit regressions (v0.2-review Task 1) ───────────────────────────
    //
    // These tests pin down the edges the audit turned up: out-of-order
    // driver calls, destroyed-id reuse, and the two guards added to keep
    // the state machine self-consistent.

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
}
