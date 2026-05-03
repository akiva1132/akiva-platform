//! System clipboard — Stone 14.4.
//!
//! Apps with `akiva.permission.CLIPBOARD` declared in their manifest
//! can read and write the system clipboard. The browser layer adds
//! its own runtime checks: most browsers require `navigator.clipboard`
//! calls to come from a focused page on a secure origin, with a
//! recent user activation. The SDK surfaces failures from any of
//! those layers as [`ClipboardError::Host`] with the browser's reason
//! attached.
//!
//! Today only UTF-8 text round-trips — `read_text` / `write_text`.
//! Binary clipboard items (images, custom MIME types) live behind
//! the `navigator.clipboard.read()` / `write()` APIs and are a
//! follow-up; the wire shape is sized to extend without renumbering.
//!
//! ```ignore
//! use akiva_sdk::clipboard;
//!
//! async fn copy_id(s: &str) -> Result<(), clipboard::ClipboardError> {
//!     clipboard::write_text(s).await
//! }
//!
//! async fn read_or_blank() -> String {
//!     clipboard::read_text().await.unwrap_or_default()
//! }
//! ```

use core::future::Future;
use core::pin::Pin;
use core::task::{Context as TaskContext, Poll};

use crate::op_kinds::io;
#[cfg(target_arch = "wasm32")]
use crate::op_kinds::{status, unpack_poll};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClipboardError {
    /// The package doesn't hold `akiva.permission.CLIPBOARD`, or
    /// the host rejected the call before posting a future.
    Denied,
    /// Browser refused: typically "no user activation", "no focus",
    /// "no text on the clipboard", or a quota / IO error. Reason
    /// carries the browser's error text.
    Host(String),
    /// Stone 11.6.8 #2 — slot backpressure.
    Busy,
}

/// Read clipboard text. Resolves with the current clipboard
/// contents, or an [`ClipboardError::Host`] if the browser refuses
/// (no text, no focus, no user activation, etc.).
pub fn read_text() -> ReadTextFuture {
    let fid = io_start(io::CLIPBOARD_READ_TEXT, &[]);
    if fid == 0 {
        return ReadTextFuture::pre_resolved(Err(ClipboardError::Denied));
    }
    ReadTextFuture { future_id: fid, cached: None }
}

/// Write `text` to the clipboard. Resolves `Ok(())` on a successful
/// write or [`ClipboardError::Host`] on browser refusal.
pub fn write_text(text: &str) -> WriteTextFuture {
    let bytes = text.as_bytes();
    let fid = io_start(io::CLIPBOARD_WRITE_TEXT, bytes);
    if fid == 0 {
        return WriteTextFuture::pre_resolved(Err(ClipboardError::Denied));
    }
    WriteTextFuture { future_id: fid, cached: None }
}

#[must_use]
pub struct ReadTextFuture {
    future_id: u64,
    cached: Option<Result<String, ClipboardError>>,
}

impl ReadTextFuture {
    fn pre_resolved(r: Result<String, ClipboardError>) -> Self {
        Self { future_id: 0, cached: Some(r) }
    }
}

impl Future for ReadTextFuture {
    type Output = Result<String, ClipboardError>;

    fn poll(mut self: Pin<&mut Self>, _cx: &mut TaskContext<'_>) -> Poll<Self::Output> {
        if let Some(cached) = self.cached.take() {
            self.cached = Some(cached.clone());
            return Poll::Ready(cached);
        }
        match poll_read_text_inner(self.future_id) {
            None => Poll::Pending,
            Some(result) => {
                self.cached = Some(result.clone());
                Poll::Ready(result)
            }
        }
    }
}

#[must_use]
pub struct WriteTextFuture {
    future_id: u64,
    cached: Option<Result<(), ClipboardError>>,
}

impl WriteTextFuture {
    fn pre_resolved(r: Result<(), ClipboardError>) -> Self {
        Self { future_id: 0, cached: Some(r) }
    }
}

impl Future for WriteTextFuture {
    type Output = Result<(), ClipboardError>;

