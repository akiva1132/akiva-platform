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

    /// Hand back an opaque blob the framework will persist before the
    /// activity is stopped. The wire format lives in the SDK — the
    /// framework treats the bytes as opaque and never inspects them.
    ///
    /// Default returns empty, which the framework reads as "nothing to
    /// save" and skips the storage write. That keeps in-kernel activities
    /// (Shell, system settings) ignorant of the mechanism while
    /// `WasmActivity` opts in by delegating to its wasm host.
    fn save_state_bytes(&self) -> Vec<u8> { Vec::new() }

    /// Restore previously-persisted bytes. Called after `on_create` and
    /// before `on_start` if the framework found bytes in storage keyed to
    /// this activity's `(package_id, class)`. Default is a no-op.
    fn restore_state_bytes(&mut self, bytes: &[u8]) { let _ = bytes; }
}
