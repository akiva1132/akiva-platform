//! Cooperative scheduler.
//!
//! The kernel runs a single work queue per OS instance. Tasks come from
//! three sources:
//!   1. Input events delivered by `akiva_on_event` (drained each frame,
//!      dispatched in `Kernel::handle_event`, not here).
//!   2. Timers armed by apps (min resolution = one frame).
//!   3. IPC messages: intent resolution, broadcast delivery (post-MVP).
//!
//! There is no preemption: each task runs to completion. Apps are expected
//! to split long work across frames. Watchdog (Phase 2+) will trap apps
//! that exceed a budget per frame.

use std::collections::VecDeque;

pub type TaskFn = Box<dyn FnOnce() + 'static>;

pub struct Scheduler {
    queue: VecDeque<TaskFn>,
    deadline_ms: Option<f64>,
}

impl Scheduler {
    pub const fn new() -> Self {
        Self { queue: VecDeque::new(), deadline_ms: None }
    }

    pub fn init(&mut self) {
        // Reserve a small working set to avoid allocator thrash on boot.
        self.queue.reserve(64);
    }

    pub fn spawn(&mut self, task: TaskFn) {
        self.queue.push_back(task);
    }

    /// Drain a ready batch. Called once per frame by the kernel.
    pub fn run_ready(&mut self, now_ms: f64) {
        // Budget per frame: 8ms at 60Hz leaves headroom for rendering.
        let budget_end = now_ms + 8.0;
        self.deadline_ms = Some(budget_end);
        while let Some(task) = self.queue.pop_front() {
            task();
            // SAFETY: hal_now_ms is a pure getter provided by the HAL.
            let now = unsafe { crate::host::hal_now_ms() };
            if now >= budget_end {
                // Deferred tasks stay on the queue for the next frame.
                break;
            }
        }
        self.deadline_ms = None;
    }

    pub fn shutdown(&mut self) {
        self.queue.clear();
    }
}
