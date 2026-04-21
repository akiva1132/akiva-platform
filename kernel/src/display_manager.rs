//! Display manager.
//!
//! A "display" is one OffscreenCanvas owned by a tab. The leader owns the
//! kernel; each follower tab contributes exactly one display surface. The
//! display manager tracks per-display metrics (size, density) and routes
//! the frame to the right renderer backend.
//!
//! Phase 1a: single display, immediate-mode HAL calls. The boot screen is
//! a solid fill + the kernel version string — just enough to prove that
//! end-to-end wiring works.
//!
//! Phase 2 replaces the inline draw calls with the RendererBackend trait
//! described in ARCHITECTURE.md §10: the kernel builds a FrameCommands
//! buffer and submits it to a pluggable backend (Canvas2D / WebGPU).

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

    pub fn render_frame(&mut self, _timestamp_ms: f64) {
        if !self.initialised { return; }
        // Phase 1a: clear + version label. This is the minimal proof of
        // life for the end-to-end boot path.
        const BG: u32 = 0xff101418; // very dark charcoal
        const FG: u32 = 0xffe0e0e0;

        // SAFETY: all HAL draw calls accept primitive args; no pointers here.
        unsafe {
            host::hal_draw_clear(BG);
            let label = "Akiva Kernel v0.0.1";
            host::hal_draw_text(
                24.0,
                48.0,
                18.0,
                FG,
                label.as_ptr(),
                label.len(),
            );
            host::hal_draw_present();
        }
    }

    pub fn shutdown(&mut self) {
        self.initialised = false;
    }

    pub fn primary(&self) -> Display { self.primary }
}
