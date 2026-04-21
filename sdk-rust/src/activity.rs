//! The `Activity` trait app authors implement.
//!
//! An activity is one screen worth of UI + interaction. Over the course
//! of its life the runtime drives it through lifecycle callbacks
//! (`on_create` → `on_resume` → `on_pause` → ...) interleaved with input
//! events and render requests. All callbacks have default no-op impls so
//! a minimal app overrides only what it cares about — typically just
//! `on_render`.
//!
//! The trait is deliberately flat: no sub-traits, no associated types.
//! More structure (widget-tree composition, retained-mode UI, background
//! tasks) is a library concern that can be built on top without touching
//! the ABI.
//!
//! See [docs/activity-model.md] for the state-machine rules and [docs/abi.md §2.2]
//! for how each callback maps to a raw wasm export.

use std::collections::HashMap;

use crate::canvas::Canvas;

// ── Public data types ─────────────────────────────────────────────────

/// Surface the activity is painting onto. Filled in by the runtime before
/// every callback; activities read it from `Context::metrics`.
#[derive(Debug, Default, Clone, Copy)]
pub struct Metrics {
    pub width_px: u32,
    pub height_px: u32,
    /// Device pixel ratio. `1.0` on a 160dpi reference surface.
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

/// Per-callback context the runtime constructs fresh each tick. Carries
/// display metrics, a frame timestamp, and (when relevant) the most
/// recent pointer state — so callbacks that need to look at the pointer
/// outside `on_pointer` (e.g. during render) don't need to thread state.
///
/// `class` is the activity class name the host registered this handle
/// under (the string from the manifest's `[[activities]].class`). Useful
/// when one Rust type backs multiple declared activity classes.
pub struct Context<'a> {
    pub metrics: Metrics,
    pub timestamp_ms: f64,
    pub pointer: Option<PointerEvent>,
    pub class: &'a str,
}

// ── The trait ─────────────────────────────────────────────────────────

pub trait Activity: 'static {
    // ── Lifecycle (docs/activity-model.md §2) ──────────────────────────
    fn on_create(&mut self, ctx: &mut Context<'_>)  { let _ = ctx; }
    fn on_start(&mut self, ctx: &mut Context<'_>)   { let _ = ctx; }
    fn on_resume(&mut self, ctx: &mut Context<'_>)  { let _ = ctx; }
    fn on_pause(&mut self, ctx: &mut Context<'_>)   { let _ = ctx; }
    fn on_stop(&mut self, ctx: &mut Context<'_>)    { let _ = ctx; }
    fn on_destroy(&mut self, ctx: &mut Context<'_>) { let _ = ctx; }

    // ── Input ───────────────────────────────────────────────────────────
    fn on_pointer(&mut self, ctx: &mut Context<'_>, ev: PointerEvent) {
        let _ = (ctx, ev);
    }
    fn on_key(&mut self, ctx: &mut Context<'_>, ev: KeyEvent) {
        let _ = (ctx, ev);
    }
    fn on_resize(&mut self, ctx: &mut Context<'_>, metrics: Metrics) {
        let _ = (ctx, metrics);
    }

    // ── Render ──────────────────────────────────────────────────────────
    /// Paint one frame. Called at the runtime's frame rate (rAF, typically
    /// 60 Hz). Must be fast — all ops are serialised immediately.
    fn on_render(&mut self, ctx: &mut Context<'_>, canvas: &mut Canvas<'_>) {
        let _ = (ctx, canvas);
    }

    // ── Saved state — forward-compat stubs for Phase 3 ─────────────────
    /// UI-state snapshot called when the system is about to release the
    /// activity. Budget: 64 KiB — use `storage` for larger data. Not wired
    /// up in v0; kept so app code can start using it today and keep
    /// working when the runtime honours it.
    fn on_save_state(&self, out: &mut SavedState) { let _ = out; }
    fn on_restore_state(&mut self, saved: &SavedState) { let _ = saved; }
}

// ── Saved state (forward-compat; not driven by v0 runtime) ────────────

#[derive(Debug, Default, Clone)]
pub struct SavedState {
    pub entries: HashMap<String, SavedValue>,
}

#[derive(Debug, Clone)]
pub enum SavedValue {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    String(String),
    Bytes(Vec<u8>),
}

impl SavedState {
    pub const MAX_BYTES: usize = 64 * 1024;

    pub fn put_int(&mut self, key: &str, v: i64) {
        self.entries.insert(key.into(), SavedValue::Int(v));
    }
    pub fn put_string(&mut self, key: &str, v: impl Into<String>) {
        self.entries.insert(key.into(), SavedValue::String(v.into()));
    }
    pub fn get_int(&self, key: &str) -> Option<i64> {
        match self.entries.get(key)? { SavedValue::Int(v) => Some(*v), _ => None }
    }
    pub fn get_string(&self, key: &str) -> Option<&str> {
        match self.entries.get(key)? { SavedValue::String(v) => Some(v), _ => None }
    }
}

// ── Backward-compat alias ─────────────────────────────────────────────
// The original scaffold called the context `ActivityContext`. Keeping the
// alias means the old name in `lib.rs`'s prelude still resolves during
// the transition.
pub type ActivityContext<'a> = Context<'a>;
