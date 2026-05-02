//! Stone 11.5e test app — verifies the kernel-side watchdog.
//!
//! Press the button: `on_pointer` enters a `loop {}` that never
//! returns. The wasm thread of this app's worker stops pumping
//! incoming postMessage events. The kernel-worker's ping loop sees
//! no pong for ≥1.5s, calls `worker.terminate()`, and stages this
//! package in `_pendingTraps`. Next frame's drain marks every
//! activity in this package `Trapped`; `mark_package_trapped` pops
//! us off the foreground stack and drives whatever's underneath
//! (the Shell) back to Resumed. The user lands at the launcher.
//!
//! Pre-button: behaves like a tiny normal app so launching feels
//! ordinary; the trap-machinery only fires on the explicit click.

use akiva_sdk::prelude::*;

#[derive(Default)]
struct InfiniteLoopMain {
    ui: UiState,
}

#[async_trait(?Send)]
impl Activity for InfiniteLoopMain {
    async fn on_create(&mut self, _ctx: &mut Context<'_>) {
        log_info!("InfiniteLoopMain ready — click 'Hang me' to test the watchdog");
    }

    async fn on_pointer(&mut self, _ctx: &mut Context<'_>, ev: PointerEvent) {
        self.ui.feed_pointer(ev);
    }

    async fn on_render(&mut self, ctx: &mut Context<'_>, c: &mut Canvas<'_>) {
        c.clear(0xff1a0f1a);
        let mut ui = Ui::new(&mut self.ui, ctx, c);

        let title = ui.text("Watchdog test").size(24.0).color(0xfff0f4fa).build();
        let blurb = ui.text("Click 'Hang me' to enter a busy loop in on_pointer.")
            .size(13.0).color(0xffaa9bb5).build();
        let blurb2 = ui.text("Kernel should terminate this worker in ~1.5s and bounce you to Shell.")
            .size(13.0).color(0xffaa9bb5).build();

        let hang = ui.button("hang", "Hang me");

        let root = ui.v_stack(&[title, blurb, blurb2, hang])
            .gap(14.0)
            .padding(24.0)
            .build();
        ui.present(root);

        if ui.was_clicked("hang") {
            log_warn!("Hang requested — entering busy loop. Watchdog should terminate this worker shortly.");
            // Volatile-style read so the optimiser can't lower the loop
            // to `unreachable!()` — release builds with LTO are fond
            // of that. Spin on a counter the compiler can't prove
            // unobservable; the side effect is the spin itself.
            //
            // In a real wasm worker, this hangs the JS event loop in
            // the app-worker (single-threaded). The kernel-worker
            // remains alive on its own thread and triggers
            // `worker.terminate()` once 1.5s of silent ping has
            // elapsed.
            let mut spin: u64 = 0;
            loop {
                spin = spin.wrapping_add(1);
                if spin == u64::MAX {
                    // Unreachable in any real timeline — this branch
                    // exists solely to keep `spin` "live" so the
                    // compiler doesn't constant-fold the loop away.
                    log_error!("loop counter wrapped (impossible)");
                    break;
                }
            }
        }
    }
}

akiva_sdk::akiva_app!(InfiniteLoopMain);
