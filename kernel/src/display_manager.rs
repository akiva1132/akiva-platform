//! Display manager.
//!
//! A "display" is one OffscreenCanvas owned by a tab. The leader owns the
//! kernel; each follower tab contributes exactly one display surface. The
//! display manager tracks per-display metrics (size, density) and routes
//! the frame to the right renderer backend.
//!
//! Phase 1b: single display, immediate-mode HAL calls, plus a pointer
//! indicator that moves with the mouse — this is the visible proof that
//! the full event pipeline (DOM → tab.js → worker → akiva_on_event →
//! EventQueue → Kernel::handle_event → DisplayManager::set_pointer)
//! works end-to-end.
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

#[derive(Debug, Default, Clone, Copy)]
pub struct PointerState {
    pub x: f32,
    pub y: f32,
    pub pressed: bool,
    pub active: bool,   // false = we've never seen a pointer event
}

pub struct DisplayManager {
    primary: Display,
    pointer: PointerState,
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
            pointer: PointerState { x: 0.0, y: 0.0, pressed: false, active: false },
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

    pub fn set_pointer(&mut self, x: f32, y: f32, pressed: bool) {
        self.pointer = PointerState { x, y, pressed, active: true };
    }

    pub fn pointer_leave(&mut self) {
        self.pointer.active = false;
        self.pointer.pressed = false;
    }

    pub fn pointer_pressed(&self) -> bool {
        self.pointer.pressed
    }

    pub fn render_frame(&mut self, _timestamp_ms: f64) {
        if !self.initialised { return; }

        const BG: u32 = 0xff101418; // very dark charcoal
        const FG: u32 = 0xffe0e0e0;
        const POINTER_IDLE: u32 = 0x80_88ccff;
        const POINTER_DOWN: u32 = 0xff_ff5577;

        // SAFETY: all HAL draw calls accept primitive args; no pointers here.
        unsafe {
            host::hal_draw_clear(BG);

            let label = "Akiva Kernel v0.0.1";
            host::hal_draw_text(24.0, 48.0, 18.0, FG, label.as_ptr(), label.len());

            if self.pointer.active {
                let colour = if self.pointer.pressed { POINTER_DOWN } else { POINTER_IDLE };
                // Simple 24×24 square centred on the pointer.
                let size: f32 = 24.0;
                host::hal_draw_rect(
                    self.pointer.x - size * 0.5,
                    self.pointer.y - size * 0.5,
                    size, size, colour,
                );
            }

            host::hal_draw_present();
        }
    }

    pub fn shutdown(&mut self) {
        self.initialised = false;
    }

    pub fn primary(&self) -> Display { self.primary }
}
