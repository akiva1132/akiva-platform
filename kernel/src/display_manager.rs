//! Display manager.
//!
//! A "display" is one OffscreenCanvas owned by a tab. The leader owns the
//! kernel; each follower tab contributes its own display surface. Stone
//! 13.4 made this multi-display: `DisplayManager` now keeps a
//! `HashMap<DisplayId, Display>` instead of a single `primary`, and the
//! kernel renders each display independently in the per-frame loop.
//!
//! Responsibilities:
//!   1. Track per-display surface metrics (size, density). Owners
//!      update via `on_resize(display_id, ...)`.
//!   2. Allocate / release displays. `alloc_display` returns a fresh
//!      monotonically-increasing `DisplayId` the host (tab.js) uses to
//!      claim its surface; `release_display` is called when the
//!      claiming tab closes (or the host fires it directly).
//!   3. The "apply FrameCommands" loop. The kernel passes the
//!      framework's per-display frame buffer here; flush issues the
//!      HAL draw calls for that display.
//!
//! Per-display draw routing on the JS side: `hal_draw_*` calls today
//! go to the singleton `backend` in the worker. Stone 13.4 keeps that
//! contract for the **leader's display** (DisplayId 0, the default
//! transferred OffscreenCanvas) and routes follower displays via a
//! serialised paint stream over `hal_post_to_display(displayId,
//! bytes)` — the host BroadcastChannels the bytes to the claiming tab,
//! which decodes and paints main-thread. The kernel switches between
//! the two paths in `flush` based on the display id.

use std::collections::HashMap;

use akiva_framework::compositor::{FrameCommands, PaintOp};

use crate::host;

pub type DisplayId = u32;

/// `DisplayId(0)` is the always-allocated leader display — the
/// OffscreenCanvas the leader tab transferred to its worker. Followers
/// get fresh ids starting at `1`.
pub const PRIMARY_DISPLAY: DisplayId = 0;

#[derive(Debug, Default, Clone, Copy)]
pub struct SurfaceMetrics {
    pub width_px: u32,
    pub height_px: u32,
    pub density: f32,
}

#[derive(Debug, Default, Clone, Copy)]
pub struct Display {
    pub id: DisplayId,
    pub metrics: SurfaceMetrics,
    pub visible: bool,
}

pub struct DisplayManager {
    displays: HashMap<DisplayId, Display>,
    /// Monotonic id allocator. `0` is reserved for the primary; fresh
    /// follower displays start at `1`.
    next_id: DisplayId,
    /// True once the primary display has been initialised by the host
    /// (`init` after the OffscreenCanvas is transferred to the worker).
    initialised: bool,
}

impl DisplayManager {
    pub fn new() -> Self {
        Self {
            displays: HashMap::new(),
            next_id: 1,
            initialised: false,
        }
    }

    /// Initialise the primary display. Called by the kernel's
    /// `akiva_boot` after the host has transferred the OffscreenCanvas
    /// to the worker. Reads the surface metrics through the HAL
    /// imports.
    pub fn init(&mut self) {
        let m = unsafe {
            SurfaceMetrics {
                width_px: host::hal_surface_width(),
                height_px: host::hal_surface_height(),
                density: host::hal_surface_density(),
            }
        };
        self.displays.insert(PRIMARY_DISPLAY, Display {
            id: PRIMARY_DISPLAY,
            metrics: m,
            visible: true,
        });
        self.initialised = true;
        crate::log::info("display: primary surface ready");
    }

    /// Stone 13.4 — allocate a fresh display id and seed it with the
    /// initial metrics the claiming tab reported. Returns the id; the
    /// kernel hands it to the JS host (via the export the host calls)
    /// so the host can record the displayId↔tabId mapping for paint
    /// stream routing.
    pub fn alloc_display(&mut self, metrics: SurfaceMetrics) -> DisplayId {
        let id = self.next_id;
        self.next_id = self.next_id.checked_add(1).unwrap_or(1);
        self.displays.insert(id, Display {
            id,
            metrics,
            visible: true,
        });
        crate::log::info(&format!(
            "display: allocated #{id} ({}×{} @ {:.1}x)",
            metrics.width_px, metrics.height_px, metrics.density,
        ));
        id
    }

    /// Stone 13.4 — release a previously-allocated display. No-op for
    /// `PRIMARY_DISPLAY` (the leader's own canvas — releasing it would
    /// leave the OS with no surface), and for unknown ids.
    pub fn release_display(&mut self, id: DisplayId) -> bool {
        if id == PRIMARY_DISPLAY {
            crate::log::warn("display: refusing to release PRIMARY_DISPLAY");
            return false;
        }
        let removed = self.displays.remove(&id).is_some();
        if removed {
            crate::log::info(&format!("display: released #{id}"));
        }
        removed
    }

    /// Stone 13.4 — every live display id, sorted ascending. The
    /// kernel iterates this in `akiva_frame` to drive each display's
    /// render pass.
    pub fn ids(&self) -> Vec<DisplayId> {
        let mut v: Vec<DisplayId> = self.displays.keys().copied().collect();
        v.sort();
        v
    }

