//! Stone 13.2 — System dialog overlays.
//!
//! Apps call [`Context::show_dialog`](crate::activity::Context::show_dialog)
//! to pop a modal dialog above their own surface. The framework — not
//! the app — paints the dialog on a dedicated `WindowLayer::Overlay`
//! surface and routes pointer hit-testing to it. While the dialog is
//! open, the calling activity continues to render but receives no
//! pointer events; the framework dims its surface for visual feedback.
//!
//! The future returned by `show_dialog` resolves when:
//! - the user taps a labeled button → [`DialogResult::Button(idx)`]
//! - the user taps outside the dialog box → [`DialogResult::Dismissed`]
//!
//! ## Wire format (IO::SHOW_DIALOG, 0x0107)
//!
//! Args (app→host):
//! ```text
//! [handle: u32 LE]
//! [title_len: u16 LE | title: utf8...]
//! [body_len:  u16 LE | body:  utf8...]
//! [btn_count: u8]
//! repeat btn_count: [btn_len: u16 LE | btn: utf8...]
//! ```
//!
//! Result (host→app):
//! ```text
//! [kind: u8 | btn_idx: u8]
//! ```
//! `kind == 0` → Button(btn_idx); `kind == 1` → Dismissed (btn_idx ignored).

use core::future::Future;
use core::pin::Pin;
use core::task::{Context as TaskContext, Poll};

use crate::op_kinds::io;
#[cfg(target_arch = "wasm32")]
use crate::op_kinds::{status, unpack_poll};

/// Outcome of a dialog the framework displayed on behalf of an app.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DialogResult {
    /// The user tapped the button at this index in the original
    /// `buttons` slice. Always `< buttons.len()`.
    Button(usize),
    /// The user tapped outside the dialog box, or the framework
    /// dismissed the dialog (e.g. activity destroyed).
    Dismissed,
}

/// Wire kind byte for [`DialogResult::Button`]. Mirrored framework-side
/// in `framework::window_manager::dialog`.
pub const KIND_BUTTON: u8 = 0;
/// Wire kind byte for [`DialogResult::Dismissed`].
pub const KIND_DISMISSED: u8 = 1;

/// Future returned by [`crate::activity::Context::show_dialog`]. Polls
/// `host_io_poll` on each `poll` call until the framework signals that
/// the user has resolved the dialog.
#[must_use = "DialogFuture must be awaited; dropping it without awaiting \
              leaves the framework dialog open until the user taps it"]
pub struct DialogFuture {
    future_id: u64,
    cached: Option<DialogResult>,
}

impl DialogFuture {
    /// Pre-resolved future for the immediate-rejection paths (empty
    /// buttons, host refused outright, non-wasm stub). The first poll
    /// hands back `Dismissed` and the future is done.
    fn pre_dismissed() -> Self {
        Self { future_id: 0, cached: Some(DialogResult::Dismissed) }
    }
}

impl Future for DialogFuture {
    type Output = DialogResult;

    fn poll(mut self: Pin<&mut Self>, _cx: &mut TaskContext<'_>) -> Poll<Self::Output> {
        if let Some(cached) = self.cached {
            return Poll::Ready(cached);
        }
        match poll_dialog_inner(self.future_id) {
            None => Poll::Pending,
            Some(result) => {
                self.cached = Some(result);
                Poll::Ready(result)
            }
        }
    }
}

/// Encode the SHOW_DIALOG args. `handle` identifies the calling
/// activity so the framework can attach the dialog to the right
/// surface.
pub(crate) fn encode_args(handle: u32, title: &str, body: &str, buttons: &[&str]) -> Vec<u8> {
    let mut out = Vec::with_capacity(
        4 + 2 + title.len() + 2 + body.len() + 1
            + buttons.iter().map(|b| 2 + b.len()).sum::<usize>(),
    );
    out.extend_from_slice(&handle.to_le_bytes());
    let title_len = (title.len().min(u16::MAX as usize)) as u16;
    out.extend_from_slice(&title_len.to_le_bytes());
    out.extend_from_slice(&title.as_bytes()[..title_len as usize]);
    let body_len = (body.len().min(u16::MAX as usize)) as u16;
    out.extend_from_slice(&body_len.to_le_bytes());
    out.extend_from_slice(&body.as_bytes()[..body_len as usize]);
    let count = (buttons.len().min(u8::MAX as usize)) as u8;
    out.push(count);
    for label in &buttons[..count as usize] {
        let label_len = (label.len().min(u16::MAX as usize)) as u16;
        out.extend_from_slice(&label_len.to_le_bytes());
        out.extend_from_slice(&label.as_bytes()[..label_len as usize]);
    }
    out
}

