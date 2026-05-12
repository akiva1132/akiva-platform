//! Carousel — SDK demo for `Node::LazyRow` + `SnapMode::Page`.
//!
//! 12 colourful cards laid out horizontally. Each card is the
//! viewport's width (minus the side gutter), so dragging left/right
//! flips one page at a time — the iOS-style "page-swipe" feel
//! Compose calls `HorizontalPager`.
//!
//! Pointer-drag + fling + page-snap all come from the SDK; the app
//! only forwards events. Below the row, a "n / 12" counter reads
//! `Ui::scroll_offset("carousel")` and divides by the viewport
//! width to derive the current page.

use akiva_sdk::prelude::*;
use akiva_sdk::ui::SnapMode;

const ITEM_COUNT: u32 = 12;
const GUTTER: f32 = 16.0;
const STRIP_BG: u32 = 0xfff3f4f7;

#[derive(Default)]
struct CarouselMain {
    ui: UiState,
}

#[async_trait(?Send)]
impl Activity for CarouselMain {
    async fn on_create(&mut self, ctx: &mut Context<'_>) {
        log_info!(
            "Carousel created ({}x{} @ {:.1}x)",
            ctx.metrics.width_px, ctx.metrics.height_px, ctx.metrics.density,
        );
    }

    async fn on_pointer(&mut self, _ctx: &mut Context<'_>, ev: PointerEvent) {
        self.ui.feed_pointer(ev);
    }

    async fn on_wheel(&mut self, _ctx: &mut Context<'_>, ev: WheelEvent) {
        self.ui.feed_wheel(ev);
    }

    async fn on_render(&mut self, ctx: &mut Context<'_>, c: &mut Canvas<'_>) {
        c.clear(STRIP_BG);

        let viewport_w = ctx.metrics.width_px as f32;
        let viewport_h = ctx.metrics.height_px as f32;

        let mut ui = Ui::new(&mut self.ui, ctx, c);
        let title = ui.text("Akiva Carousel")
            .size(22.0).color(0xff15172a).build();
        let subtitle = ui.text("drag left/right — fling + snap-to-page")
            .size(13.0).color(0xff6c7686).build();

        // Title strip ~60 dp, indicator strip ~36 dp, row fills the
        // remainder. Page width = full viewport so SnapMode::Page
        // lands on integer pages.
        let title_band: f32 = 60.0;
        let indicator_band: f32 = 36.0;
        let row_h = (viewport_h - title_band - indicator_band).max(0.0);
        let row = ui.lazy_row(ITEM_COUNT)
            .key("carousel")
            .item_w(viewport_w)
            .gap(0.0)
            .size(viewport_w, row_h)
            .snap(SnapMode::Page)
            .items(|ui, i| card(ui, i, viewport_w, row_h));

        // Indicator text uses the just-cached scroll offset. The
        // first frame reads 0 (no cell yet — scroll_offset returns
        // the zero default), so "1 / 12" shows correctly on first
        // paint. Subsequent frames read the previous-frame snapshot.
        let scroll_now = ui.scroll_offset("carousel");
        let page = if viewport_w > 0.0 {
            ((scroll_now / viewport_w).round() as i32)
                .clamp(0, ITEM_COUNT as i32 - 1) as u32
        } else {
            0
        };
        let indicator_text = format!("{} / {}", page + 1, ITEM_COUNT);
        let indicator = ui.text(&indicator_text)
            .size(13.0).color(0xff15172a).build();

        let root = ui.v_stack(&[title, subtitle, row, indicator])
            .gap(6.0).padding_trbl(20.0, 0.0, 16.0, 0.0)
            .cross(CrossAxis::Stretch)
            .build();
        ui.present(root);
    }
}

