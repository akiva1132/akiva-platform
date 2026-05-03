//! System notifications — Stone 14.3.
//!
//! Apps with `akiva.permission.NOTIFICATIONS` declared in their
//! manifest can post OS-level notifications that the host surfaces
//! in the system tray (or whatever the platform's notification
//! shade is — Akiva runtime delegates to the browser's
//! [`Notification`] API, which routes through the platform's native
//! notification center on every supported OS).
//!
//! Two layers of permission gate the API:
//!
//! 1. **System layer** — `akiva.permission.NOTIFICATIONS` declared in
//!    `manifest.toml`, granted at install time. Apps without it see
//!    [`NotifyError::Denied`] from every call.
//! 2. **Browser layer** — the user's `Notification.permission` state.
//!    `Default` (the install-time state) means the user hasn't
//!    decided yet; calling [`post`] in this state returns
//!    [`NotifyError::PermissionNotGranted`] without trying to display
//!    anything. Apps must call [`request_permission`] from a user-
//!    gesture handler to surface the browser prompt; the resolved
//!    [`PermissionStatus`] tells the app what the user picked.
//!
//! Posting the same `tag` twice updates the existing notification in
//! place rather than spawning a duplicate, matching the standard
//! browser convention. Apps that want every event to surface
//! independently should leave the tag empty — the host then
//! generates a fresh internal id per call.
//!
//! ```ignore
//! use akiva_sdk::notification;
//!
//! async fn warn_user(ctx: &mut Context<'_>) {
//!     match notification::request_permission().await {
//!         notification::PermissionStatus::Granted => {
//!             let _ = notification::post("Disk full", "Free up some space").await;
//!         }
//!         notification::PermissionStatus::Denied => { /* user said no */ }
//!         notification::PermissionStatus::Default => {
//!             // Browser prompt didn't surface; probably called outside
//!             // a user gesture. Try again from a click handler.
//!         }
//!     }
//! }
//! ```

use core::cell::Cell;
use core::future::Future;
use core::pin::Pin;
use core::task::{Context as TaskContext, Poll};

use crate::op_kinds::{fire, io};
#[cfg(target_arch = "wasm32")]
use crate::op_kinds::{status, unpack_poll};

/// Result of [`request_permission`]. Maps the three browser
/// `NotificationPermission` values 1:1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionStatus {
    /// User has not yet decided. Apps that haven't called
    /// `request_permission` from a user-gesture handler also see
    /// this — browsers may auto-deny non-gesture prompts but the
    /// state itself stays `Default`.
    Default = 0,
    Granted = 1,
    Denied = 2,
}

impl PermissionStatus {
    /// Decode a single status byte (the wire reply for
    /// [`io::NOTIFY_REQUEST_PERMISSION`]).
    pub fn from_wire(byte: u8) -> Option<Self> {
        match byte {
            0 => Some(PermissionStatus::Default),
            1 => Some(PermissionStatus::Granted),
            2 => Some(PermissionStatus::Denied),
            _ => None,
        }
    }

    pub fn is_granted(self) -> bool { matches!(self, PermissionStatus::Granted) }
}

/// Errors surfaced from the notification API.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotifyError {
    /// The package doesn't hold `akiva.permission.NOTIFICATIONS`,
    /// or the host rejected the call before posting a future.
    Denied,
    /// The browser's `Notification.permission` is not `granted`.
    /// Apps should call [`request_permission`] first.
    PermissionNotGranted,
    /// Empty title — the wire format requires at least a
    /// non-empty title to render anything visible.
    BadTitle,
    /// Underlying browser refusal or constructor throw. Reason
    /// carries the host's error message.
    Host(String),
    /// Stone 11.6.8 #2 — slot backpressure. Co-operative apps
    /// retry after a yield.
    Busy,
}

/// Handle to a posted notification. Drop or call [`close`] to
/// dismiss it.
#[derive(Debug)]
pub struct Notification {
    id: u32,
    closed: Cell<bool>,
}

impl Notification {
    /// Dismiss this notification. Idempotent.
    pub fn close(&self) {
        if self.closed.replace(true) { return; }
        let mut args = [0u8; 4];
        args.copy_from_slice(&self.id.to_le_bytes());
        host_fire(fire::NOTIFY_CLOSE, &args);
    }

    /// The opaque host-side id. Useful only for diagnostics — the
    /// dispatcher rejects cross-package use.
    pub fn id(&self) -> u32 { self.id }
}

impl Drop for Notification {
    fn drop(&mut self) {
        if !self.closed.get() {
            self.close();
        }
    }
}

/// Post a system notification with `title`, `body`, and optional
/// `tag`. An empty `tag` lets the browser allocate a fresh slot per
/// call; a non-empty `tag` collapses duplicates with the same tag
/// into a single visible notification (this is the standard
/// convention apps lean on for "you have N unread …" updates).
pub fn post(title: &str, body: &str) -> PostFuture {
    post_with_tag(title, body, "")
}

