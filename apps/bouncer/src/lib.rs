//! Bouncer — SDK demo for `Spring` + `AnimatedValue<f32>`.
//!
//! A single square in the middle of a track. Drag it anywhere
//! inside the track; release, and the square springs back to the
//! centre. Three chips below the track swap the spring preset
//! (Bouncy / Stiff / Wobbly) so you can feel the difference between
//! stiffness and damping.
//!
//! Everything in this app is built on the public 2e API:
//!   - `Spring` for the physics config.
//!   - `AnimatedValue<f32>` for the per-axis animated position.
//!   - One `Spring::tick`-equivalent per frame via
//!     `AnimatedValue::tick(dt_ms)`.

use akiva_sdk::prelude::*;

const SQUARE_SIZE: f32 = 88.0;
const TRACK_H: f32 = 360.0;
const TITLE_BAND_H: f32 = 64.0;
const TRACK_PAD_X: f32 = 24.0;

const BG: u32 = 0xfff3f4f7;
const TRACK_BG: u32 = 0xffe3e6ee;
const SQUARE_COLOR_BASE: u32 = 0xff2256d3;       // royal blue
const SQUARE_COLOR_GRABBED: u32 = 0xff4e7cf0;    // brighter while held

#[derive(Default, Clone, Copy, PartialEq, Eq)]
enum Preset { #[default] Bouncy, Stiff, Wobbly }

impl Preset {
    fn spring(self) -> Spring {
        match self {
            Preset::Bouncy => Spring::BOUNCY,
            Preset::Stiff  => Spring::HIGH,
            // Wobbly: very low damping. Will overshoot multiple
            // times before settling — fun to watch.
            Preset::Wobbly => Spring {
                stiffness: 200.0,
                damping_ratio: 0.3,
                epsilon: 0.5,
            },
        }
    }
    fn label(self) -> &'static str {
        match self {
            Preset::Bouncy => "Bouncy",
            Preset::Stiff  => "Stiff",
            Preset::Wobbly => "Wobbly",
        }
    }
    fn id(self) -> &'static str {
        match self {
            Preset::Bouncy => "preset-bouncy",
            Preset::Stiff  => "preset-stiff",
            Preset::Wobbly => "preset-wobbly",
        }
    }
}

/// In-flight drag state. `offset` is the finger-relative anchor
/// inside the square at the moment of Down — we keep this so the
/// square doesn't "jump" under the finger on Down (the touch
/// stays glued to where it grabbed).
#[derive(Clone, Copy)]
struct Grab { offset_x: f32, offset_y: f32 }

#[derive(Default)]
struct BouncerMain {
    ui: UiState,
    /// Box position offset from track centre, in dp. (0, 0) =
    /// resting at centre. Tracked as two `AnimatedValue`s so the
    /// per-axis springs are independent (release-toward-centre
    /// settles each axis at its own pace).
    av_x: AnimatedValue<f32>,
    av_y: AnimatedValue<f32>,
    grab: Option<Grab>,
    preset: Preset,
    /// Wall-clock timestamp of the previous `on_render`, used to
    /// derive `dt_ms` for the spring tick. `0.0` until the first
    /// frame has run.
    last_ts: f64,
}

/// Geometry helper shared by `on_pointer` (hit-test) and
/// `on_render` (paint). Both must agree on where the track lives.
struct Layout {
    track_w: f32,
    track_h: f32,
    /// Track centre in screen space (centre of the track rect,
    /// not the square's centre).
    centre_x: f32,
    centre_y: f32,
}

impl Layout {
    fn compute(viewport_w: f32) -> Self {
        let track_w = (viewport_w - 2.0 * TRACK_PAD_X).max(0.0);
        Self {
            track_w, track_h: TRACK_H,
            centre_x: TRACK_PAD_X + track_w / 2.0,
            centre_y: TITLE_BAND_H + TRACK_H / 2.0,
        }
    }

    /// Top-left of the square given the current animated offset.
    fn square_top_left(&self, av_x: f32, av_y: f32) -> (f32, f32) {
        (
            self.centre_x - SQUARE_SIZE / 2.0 + av_x,
            self.centre_y - SQUARE_SIZE / 2.0 + av_y,
        )
    }

