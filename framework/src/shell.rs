//! System Shell — the home-screen activity.
//!
//! A stand-in for the future Shell app (which will be a regular wasm
//! package like any other). For Phase 2b the Shell is a built-in,
//! in-framework `Activity` impl so we can exercise the lifecycle + render
//! path end-to-end without needing the AKML + widget stack yet.
//!
//! In Phase 2b.5 the Shell gained a launcher: a column of tappable rows,
//! one per installed user package. Tapping a row sets `launch_requested`
//! on the context, and the framework brings the app to the foreground
//! while pushing the Shell into Paused. Escape pops the app and resumes
//! the Shell again.

use crate::activity::{Activity, ActivityContext, KeyEvent, LaunchRequest, PointerEvent, PointerPhase, SurfaceMetrics};
use crate::compositor::{FrameCommands, PaintTarget};

pub const PACKAGE_ID: &str = "sys.shell";
pub const ENTRY_CLASS: &str = "ShellActivity";

const BG: u32             = 0xff101418;
const FG: u32             = 0xffe0e0e0;
const FG_DIM: u32         = 0xff8a94a8;
const BUTTON_BG: u32      = 0xff1f2a3a;
const BUTTON_BG_HOVER: u32= 0xff2a3a50;
const BUTTON_BG_PRESS: u32= 0xff3a5070;
const BUTTON_FG: u32      = 0xfff0f4fa;
const BUTTON_SUB: u32     = 0xff9aa8bc;

const HEADER_PAD_X: f32    = 24.0;
const HEADER_TITLE_Y: f32  = 48.0;
const HEADER_SUB_Y: f32    = 78.0;
const LIST_TOP: f32        = 120.0;
const ROW_HEIGHT: f32      = 56.0;
const ROW_GAP: f32         = 8.0;
const ROW_PAD_X: f32       = 24.0;
const ROW_INSET_LEFT: f32  = 20.0;
const LIST_MAX_WIDTH: f32  = 640.0;

/// One launcher row: what to show + what to launch. Cloned into the Shell
/// at construction so the Shell doesn't need to keep a live borrow on the
/// PackageManager.
#[derive(Debug, Clone)]
pub struct AppEntry {
    pub package_id: String,
    pub class: String,
    pub name: String,
}

pub struct ShellActivity {
    boot_count: u32,
    apps: Vec<AppEntry>,
    pointer: Option<PointerState>,
    /// Row index of the app whose button is currently pressed, if any.
    /// Cleared on `Up`/`Cancel`; used for the "pressed" visual.
    pressed_row: Option<usize>,
}

#[derive(Debug, Clone, Copy)]
struct PointerState {
    x: f32,
    y: f32,
    pressed: bool,
}

impl ShellActivity {
    pub fn new(boot_count: u32, apps: Vec<AppEntry>) -> Self {
        Self { boot_count, apps, pointer: None, pressed_row: None }
    }

    /// Y of the top edge of row `i`, in surface pixels.
    fn row_top(i: usize) -> f32 {
        LIST_TOP + (i as f32) * (ROW_HEIGHT + ROW_GAP)
    }

    fn row_width(surface_width: f32) -> f32 {
        let avail = (surface_width - 2.0 * ROW_PAD_X).max(0.0);
        avail.min(LIST_MAX_WIDTH)
    }

    /// Row under the cursor, if any — same hit-test used for press visual
    /// and for launch dispatch.
    fn hit_row(&self, x: f32, y: f32, surface_width: f32) -> Option<usize> {
        let width = Self::row_width(surface_width);
        if x < ROW_PAD_X || x > ROW_PAD_X + width { return None; }
        for i in 0..self.apps.len() {
            let top = Self::row_top(i);
            if y >= top && y < top + ROW_HEIGHT { return Some(i); }
        }
        None
    }
}

