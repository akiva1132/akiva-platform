//! Stone 13.0prep — async timer primitives.
//!
//! [`sleep`] parks a future for ≥ `delay_ms` of wall-clock time, then
//! resolves. The host caps very long delays at the equivalent of one
//! frame for backgrounded tabs to avoid wakeup storms; foreground tabs
//! get the full requested delay subject to JS event-loop scheduling.
//!
//! [`timeout`] races an arbitrary future against a sleep — useful for
//! "give up after N ms" patterns. Uses a no-op stub on non-wasm
//! targets (host tests).
//!
//! [`Interval`] is a polling stream-like helper: each `tick().await`
//! waits the configured period before resolving. Built from chained
//! `sleep` calls; cancelling the interval is `drop` on the handle.
//!
//! ## Cancel-on-drop semantics
//!
//! Today: a [`Sleep`] dropped before its timer fires leaves a stale
//! entry in the worker's `futures` map until the worker dies. The
//! pre-resolved `STATUS::READY` payload is small (one map entry) so
//! the leak is bounded by `MAX_PENDING_FUTURES = 256`. Honest
//! cancel-on-drop is a 13.0prep follow-up — would require a
//! `host_io_cancel(future_id)` import that doesn't exist yet.

use core::future::Future;
use core::pin::Pin;
use core::task::{Context as TaskContext, Poll};

#[cfg(target_arch = "wasm32")]
use crate::op_kinds::{io, status, unpack_poll};

/// Future returned by [`sleep()`]. Resolves to `()` after the host's
/// wall-clock has advanced by ≥ `delay_ms`. Best-effort —
/// browser-throttling on backgrounded tabs may delay further.
#[must_use = "futures do nothing unless awaited"]
pub struct Sleep {
    #[cfg(target_arch = "wasm32")]
    future_id: u64,
    #[cfg(not(target_arch = "wasm32"))]
    _phantom: core::marker::PhantomData<()>,
}

impl Future for Sleep {
    type Output = ();

