//! Activity manager.
//!
//! Owns the task stack and drives the activity lifecycle documented in
//! docs/activity-model.md. MVP supports `standard` and `singleTop` launch
//! modes; `singleTask` and `singleInstance` land in Phase 2.

use std::collections::HashMap;

pub type ActivityId = u64;
pub type TaskId = u64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LifecycleState {
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
    pub stack: Vec<ActivityId>,
}

pub struct ActivityManager {
    activities: HashMap<ActivityId, ActivityRecord>,
    tasks: HashMap<TaskId, Task>,
    foreground_task: Option<TaskId>,
    next_activity_id: ActivityId,
    next_task_id: TaskId,
}

impl ActivityManager {
    pub fn new() -> Self {
        Self {
            activities: HashMap::new(),
            tasks: HashMap::new(),
            foreground_task: None,
            next_activity_id: 1,
            next_task_id: 1,
        }
    }

    // ── Lifecycle transitions ────────────────────────────────────────────
    // Phase 2 fills these in with the state-machine checks documented in
    // docs/activity-model.md. Stubs return IDs so callers can be wired now.

    pub fn start_activity(
        &mut self,
        _package_id: &str,
        _class: &str,
        _launch_mode: LaunchMode,
    ) -> ActivityId {
        self.next_activity_id += 1;
        self.next_activity_id - 1
    }

    pub fn finish_activity(&mut self, _id: ActivityId) {
        // no-op; Phase 2.
    }

    pub fn move_task_to_front(&mut self, task: TaskId) {
        self.foreground_task = Some(task);
    }

    pub fn foreground(&self) -> Option<TaskId> { self.foreground_task }
}
