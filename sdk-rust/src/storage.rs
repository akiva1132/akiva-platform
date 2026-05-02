//! Storage facade for app code.
//!
//! Per-package sandboxed key/value store backed by OPFS. Every key
//! the app supplies is namespaced under `pkgs/{package_id}/` by the
//! kernel before it lands in OPFS, so two packages writing to the
//! same logical key see independent values and one app can't read
//! another's data even if both have `STORAGE_SANDBOX` granted.
//!
//! ## API
//!
//! P0 #4 made the SDK's dispatch path async-aware: futures returned by
//! [`read`] can suspend mid-callback, which lets app code use direct
//! `.await` instead of the multi-frame `ReadHandle::poll()` pattern
//! that came before.
//!
//! ```ignore
//! struct Notes {
//!     ui: UiState,
//!     text: String,
//! }
//!
//! #[async_trait(?Send)]
//! impl Activity for Notes {
//!     async fn on_create(&mut self, _ctx: &mut Context<'_>) {
//!         match storage::read("notes").await {
//!             Ok(Some(bytes)) => self.text = String::from_utf8_lossy(&bytes).into_owned(),
//!             Ok(None) => {}                       // first launch
//!             Err(_) => {}                          // denied / IO failure
//!         }
//!     }
//! }
//! ```
//!
//! The `.await` here legitimately suspends: the kernel-worker has to
//! round-trip a `host_op` to read OPFS, and the reply only lands on the
//! NEXT JS event-loop tick. The SDK runtime parks the dispatch's
//! future, the worker shell waits for `host_op_done`, then re-polls.
//! From the app's point of view it's a normal `await`.
//!
//! ## Why writes are fire-and-forget
//!
//! [`write`] and [`remove`] return `()`: there's no observable
//! "did the write succeed" path within the same frame for the same
//! reason as reads. The kernel logs failures (quota, IO error),
//! and we accept the standard sandbox-storage discipline that
//! writes are best-effort and durable when possible. A future
//! `WriteHandle` returning a confirmation future is filed under
//! `docs/risks.md` §7.x as a P1 follow-up.

use core::future::Future;
use core::pin::Pin;
use core::task::{Context as TaskContext, Poll};

use crate::op_kinds::io;
#[cfg(target_arch = "wasm32")]
use crate::op_kinds::{status, unpack_poll};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StorageError {
    /// The package doesn't hold `akiva.permission.STORAGE_SANDBOX`,
    /// or the host rejected the call before posting a future.
    Denied,
    /// Empty key, key larger than the host accepts, or args truncated.
    BadKey,
    /// Underlying OPFS / quota / kernel-side reason. The host's
    /// error blob carried details; the SDK doesn't surface them
    /// today (the typed enum is the stable bit; raw reason strings
    /// would couple the SDK to host wording).
    Io,
}

/// Future returned by [`read`]. Polls `host_io_poll` on each `poll`
/// call. When the kernel-worker hasn't replied yet, returns
/// `Poll::Pending`; the SDK runtime parks the parent dispatch and the
/// worker shell drives a re-poll once `host_op_done` arrives. On
/// `READY`/`ERROR` the future resolves to the typed result.
#[must_use = "ReadFuture is the only way to observe the read; dropping it cancels"]
pub struct ReadFuture {
    future_id: u64,
    /// Result is cached the first time we hit a terminal status, so
    /// repeated polls (which Rust forbids in safe code, but no harm
    /// in defending against) return the same value.
    cached: Option<Result<Option<Vec<u8>>, StorageError>>,
}

impl Future for ReadFuture {
    type Output = Result<Option<Vec<u8>>, StorageError>;

    fn poll(mut self: Pin<&mut Self>, _cx: &mut TaskContext<'_>) -> Poll<Self::Output> {
        if let Some(cached) = self.cached.take() {
            // Re-arm in case the user polls again; Future::poll's
            // contract is that returning Ready is terminal but we'd
            // rather defend than UB. Same value reported each time.
            self.cached = Some(cached.clone());
            return Poll::Ready(cached);
        }
        match poll_read_inner(self.future_id) {
            None => Poll::Pending,
            Some(result) => {
                self.cached = Some(result.clone());
                Poll::Ready(result)
            }
        }
    }
}

/// Begin a sandboxed read. Returns `None` if the host rejected the
/// op outright (no grant, malformed args). Otherwise the caller
/// `.await`s the returned future. With the P0 #4 async runtime the
/// `.await` legitimately suspends the dispatch when the kernel-worker
/// hasn't replied yet — no manual polling required.
pub fn read(key: &str) -> Option<ReadFuture> {
    if key.is_empty() { return None; }
    let fid = io_start(io::STORAGE_GET, key.as_bytes());
    if fid == 0 { return None; }
    Some(ReadFuture { future_id: fid, cached: None })
}