    #[cfg(target_arch = "wasm32")]
    fn poll(self: Pin<&mut Self>, _cx: &mut TaskContext<'_>) -> Poll<()> {
        let mut buf = [0u8; 0];
        let packed = unsafe {
            crate::runtime::host_io_poll(self.future_id, &mut buf)
        };
        let (status_code, _len) = unpack_poll(packed);
        match status_code {
            status::READY  => Poll::Ready(()),
            status::ERROR  => Poll::Ready(()),  // host couldn't honour — treat as elapsed
            status::BUSY   => Poll::Ready(()),  // backpressure cap; same harmless reaction
            _              => Poll::Pending,    // PENDING and any future code
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn poll(self: Pin<&mut Self>, _cx: &mut TaskContext<'_>) -> Poll<()> {
        // Host tests / non-wasm builds: resolve immediately. Tests that
        // care about timing should use a manual mock; integration timing
        // is verified on-platform (vitest).
        Poll::Ready(())
    }
}

/// Park the current task for ≥ `delay_ms` of wall-clock time. Returns
/// a [`Sleep`] future the caller `.await`s.
///
/// `delay_ms == 0` resolves on the next host poll (effectively a yield);
/// the host may choose to short-circuit it but the SDK doesn't.
///
/// ```ignore
/// async fn flash(&mut self, ctx: &mut Context<'_>) {
///     self.highlight = true;
///     akiva_sdk::time::sleep(200).await;
///     self.highlight = false;
/// }
/// ```
pub fn sleep(delay_ms: u32) -> Sleep {
    #[cfg(target_arch = "wasm32")]
    {
        let mut args = [0u8; 4];
        args.copy_from_slice(&delay_ms.to_le_bytes());
        let fid = unsafe { crate::runtime::host_io_start(io::TIMER_SLEEP, &args) };
        Sleep { future_id: fid }
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = delay_ms;
        Sleep { _phantom: core::marker::PhantomData }
    }
}

/// Race `fut` against a [`sleep()`] of `ms` milliseconds. Resolves to
/// `Ok(value)` if `fut` finished first, or `Err(Elapsed)` if the timer
/// won. The losing future is dropped (not cancelled — see
/// cancel-on-drop notes at the module top).
///
/// ```ignore
/// match time::timeout(storage::read("k").unwrap(), 500).await {
///     Ok(Ok(Some(bytes))) => self.value = bytes,
///     Ok(Ok(None)) | Ok(Err(_)) => self.value = Vec::new(),
///     Err(time::Elapsed) => log_warn!("storage read timed out"),
/// }
/// ```
pub async fn timeout<F: Future>(fut: F, ms: u32) -> Result<F::Output, Elapsed> {
    // Tiny manual select! — poll both futures each task wake; resolve
    // on whichever lands first. Avoids pulling in a `futures` crate dep.
    use core::future::Future as F2;
    let mut pinned_fut = Box::pin(fut);
    let mut pinned_sleep = Box::pin(sleep(ms));
    core::future::poll_fn(move |cx| {
        if let Poll::Ready(out) = pinned_fut.as_mut().poll(cx) {
            return Poll::Ready(Ok(out));
        }
        if let Poll::Ready(()) = pinned_sleep.as_mut().poll(cx) {
            return Poll::Ready(Err(Elapsed));
        }
        Poll::Pending
    }).await
}

/// Marker error returned by [`timeout`] when the timer won the race.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Elapsed;

/// Periodic ticker. Each `tick().await` waits the configured period.
/// First tick is at `period_ms` (not immediate); call `interval(0)` if
/// you want the first tick to land synchronously.
pub struct Interval {
    period_ms: u32,
}

impl Interval {
    pub async fn tick(&mut self) {
        sleep(self.period_ms).await;
    }
}

/// Build an [`Interval`] that ticks every `period_ms`. Drop the
/// returned handle to stop ticking — the handle owns no host
/// resources beyond what each individual `tick()` allocates.
pub fn interval(period_ms: u32) -> Interval {
    Interval { period_ms }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// On non-wasm host builds, `sleep` resolves immediately. Just a
    /// smoke test that the future shape compiles and isn't !Unpin.
    #[test]
    fn sleep_future_resolves_on_host() {
        let fut = sleep(100);
        let mut pinned = Box::pin(fut);
        let waker = futures_test_waker();
        let mut cx = TaskContext::from_waker(&waker);
        match pinned.as_mut().poll(&mut cx) {
            Poll::Ready(()) => {}
            Poll::Pending => panic!("non-wasm sleep must resolve immediately"),
        }
    }

    #[test]
    fn timeout_returns_ok_when_inner_finishes_first() {
        let inner = async { 42u32 };
        let result: Result<u32, Elapsed> = pollster::block_on(timeout(inner, 1000));
        assert_eq!(result, Ok(42));
    }

    fn futures_test_waker() -> core::task::Waker {
        use core::task::{RawWaker, RawWakerVTable, Waker};
        const VTABLE: RawWakerVTable = RawWakerVTable::new(
            |_| RawWaker::new(core::ptr::null(), &VTABLE),
            |_| {}, |_| {}, |_| {},
        );
        unsafe { Waker::from_raw(RawWaker::new(core::ptr::null(), &VTABLE)) }
    }

    /// Tiny synchronous executor for tests. Avoids pulling in `tokio`
    /// or `futures` for the one or two host-side timing tests.
    mod pollster {
        use core::future::Future;
        use core::pin::Pin;
        use core::task::{Context as TaskContext, Poll};
        pub fn block_on<F: Future>(fut: F) -> F::Output {
            let waker = super::futures_test_waker();
            let mut cx = TaskContext::from_waker(&waker);
            let mut pinned = Box::pin(fut);
            loop {
                match pinned.as_mut().poll(&mut cx) {
                    Poll::Ready(v) => return v,
                    Poll::Pending => {} // tight loop OK on host — no real I/O
                }
            }
        }
    }
}
