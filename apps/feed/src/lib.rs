//! Feed — SDK demo for `Node::Scroll` + `Node::LazyColumn`.
//!
//! 80 mock posts in a virtualised list. Memory is bounded by the
//! viewport (+ overscan): a 10 000-row variant would cost the same as
//! this one. Pointer-drag scrolls, wheel scrolls, and pointer-release
//! after a fast drag flings — all owned by the SDK, no `overflow:
//! scroll` anywhere.
//!
//! Each item is a card laid out as `Frame { bg Rect + v_stack(title,
//! body, meta) }`. `Frame::size` locks the card width so the body
//! wrap doesn't blow up past the viewport.

use akiva_sdk::prelude::*;

const ITEM_H: f32 = 112.0;
const ITEM_GAP: f32 = 8.0;
const ITEM_COUNT: u32 = 80;

#[derive(Default)]
struct FeedMain {
    ui: UiState,
}

#[async_trait(?Send)]
impl Activity for FeedMain {
    async fn on_create(&mut self, ctx: &mut Context<'_>) {
        log_info!(
            "Feed created ({}x{} @ {:.1}x)",
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
        c.clear(0xfff3f4f7);

        let mut ui = Ui::new(&mut self.ui, ctx, c);
        let viewport_h = ctx.metrics.height_px as f32;
        let viewport_w = ctx.metrics.width_px as f32;

        let title = ui.text("Akiva Feed")
            .size(22.0).color(0xff15172a).build();
        let subtitle = ui.text("virtualised — only visible items realise")
            .size(13.0).color(0xff6c7686).build();

        // Title strip is fixed at the top; the LazyColumn fills the
        // remainder. 60 dp covers title (22) + subtitle (13) + line
        // spacing + 16 dp bottom gap.
        let title_band_h: f32 = 60.0;
        let list_h = (viewport_h - title_band_h).max(0.0);

        let list = ui.lazy_column(ITEM_COUNT)
            .key("feed")
            .item_h(ITEM_H).gap(ITEM_GAP)
            .padding_trbl(0.0, 16.0, 16.0, 16.0)
            .size(viewport_w, list_h)
            .items(|ui, i| post_card(ui, i));

        let root = ui.v_stack(&[title, subtitle, list])
            .gap(6.0).padding_trbl(20.0, 16.0, 0.0, 16.0)
            .cross(CrossAxis::Stretch)
            .build();
        ui.present(root);
    }
}

/// Build one post card. The card's outer Frame locks its size to the
/// inherited slot via `cross: Stretch` on the inner v_stack — the
/// LazyColumn places each item at `(inner_w, item_h)`, and `Frame`
/// inherits both dimensions.
fn post_card(ui: &mut Ui<'_, '_, '_>, i: u32) -> NodeId {
    // Per-item deterministic content. Picking from a small pool keeps
    // the demo varied without bloating the wasm with a megaword of
    // sample text.
    let titles = [
        "Morning sync notes",
        "Async runtime musings",
        "Pixels at the boundary",
        "Layout invariants",
        "What's on the roadmap",
        "Inspector pattern",
        "Native-grade scroll",
        "Stage 2c is shipping",
    ];
    let bodies = [
        "wrap, ellipsis, clip and the SDK still owns every pixel.",
        "the framework calls render every rAF — fling rides on that.",
        "drag past the slop, release — the glide is OverScroller-style.",
        "Frame::size locks the card; cross=Stretch passes width through.",
        "next up: gestures (2d), springs (2e), then a real feed (Stage 3).",
        "PUBLISH_SEMANTICS bytes; window.__akiva over a flat tree.",
        "no overflow:scroll, no browser scrollbars, no DOM mirror.",
        "wheel, drag, fling — three input shapes, one scroll cell.",
    ];
    let title = titles[i as usize % titles.len()];
    let body = bodies[i as usize % bodies.len()];

    let title_node = ui.text(title).size(15.0).color(0xff15172a).build();
    let body_node = ui.text(body).size(13.0).color(0xff444a55).max_lines(2).build();
    let meta_node = ui.text(&format!("post #{i:03}  ·  2026"))
        .size(11.0).color(0xff8a93a4).build();

    let inner = ui.v_stack(&[title_node, body_node, meta_node])
        .gap(4.0).padding(12.0).cross(CrossAxis::Stretch).build();

    let bg = ui.rect(0xffffffff).radius(12.0).build();
    // The LazyColumn hands every item a slot at (inner_w, ITEM_H);
    // Frame fills it (`stretch_w=true`, `stretch_h=true`) so the bg
    // Rect and inner stack share that slot. No `size()` lock needed.
    ui.frame(&[bg, inner]).build()
}

akiva_sdk::akiva_app!(FeedMain);
