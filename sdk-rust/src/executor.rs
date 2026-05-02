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
//! through raw pointers ([`build_async_future`] in `runtime.rs`)
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
//! 3. **Callback dispatches** that hold raw-pointer borrows into
//!    `&mut Activity` / `&mut Context` are serialised per handle by
//!    the framework's `PendingTransition` discipline — at most one
//!    callback runs at a time, so the raw-ptr `&mut Activity` borrow
//!    never aliases. **User-spawned tasks** (Stone 11.6.5 #11) live
//!    in `Slot.spawned` and DO NOT borrow `&mut Activity`/`&mut
//!    Context`; they're independent `'static` futures, so multiple
//!    of them — and concurrent host calls within them — are safe to
//!    run in parallel with a callback dispatch.
//!
//! Stone 11.6.5 (#12) made `Slot.pending` a `HashMap<u64,
//! PendingDispatch>` so the surface admits multiple concurrent
//! callback entries in principle. The framework still serialises
//! today (the soundness above is upheld); the structural change just
//! removes the false invariant that `slot.pending` is `Option<...>`.
//!
//! Violating any of these is undefined behaviour. The runtime asserts
//! invariant #2 on slot teardown (slot Drop runs spawned + pending
//! futures' drops before the activity drops, see field declaration
//! order in `Slot`); #3 is upheld by the framework, not the SDK.

use core::cell::Cell;
use core::future::Future;
use core::pin::Pin;
use core::task::{Context as TaskContext, Poll, RawWaker, RawWakerVTable, Waker};

thread_local! {
    /// Stone 11.6.8 #3 — single global "someone called wake" flag.
    ///
    /// Set by `make_waker()` whenever a future calls `.wake()` /
    /// `.wake_by_ref()`. Read by `drive_spawned` (in `runtime.rs`) so
    /// it can re-poll while internal coordination is still making
    /// progress. The single global flag is sufficient because wasm is
    /// single-threaded and per-task waker fidelity isn't needed —
    /// `drive_spawned` re-polls ALL spawned tasks anyway, so a single
    /// "someone is awake" signal triggers the correct behaviour.
    ///
    /// Use [`take_wake_flag`] to read-and-reset atomically; do not
    /// touch the cell directly outside this module.
    static WAKE_FLAG: Cell<bool> = const { Cell::new(false) };
}

/// Read the wake flag and clear it. Returns `true` if any future
/// called its waker since the last call to this function.
///
/// Called by `drive_spawned` between poll passes; if true, repeat the
/// pass because at least one task has fresh work to do (e.g. a
/// channel `recv()` got a value pushed by another spawned task's
/// `send()`). Called once per dispatch poll cycle from `poll_dispatch`
/// for the same reason on the callback path.
pub fn take_wake_flag() -> bool {
    WAKE_FLAG.with(|f| f.replace(false))
}

/// Smart waker that sets the wake flag on every `wake` /
/// `wake_by_ref`. The data pointer is null and is never dereferenced
/// — the wake side stores its signal in a thread-local, not in the
/// waker payload, because every waker in this runtime points at the
/// same single-threaded executor.
fn make_waker() -> Waker {
    static VTABLE: RawWakerVTable = RawWakerVTable::new(
        |_| RawWaker::new(core::ptr::null(), &VTABLE),
        |_| WAKE_FLAG.with(|f| f.set(true)), // wake
        |_| WAKE_FLAG.with(|f| f.set(true)), // wake_by_ref
        |_| {},                              // drop
    );
    let raw = RawWaker::new(core::ptr::null(), &VTABLE);
    // SAFETY: the vtable is `'static` and never dereferences the data
    // pointer; null is a safe stand-in.
    unsafe { Waker::from_raw(raw) }
}

/// Poll a stored future once with the runtime's smart waker. Returns
/// the same `Poll` the future itself returned, so the caller can
/// decide how to react (park on Pending, finalise on Ready). If the
/// future calls `.wake()` during this poll, [`take_wake_flag`] reads
/// `true` afterward — the caller (`drive_spawned`, `poll_dispatch`)
/// uses that to decide whether to re-poll.
pub fn poll_once<T>(
    future: &mut Pin<Box<dyn Future<Output = T> + 'static>>,
) -> Poll<T> {
    let waker = make_waker();
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
    let waker = make_waker();
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