impl Activity for ShellActivity {
    fn on_create(&mut self, _ctx: &mut ActivityContext<'_>) {}

    fn on_pointer(&mut self, ctx: &mut ActivityContext<'_>, ev: PointerEvent) {
        let surface_w = ctx.metrics.width_px as f32;
        match ev.phase {
            PointerPhase::Down => {
                self.pointer = Some(PointerState { x: ev.x, y: ev.y, pressed: true });
                self.pressed_row = self.hit_row(ev.x, ev.y, surface_w);
            }
            PointerPhase::Move => {
                let pressed = self.pointer.map(|p| p.pressed).unwrap_or(false);
                self.pointer = Some(PointerState { x: ev.x, y: ev.y, pressed });
                // If the cursor slides off the pressed row while held, drop
                // the press state so the release doesn't launch. Matches
                // platform conventions for touch UI.
                if pressed {
                    if self.pressed_row != self.hit_row(ev.x, ev.y, surface_w) {
                        self.pressed_row = None;
                    }
                }
            }
            PointerPhase::Up => {
                self.pointer = Some(PointerState { x: ev.x, y: ev.y, pressed: false });
                // Tap completed: if Up happens on the same row the Down
                // started on, request a launch.
                if let Some(row) = self.pressed_row.take() {
                    if self.hit_row(ev.x, ev.y, surface_w) == Some(row) {
                        if let Some(entry) = self.apps.get(row) {
                            *ctx.launch_requested = Some(LaunchRequest {
                                package_id: entry.package_id.clone(),
                                class: entry.class.clone(),
                            });
                        }
                    }
                }
            }
            PointerPhase::Cancel => {
                self.pointer = None;
                self.pressed_row = None;
            }
        }
    }

    fn on_key(&mut self, _ctx: &mut ActivityContext<'_>, _ev: KeyEvent) {}

    fn on_resize(&mut self, _ctx: &mut ActivityContext<'_>, _m: SurfaceMetrics) {}

    fn render(&mut self, ctx: &mut ActivityContext<'_>, out: &mut FrameCommands) {
        let surface_w = ctx.metrics.width_px as f32;
        out.push(crate::compositor::PaintOp::Clear { rgba: BG });

        // Header.
        PaintTarget::draw_text(out, HEADER_PAD_X, HEADER_TITLE_Y, 24.0, FG, "Akiva");
        let sub = if self.apps.is_empty() {
            format!("boot #{} — no apps installed", self.boot_count)
        } else {
            format!("boot #{} — {} app{}", self.boot_count, self.apps.len(),
                if self.apps.len() == 1 { "" } else { "s" })
        };
        PaintTarget::draw_text(out, HEADER_PAD_X, HEADER_SUB_Y, 13.0, FG_DIM, &sub);

        // Launcher list.
        let width = Self::row_width(surface_w);
        let hovered = self.pointer
            .filter(|_| !self.apps.is_empty())
            .and_then(|p| self.hit_row(p.x, p.y, surface_w));
        for (i, app) in self.apps.iter().enumerate() {
            let top = Self::row_top(i);
            let bg = if self.pressed_row == Some(i) {
                BUTTON_BG_PRESS
            } else if hovered == Some(i) {
                BUTTON_BG_HOVER
            } else {
                BUTTON_BG
            };
            PaintTarget::fill_rect(out, ROW_PAD_X, top, width, ROW_HEIGHT, bg);
            // Name + package id subtitle. Text baseline sits at the y arg,
            // so we offset from `top` into the row, not to its edge.
            PaintTarget::draw_text(
                out,
                ROW_PAD_X + ROW_INSET_LEFT,
                top + 24.0,
                16.0,
                BUTTON_FG,
                &app.name,
            );
            PaintTarget::draw_text(
                out,
                ROW_PAD_X + ROW_INSET_LEFT,
                top + 44.0,
                12.0,
                BUTTON_SUB,
                &app.package_id,
            );
        }

        // Empty state — if no apps are installed, explain what to do so
        // the "blank screen" below the header doesn't look like a bug.
        if self.apps.is_empty() {
            PaintTarget::draw_text(
                out,
                HEADER_PAD_X,
                LIST_TOP + 24.0,
                13.0,
                FG_DIM,
                "No apps installed. Drop one into runtime/web/dev-apps/ and reboot.",
            );
        }

        // Back hint — only useful when something is above us, and by
        // definition the Shell is at the bottom, so this is just for the
        // very first launch where the user has no idea Escape pops back.
        PaintTarget::draw_text(
            out,
            HEADER_PAD_X,
            ctx.metrics.height_px as f32 - 16.0,
            11.0,
            FG_DIM,
            "Press Escape to return here from an app.",
        );
    }
}