pub fn post_with_tag(title: &str, body: &str, tag: &str) -> PostFuture {
    if title.is_empty() {
        return PostFuture::pre_resolved(Err(NotifyError::BadTitle));
    }
    let args = encode_post_args(title, body, tag);
    let fid = io_start(io::NOTIFY_POST, &args);
    if fid == 0 {
        return PostFuture::pre_resolved(Err(NotifyError::Denied));
    }
    PostFuture { future_id: fid, cached: None }
}

/// Request the browser-level notification permission. Should be
/// called from a user-gesture handler (e.g. a button-click path);
/// browsers auto-deny non-gesture prompts on most platforms.
pub fn request_permission() -> RequestPermissionFuture {
    let fid = io_start(io::NOTIFY_REQUEST_PERMISSION, &[]);
    if fid == 0 {
        return RequestPermissionFuture::pre_resolved(Err(NotifyError::Denied));
    }
    RequestPermissionFuture { future_id: fid, cached: None }
}

#[must_use]
pub struct PostFuture {
    future_id: u64,
    cached: Option<Result<u32, NotifyError>>,
}

impl PostFuture {
    fn pre_resolved(r: Result<Notification, NotifyError>) -> Self {
        match r {
            Ok(_) => Self { future_id: 0, cached: Some(Err(NotifyError::Denied)) },
            Err(e) => Self { future_id: 0, cached: Some(Err(e)) },
        }
    }
}

impl Future for PostFuture {
    type Output = Result<Notification, NotifyError>;

    fn poll(mut self: Pin<&mut Self>, _cx: &mut TaskContext<'_>) -> Poll<Self::Output> {
        if let Some(cached) = self.cached.take() {
            self.cached = Some(cached.clone());
            return Poll::Ready(cached.map(|id| Notification { id, closed: Cell::new(false) }));
        }
        match poll_post_inner(self.future_id) {
            None => Poll::Pending,
            Some(result) => {
                self.cached = Some(result.clone());
                Poll::Ready(result.map(|id| Notification { id, closed: Cell::new(false) }))
            }
        }
    }
}

#[must_use]
pub struct RequestPermissionFuture {
    future_id: u64,
    cached: Option<Result<PermissionStatus, NotifyError>>,
}

impl RequestPermissionFuture {
    fn pre_resolved(r: Result<PermissionStatus, NotifyError>) -> Self {
        Self { future_id: 0, cached: Some(r) }
    }
}

impl Future for RequestPermissionFuture {
    type Output = Result<PermissionStatus, NotifyError>;

    fn poll(mut self: Pin<&mut Self>, _cx: &mut TaskContext<'_>) -> Poll<Self::Output> {
        if let Some(cached) = self.cached.take() {
            self.cached = Some(cached.clone());
            return Poll::Ready(cached);
        }
        match poll_request_permission_inner(self.future_id) {
            None => Poll::Pending,
            Some(result) => {
                self.cached = Some(result.clone());
                Poll::Ready(result)
            }
        }
    }
}

/// Encode the [`io::NOTIFY_POST`] args wire format.
pub(crate) fn encode_post_args(title: &str, body: &str, tag: &str) -> Vec<u8> {
    let title_len = (title.len().min(u16::MAX as usize)) as u16;
    let body_len = (body.len().min(u16::MAX as usize)) as u16;
    let tag_len = (tag.len().min(u16::MAX as usize)) as u16;
    let mut out = Vec::with_capacity(
        2 + title_len as usize + 2 + body_len as usize + 2 + tag_len as usize,
    );
    out.extend_from_slice(&title_len.to_le_bytes());
    out.extend_from_slice(&title.as_bytes()[..title_len as usize]);
    out.extend_from_slice(&body_len.to_le_bytes());
    out.extend_from_slice(&body.as_bytes()[..body_len as usize]);
    out.extend_from_slice(&tag_len.to_le_bytes());
    out.extend_from_slice(&tag.as_bytes()[..tag_len as usize]);
    out
}

// ── wasm/non-wasm glue ─────────────────────────────────────────────────

#[cfg(target_arch = "wasm32")]
fn io_start(op_kind: u32, args: &[u8]) -> u64 { crate::runtime::host_io_start(op_kind, args) }

#[cfg(not(target_arch = "wasm32"))]
fn io_start(_op_kind: u32, _args: &[u8]) -> u64 { 0 }

#[cfg(target_arch = "wasm32")]
fn host_fire(op_kind: u32, args: &[u8]) -> u32 { crate::runtime::host_fire(op_kind, args) }

#[cfg(not(target_arch = "wasm32"))]
fn host_fire(_op_kind: u32, _args: &[u8]) -> u32 { 0 }

