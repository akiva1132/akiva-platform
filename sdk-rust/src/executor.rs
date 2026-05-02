//! Mini async executor for the SDK runtime.
//!
//! P0 #4 — gives `akiva_app_dispatch` a real async runtime so app code
//! can `.await` on host operations that legitimately suspend (storage,
//! eventually network, etc.). The legacy [`crate::activity::block_on_ready`]
//! poller panicked on `Pending`; this module replaces it with a single-
//! task-per-handle executor that parks the future when the host op
//! hasn't resolved and resumes it on the next `akiva_app_poll_dispatch`
//! call.
//!
//! ## Why not embassy
//!
//! `embassy-executor` is a robust no_std runtime with built-in timer
//! drivers, interrupt-driven wakers, and thread-pool task spawning.
//! None of that applies in our context: wasm in a worker is single-
//! threaded, the only "wake" event is a postMessage from the kernel
//! worker arriving at the JS host, and we don't have hardware timers
//! at all. The full embassy footprint (~10 KiB compiled) for features
//! we don't use isn't justified — this module is ~80 lines of code,
//! tightly fitted to the single-future-per-slot pattern.
//!
//! ## Soundness — self-referential storage
//!
//! Each `PendingDispatch` holds a `Pin<Box<dyn Future<Output = ...> +
//! 'static>>` whose body internally borrows `&mut Box<dyn Activity>`
//! and `&mut Context` from the same `Slot`. The borrows are encoded
//! through raw pointers ([`build_dispatch_future`] in `runtime.rs`)
//! because Rust can't express "two mutable borrows of disjoint
//! fields, captured into a 'static future."
//!
//! The invariant the runtime maintains:
//!
//! 1. Every `Slot` lives in `HashMap<u32, Box<Slot>>`. The `Box<Slot>`
//!    pins the slot at a stable heap address until the entry is
//!    removed.
//! 2. A `PendingDispatch` is associated with exactly one slot. Before
//!    the slot is removed, every pending dispatch targeting it is
//!    dropped.
//! 3. The framework's `PendingTransition` mechanism guarantees only
//!    one in-flight dispatch per handle — no aliasing of the same
//!    `&mut Activity` from concurrent futures.
//!
//! Violating any of these is undefined behaviour. The runtime asserts
//! invariant #2 on slot teardown (`debug_assert!`); #3 is upheld by
//! the framework, not the SDK.

use core::future::Future;
use core::pin::Pin;
use core::task::{Context as TaskContext, Poll, RawWaker, RawWakerVTable, Waker};

/// Single-task waker that does nothing on `wake`. We don't need a real
/// notification mechanism: the runtime always re-polls a parked future
/// on `akiva_app_poll_dispatch`, which is driven from outside by the
/// app worker shell whenever a `host_op_done` reply arrives. The wasm
/// side has no clock and no thread to schedule against, so a real
/// waker would be inert anyway.
fn noop_waker() -> Waker {
    static VTABLE: RawWakerVTable = RawWakerVTable::new(
        |_| RawWaker::new(core::ptr::null(), &VTABLE),
        |_| {},
        |_| {},
        |_| {},
    );
    let raw = RawWaker::new(core::ptr::null(), &VTABLE);
    // SAFETY: the vtable is `'static` and never deref's the data
    // pointer; null is a valid stand-in for a no-op waker.
    unsafe { Waker::from_raw(raw) }
}

/// Poll a stored future once with a no-op waker. Returns the same
/// `Poll` the future itself returned, so the caller can decide how
/// to react (park on Pending, finalise on Ready). Existence of this
/// helper rather than inlining at the call site makes it easy to
/// thread an additional concern (logging, metrics) through every
/// poll later.
pub fn poll_once<T>(
    future: &mut Pin<Box<dyn Future<Output = T> + 'static>>,
) -> Poll<T> {
    let waker = noop_waker();
    let mut cx = TaskContext::from_waker(&waker);
    future.as_mut().poll(&mut cx)
}

/// Drive a future to its first `Ready` result with a no-op waker.
/// Equivalent to the legacy `block_on_ready`, retained for synchronous
/// callbacks (e.g. `on_save_state`) that the runtime guarantees never
/// suspend. Panics on `Pending` — same canary as before.
///
/// The new async-aware path is `poll_once` plus the parking machinery
/// in [`crate::runtime`]; this helper is for code paths that don't
/// participate in that.
pub fn run_to_ready<F: Future>(f: F) -> F::Output {
    let waker = noop_waker();
    let mut cx = TaskContext::from_waker(&waker);
    let mut fut = Box::pin(f);
    match fut.as_mut().poll(&mut cx) {
        Poll::Ready(v) => v,
        Poll::Pending => panic!(
            "executor::run_to_ready: future returned Pending — this code \
             path requires synchronous resolution. Use the dispatch \
             machinery in crate::runtime for futures that may suspend."
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn poll_once_returns_ready_for_immediate_future() {
        let mut fut: Pin<Box<dyn Future<Output = i32> + 'static>> =
            Box::pin(async { 42 });
        match poll_once(&mut fut) {
            Poll::Ready(v) => assert_eq!(v, 42),
            Poll::Pending => panic!("expected Ready"),
        }
    }

    #[test]
    fn poll_once_returns_pending_without_panic() {
        struct Always;
        impl Future for Always {
            type Output = ();
            fn poll(self: Pin<&mut Self>, _cx: &mut TaskContext<'_>) -> Poll<()> {
                Poll::Pending
            }
        }
        let mut fut: Pin<Box<dyn Future<Output = ()> + 'static>> = Box::pin(Always);
        // Repeated polls keep returning Pending — no panic, no state loss.
        assert!(matches!(poll_once(&mut fut), Poll::Pending));
        assert!(matches!(poll_once(&mut fut), Poll::Pending));
    }

    #[test]
    fn run_to_ready_threads_immediate_value() {
        let v = run_to_ready(async { 7 });
        assert_eq!(v, 7);
    }

    #[test]
    #[should_panic(expected = "future returned Pending")]
    fn run_to_ready_panics_on_pending() {
        struct Pending;
        impl Future for Pending {
            type Output = ();
            fn poll(self: Pin<&mut Self>, _cx: &mut TaskContext<'_>) -> Poll<()> {
                Poll::Pending
            }
        }
        run_to_ready(Pending);
    }
}