/// Fire-and-forget write. The kernel logs any underlying failure
/// (quota, IO) — apps that want stronger guarantees should add an
/// explicit "I just saved successfully" handle in a follow-up Stone.
pub fn write(key: &str, value: &[u8]) {
    if key.is_empty() { return; }
    let mut args = Vec::with_capacity(4 + key.len() + value.len());
    args.extend_from_slice(&(key.len() as u32).to_le_bytes());
    args.extend_from_slice(key.as_bytes());
    args.extend_from_slice(value);
    let _ = io_start(io::STORAGE_SET, &args);
}

/// Fire-and-forget delete. Idempotent — removing a missing key is
/// not an error (the user's intent was "make sure it's gone").
pub fn remove(key: &str) {
    if key.is_empty() { return; }
    let _ = io_start(io::STORAGE_DELETE, key.as_bytes());
}

// ── Wasm-only glue ─────────────────────────────────────────────────────

#[cfg(target_arch = "wasm32")]
fn io_start(op_kind: u32, args: &[u8]) -> u64 {
    crate::runtime::host_io_start(op_kind, args)
}

#[cfg(target_arch = "wasm32")]
fn poll_read_inner(future_id: u64) -> Option<Result<Option<Vec<u8>>, StorageError>> {
    use crate::runtime::host_io_poll;
    // 1 KiB initial guess covers the common "settings blob" case
    // (a few hundred bytes); the retry branch handles larger
    // payloads. Keep the buffer on the heap so a multi-MB value
    // doesn't blow the wasm stack.
    let mut buf = vec![0u8; 1024];
    let packed = host_io_poll(future_id, &mut buf);
    let (st, len) = unpack_poll(packed);
    let len = len as usize;
    match st {
        status::PENDING => None,
        status::ERROR => Some(Err(StorageError::Io)),
        status::READY => {
            if len == 0 {
                return Some(Ok(None));
            }
            if len > buf.len() {
                buf = vec![0u8; len];
                let packed = host_io_poll(future_id, &mut buf);
                let (st2, len2) = unpack_poll(packed);
                if st2 != status::READY {
                    return Some(Err(StorageError::Io));
                }
                buf.truncate((len2 as usize).min(buf.len()));
            } else {
                buf.truncate(len);
            }
            // Result: [present:u8, value_bytes...]
            if buf.is_empty() { return Some(Ok(None)); }
            let present = buf[0];
            if present == 0 {
                Some(Ok(None))
            } else {
                Some(Ok(Some(buf[1..].to_vec())))
            }
        }
        _ => Some(Err(StorageError::Io)),
    }
}

// ── Non-wasm builds: stub out so cargo test on the host doesn't
// need a runtime. Apps shouldn't call storage functions outside of
// wasm anyway; the stubs match the wasm shape so caller code
// compiles uniformly.

#[cfg(not(target_arch = "wasm32"))]
fn io_start(_op_kind: u32, _args: &[u8]) -> u64 {
    0
}

#[cfg(not(target_arch = "wasm32"))]
fn poll_read_inner(_future_id: u64) -> Option<Result<Option<Vec<u8>>, StorageError>> {
    Some(Ok(None))
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::task::{Context as TaskContext, Poll, RawWaker, RawWakerVTable, Waker};

    fn noop_waker() -> Waker {
        static VT: RawWakerVTable = RawWakerVTable::new(
            |_| RawWaker::new(core::ptr::null(), &VT),
            |_| {}, |_| {}, |_| {},
        );
        unsafe { Waker::from_raw(RawWaker::new(core::ptr::null(), &VT)) }
    }

    #[test]
    fn read_with_empty_key_returns_none() {
        // Empty-key reject happens before any host call. The wasm
        // stub for io_start also returns 0, so empty + non-wasm both
        // produce None — the typed signature pins it either way.
        assert!(read("").is_none());
    }

    #[test]
    fn write_with_empty_key_is_a_silent_no_op() {
        write("", b"value");
    }

    #[test]
    fn remove_with_empty_key_is_a_silent_no_op() {
        remove("");
    }

    #[test]
    fn read_future_resolves_to_none_on_non_wasm_stub() {
        // The non-wasm `poll_read_inner` returns Some(Ok(None)) on
        // every call — the tests path doesn't have a real host. So
        // a fake ReadFuture polled directly should resolve to that
        // value on the first poll.
        let mut fut = ReadFuture { future_id: 1, cached: None };
        let waker = noop_waker();
        let mut cx = TaskContext::from_waker(&waker);
        let pinned = Pin::new(&mut fut);
        match pinned.poll(&mut cx) {
            Poll::Ready(Ok(None)) => {}
            other => panic!("expected Ready(Ok(None)), got {other:?}"),
        }
    }
}