/// Internal entry: kick off a dialog and hand back the future. Empty
/// `buttons` is a programmer error and resolves immediately to
/// `Dismissed` rather than panicking — the host would refuse the
/// op anyway.
pub(crate) fn show_dialog_inner(
    handle: u32,
    title: &str,
    body: &str,
    buttons: &[&str],
) -> DialogFuture {
    if buttons.is_empty() {
        return DialogFuture::pre_dismissed();
    }
    let args = encode_args(handle, title, body, buttons);
    let fid = io_start(io::SHOW_DIALOG, &args);
    if fid == 0 {
        return DialogFuture::pre_dismissed();
    }
    DialogFuture { future_id: fid, cached: None }
}

// ── Wasm-only glue ─────────────────────────────────────────────────────

#[cfg(target_arch = "wasm32")]
fn io_start(op_kind: u32, args: &[u8]) -> u64 {
    crate::runtime::host_io_start(op_kind, args)
}

#[cfg(not(target_arch = "wasm32"))]
fn io_start(_op_kind: u32, _args: &[u8]) -> u64 {
    0
}

#[cfg(target_arch = "wasm32")]
fn poll_dialog_inner(future_id: u64) -> Option<DialogResult> {
    use crate::runtime::host_io_poll;
    let mut buf = [0u8; 2];
    let packed = host_io_poll(future_id, &mut buf);
    let (st, len) = unpack_poll(packed);
    match st {
        status::PENDING => None,
        status::READY => {
            if len == 0 {
                return Some(DialogResult::Dismissed);
            }
            let kind = buf[0];
            let idx = if len >= 2 { buf[1] as usize } else { 0 };
            Some(match kind {
                KIND_BUTTON => DialogResult::Button(idx),
                _ => DialogResult::Dismissed,
            })
        }
        // ERROR / BUSY both collapse to Dismissed — the dialog never
        // showed, so from the app's perspective the user took no
        // action. Apps that want a richer error channel can wrap
        // their show_dialog calls in a layer that observes log lines.
        _ => Some(DialogResult::Dismissed),
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn poll_dialog_inner(_future_id: u64) -> Option<DialogResult> {
    Some(DialogResult::Dismissed)
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
    fn encode_args_round_trip_two_buttons() {
        let bytes = encode_args(7, "Clear?", "Reset state", &["Yes", "No"]);
        // [handle:4][title_len:2][title:6][body_len:2][body:11][count:1][len:2][Yes:3][len:2][No:2]
        assert_eq!(bytes.len(), 4 + 2 + 6 + 2 + 11 + 1 + 2 + 3 + 2 + 2);
        assert_eq!(&bytes[0..4], &7u32.to_le_bytes());
        assert_eq!(u16::from_le_bytes([bytes[4], bytes[5]]), 6);
        assert_eq!(&bytes[6..12], b"Clear?");
        assert_eq!(u16::from_le_bytes([bytes[12], bytes[13]]), 11);
        assert_eq!(&bytes[14..25], b"Reset state");
        assert_eq!(bytes[25], 2); // btn_count
    }

    #[test]
    fn encode_args_zero_buttons() {
        let bytes = encode_args(1, "t", "b", &[]);
        // count byte exists, just zero
        assert_eq!(bytes[bytes.len() - 1], 0);
    }

    #[test]
    fn empty_buttons_resolves_dismissed_synchronously() {
        let mut fut = show_dialog_inner(1, "t", "b", &[]);
        let waker = noop_waker();
        let mut cx = TaskContext::from_waker(&waker);
        match Pin::new(&mut fut).poll(&mut cx) {
            Poll::Ready(DialogResult::Dismissed) => {}
            other => panic!("expected Ready(Dismissed), got {other:?}"),
        }
    }

    #[test]
    fn non_wasm_stub_resolves_dismissed() {
        // Without a host the io_start returns 0 and we hand back a
        // pre-resolved Dismissed future — same surface the wasm path
        // produces when the host refuses outright.
        let mut fut = show_dialog_inner(1, "t", "b", &["OK"]);
        let waker = noop_waker();
        let mut cx = TaskContext::from_waker(&waker);
        match Pin::new(&mut fut).poll(&mut cx) {
            Poll::Ready(DialogResult::Dismissed) => {}
            other => panic!("expected Ready(Dismissed), got {other:?}"),
        }
    }

    #[test]
    fn dialog_result_variants_are_distinct() {
        assert_ne!(DialogResult::Button(0), DialogResult::Button(1));
        assert_ne!(DialogResult::Button(0), DialogResult::Dismissed);
    }
}
