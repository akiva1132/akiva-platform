//! System Shell — the home-screen activity.
//!
//! A stand-in for the future Shell app (which will be a regular wasm
//! package like any other). For Phase 2a the Shell is a built-in,
//! in-framework `Activity` impl so we can exercise the lifecycle + render
//! path end-to-end without needing a wasm loader yet.
//!
//! Carries over the Phase 1b diagnostic UI: a title banner plus a pointer
//! indicator that tracks the mouse and changes colour on press. This is
//! the same visual contract users are already used to seeing at boot.

use crate::activity::{Activity, ActivityContext, KeyEvent, PointerEvent, PointerPhase, SurfaceMetrics};
use crate::compositor::FrameCommands;
use crate::widgets::PaintTarget;

pub const PACKAGE_ID: &str = "sys.shell";
pub const ENTRY_CLASS: &str = "ShellActivity";

const BG: u32 = 0xff101418;
const FG: u32 = 0xffe0e0e0;
const POINTER_IDLE: u32 = 0x8088ccff;
const POINTER_DOWN: u32 = 0xffff5577;
const POINTER_SIZE: f32 = 24.0;

pub struct ShellActivity {
    pointer: Option<PointerState>,
    boot_count: u32,
}

#[derive(Debug, Clone, Copy)]
struct PointerState {
    x: f32,
    y: f32,
    pressed: bool,
}

impl ShellActivity {
    pub fn new(boot_count: u32) -> Self {
        Self { pointer: None, boot_count }
    }
}

impl Activity for ShellActivity {
    fn on_create(&mut self, _ctx: &mut ActivityContext<'_>) {}

    fn on_pointer(&mut self, _ctx: &mut ActivityContext<'_>, ev: PointerEvent) {
        match ev.phase {
            PointerPhase::Down => self.pointer = Some(PointerState { x: ev.x, y: ev.y, pressed: true }),
            PointerPhase::Move => {
                let pressed = self.pointer.map(|p| p.pressed).unwrap_or(false);
                self.pointer = Some(PointerState { x: ev.x, y: ev.y, pressed });
            }
            PointerPhase::Up => self.pointer = Some(PointerState { x: ev.x, y: ev.y, pressed: false }),
            PointerPhase::Cancel => self.pointer = None,
        }
    }

    fn on_key(&mut self, _ctx: &mut ActivityContext<'_>, _ev: KeyEvent) {}

    fn on_resize(&mut self, _ctx: &mut ActivityContext<'_>, _m: SurfaceMetrics) {}

    fn render(&mut self, _ctx: &mut ActivityContext<'_>, out: &mut FrameCommands) {
        // Background.
        out.push(crate::compositor::PaintOp::Clear { rgba: BG });

        // Title + subtitle.
        PaintTarget::draw_text(out, 24.0, 48.0, 18.0, FG, "Akiva Kernel v0.0.1");
        let sub = format!("Shell — boot #{}", self.boot_count);
        PaintTarget::draw_text(out, 24.0, 72.0, 13.0, FG, &sub);

        // Pointer indicator.
        if let Some(p) = self.pointer {
            let colour = if p.pressed { POINTER_DOWN } else { POINTER_IDLE };
            PaintTarget::fill_rect(
                out,
                p.x - POINTER_SIZE * 0.5,
                p.y - POINTER_SIZE * 0.5,
                POINTER_SIZE,
                POINTER_SIZE,
                colour,
            );
        }
    }
}