    /// True if the point `(px, py)` is inside the square at its
    /// current animated offset.
    fn hit_square(&self, av_x: f32, av_y: f32, px: f32, py: f32) -> bool {
        let (sx, sy) = self.square_top_left(av_x, av_y);
        px >= sx && px < sx + SQUARE_SIZE
            && py >= sy && py < sy + SQUARE_SIZE
    }

    /// Clamp an animated offset so the square stays inside the
    /// track rect (no escape past the edges).
    fn clamp_offset(&self, av_x: f32, av_y: f32) -> (f32, f32) {
        let half_w = (self.track_w - SQUARE_SIZE) / 2.0;
        let half_h = (self.track_h - SQUARE_SIZE) / 2.0;
        (av_x.clamp(-half_w, half_w), av_y.clamp(-half_h, half_h))
    }
}

#[async_trait(?Send)]
impl Activity for BouncerMain {
    async fn on_create(&mut self, ctx: &mut Context<'_>) {
        log_info!(
            "Bouncer created ({}x{} @ {:.1}x)",
            ctx.metrics.width_px, ctx.metrics.height_px, ctx.metrics.density,
        );
    }

    async fn on_pointer(&mut self, ctx: &mut Context<'_>, ev: PointerEvent) {
        // Feed the SDK's hit-test pipeline so the chip buttons get
        // click resolution. Then handle the drag-the-square gesture
        // ourselves — there's no built-in primitive for arbitrary
        // free drag.
        self.ui.feed_pointer(ev);
        let layout = Layout::compute(ctx.metrics.width_px as f32);
        match ev.phase {
            PointerPhase::Down => {
                if layout.hit_square(self.av_x.value(), self.av_y.value(), ev.x, ev.y) {
                    let (sx, sy) = layout.square_top_left(self.av_x.value(), self.av_y.value());
                    self.grab = Some(Grab {
                        offset_x: ev.x - sx,
                        offset_y: ev.y - sy,
                    });
                    // Drag overrides any in-flight spring.
                    self.av_x.set_immediately(self.av_x.value());
                    self.av_y.set_immediately(self.av_y.value());
                }
            }
            PointerPhase::Move => {
                if let Some(g) = self.grab {
                    // New square top-left position derived from the
                    // current finger position and the grab offset.
                    let new_sx = ev.x - g.offset_x;
                    let new_sy = ev.y - g.offset_y;
                    // Convert screen-space top-left back to
                    // centre-relative offset.
                    let raw_x = new_sx - (layout.centre_x - SQUARE_SIZE / 2.0);
                    let raw_y = new_sy - (layout.centre_y - SQUARE_SIZE / 2.0);
                    let (cx, cy) = layout.clamp_offset(raw_x, raw_y);
                    self.av_x.set_immediately(cx);
                    self.av_y.set_immediately(cy);
                }
            }
            PointerPhase::Up => {
                if self.grab.take().is_some() {
                    // Spring back to centre with the active preset.
                    let s = self.preset.spring();
                    self.av_x.spring_to(0.0, s);
                    self.av_y.spring_to(0.0, s);
                }
            }
            PointerPhase::Cancel => {
                self.grab = None;
            }
        }
    }

