//! In-kernel Activity trait.
//!
//! This is the framework-side trait used by **built-in** activities
//! (Shell, Settings, etc.) that run directly inside the kernel wasm. It is
//! deliberately separate from [`akiva_sdk::activity::Activity`], which is
//! the trait user-land wasm apps implement: the two have different
//! lifetimes, different context types, and different error handling
//! requirements (in-kernel must never panic the whole OS).
//!
//! The lifecycle transitions here match those documented in
//! [docs/activity-model.md](../../docs/activity-model.md). Transition
//! legality is enforced by [`crate::activity_manager::ActivityManager`];
//! the callbacks below are only invoked when a transition has already been
//! validated, so implementors don't re-check state.
//!
//! ```text
//!   on_create → on_start → on_resume ⇆ on_pause → on_stop → on_destroy
//!                                         ↑              │
//!                                         └── on_restart ┘
//! ```
//!
//! Each callback receives an [`ActivityContext`] that exposes display
//! metrics, the current pointer, and a handle the activity can use to
//! finish itself.

use crate::compositor::FrameCommands;
use crate::intent::Intent;
use crate::result::ActivityResult;

/// Display metrics mirrored from the kernel's DisplayManager. Copied into
/// the context so activities never need to reach into the kernel directly.
#[derive(Debug, Default, Clone, Copy)]
pub struct SurfaceMetrics {
    pub width_px: u32,
    pub height_px: u32,
    pub density: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PointerPhase { Down, Move, Up, Cancel }

#[derive(Debug, Clone, Copy)]
pub struct PointerEvent {
    pub x: f32,
    pub y: f32,
    pub button: u8,
    pub phase: PointerPhase,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyPhase { Down, Repeat, Up }

#[derive(Debug, Clone, Copy)]
pub struct KeyEvent {
    pub code: u32,
    pub modifiers: u8,
    pub phase: KeyPhase,
}

/// State the framework hands to an activity on every callback. Cheap to
/// construct — the framework rebuilds one per dispatch so activities never
/// observe a stale view.
pub struct ActivityContext<'a> {
    pub metrics: SurfaceMetrics,
    pub timestamp_ms: f64,
    /// Last-known pointer, or `None` if no pointer event has ever been
    /// seen (hover-less environments) or the pointer has left.
    pub pointer: Option<PointerEvent>,
    /// Set to `true` by the activity to request graceful finish. The
    /// framework will call `on_pause → on_stop → on_destroy` on the next
    /// frame boundary.
    pub finish_requested: &'a mut bool,
    /// Set by the activity to request launching another activity on top
    /// of it. The framework consumes this after the callback returns: the
    /// intent is handed to the IntentRouter for resolution, then the
    /// resolved activity is brought up as the new foreground top while
    /// the requester is paused. `None` means no launch requested this
    /// tick. Activities can submit either a fully-explicit intent
    /// (`Intent::explicit`) or an implicit one (action + categories +
    /// optional `target_package`) — the matcher handles both.
    pub launch_requested: &'a mut Option<Intent>,
    /// Stone 9.5 — set by the activity to launch `intent` *for a result*.
    /// `launch_requested` and this field are mutually exclusive; if both
    /// are set in the same callback, `start_for_result_requested` wins
    /// (it's the richer call). On resolution the framework records the
    /// (caller, request_code) pair against the new callee activity;
    /// when the callee finishes the caller's `on_activity_result` fires.
    pub start_for_result_requested: &'a mut Option<(Intent, u32)>,
    /// Stone 9.5 — set by the activity to stash the bytes it wants to
    /// deliver back to whoever launched it via
    /// `start_activity_for_result`. `None` means "no result yet"; the
    /// framework will synthesise `RESULT_CANCELED` when the callee is
    /// destroyed without producing one. Calling set_result repeatedly
    /// in the same lifetime is fine — the last call wins because the
    /// framework overwrites on each set.
    pub set_result_requested: &'a mut Option<ActivityResult>,
}

/// In-kernel activity trait. Every method has a default no-op except
/// [`render`], which activities must implement — there is no such thing as
/// a paint-less activity in this OS.
pub trait Activity: 'static {
    fn on_create(&mut self, ctx: &mut ActivityContext<'_>) { let _ = ctx; }
    fn on_start(&mut self, ctx: &mut ActivityContext<'_>) { let _ = ctx; }
    fn on_resume(&mut self, ctx: &mut ActivityContext<'_>) { let _ = ctx; }
    fn on_pause(&mut self, ctx: &mut ActivityContext<'_>) { let _ = ctx; }
    fn on_stop(&mut self, ctx: &mut ActivityContext<'_>) { let _ = ctx; }
    fn on_destroy(&mut self, ctx: &mut ActivityContext<'_>) { let _ = ctx; }

    fn on_pointer(&mut self, ctx: &mut ActivityContext<'_>, ev: PointerEvent) {
        let _ = (ctx, ev);
    }
    fn on_key(&mut self, ctx: &mut ActivityContext<'_>, ev: KeyEvent) {
        let _ = (ctx, ev);
    }
    fn on_resize(&mut self, ctx: &mut ActivityContext<'_>, m: SurfaceMetrics) {
        let _ = (ctx, m);
    }

    /// Produce paint commands for the current frame. Called only while the
    /// activity is in `Resumed` state.
    fn render(&mut self, ctx: &mut ActivityContext<'_>, out: &mut FrameCommands);

    /// Deliver an already-resolved intent to an activity that is already
    /// live on the stack — i.e. `SingleTop` reuse (Stone 9.4) or
    /// `SingleTask`/`SingleInstance` reuse (Stone 9.2). The activity
    /// stays `Resumed`; only `on_new_intent` fires, no bring-up chain
    /// runs. Default is a no-op for in-kernel activities that don't
    /// care about being re-addressed. See
    /// [docs/activity-model.md §2](../../docs/activity-model.md).
    fn on_new_intent(&mut self, ctx: &mut ActivityContext<'_>, intent: &Intent) {
        let _ = (ctx, intent);
    }

    /// Stone 9.5 — called on the caller after the activity it launched
    /// via `start_activity_for_result` finishes. `request_code` is the
    /// identifier the caller passed at launch; `result` is the bytes
    /// the callee stashed via `set_result`, or a synthesised
    /// `RESULT_CANCELED` if the callee finished without producing one.
    /// Default is a no-op for in-kernel activities that don't launch
    /// for results.
    fn on_activity_result(
        &mut self,
        ctx: &mut ActivityContext<'_>,
        request_code: u32,
        result: &ActivityResult,
    ) {
        let _ = (ctx, request_code, result);
    }

    /// Hand back an opaque blob the framework will persist before the
    /// activity is stopped. The wire format lives in the SDK — the
    /// framework treats the bytes as opaque and never inspects them.
    ///
    /// Default returns empty, which the framework reads as "nothing to
    /// save" and skips the storage write. In-kernel activities that need
    /// synchronous save (test fixtures, future built-ins) override this.
    /// Cross-worker `WasmActivity` overrides
    /// [`Activity::dispatch_save_state_async`] instead — the worker
    /// boundary makes a sync return impossible, so the bytes arrive on a
    /// later frame via [`crate::Framework::drive_pending_saves`].
    fn save_state_bytes(&self) -> Vec<u8> { Vec::new() }

    /// Cross-worker save: fire `save_state` at the host and return the
    /// `future_id` the framework should poll. Default `0` means "no async
    /// save" — the framework falls back to [`Activity::save_state_bytes`]
    /// (the in-process sync path) inside the destroy walk. WasmActivity
    /// returns the host's future_id so the framework records a
    /// `PendingSave` and persists the bytes whenever the worker replies.
    ///
    /// Called once on the destroy edge (before pause→stop→destroy walks
    /// the worker queue). The returned future_id outlives the activity's
    /// handle on the host side — the JS AppRegistry keys dispatches by
    /// id, not by handle, so unregister doesn't drop the in-flight
    /// reply.
    fn dispatch_save_state_async(&self) -> u64 { 0 }

    /// Restore previously-persisted bytes. Called after `on_create` and
    /// before `on_start` if the framework found bytes in storage keyed to
    /// this activity's `(package_id, class)`. Default is a no-op.
    fn restore_state_bytes(&mut self, bytes: &[u8]) { let _ = bytes; }
}
