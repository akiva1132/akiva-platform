//! Hello Akiva — first-party smoke-test app.
//!
//! Paints a solid background, centres a title, draws a frame counter, and
//! flashes a square at the pointer. That's enough to prove every lane of
//! the ABI: alloc/dealloc, lifecycle, resize, pointer, render.

use akiva_sdk::prelude::*;

#[derive(Default)]
struct HelloMain {
    frame: u64,
    pointer: Option<(f32, f32)>,
    pointer_down: bool,
}

impl Activity for HelloMain {
    fn on_create(&mut self, ctx: &mut Context<'_>) {
        log_info!("HelloMain created ({}x{} @ {:.1}x)",
            ctx.metrics.width_px, ctx.metrics.height_px, ctx.metrics.density);
    }

    fn on_pointer(&mut self, _ctx: &mut Context<'_>, ev: PointerEvent) {
        self.pointer = Some((ev.x, ev.y));
        self.pointer_down = matches!(ev.phase, PointerPhase::Down | PointerPhase::Move)
            && ev.button != 0xff;
        // Collapse noisy `Move` spam: only log Down / Up.
        if matches!(ev.phase, PointerPhase::Down) {
            log_debug!("pointer down at ({:.0}, {:.0})", ev.x, ev.y);
        }
    }

    fn on_render(&mut self, ctx: &mut Context<'_>, c: &mut Canvas<'_>) {
        self.frame = self.frame.wrapping_add(1);
        let w = ctx.metrics.width_px as f32;
        let h = ctx.metrics.height_px as f32;

        // Background — slight gradient feeling via two rects.
        c.clear(0x0f1420ff);
        c.fill_rect(0.0, h - 4.0, w, 4.0, 0x2a3f66ff);

        // Title.
        c.draw_text(24.0, 48.0, 28.0, 0xf0f4faff, "Hello, Akiva!");
        c.draw_text(24.0, 76.0, 14.0, 0x9aa8bcff, "akiva-app-hello · ABI v0.1.0");

        // Frame counter + canvas size.
        let line = format!("frame {} · surface {}×{}", self.frame, ctx.metrics.width_px, ctx.metrics.height_px);
        c.draw_text(24.0, h - 24.0, 14.0, 0x7e8aa0ff, &line);

        // Pointer reticle — coloured by button state.
        if let Some((px, py)) = self.pointer {
            let color = if self.pointer_down { 0xff6a4aff } else { 0x4a9eff80 };
            let s = 20.0;
            c.fill_rect(px - s / 2.0, py - s / 2.0, s, s, color);
        }
    }
}

akiva_sdk::akiva_app!(HelloMain);
