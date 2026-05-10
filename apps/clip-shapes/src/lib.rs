//! Clip-Shapes — SDK demo for the rounded-shape primitives.
//!
//! Two layers of API for rounded UI:
//!
//!   1. **`Rect::radius`** — a rounded-corner fill, painted via the
//!      compositor's `fill_round_rect` op. Single colour, smooth
//!      anti-aliased edge. The right tool for circles, pills, and
//!      flat cards where the shape *is* the colour.
//!
//!   2. **`Frame::clip_radius`** — a rounded clip applied around a
//!      Frame's children. Necessary when the inside of the rounded
//!      shape contains *more than one* paint op (image + overlay, or
//!      multiple stripes that would otherwise paint past the corner).
//!      Edge AA quality is whatever the renderer's clip mask offers.
//!
//! The first row uses `Rect::radius` for clean circles, pills, and
//! cards. The second row uses `Frame::clip_radius` to clip a
//! deliberately-oversized stripe pattern to a circle and a card —
//! that's the case `Rect::radius` can't cover, and the test sweeps
//! pixels around the bounds to confirm the clip is doing real work.

use akiva_sdk::prelude::*;

#[derive(Default)]
struct ClipShapesMain {
    ui: UiState,
}

#[async_trait(?Send)]
impl Activity for ClipShapesMain {
    async fn on_create(&mut self, ctx: &mut Context<'_>) {
        log_info!(
            "ClipShapes created ({}x{} @ {:.1}x)",
            ctx.metrics.width_px, ctx.metrics.height_px, ctx.metrics.density,
        );
    }

    async fn on_render(&mut self, ctx: &mut Context<'_>, c: &mut Canvas<'_>) {
        c.clear(0xfff3f4f7);

        let mut ui = Ui::new(&mut self.ui, ctx, c);

        let title = ui.text("Clip Shapes").size(22.0).color(0xff15172a).build();

        // ── Row 1: smooth shapes via Rect::radius ──────────────────
        let row1_label = ui.text("Smooth fills · Rect::radius")
            .size(13.0).color(0xff6c7686).build();

        let avatar_size: f32 = 88.0;
        let smooth_circle = |ui: &mut Ui<'_, '_, '_>, tint: u32| {
            ui.rect(tint).size(avatar_size, avatar_size).radius(avatar_size / 2.0).build()
        };
        let c1 = smooth_circle(&mut ui, 0xff4e6cef);
        let c2 = smooth_circle(&mut ui, 0xffe07a4d);
        let c3 = smooth_circle(&mut ui, 0xff35a884);
        let circles_row = ui.h_stack(&[c1, c2, c3]).gap(20.0).build();

        // Pill: dark Rect with `radius == h/2` + centred label. No
        // clip needed — the label fits inside the pill bounds, so a
        // round-rect fill is the simplest correct primitive.
        let pill_h: f32 = 44.0;
        let pill_w: f32 = 200.0;
        let pill_bg = ui.rect(0xff15172a).size(pill_w, pill_h).radius(pill_h / 2.0).build();
        let pill_label_inner = ui.text("View profile").size(14.0).color(0xfff5f5fa).build();
        let pill_label = ui.h_stack(&[pill_label_inner])
            .main(MainAxis::Center).cross(CrossAxis::Center).build();
        let pill = ui.frame(&[pill_bg, pill_label]).build();

        // Card: solid tinted Rect with subtle radius.
        let card_w: f32 = 360.0;
        let card_h: f32 = 100.0;
        let card_bg = ui.rect(0xffffffff).size(card_w, card_h).radius(14.0).build();
        let card_caption = ui.text("Card · Rect::radius=14")
            .size(13.0).color(0xff15172a).build();
        let card_inner = ui.h_stack(&[card_caption])
            .main(MainAxis::Center).cross(CrossAxis::Center).build();
        let card = ui.frame(&[card_bg, card_inner]).build();

        // ── Row 2: Frame::clip_radius — content that needs masking ─
        let row2_label = ui.text("Clipped overflow · Frame::clip_radius")
            .size(13.0).color(0xff6c7686).build();

        // Striped circle. Multiple coloured Rects at fixed offsets
        // whose union exceeds the bounding circle. Without clip, the
        // overflow paints past the circle outline; with clip, the
        // pattern terminates exactly at the rounded edge — that's
        // the test the geometric pixel sweep gates on.
        let striped = striped_circle(&mut ui, avatar_size, 0xff5b66ff);
        let striped_label = ui.text("striped").size(11.0).color(0xff6c7686).build();
        let striped_col = ui.v_stack(&[striped, striped_label]).gap(6.0).cross(CrossAxis::Center).build();

        // "Image-like" placeholder: a band of three coloured rects
        // (a stand-in for an actual image, which lands later when
        // URL-loading is wired in) clipped to a card shape.
        let band_clip = clipped_band_card(&mut ui, card_w, 80.0, 14.0);
        let band_caption = ui.text("Card · clip masks the band overflow")
            .size(12.0).color(0xff6c7686).build();

        let root = ui.v_stack(&[
            title,
            row1_label, circles_row, pill, card,
            row2_label, striped_col, band_clip, band_caption,
        ]).gap(16.0).padding(24.0).build();

        ui.present(root);
    }
}

/// Circle with a horizontal stripe overlay. Stripes are intentionally
/// drawn beyond the circle's bounds; without `clip_radius` they would
/// paint outside the rounded outline. The visual baseline + the
/// per-angle pixel sweep both rely on the clip carving them away.
fn striped_circle(ui: &mut Ui<'_, '_, '_>, size: f32, tint: u32) -> NodeId {
    let bg = ui.rect(tint).size(size, size).build();
    // Three stripes — each 12 dp tall, full width. They paint *over*
    // the bg inside the same Frame, so the Frame's z-stack stretch
    // gives them the same rect as the bg. The clip handles the
    // rounded carve-out.
    let stripe_a = ui.rect(0x33ffffff).size(size, 10.0).build();
    let stripe_b = ui.rect(0x33ffffff).size(size, 10.0).build();
    let stripe_c = ui.rect(0x33ffffff).size(size, 10.0).build();
    // Stack the stripes vertically with gaps so they spread across
    // the frame body. They paint at the v_stack's positions; without
    // the surrounding Frame's clip, the stripes would extend to the
    // bounding rect's corners. With clip_radius=size/2, the corners
    // get carved into the circle.
    let stripes_col = ui.v_stack(&[stripe_a, stripe_b, stripe_c])
        .gap(14.0).build();
    ui.frame(&[bg, stripes_col]).clip_radius(size / 2.0).build()
}

/// A "card" whose body has a stripe band that would overflow the
/// rounded corners without a clip. The band is intentionally drawn
/// at the full frame width and a fixed mid-band height; with
/// `clip_radius`, the band's corners get masked into the card's
/// rounded outline.
fn clipped_band_card(ui: &mut Ui<'_, '_, '_>, w: f32, h: f32, radius: f32) -> NodeId {
    let bg   = ui.rect(0xffffffff).size(w, h).build();
    let band = ui.rect(0xff5b66ff).size(w, 24.0).build();
    let band_row = ui.v_stack(&[band]).main(MainAxis::Center).build();
    ui.frame(&[bg, band_row]).clip_radius(radius).build()
}

akiva_sdk::akiva_app!(ClipShapesMain);