    pub fn on_resize(&mut self, display_id: DisplayId, w: u32, h: u32, density: f32) {
        if let Some(d) = self.displays.get_mut(&display_id) {
            d.metrics = SurfaceMetrics { width_px: w, height_px: h, density };
        } else {
            crate::log::warn(&format!("display: resize on unknown id {display_id}"));
        }
    }

    /// Stone 13.4 — flush a frame buffer to a specific display. The
    /// primary display (`0`) goes through the immediate-mode HAL draw
    /// imports unchanged; follower displays serialise their ops to a
    /// byte stream and post via `hal_post_to_display(id, bytes)` so
    /// the JS host can BroadcastChannel them to the claiming tab.
    pub fn flush(&mut self, display_id: DisplayId, frame: &FrameCommands) {
        if !self.initialised { return; }
        if !self.displays.contains_key(&display_id) { return; }
        if display_id == PRIMARY_DISPLAY {
            self.flush_primary(frame);
        } else {
            self.flush_follower(display_id, frame);
        }
    }

    fn flush_primary(&mut self, frame: &FrameCommands) {
        // SAFETY: all HAL draw calls take primitives or pointers into
        // UTF-8 slices we own for the duration of the call.
        unsafe {
            for op in frame.ops() {
                match op {
                    PaintOp::Clear { rgba } => host::hal_draw_clear(*rgba),
                    PaintOp::FillRect { x, y, w, h, rgba } => {
                        host::hal_draw_rect(*x, *y, *w, *h, *rgba);
                    }
                    PaintOp::FillRoundRect { x, y, w, h, radius, rgba } => {
                        host::hal_draw_round_rect(*x, *y, *w, *h, *radius, *rgba);
                    }
                    PaintOp::DrawText { x, y, size, rgba, text } => {
                        let bytes = text.as_bytes();
                        host::hal_draw_text(*x, *y, *size, *rgba, bytes.as_ptr(), bytes.len());
                    }
                    // Stone 13.0prep — outline ops.
                    PaintOp::StrokeRect { x, y, w, h, width, rgba } => {
                        host::hal_draw_stroke_rect(*x, *y, *w, *h, *width, *rgba);
                    }
                    PaintOp::StrokeRoundRect { x, y, w, h, radius, width, rgba } => {
                        host::hal_draw_stroke_round_rect(*x, *y, *w, *h, *radius, *width, *rgba);
                    }
                    PaintOp::DrawLine { x0, y0, x1, y1, width, rgba } => {
                        host::hal_draw_line(*x0, *y0, *x1, *y1, *width, *rgba);
                    }
                    PaintOp::DrawImage { drawable_id, x, y, w, h } => {
                        host::hal_draw_image(*drawable_id, *x, *y, *w, *h);
                    }
                    // Stone 13.3 — wire clip/transform to the Canvas 2D
                    // state stack. Pre-13.3 these were dropped on the
                    // floor (a TODO from Phase 2b that bit Stone 13.3's
                    // animation rewriting: `apply_props` adds
                    // PushTransform → ignored → no visible slide).
                    PaintOp::PushClip { x, y, w, h } => {
                        host::hal_draw_push_clip(*x, *y, *w, *h);
                    }
                    PaintOp::PopClip => {
                        host::hal_draw_pop_clip();
                    }
                    PaintOp::PushTransform { dx, dy, scale } => {
                        host::hal_draw_push_transform(*dx, *dy, *scale);
                    }
                    PaintOp::PopTransform => {
                        host::hal_draw_pop_transform();
                    }
                }
            }
            host::hal_draw_present();
        }
    }

    /// Stone 13.4 — serialise a follower display's frame into the
    /// shared paint-stream wire format and post via
    /// `hal_post_to_display`. Empty frames are still posted (they
    /// double as a heartbeat the follower can use to detect a stalled
    /// leader); ops-equal-to-zero is the worst case, which is one
    /// HAL call per frame.
    fn flush_follower(&mut self, display_id: DisplayId, frame: &FrameCommands) {
        let mut bytes: Vec<u8> = Vec::with_capacity(frame.ops().len() * 16);
        for op in frame.ops() {
            akiva_framework::wasm_activity::encode_op(&mut bytes, op);
        }
        // SAFETY: ptr/len describe bytes we own for the duration of
        // the call; the host copies before returning.
        unsafe {
            host::hal_post_to_display(display_id, bytes.as_ptr(), bytes.len());
        }
    }

    pub fn shutdown(&mut self) {
        self.initialised = false;
    }

    pub fn primary(&self) -> Display {
        self.displays.get(&PRIMARY_DISPLAY).copied().unwrap_or_default()
    }

    pub fn metrics(&self) -> SurfaceMetrics {
        self.primary().metrics
    }

    /// Stone 13.4 — per-display metrics. Used by the framework's
    /// per-display render path to pick the right `SurfaceMetrics`
    /// when laying out the activity assigned to each display.
    pub fn metrics_of(&self, id: DisplayId) -> Option<SurfaceMetrics> {
        self.displays.get(&id).map(|d| d.metrics)
    }
}
