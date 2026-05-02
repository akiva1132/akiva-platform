//! `game` template — animation loop on the raw `Canvas`.
//!
//! Where `default` and `compose-style` use the widget toolkit, this
//! template paints directly. The kernel calls `on_render` every
//! animation frame regardless of input, so an app that ignores the
//! widget tree can run physics and animations by reading
//! `Context::timestamp_ms` and integrating from the previous frame.
//!
//! The scaffolded app is a single bouncing ball. Pointer-down teleports
//! the ball to the tap and gives it a fresh impulse; `on_save_state`
//! persists position + velocity across destroy/restore.

use std::io;
use std::path::Path;

use crate::new::Names;

pub(super) fn write(app_dir: &Path, n: &Names) -> io::Result<()> {
    std::fs::create_dir_all(app_dir.join("src"))?;
    super::write_file(&app_dir.join("Cargo.toml"),    &super::render_cargo_toml(n, "Game-style Akiva app."))?;
    super::write_file(&app_dir.join("manifest.toml"), &super::render_manifest(n, &["akiva.permission.STORAGE_SANDBOX"]))?;
    super::write_file(&app_dir.join("src/lib.rs"),    &render_lib_rs(n))?;
    super::write_file(&app_dir.join(".gitignore"),    super::GITIGNORE)?;
    super::write_file(&app_dir.join("README.md"),     &render_readme(n))?;
    Ok(())
}

fn render_lib_rs(n: &Names) -> String {
    format!(
r#"//! {display} — scaffolded by `akiva new --template game`.
//!
//! Single-ball physics. The runtime calls `on_render` every animation
//! frame; we integrate position by `dt = now - last_ts`, bounce off
//! the canvas edges, and paint the ball as a round-rect with radius =
//! half the side length (a circle).

use akiva_sdk::prelude::*;

const BALL_R: f32       = 24.0;
const INIT_SPEED: f32   = 280.0;        // dp / second
const MAX_DT_MS: f32    = 100.0;        // clamp on tab-resume / first frame
const BG_COLOR: u32     = 0xff0a0e1a;
const BALL_COLOR: u32   = 0xfff97316;
const TEXT_COLOR: u32   = 0xff9aa8bc;

pub struct {class} {{
    /// Ball centre in physical pixels. Centre, not top-left, so wall
    /// collisions are symmetric.
    cx: f32,
    cy: f32,
    /// Velocity in dp / second. Same units as INIT_SPEED.
    vx: f32,
    vy: f32,
    /// Last frame's timestamp (host's `performance.now()` in ms).
    /// `None` until the first frame so `dt` clamps cleanly.
    last_ts: Option<f64>,
    /// Width / height observed on the most recent frame, cached for
    /// `on_pointer` (which has no `metrics`).
    bounds: (f32, f32),
    /// Frames painted in the current second — used to derive an FPS
    /// readout without keeping a rolling history.
    fps_counter: u32,
    /// Wall-clock second the FPS counter is accumulating into. When
    /// `floor(timestamp_ms / 1000)` rolls over, latch and reset.
    fps_window: u64,
    fps_displayed: u32,
}}

impl Default for {class} {{
    fn default() -> Self {{
        Self {{
            cx: 0.0, cy: 0.0, vx: INIT_SPEED, vy: INIT_SPEED * 0.85,
            last_ts: None,
            bounds: (1.0, 1.0),
            fps_counter: 0,
            fps_window: 0,
            fps_displayed: 0,
        }}
    }}
}}

#[async_trait(?Send)]
impl Activity for {class} {{
    async fn on_create(&mut self, ctx: &mut Context<'_>) {{
        log_info!("{class} created ({{}}x{{}} @ {{:.1}}x)",
            ctx.metrics.width_px, ctx.metrics.height_px, ctx.metrics.density);
        // Centre the ball on the first metrics we ever see — a fresh
        // launch has no saved state and `Default` left cx/cy at 0.
        let w = ctx.metrics.width_px as f32;
        let h = ctx.metrics.height_px as f32;
        if self.cx == 0.0 && self.cy == 0.0 {{
            self.cx = w * 0.5;
            self.cy = h * 0.5;
        }}
        self.bounds = (w, h);
    }}

    async fn on_pointer(&mut self, _ctx: &mut Context<'_>, ev: PointerEvent) {{
        if ev.phase == PointerPhase::Down {{
            // Snap the ball to the tap and give it a fresh impulse
            // pointing away from the centre. Feels like a "kick" without
            // needing real RNG.
            self.cx = ev.x;
            self.cy = ev.y;
            let (w, h) = self.bounds;
            let dx = ev.x - w * 0.5;
            let dy = ev.y - h * 0.5;
            let len = (dx * dx + dy * dy).sqrt().max(1.0);
            self.vx = INIT_SPEED * (dx / len);
            self.vy = INIT_SPEED * (dy / len);
        }}
    }}

    async fn on_render(&mut self, ctx: &mut Context<'_>, c: &mut Canvas<'_>) {{
        let w = ctx.metrics.width_px as f32;
        let h = ctx.metrics.height_px as f32;
        self.bounds = (w, h);

        // dt in seconds, clamped. The first frame has no `last_ts`
        // (just booted or just resumed); fall through with dt = 0 so
        // physics stays stable.
        let dt = match self.last_ts {{
            Some(prev) => ((ctx.timestamp_ms - prev) as f32 / 1000.0).clamp(0.0, MAX_DT_MS / 1000.0),
            None       => 0.0,
        }};
        self.last_ts = Some(ctx.timestamp_ms);

        // Integrate position.
        self.cx += self.vx * dt;
        self.cy += self.vy * dt;

        // Bounce off each wall. Reflect velocity sign-wise and clamp the
        // position back inside so the ball can't tunnel on a slow frame.
        if self.cx - BALL_R < 0.0       {{ self.cx = BALL_R;       self.vx = self.vx.abs(); }}
        if self.cx + BALL_R > w         {{ self.cx = w - BALL_R;   self.vx = -self.vx.abs(); }}
        if self.cy - BALL_R < 0.0       {{ self.cy = BALL_R;       self.vy = self.vy.abs(); }}
        if self.cy + BALL_R > h         {{ self.cy = h - BALL_R;   self.vy = -self.vy.abs(); }}

        // Tick the FPS counter once per real-time second.
        let sec = (ctx.timestamp_ms as u64) / 1000;
        if sec != self.fps_window {{
            self.fps_displayed = self.fps_counter;
            self.fps_counter = 0;
            self.fps_window = sec;
        }}
        self.fps_counter += 1;

        // Paint.
        c.clear(BG_COLOR);
        c.fill_round_rect(
            self.cx - BALL_R, self.cy - BALL_R,
            BALL_R * 2.0, BALL_R * 2.0,
            BALL_R, BALL_COLOR,
        );
        c.draw_text(
            16.0, 28.0, 14.0, TEXT_COLOR,
            &format!("{{}} fps · tap to kick", self.fps_displayed),
        );
    }}

    fn on_save_state(&self, state: &mut SavedState) {{
        // f32 bits → i64 round-trip. Saving as ints keeps the wire
        // format small and avoids platform float-format gotchas.
        state.put_int("cx", self.cx.to_bits() as i64);
        state.put_int("cy", self.cy.to_bits() as i64);
        state.put_int("vx", self.vx.to_bits() as i64);
        state.put_int("vy", self.vy.to_bits() as i64);
    }}
    fn on_restore_state(&mut self, state: &SavedState) {{
        if let Some(v) = state.get_int("cx") {{ self.cx = f32::from_bits(v as u32); }}
        if let Some(v) = state.get_int("cy") {{ self.cy = f32::from_bits(v as u32); }}
        if let Some(v) = state.get_int("vx") {{ self.vx = f32::from_bits(v as u32); }}
        if let Some(v) = state.get_int("vy") {{ self.vy = f32::from_bits(v as u32); }}
    }}
}}

