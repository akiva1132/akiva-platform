//! Display manager.
//!
//! A "display" is one OffscreenCanvas owned by a tab. The leader owns the
//! kernel; each follower tab contributes exactly one display surface. The
//! display manager tracks per-display metrics (size, density) and flushes
//! the framework's [`FrameCommands`] buffer to the HAL's immediate-mode
//! draw calls each frame.
//!
//! Phase 2a handover: the Phase 1b pointer indicator + banner moved into
//! the framework's Shell activity. This module now only owns:
//!   1. Surface metrics (picked up at init from HAL getters).
//!   2. The "apply FrameCommands" loop (rendering happens here because
//!      the kernel owns the `extern "C"` HAL imports, not the framework).
//!
//! Phase 2b will swap the immediate-mode HAL calls for a pluggable
//! [`RendererBackend`] trait per ARCHITECTURE.md §10.

use akiva_framework::compositor::{FrameCommands, PaintOp};

use crate::host;

pub type DisplayId = u32;

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
    primary: Display,
    initialised: bool,
}

impl DisplayManager {
    pub const fn new() -> Self {
        Self {
            primary: Display {
                id: 0,
                metrics: SurfaceMetrics { width_px: 0, height_px: 0, density: 1.0 },
                visible: true,
            },
            initialised: false,
        }
    }

    pub fn init(&mut self) {
        // SAFETY: metric getters are pure and always defined by the HAL.
        let m = unsafe {
            SurfaceMetrics {
                width_px: host::hal_surface_width(),
                height_px: host::hal_surface_height(),
                density: host::hal_surface_density(),
            }
        };
        self.primary.metrics = m;
        self.initialised = true;
        crate::log::info("display: primary surface ready");
    }

    pub fn on_resize(&mut self, w: u32, h: u32, density: f32) {
        self.primary.metrics = SurfaceMetrics { width_px: w, height_px: h, density };
    }

    /// Walk a frame buffer and issue the corresponding HAL calls. No-op
    /// until the display has been initialised.
    pub fn flush(&mut self, frame: &FrameCommands) {
        if !self.initialised { return; }
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
                    // Clip/transform are no-ops against the immediate-mode
                    // HAL for now; Phase 2b renderer backend honours them.
                    PaintOp::PushClip { .. }
                    | PaintOp::PopClip
                    | PaintOp::PushTransform { .. }
                    | PaintOp::PopTransform => {}
                }
            }
            host::hal_draw_present();
        }
    }

    pub fn shutdown(&mut self) {
        self.initialised = false;
    }

    pub fn primary(&self) -> Display { self.primary }

    pub fn metrics(&self) -> SurfaceMetrics { self.primary.metrics }
}