#[cfg(target_arch = "wasm32")]
fn poll_post_inner(future_id: u64) -> Option<Result<u32, NotifyError>> {
    use crate::runtime::host_io_poll;
    if future_id == 0 { return Some(Err(NotifyError::Denied)); }
    let mut buf = [0u8; 4];
    let packed = host_io_poll(future_id, &mut buf);
    let (st, len) = unpack_poll(packed);
    match st {
        status::PENDING => None,
        status::READY if len == 4 => Some(Ok(u32::from_le_bytes(buf))),
        status::READY => Some(Err(NotifyError::Host("unexpected reply length".into()))),
        status::ERROR => {
            // Best-effort: re-poll for a longer reason buffer and
            // surface as Host(reason). The error categories most
            // apps care about (denied / permission) are surfaced
            // synchronously by the dispatcher already.
            let mut reason = vec![0u8; len.max(1) as usize];
            let _ = host_io_poll(future_id, &mut reason);
            let s = String::from_utf8_lossy(&reason).into_owned();
            if s.contains("permission") {
                Some(Err(NotifyError::PermissionNotGranted))
            } else {
                Some(Err(NotifyError::Host(s)))
            }
        }
        status::BUSY => Some(Err(NotifyError::Busy)),
        _ => Some(Err(NotifyError::Host("unknown status".into()))),
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn poll_post_inner(_future_id: u64) -> Option<Result<u32, NotifyError>> {
    Some(Err(NotifyError::Denied))
}

#[cfg(target_arch = "wasm32")]
fn poll_request_permission_inner(future_id: u64) -> Option<Result<PermissionStatus, NotifyError>> {
    use crate::runtime::host_io_poll;
    if future_id == 0 { return Some(Err(NotifyError::Denied)); }
    let mut buf = [0u8; 1];
    let packed = host_io_poll(future_id, &mut buf);
    let (st, len) = unpack_poll(packed);
    match st {
        status::PENDING => None,
        status::READY if len >= 1 => match PermissionStatus::from_wire(buf[0]) {
            Some(s) => Some(Ok(s)),
            None => Some(Err(NotifyError::Host(format!("bad status byte {}", buf[0])))),
        },
        status::READY => Some(Err(NotifyError::Host("empty permission reply".into()))),
        status::ERROR => Some(Err(NotifyError::Host("permission request failed".into()))),
        status::BUSY => Some(Err(NotifyError::Busy)),
        _ => Some(Err(NotifyError::Host("unknown status".into()))),
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn poll_request_permission_inner(_future_id: u64) -> Option<Result<PermissionStatus, NotifyError>> {
    Some(Err(NotifyError::Denied))
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
    fn permission_status_from_wire() {
        assert_eq!(PermissionStatus::from_wire(0), Some(PermissionStatus::Default));
        assert_eq!(PermissionStatus::from_wire(1), Some(PermissionStatus::Granted));
        assert_eq!(PermissionStatus::from_wire(2), Some(PermissionStatus::Denied));
        assert_eq!(PermissionStatus::from_wire(99), None);
    }

    #[test]
    fn encode_post_args_roundtrips() {
        let bytes = encode_post_args("Hi", "there", "tag-1");
        // [tlen:2][title:2][blen:2][body:5][tlen:2][tag:5]
        assert_eq!(bytes.len(), 2 + 2 + 2 + 5 + 2 + 5);
        assert_eq!(u16::from_le_bytes([bytes[0], bytes[1]]), 2);
        assert_eq!(&bytes[2..4], b"Hi");
        assert_eq!(u16::from_le_bytes([bytes[4], bytes[5]]), 5);
        assert_eq!(&bytes[6..11], b"there");
        assert_eq!(u16::from_le_bytes([bytes[11], bytes[12]]), 5);
        assert_eq!(&bytes[13..18], b"tag-1");
    }

    #[test]
    fn encode_post_args_empty_tag() {
        let bytes = encode_post_args("t", "b", "");
        // [tlen:2][t:1][blen:2][b:1][tag_len:2 = 0]
        assert_eq!(bytes.len(), 2 + 1 + 2 + 1 + 2);
        assert_eq!(u16::from_le_bytes([bytes[6], bytes[7]]), 0);
    }

    #[test]
    fn empty_title_resolves_bad_title() {
        let mut f = post("", "body");
        let waker = noop_waker();
        let mut cx = TaskContext::from_waker(&waker);
        match Pin::new(&mut f).poll(&mut cx) {
            Poll::Ready(Err(NotifyError::BadTitle)) => {}
            other => panic!("expected Ready(Err(BadTitle)), got {other:?}"),
        }
    }

    #[test]
    fn post_on_non_wasm_resolves_denied() {
        let mut f = post("title", "body");
        let waker = noop_waker();
        let mut cx = TaskContext::from_waker(&waker);
        match Pin::new(&mut f).poll(&mut cx) {
            Poll::Ready(Err(NotifyError::Denied)) => {}
            other => panic!("expected Ready(Err(Denied)), got {other:?}"),
        }
    }

    #[test]
    fn permission_status_is_granted_helper() {
        assert!(PermissionStatus::Granted.is_granted());
        assert!(!PermissionStatus::Denied.is_granted());
        assert!(!PermissionStatus::Default.is_granted());
    }

    #[test]
    fn request_permission_on_non_wasm_resolves_denied() {
        let mut f = request_permission();
        let waker = noop_waker();
        let mut cx = TaskContext::from_waker(&waker);
        match Pin::new(&mut f).poll(&mut cx) {
            Poll::Ready(Err(NotifyError::Denied)) => {}
            other => panic!("expected Ready(Err(Denied)), got {other:?}"),
        }
    }
}
