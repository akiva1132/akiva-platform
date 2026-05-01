//! Hello Akiva — first-party smoke-test app.
//!
//! Phase 3a demo: everything is declared through the widget toolkit
//! (`akiva_sdk::ui`) rather than raw canvas ops. Paints a title, a live
//! counter, and two buttons that mutate it — exercises text, rect,
//! stacks, padding, and the click-detection path end-to-end.
//!
//! Stone 11.2 — first app on the async-native API. Lifecycle methods
//! are `async fn`; in 11.2 every host import still resolves
//! synchronously so the futures complete in one poll, but the SDK
//! contract is locked in here as the migration template for the other
//! three apps.

use akiva_sdk::prelude::*;

#[derive(Default)]
struct HelloMain {
    ui: UiState,
    count: i32,
}

#[async_trait(?Send)]
impl Activity for HelloMain {
    async fn on_create(&mut self, ctx: &mut Context<'_>) {
        log_info!("HelloMain created ({}x{} @ {:.1}x)",
            ctx.metrics.width_px, ctx.metrics.height_px, ctx.metrics.density);
    }

    async fn on_pointer(&mut self, _ctx: &mut Context<'_>, ev: PointerEvent) {
        // Feed the UI state so it can correlate pointer-down events with
        // the widgets laid out during the next `present()`. No manual
        // hit-testing in this app — the widgets own that now.
        self.ui.feed_pointer(ev);
    }

    async fn on_render(&mut self, ctx: &mut Context<'_>, c: &mut Canvas<'_>) {
        // Paint the dark backdrop ourselves — the widget toolkit doesn't
        // have a full-surface "scene background" concept yet. The rest
        // is declarative.
        c.clear(0xff0f1420);

        let mut ui = Ui::new(&mut self.ui, ctx, c);

        let title = ui.text("Hello, Akiva!").size(28.0).color(0xfff0f4fa).build();
        let subtitle = ui.text("akiva-app-hello · ABI v0.1.0 · async")
            .size(13.0).color(0xff9aa8bc).build();

        let counter_label = ui.text(&format!("count = {}", self.count))
            .size(20.0).color(0xffe0e6f0).build();

        let dec = ui.button("dec", "−1");
        let inc = ui.button("inc", "+1");
        let reset = ui.button("reset", "Reset");
        let buttons = ui.h_stack(&[dec, inc, reset]).gap(12.0).build();

        let root = ui.v_stack(&[title, subtitle, counter_label, buttons])
            .gap(16.0)
            .padding(24.0)
            .build();

        ui.present(root);

        if ui.was_clicked("inc")   { self.count += 1; log_debug!("inc → {}", self.count); }
        if ui.was_clicked("dec")   { self.count -= 1; log_debug!("dec → {}", self.count); }
        if ui.was_clicked("reset") { self.count = 0;  log_debug!("reset"); }
    }
}

akiva_sdk::akiva_app!(HelloMain);
