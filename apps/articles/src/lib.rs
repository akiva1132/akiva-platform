//! Articles — SDK demo for multi-line text.
//!
//! Three side-by-side variants of the same paragraph so the visual
//! goldens capture all three behaviours at once:
//!
//!   1. **Single line** (default, `max_lines=1`) — overflows past
//!      the right edge as a calculator-style label would.
//!   2. **Wrap, no truncation** (`max_lines = u32::MAX`, exposed
//!      via `Text::wrap`) — body copy that reflows to fit.
//!   3. **Wrap with truncation** (`max_lines(N)`) — captures the
//!      "headline + N lines + …" pattern of every list-row preview
//!      across modern feeds.
//!
//! Each variant sits in a card so the visible bounds make the wrap
//! behaviour easy to inspect: text overflows the card past its
//! right edge in row 1, fills the card body in row 2, ends with `…`
//! in row 3.

use akiva_sdk::prelude::*;

const PARAGRAPH: &str =
    "Akiva renders to canvas, so layout decisions land at the pixel \
     level — wrap, ellipsis, and line height all happen here in the \
     SDK rather than deferring to the browser's text engine. The \
     trade-off is straightforward: every behaviour we want has to be \
     written once, but we get the same shape on native shells later.";

#[derive(Default)]
struct ArticlesMain {
    ui: UiState,
}

#[async_trait(?Send)]
impl Activity for ArticlesMain {
    async fn on_create(&mut self, ctx: &mut Context<'_>) {
        log_info!(
            "Articles created ({}x{} @ {:.1}x)",
            ctx.metrics.width_px, ctx.metrics.height_px, ctx.metrics.density,
        );
    }

    async fn on_render(&mut self, ctx: &mut Context<'_>, c: &mut Canvas<'_>) {
        c.clear(0xfff3f4f7);

        let mut ui = Ui::new(&mut self.ui, ctx, c);

        let title = ui.text("Articles").size(22.0).color(0xff15172a).build();
        let subtitle = ui.text("multi-line text — single, wrap, max_lines + …")
            .size(13.0).color(0xff6c7686).build();

        // Card width drives the wrap width every variant inherits
        // through `cross=Stretch` on the inner v_stack.
        const CARD_W: f32 = 360.0;

        // Row 1 — single line (default `max_lines=1`). Overflows
        // past the card's right edge intentionally, demonstrating
        // the no-wrap baseline.
        let row1 = card_with_caption(
            &mut ui, CARD_W, 80.0, "single line · overflows",
            |ui| ui.text(PARAGRAPH).size(13.0).color(0xff15172a).build(),
        );

        // Row 2 — wrap, no truncation. The card body fills with as
        // many lines as the paragraph needs.
        let row2 = card_with_caption(
            &mut ui, CARD_W, 220.0, "wrap · all lines",
            |ui| ui.text(PARAGRAPH).size(13.0).color(0xff15172a).wrap().build(),
        );

        // Row 3 — wrap with `max_lines = 3` + ellipsis. The
        // "feed preview" pattern.
        let row3 = card_with_caption(
            &mut ui, CARD_W, 110.0, "max_lines(3) · trailing …",
            |ui| ui.text(PARAGRAPH).size(13.0).color(0xff15172a).max_lines(3).build(),
        );

        let root = ui.v_stack(&[title, subtitle, row1, row2, row3])
            .gap(16.0).padding(24.0).build();

        ui.present(root);
    }
}

/// Build one row card: a fixed-size white rounded rectangle with a
/// small caption + a body (provided by the caller's closure) inside.
/// `Frame::size(w, h)` locks the frame to the visible bounds so the
/// body Text wraps at `w - padding`, not at the parent stack's slot
/// — without this, the long single-line text in row 1 would stretch
/// the frame past the bg Rect's 360 dp width and rows 2/3 would wrap
/// at the screen width instead of at the card's visible right edge.
/// `cross=Stretch` on the inner v_stack passes the locked inner
/// width through to the body Text.
fn card_with_caption(
    ui: &mut Ui<'_, '_, '_>,
    w: f32, h: f32, caption: &str,
    body: impl FnOnce(&mut Ui<'_, '_, '_>) -> NodeId,
) -> NodeId {
    let cap = ui.text(caption).size(11.0).color(0xff6c7686).build();
    let body_id = body(ui);
    let inner = ui.v_stack(&[cap, body_id])
        .gap(6.0).padding(14.0).cross(CrossAxis::Stretch).build();
    let bg = ui.rect(0xffffffff).size(w, h).radius(12.0).build();
    ui.frame(&[bg, inner]).size(w, h).build()
}

akiva_sdk::akiva_app!(ArticlesMain);
