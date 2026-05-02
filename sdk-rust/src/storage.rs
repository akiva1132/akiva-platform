//! Storage facade for app code (Stone 11.6).
//!
//! Per-package sandboxed key/value store backed by OPFS. Every key
//! the app supplies is namespaced under `pkgs/{package_id}/` by the
//! kernel before it lands in OPFS, so two packages writing to the
//! same logical key see independent values and one app can't read
//! another's data even if both have `STORAGE_SANDBOX` granted.
//!
//! ## Why reads are deferred (multi-frame polling)
//!
//! Storage handlers run in the kernel-worker, but apps run in their
//! own per-package workers. A read involves two postMessages: the
//! app-worker queues a `host_op` request, and the kernel-worker
//! replies with `host_op_done`. The reply CAN'T arrive while the
//! current `akiva_app_dispatch` callback is still running — the
//! worker's JS event loop is blocked by the wasm thread.
//!
//! That's why [`read`] returns a [`ReadHandle`] you poll across
//! frames, not a synchronous `Result`. A typical pattern:
//!
//! ```ignore
//! struct Notes {
//!     ui: UiState,
//!     text: String,
//!     loading: Option<storage::ReadHandle>,
//! }
//!
//! #[async_trait(?Send)]
//! impl Activity for Notes {
//!     async fn on_create(&mut self, _ctx: &mut Context<'_>) {
//!         self.loading = storage::read("notes");
//!     }
//!     async fn on_render(&mut self, ctx: &mut Context<'_>, c: &mut Canvas<'_>) {
//!         if let Some(handle) = &self.loading {
//!             if let Some(result) = handle.poll() {
//!                 if let Ok(Some(bytes)) = result {
//!                     self.text = String::from_utf8(bytes).unwrap_or_default();
//!                 }
//!                 self.loading = None;
//!             }
//!         }
//!         // ... render with self.text ...
//!     }
//! }
//! ```
//!
//! ## Why writes are fire-and-forget
//!
//! [`write`] and [`remove`] return `()`: there's no observable
//! "did the write succeed" path within the same frame for the same
//! reason as reads. The kernel logs failures (quota, IO error),
//! and we accept the standard sandbox-storage discipline that
//! writes are best-effort and durable when possible.

use crate::op_kinds::{io, status, unpack_poll};

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

/// Pending read. Call [`Self::poll`] each render until it returns
/// `Some`; the round-trip resolves at most one frame after `read`
/// returned the handle. Single-shot — once `poll` yields `Some`,
/// the underlying future is consumed and subsequent polls return
/// `Some(Err(Io))`.
#[must_use = "ReadHandle is the only way to observe the read; dropping it is a leak"]
pub struct ReadHandle {
    future_id: u64,
}

impl ReadHandle {
    /// Poll once. Returns:
    /// - `None` — kernel hasn't replied yet; try again next frame.
    /// - `Some(Ok(Some(bytes)))` — key existed, value is `bytes`
    ///   (possibly empty).
    /// - `Some(Ok(None))` — key not present in this package's sandbox.
    /// - `Some(Err(...))` — denied / IO failure.
    pub fn poll(&self) -> Option<Result<Option<Vec<u8>>, StorageError>> {
        poll_read_inner(self.future_id)
    }
}

/// Begin a sandboxed read. Returns `None` if the host rejected the
/// op outright (no grant, malformed args). Otherwise the caller must
/// drive the returned handle to completion via [`ReadHandle::poll`].
pub fn read(key: &str) -> Option<ReadHandle> {
    if key.is_empty() { return None; }
    let fid = io_start(io::STORAGE_GET, key.as_bytes());
    if fid == 0 { return None; }
    Some(ReadHandle { future_id: fid })
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
                // Empty payload means the host rejected the request
                // post-future-start (shouldn't happen for storage)
                // or returned present:0 with no value bytes. We
                // can't disambiguate without a payload, so default
                // to "not present" — the only way `len==0` happens
                // for a sandboxed read.
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

    #[test]
    fn read_with_empty_key_returns_none() {
        // Empty-key reject happens before any host call. On non-wasm
        // the `io_start` stub also returns 0, so the empty path is
        // the only observable difference — but it's the right
        // behavior either way, and pinning it here means the SDK
        // doesn't accidentally drift if io_start is ever changed
        // to accept empty keys.
        assert!(read("").is_none());
    }

    #[test]
    fn write_with_empty_key_is_a_silent_no_op() {
        // No way to observe from a unit test that this didn't post
        // anything (host_io_start is stubbed on non-wasm). The
        // contract is "empty key gets dropped" — assert via the
        // function's typed signature returning `()` and not
        // panicking.
        write("", b"value");
    }

    #[test]
    fn remove_with_empty_key_is_a_silent_no_op() {
        remove("");
    }
}
