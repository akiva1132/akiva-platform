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

        // Stash the finish request out of the Ui borrow so we can fire
        // ctx.finish() after present() returns the Ui borrow.
        let mut want_back = false;

        {
            let mut ui = Ui::new(&mut self.ui, ctx, c);

            let title = ui.text("Watchdog test").size(24.0).color(0xfff0f4fa).build();
            let blurb = ui.text("Click 'Hang me' to enter a busy loop in on_pointer.")
                .size(13.0).color(0xffaa9bb5).build();
            let blurb2 = ui.text("Kernel should terminate this worker in ~1.5s and bounce you to Shell.")
                .size(13.0).color(0xffaa9bb5).build();

            let hang = ui.button("hang", "Hang me");
            // Touch-friendly exit: the framework has a Back keycode for
            // physical keyboards but phones don't have one, and Akiva's
            // window manager doesn't expose a system-level back gesture
            // yet. Until it does, every test app should ship its own
            // way out — `ctx.finish()` pops the activity off the stack
            // identically to the Back key.
            let back = ui.button("back", "Back to Shell");

            let root = ui.v_stack(&[title, blurb, blurb2, hang, back])
                .gap(14.0)
                .padding(24.0)
                .build();
            ui.present(root);

            if ui.was_clicked("back") {
                want_back = true;
            }

            if ui.was_clicked("hang") {
                log_warn!("Hang requested — entering busy loop. Watchdog should terminate this worker shortly.");
                // First attempt used a `loop { spin += 1; if spin == MAX
                // break }` pattern with a tail log_error! to "keep spin
                // observable". LLVM at `opt-level = "z"` + LTO solved
                // the loop algebraically (linear counter with constant
                // termination → replace with `spin = MAX; break;`) and
                // the loop exited in microseconds — the watchdog never
                // saw a hang because the worker was never hung.
                //
                // `core::hint::black_box` is the right tool: it laund-
                // ers a value through an opaque-to-the-optimizer barrier.
                // The compiler can't prove the side effect doesn't matter,
                // can't fold across the call, and is forced to emit the
                // increment + comparison as real work each iteration.
                let mut spin: u64 = 0;
                loop {
                    spin = core::hint::black_box(spin).wrapping_add(1);
                    // Equally opaque comparison so the loop's exit
                    // condition isn't proven dead either.
                    if core::hint::black_box(spin) == u64::MAX {
                        break;
                    }
                }
            }
        }
        // ui borrow released; ctx is exclusively ours again.
        if want_back {
            ctx.finish().await;
        }
    }
}

akiva_sdk::akiva_app!(InfiniteLoopMain);