akiva_sdk::akiva_app!({class});
"#,
        display = n.display_name,
        class = n.class,
    )
}

fn render_readme(n: &Names) -> String {
    format!(
r#"# {display}

Akiva app scaffolded with `akiva new --template game`.

## What this template demonstrates

* **Animation loop.** The kernel calls `on_render` every animation
  frame regardless of input. Reading `Context::timestamp_ms` and
  integrating against the previous frame's timestamp produces smooth
  motion without any "request redraw" call.
* **Direct canvas drawing.** `Canvas::clear`, `fill_round_rect`, and
  `draw_text` are enough for a small game. The widget toolkit is
  bypassed entirely — useful for canvas-heavy workloads where laying
  out a tree per frame would be wasted work.
* **Pointer physics.** `on_pointer` snaps the ball to the tap and
  reorients velocity away from centre; the ball bounces off walls in
  `on_render`.
* **Saved state for floats.** `f32::to_bits()` round-trips through
  `SavedState::put_int`, so the ball survives a destroy + restore at
  the exact pixel + velocity it left.

## Build & run

```sh
akiva build {dir}
akiva run
```

## Where to look next

* `INIT_SPEED`, `BALL_R`, `MAX_DT_MS` — constants at the top of
  `src/lib.rs`. Feel.
* Add a second ball: lift the per-ball state into a `Vec<Ball>` and
  iterate. Pairwise collisions are an evening's project.
* `akiva docs` — opens the SDK reference (focus on `Canvas`,
  `Context`, `PointerEvent`, `SavedState`).
"#,
        display = n.display_name,
        dir = n.dir,
    )
}