/// Build one carousel card. The LazyRow hands every item a slot at
/// `(page_w, page_h)`; we centre a `(page_w - 2*GUTTER) × page_h`
/// rounded background inside that slot with side spacers so two
/// adjacent cards have visible separation between their rounded edges.
fn card(ui: &mut Ui<'_, '_, '_>, i: u32, page_w: f32, page_h: f32) -> NodeId {
    // 12 hand-picked card backgrounds. Pairs (bg, fg) chosen so the
    // title has good contrast on every card.
    const PALETTE: [(u32, u32); 12] = [
        (0xff2256d3, 0xfffeffff),  // royal blue
        (0xfff05d4a, 0xfffeffff),  // coral
        (0xff15b97a, 0xfffeffff),  // emerald
        (0xffd6a000, 0xff1f1500),  // amber on near-black
        (0xff8c5cf6, 0xfffeffff),  // violet
        (0xff0aa5d4, 0xfffeffff),  // azure
        (0xfff15ba0, 0xfffeffff),  // pink
        (0xff14b87b, 0xff032018),  // teal on deep-green
        (0xfff58f00, 0xff1c1100),  // tangerine on near-black
        (0xff6c45c8, 0xfffeffff),  // indigo
        (0xff2c3956, 0xfffeffff),  // slate
        (0xffd03050, 0xfffeffff),  // crimson
    ];
    let (bg, fg) = PALETTE[i as usize % PALETTE.len()];

    let titles = [
        "Page one",
        "Edges that don't tear",
        "Snap to grid",
        "OverScroller fling",
        "Compose orientation",
        "Inspector v3",
        "LazyRow virtualises",
        "Tap vs drag, decided",
        "Pixels we own",
        "No DOM scroll",
        "Three input shapes",
        "Last card",
    ];
    let bodies = [
        "the first card sits at scroll = 0.",
        "frame.size locks the surface, clip seals the edges.",
        "release a drag — the page glides to the nearest edge.",
        "exponential decay, dt-normalised, 4-frame tab-resume guard.",
        "Orientation::Horizontal lives on the cell, not in a generic.",
        "the wire format added one byte; the API barely noticed.",
        "only items in the visible band realise; the rest cost nothing.",
        "DRAG_SLOP gates a sideways wobble from eating a button press.",
        "no overflow:scroll, no scrollbars, no DOM mirror under the canvas.",
        "wheel, drag, fling — three input shapes, one scroll cell.",
        "12 cards, 12 colours, zero allocations per frame.",
        "scroll past it and you'll find the edge.",
    ];
    let title = titles[i as usize % titles.len()];
    let body = bodies[i as usize % bodies.len()];

    let inner_w = (page_w - 2.0 * GUTTER).max(0.0);

    let title_node = ui.text(title).size(26.0).color(fg).max_lines(2).build();
    let body_node = ui.text(body).size(14.0).color(fg).max_lines(3).build();
    let meta_text = format!("card {} of {}", i + 1, ITEM_COUNT);
    let meta_node = ui.text(&meta_text)
        .size(12.0).color(fg).build();
    let inner = ui.v_stack(&[title_node, body_node, meta_node])
        .gap(10.0).padding(28.0).cross(CrossAxis::Stretch).build();

    // The bg rect locks the rounded surface to (inner_w × page_h);
    // the inner stack inherits the same slot via cross: Stretch.
    let bg_node = ui.rect(bg).size(inner_w, page_h).radius(24.0).build();
    let card_node = ui.frame(&[bg_node, inner]).build();

    // Side gutters as plain Spacer nodes — fixed-size, no grow. The
    // h_stack defaults to Start alignment so the order is
    // left-spacer, card, right-spacer; everything fills page_h on
    // the cross axis.
    let left_gutter = ui.spacer().size(GUTTER, 0.0).build();
    let right_gutter = ui.spacer().size(GUTTER, 0.0).build();
    ui.h_stack(&[left_gutter, card_node, right_gutter])
        .cross(CrossAxis::Stretch).build()
}

akiva_sdk::akiva_app!(CarouselMain);