    fn poll(mut self: Pin<&mut Self>, _cx: &mut TaskContext<'_>) -> Poll<Self::Output> {
        if let Some(cached) = self.cached.take() {
            self.cached = Some(cached.clone());
            return Poll::Ready(cached);
        }
        match poll_write_text_inner(self.future_id) {
            None => Poll::Pending,
            Some(result) => {
                self.cached = Some(result.clone());
                Poll::Ready(result)
            }
        }
    }
}

// ── wasm/non-wasm glue ─────────────────────────────────────────────────

#[cfg(target_arch = "wasm32")]
fn io_start(op_kind: u32, args: &[u8]) -> u64 { crate::runtime::host_io_start(op_kind, args) }

#[cfg(not(target_arch = "wasm32"))]
fn io_start(_op_kind: u32, _args: &[u8]) -> u64 { 0 }

#[cfg(target_arch = "wasm32")]
fn poll_read_text_inner(future_id: u64) -> Option<Result<String, ClipboardError>> {
    use crate::runtime::host_io_poll;
    if future_id == 0 { return Some(Err(ClipboardError::Denied)); }
    let mut buf = vec![0u8; 4096];
    let packed = host_io_poll(future_id, &mut buf);
    let (st, len) = unpack_poll(packed);
    let len = len as usize;
    match st {
        status::PENDING => None,
        status::READY => {
            if len > buf.len() {
                buf = vec![0u8; len];
                let packed = host_io_poll(future_id, &mut buf);
                let (st2, len2) = unpack_poll(packed);
                if st2 != status::READY {
                    return Some(Err(ClipboardError::Host("retry not READY".into())));
                }
                buf.truncate((len2 as usize).min(buf.len()));
            } else {
                buf.truncate(len);
            }
            Some(Ok(String::from_utf8_lossy(&buf).into_owned()))
        }
        status::ERROR => {
            buf.truncate(len.min(buf.len()));
            Some(Err(ClipboardError::Host(String::from_utf8_lossy(&buf).into_owned())))
        }
        status::BUSY => Some(Err(ClipboardError::Busy)),
        _ => Some(Err(ClipboardError::Host("unknown status".into()))),
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn poll_read_text_inner(_future_id: u64) -> Option<Result<String, ClipboardError>> {
    Some(Err(ClipboardError::Denied))
}

#[cfg(target_arch = "wasm32")]
fn poll_write_text_inner(future_id: u64) -> Option<Result<(), ClipboardError>> {
    use crate::runtime::host_io_poll;
    if future_id == 0 { return Some(Err(ClipboardError::Denied)); }
    let mut buf = [0u8; 1];
    let packed = host_io_poll(future_id, &mut buf);
    let (st, _len) = unpack_poll(packed);
    match st {
        status::PENDING => None,
        status::READY => Some(Ok(())),
        status::ERROR => Some(Err(ClipboardError::Host("write failed".into()))),
        status::BUSY => Some(Err(ClipboardError::Busy)),
        _ => Some(Err(ClipboardError::Host("unknown status".into()))),
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn poll_write_text_inner(_future_id: u64) -> Option<Result<(), ClipboardError>> {
    Some(Err(ClipboardError::Denied))
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::task::{RawWaker, RawWakerVTable, Waker};

    fn noop_waker() -> Waker {
        static VT: RawWakerVTable = RawWakerVTable::new(
            |_| RawWaker::new(core::ptr::null(), &VT),
            |_| {}, |_| {}, |_| {},
        );
        unsafe { Waker::from_raw(RawWaker::new(core::ptr::null(), &VT)) }
    }

    #[test]
    fn read_text_on_non_wasm_resolves_denied() {
        let mut f = read_text();
        let waker = noop_waker();
        let mut cx = TaskContext::from_waker(&waker);
        match Pin::new(&mut f).poll(&mut cx) {
            Poll::Ready(Err(ClipboardError::Denied)) => {}
            other => panic!("expected Ready(Err(Denied)), got {other:?}"),
        }
    }

    #[test]
    fn write_text_on_non_wasm_resolves_denied() {
        let mut f = write_text("hello");
        let waker = noop_waker();
        let mut cx = TaskContext::from_waker(&waker);
        match Pin::new(&mut f).poll(&mut cx) {
            Poll::Ready(Err(ClipboardError::Denied)) => {}
            other => panic!("expected Ready(Err(Denied)), got {other:?}"),
        }
    }
}
