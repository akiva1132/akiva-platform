//! System Shell — home screen and app launcher (Stone 11.0b).
//!
//! Ported from the in-framework `ShellActivity` (`framework/src/shell.rs`)
//! to a regular wasm app. Visual semantics are preserved verbatim: same
//! header, same row layout, same hover/press visual states, same launch
//! intent (MAIN+LAUNCHER targeted at the picked package). The two
//! differences are how it gets the launcher list and how it dispatches:
//!
//!   - List source: `akiva_sdk::pkgs::list()` instead of a `Vec<AppEntry>`
//!     handed to the framework Shell at construction. The kernel pushes
//!     a fresh snapshot at boot via `hal_host_set_packages` (Stone 11.0a),
//!     so calling `pkgs::list()` here just decodes the cached blob.
//!   - Dispatch: `Context::start_activity` (host import) instead of
//!     writing `*ctx.launch_requested = Some(...)` on the framework's
//!     in-process `ActivityContext`. Wired through the standard
//!     `akiva_host_start_activity` import (gated by LAUNCH_ANY_APP — the
//!     manifest declares it).
//!
//! `boot_count` is dropped from the subtitle: a wasm app has no portable
//! way to read kernel-side state without inventing a permission for it,
//! and the counter is a developer breadcrumb that's already logged from
//! the kernel. Documented in the v0.3 roadmap (Stone 11.0d).

use akiva_sdk::pkgs::{self, PackageEntry};
use akiva_sdk::prelude::*;

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

#[derive(Default)]
struct ShellActivity {
    apps: Vec<PackageEntry>,
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
    fn refresh_apps(&mut self) {
        // Sorted by id for a stable boot-to-boot order, matching the
        // framework's previous `launcher.sort_by(...)` behaviour. The
        // host serves whatever order `PackageManager::list()` produces
        // (HashMap insertion order, undefined) — sorting here is the
        // app's choice, not a system invariant.
        //
        // Filter out our own id: the Shell is the HOME app, so listing
        // itself in its own launcher is dead weight (clicking it would
        // re-launch HOME which is already underneath). A future
        // `pkgs::list()` filter on intent category (LAUNCHER && !HOME)
        // would express this more cleanly, but the wire format doesn't
        // carry intent filters yet.
        let mut apps = pkgs::list();
        apps.retain(|a| a.id != "sys.shell");
        apps.sort_by(|a, b| a.id.cmp(&b.id));
        self.apps = apps;
    }

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

#[async_trait(?Send)]
impl AsyncActivity for ShellActivity {
    async fn on_create(&mut self, _ctx: &mut Context<'_>) {
        // Pull the package list eagerly so the first frame already has
        // the launcher rows. The kernel pushed the snapshot before the
        // first activity launch (Stone 11.0a), so this can't see an
        // empty list due to ordering.
        self.refresh_apps();
        log_info!("ShellActivity created: {} app(s) installed", self.apps.len());
    }

    async fn on_resume(&mut self, _ctx: &mut Context<'_>) {
        // Rehydrate every time we come back from a launched app — covers
        // any future install/uninstall flow that re-pushes the snapshot
        // while we were paused. Cheap (just decodes a few hundred bytes).
        self.refresh_apps();
    }

    async fn on_pointer(&mut self, ctx: &mut Context<'_>, ev: PointerEvent) {
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
                if pressed && self.pressed_row != self.hit_row(ev.x, ev.y, surface_w) {
                    self.pressed_row = None;
                }
            }
            PointerPhase::Up => {
                self.pointer = Some(PointerState { x: ev.x, y: ev.y, pressed: false });
                if let Some(row) = self.pressed_row.take() {
                    if self.hit_row(ev.x, ev.y, surface_w) == Some(row) {
                        if let Some(entry) = self.apps.get(row) {
                            // Same intent shape as the in-framework
                            // Shell: MAIN+LAUNCHER scoped to the picked
                            // package. Router resolves it; if the
                            // package exposes multiple LAUNCHER
                            // activities the resolution surfaces as
                            // AmbiguousMatch (intentional — the Shell
                            // has no UI for choosing).
                            let intent = Intent::new("akiva.intent.action.MAIN")
                                .with_category("akiva.intent.category.LAUNCHER")
                                .with_target_package(entry.id.clone());
                            ctx.start_activity(&intent).await;
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

    async fn on_render(&mut self, ctx: &mut Context<'_>, c: &mut Canvas<'_>) {
        let surface_w = ctx.metrics.width_px as f32;
        c.clear(BG);

        // Header.
        c.draw_text(HEADER_PAD_X, HEADER_TITLE_Y, 24.0, FG, "Akiva");
        let sub = if self.apps.is_empty() {
            "no apps installed".to_string()
        } else {
            format!("{} app{}", self.apps.len(),
                if self.apps.len() == 1 { "" } else { "s" })
        };
        c.draw_text(HEADER_PAD_X, HEADER_SUB_Y, 13.0, FG_DIM, &sub);

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
            c.fill_rect(ROW_PAD_X, top, width, ROW_HEIGHT, bg);
            // Text baseline sits at the y arg, so we offset from `top`
            // into the row, not to its edge.
            c.draw_text(
                ROW_PAD_X + ROW_INSET_LEFT,
                top + 24.0,
                16.0,
                BUTTON_FG,
                &app.name,
            );
            c.draw_text(
                ROW_PAD_X + ROW_INSET_LEFT,
                top + 44.0,
                12.0,
                BUTTON_SUB,
                &app.id,
            );
        }

        // Empty state — explain what to do so a blank screen below the
        // header doesn't look like a bug.
        if self.apps.is_empty() {
            c.draw_text(
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
        c.draw_text(
            HEADER_PAD_X,
            ctx.metrics.height_px as f32 - 16.0,
            11.0,
            FG_DIM,
            "Press Escape to return here from an app.",
        );
    }
}

akiva_sdk::akiva_async_app!(ShellActivity);