    async fn on_render(&mut self, ctx: &mut Context<'_>, c: &mut Canvas<'_>) {
        c.clear(BG);

        // Advance both springs by the wall-clock delta since the
        // last frame. First frame seeds last_ts so dt_ms is sane;
        // 0.0 is a safe sentinel since timestamps are ms since
        // activity start (always > 0 on the second frame onward).
        if self.last_ts > 0.0 {
            let dt_ms = (ctx.timestamp_ms - self.last_ts) as f32;
            self.av_x.tick(dt_ms);
            self.av_y.tick(dt_ms);
        }
        self.last_ts = ctx.timestamp_ms;

        let viewport_w = ctx.metrics.width_px as f32;
        let layout = Layout::compute(viewport_w);

        let mut ui = Ui::new(&mut self.ui, ctx, c);

        // ── Chip clicks ──────────────────────────────────────────
        // Resolve clicks before the build so the new active preset
        // is reflected in the paint.
        if ui.was_clicked(Preset::Bouncy.id()) { self.preset = Preset::Bouncy; }
        if ui.was_clicked(Preset::Stiff.id())  { self.preset = Preset::Stiff;  }
        if ui.was_clicked(Preset::Wobbly.id()) { self.preset = Preset::Wobbly; }

        // ── Title strip ──────────────────────────────────────────
        let title = ui.text("Akiva Bouncer")
            .size(22.0).color(0xff15172a).build();
        let subtitle = ui.text("drag the square — release to spring back")
            .size(13.0).color(0xff6c7686).build();
        let title_strip = ui.v_stack(&[title, subtitle])
            .gap(6.0).padding_trbl(20.0, 0.0, 0.0, 24.0).build();

        // ── Track (Frame positioning the square via padding) ─────
        // The Frame's left/top padding pushes the inner square to
        // the right/bottom, giving us absolute positioning without
        // a new primitive. Clamp the offset so an in-flight spring
        // can't push the square out of the track.
        let (clamped_x, clamped_y) = layout.clamp_offset(
            self.av_x.value(), self.av_y.value(),
        );
        let square_left = layout.track_w / 2.0 - SQUARE_SIZE / 2.0 + clamped_x;
        let square_top = layout.track_h / 2.0 - SQUARE_SIZE / 2.0 + clamped_y;
        let square_right = (layout.track_w - SQUARE_SIZE - square_left).max(0.0);
        let square_bottom = (layout.track_h - SQUARE_SIZE - square_top).max(0.0);

        let track_bg = ui.rect(TRACK_BG).size(layout.track_w, layout.track_h)
            .radius(20.0).build();
        let square_color = if self.grab.is_some() {
            SQUARE_COLOR_GRABBED
        } else {
            SQUARE_COLOR_BASE
        };
        let square = ui.rect(square_color)
            .size(SQUARE_SIZE, SQUARE_SIZE).radius(14.0).build();
        let square_layer = ui.frame(&[square])
            .padding_trbl(square_top, square_right, square_bottom, square_left)
            .build();
        let track = ui.frame(&[track_bg, square_layer])
            .build();
        let track_row = ui.h_stack(&[track])
            .main(MainAxis::Center).build();

        // ── Chip row ─────────────────────────────────────────────
        // Stock SDK buttons — click resolution comes from the
        // standard hit-test pipeline above. Visual active state
        // lives in the status line; an explicit highlight on the
        // active chip is a polish pass for once the SDK exposes
        // post-construction button-bg styling.
        let chip_for = |ui: &mut Ui<'_, '_, '_>, p: Preset| -> NodeId {
            let btn = ui.button(p.id(), p.label());
            ui.corner_radius(btn, 22.0);
            btn
        };
        let chip_b = chip_for(&mut ui, Preset::Bouncy);
        let chip_s = chip_for(&mut ui, Preset::Stiff);
        let chip_w = chip_for(&mut ui, Preset::Wobbly);
        let chips_row = ui.h_stack(&[chip_b, chip_s, chip_w])
            .gap(12.0).main(MainAxis::Center).build();

        // ── Status ───────────────────────────────────────────────
        let status_text = if self.grab.is_some() {
            "dragging".to_string()
        } else if !self.av_x.is_settled() || !self.av_y.is_settled() {
            format!("springing back · {}", self.preset.label().to_lowercase())
        } else {
            format!("at rest · preset: {}", self.preset.label().to_lowercase())
        };
        let status = ui.text(&status_text).size(12.0).color(0xff6c7686).build();
        let status_row = ui.h_stack(&[status])
            .main(MainAxis::Center).build();

        let root = ui.v_stack(&[title_strip, track_row, chips_row, status_row])
            .gap(16.0).padding_trbl(0.0, 0.0, 20.0, 0.0)
            .cross(CrossAxis::Stretch).build();
        ui.present(root);
    }
}

akiva_sdk::akiva_app!(BouncerMain);
