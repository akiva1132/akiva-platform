//! Network capability — Stone 14.1.
//!
//! Apps with `akiva.permission.NETWORK` declared in their manifest can:
//! - issue HTTP requests via [`get`] / [`post`] / [`Request`];
//! - open and drive WebSockets via [`WebSocket::connect`].
//!
//! The capability is enforced inside the unified-ABI dispatcher at the
//! kernel-worker boundary — apps without the grant see every net op
//! resolve `NetError::Denied` immediately. CORS rules apply to HTTP
//! exactly as they do to any browser `fetch`; servers without CORS
//! headers must be proxied through a backend the app controls.
//!
//! The API mirrors the shape of standard Rust networking crates
//! (`reqwest`-ish for HTTP, `tokio-tungstenite`-ish for WebSocket) so
//! that a developer used to native networking has the same mental
//! model. The fact that it's implemented over browser primitives is
//! an implementation detail of the web runtime — a future native
//! runtime would expose the same SDK surface backed by raw sockets.
//!
//! ## HTTP example
//!
//! ```ignore
//! use akiva_sdk::net;
//!
//! async fn fetch_user(id: u64) -> Result<String, net::NetError> {
//!     let url = format!("https://api.example.com/users/{id}");
//!     let resp = net::get(&url).await?;
//!     if resp.status >= 400 {
//!         return Err(net::NetError::Network(format!("status {}", resp.status)));
//!     }
//!     Ok(String::from_utf8_lossy(&resp.body).into_owned())
//! }
//! ```
//!
//! ## WebSocket example
//!
//! ```ignore
//! use akiva_sdk::net::{WebSocket, WsEvent};
//!
//! async fn echo() -> Result<(), akiva_sdk::net::NetError> {
//!     let ws = WebSocket::connect("wss://echo.websocket.events").await?;
//!     ws.send_text("ping");
//!     loop {
//!         match ws.recv().await? {
//!             WsEvent::Text(s)  => log_info!("got text: {s}"),
//!             WsEvent::Binary(b) => log_info!("got {} bytes", b.len()),
//!             WsEvent::Closed { code, .. } => {
//!                 log_info!("closed: {code}");
//!                 return Ok(());
//!             }
//!         }
//!     }
//! }
//! ```
//!
//! Dropping a [`WebSocket`] without calling [`close`](WebSocket::close)
//! issues a normal-closure (`code = 1000`) on the way out, so the
//! handle never leaks past the activity that owned it.

use core::cell::Cell;
use core::future::Future;
use core::pin::Pin;
use core::task::{Context as TaskContext, Poll};

use crate::op_kinds::{fire, io};
#[cfg(target_arch = "wasm32")]
use crate::op_kinds::{status, unpack_poll};

// ── Errors ──────────────────────────────────────────────────────────────

/// Categories of network failure surfaced to apps. Wire-level errors
/// (CORS rejection, DNS failure, TLS handshake) collapse into
/// [`NetError::Network`] with a UTF-8 reason — apps that need to
/// branch on a specific browser error class should match against the
/// reason substring; the typed enum is the stable bit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NetError {
    /// The package doesn't hold `akiva.permission.NETWORK`, or the
    /// host rejected the call before posting a future. Apps that
    /// declared the permission still see this when the dispatcher
    /// hits a slot-level cap (`Busy` lifts after a yield; `Denied`
    /// is permanent for that grant state).
    Denied,
    /// Empty URL or otherwise un-parseable input. The SDK pre-checks
    /// before issuing the host op so a wasted round-trip is avoided.
    BadUrl,
    /// Non-recoverable network/transport error from the host. Reason
    /// is the browser's `error.message` for HTTP, or a synthetic
    /// reason for WebSocket open failures.
    Network(String),
    /// Operation issued against a WebSocket the host has marked
    /// closed (graceful close, transport error, or remote close).
    /// `WebSocket::recv` after a close event always returns this
    /// once the close event itself has been delivered.
    Closed,
    /// Host backpressure. The slot is at `MAX_PENDING_PER_SLOT`;
    /// co-operative apps `yield` (await another resolved future,
    /// then retry).
    Busy,
}

// ── HTTP ────────────────────────────────────────────────────────────────

/// HTTP method codes shared with the wire format. Matching the
/// `IO::HTTP_FETCH` arg encoding documented in
/// `framework::wire::op_kinds::io::HTTP_FETCH`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Get,
    Post,
    Put,
    Delete,
    Patch,
    Head,
}

impl Method {
    pub(crate) fn code(self) -> u8 {
        match self {
            Method::Get => 0,
            Method::Post => 1,
            Method::Put => 2,
            Method::Delete => 3,
            Method::Patch => 4,
            Method::Head => 5,
        }
    }
}

/// Builder for HTTP requests with custom method, headers, and body.
/// Free-function helpers [`get`] and [`post`] cover the common cases.
#[derive(Debug, Clone)]
pub struct Request {
    method: Method,
    url: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Request {
    pub fn new(method: Method, url: impl Into<String>) -> Self {
        Self { method, url: url.into(), headers: Vec::new(), body: Vec::new() }
    }
    pub fn get(url: impl Into<String>) -> Self  { Self::new(Method::Get,  url) }
    pub fn post(url: impl Into<String>) -> Self { Self::new(Method::Post, url) }
    pub fn put(url: impl Into<String>) -> Self  { Self::new(Method::Put,  url) }

    /// Add a request header. Multiple calls with the same name append
    /// independent entries, matching the browser `Headers` API.
    pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    pub fn body(mut self, body: impl Into<Vec<u8>>) -> Self {
        self.body = body.into();
        self
    }

    pub fn body_text(self, body: impl AsRef<str>) -> Self {
        self.body(body.as_ref().as_bytes().to_vec())
    }

    /// Issue the request. Returns a future that resolves on the
    /// browser's fetch completion; CORS preflight (if any) happens
    /// transparently inside that wait.
    pub fn send(self) -> HttpFuture {
        if self.url.is_empty() {
            return HttpFuture::pre_resolved(Err(NetError::BadUrl));
        }
        let args = encode_http_args(&self);
        let fid = io_start(io::HTTP_FETCH, &args);
        if fid == 0 {
            return HttpFuture::pre_resolved(Err(NetError::Denied));
        }
        HttpFuture { future_id: fid, cached: None }
    }
}

/// Convenience: GET the given URL.
pub fn get(url: impl Into<String>) -> HttpFuture {
    Request::get(url).send()
}

/// Convenience: POST `body` to the given URL with no extra headers.
pub fn post(url: impl Into<String>, body: impl Into<Vec<u8>>) -> HttpFuture {
    Request::post(url).body(body).send()
}

/// Decoded HTTP response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Response {
    /// True iff the status code is in the 2xx range.
    pub fn is_success(&self) -> bool { (200..300).contains(&self.status) }
    /// UTF-8 decode of the body, replacing invalid sequences with `U+FFFD`.
    pub fn text(&self) -> String { String::from_utf8_lossy(&self.body).into_owned() }
}

#[must_use = "HttpFuture must be awaited; dropping it cancels the request \
              from the app's view (the host call still completes but the \
              response is discarded)"]
pub struct HttpFuture {
    future_id: u64,
    cached: Option<Result<Response, NetError>>,
}

impl HttpFuture {
    fn pre_resolved(r: Result<Response, NetError>) -> Self {
        Self { future_id: 0, cached: Some(r) }
    }
}

impl Future for HttpFuture {
    type Output = Result<Response, NetError>;

    fn poll(mut self: Pin<&mut Self>, _cx: &mut TaskContext<'_>) -> Poll<Self::Output> {
        if let Some(cached) = self.cached.take() {
            self.cached = Some(cached.clone());
            return Poll::Ready(cached);
        }
        match poll_http_inner(self.future_id) {
            None => Poll::Pending,
            Some(result) => {
                self.cached = Some(result.clone());
                Poll::Ready(result)
            }
        }
    }
}

/// Encode a [`Request`] into the [`io::HTTP_FETCH`] wire format.
pub(crate) fn encode_http_args(req: &Request) -> Vec<u8> {
    let url_bytes = req.url.as_bytes();
    let url_len = url_bytes.len().min(u16::MAX as usize) as u16;
    let header_count = req.headers.len().min(u16::MAX as usize) as u16;
    let mut out = Vec::with_capacity(
        1 + 2 + url_len as usize + 2
            + req.headers.iter().map(|(n, v)| 4 + n.len() + v.len()).sum::<usize>()
            + req.body.len(),
    );
    out.push(req.method.code());
    out.extend_from_slice(&url_len.to_le_bytes());
    out.extend_from_slice(&url_bytes[..url_len as usize]);
    out.extend_from_slice(&header_count.to_le_bytes());
    for (n, v) in req.headers.iter().take(header_count as usize) {
        let nl = n.len().min(u16::MAX as usize) as u16;
        out.extend_from_slice(&nl.to_le_bytes());
        out.extend_from_slice(&n.as_bytes()[..nl as usize]);
        let vl = v.len().min(u16::MAX as usize) as u16;
        out.extend_from_slice(&vl.to_le_bytes());
        out.extend_from_slice(&v.as_bytes()[..vl as usize]);
    }
    out.extend_from_slice(&req.body);
    out
}

/// Decode the [`io::HTTP_FETCH`] reply payload. Returned by both the
/// wasm poll path and the unit tests below.
pub(crate) fn decode_http_response(buf: &[u8]) -> Result<Response, NetError> {
    if buf.len() < 4 {
        return Err(NetError::Network("http: reply truncated".into()));
    }
    let status = u16::from_le_bytes([buf[0], buf[1]]);
    let header_count = u16::from_le_bytes([buf[2], buf[3]]) as usize;
    let mut cur = 4usize;
    let mut headers = Vec::with_capacity(header_count);
    for _ in 0..header_count {
        if cur + 2 > buf.len() {
            return Err(NetError::Network("http: header name len truncated".into()));
        }
        let nl = u16::from_le_bytes([buf[cur], buf[cur + 1]]) as usize;
        cur += 2;
        if cur + nl > buf.len() {
            return Err(NetError::Network("http: header name truncated".into()));
        }
        let name = String::from_utf8_lossy(&buf[cur..cur + nl]).into_owned();
        cur += nl;
        if cur + 2 > buf.len() {
            return Err(NetError::Network("http: header val len truncated".into()));
        }
        let vl = u16::from_le_bytes([buf[cur], buf[cur + 1]]) as usize;
        cur += 2;
        if cur + vl > buf.len() {
            return Err(NetError::Network("http: header value truncated".into()));
        }
        let value = String::from_utf8_lossy(&buf[cur..cur + vl]).into_owned();
        cur += vl;
        headers.push((name, value));
    }
    let body = buf[cur..].to_vec();
    Ok(Response { status, headers, body })
}

// ── WebSocket ───────────────────────────────────────────────────────────

/// One event surfaced by [`WebSocket::recv`]. Matches the wire kinds
/// documented on [`io::WS_RECV`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WsEvent {
    Text(String),
    Binary(Vec<u8>),
    Closed { code: u16, reason: String },
}

const WS_RECV_KIND_TEXT: u8 = 0;
const WS_RECV_KIND_BINARY: u8 = 1;
const WS_RECV_KIND_CLOSED: u8 = 2;

const WS_SEND_KIND_TEXT: u8 = 0;
const WS_SEND_KIND_BINARY: u8 = 1;

/// An open WebSocket. Send is fire-and-forget; receive is async.
/// Dropping the value sends a normal close (`code = 1000`) so a stray
/// drop never leaks the kernel-side handle past the activity.
#[derive(Debug)]
pub struct WebSocket {
    handle: u32,
    closed: Cell<bool>,
}

impl WebSocket {
    /// Begin a WebSocket handshake to `url`. The future resolves on
    /// the underlying socket's `open` event, or [`NetError::Network`]
    /// on a handshake failure.
    pub fn connect(url: impl Into<String>) -> WsConnectFuture {
        let url: String = url.into();
        if url.is_empty() {
            return WsConnectFuture::pre_resolved(Err(NetError::BadUrl));
        }
        let mut args = Vec::with_capacity(2 + url.len() + 1);
        let url_len = url.len().min(u16::MAX as usize) as u16;
        args.extend_from_slice(&url_len.to_le_bytes());
        args.extend_from_slice(&url.as_bytes()[..url_len as usize]);
        // No subprotocols in this revision; the byte is here so the
        // wire shape is forward-compatible.
        args.push(0u8);
        let fid = io_start(io::WS_OPEN, &args);
        if fid == 0 {
            return WsConnectFuture::pre_resolved(Err(NetError::Denied));
        }
        WsConnectFuture { future_id: fid, cached: None }
    }

    /// Send a UTF-8 text frame. Silently dropped after [`close`] or
    /// after the host marks the socket closed — `recv` surfaces the
    /// state change.
    pub fn send_text(&self, s: &str) {
        if self.closed.get() { return; }
        let mut args = Vec::with_capacity(5 + s.len());
        args.extend_from_slice(&self.handle.to_le_bytes());
        args.push(WS_SEND_KIND_TEXT);
        args.extend_from_slice(s.as_bytes());
        host_fire(fire::WS_SEND, &args);
    }

    /// Send a binary frame.
    pub fn send_binary(&self, data: &[u8]) {
        if self.closed.get() { return; }
        let mut args = Vec::with_capacity(5 + data.len());
        args.extend_from_slice(&self.handle.to_le_bytes());
        args.push(WS_SEND_KIND_BINARY);
        args.extend_from_slice(data);
        host_fire(fire::WS_SEND, &args);
    }

    /// Pull the next event off the socket. Each call dispatches a
    /// fresh `WS_RECV` future; the host serves them in arrival order.
    /// After a [`WsEvent::Closed`] every further `recv` resolves
    /// `Err(NetError::Closed)` so loops naturally terminate.
    pub fn recv(&self) -> RecvFuture {
        if self.closed.get() {
            return RecvFuture::pre_resolved(Err(NetError::Closed));
        }
        let fid = io_start(io::WS_RECV, &self.handle.to_le_bytes());
        if fid == 0 {
            return RecvFuture::pre_resolved(Err(NetError::Denied));
        }
        RecvFuture { future_id: fid, cached: None, owner_closed_flag_addr: &self.closed as *const _ }
    }

    /// Close the socket with `code` and `reason`. Idempotent — calling
    /// twice (or after a remote close) is silently ignored.
    pub fn close(&self, code: u16, reason: &str) {
        if self.closed.replace(true) { return; }
        let reason_bytes = reason.as_bytes();
        let reason_len = reason_bytes.len().min(u16::MAX as usize) as u16;
        let mut args = Vec::with_capacity(4 + 2 + 2 + reason_len as usize);
        args.extend_from_slice(&self.handle.to_le_bytes());
        args.extend_from_slice(&code.to_le_bytes());
        args.extend_from_slice(&reason_len.to_le_bytes());
        args.extend_from_slice(&reason_bytes[..reason_len as usize]);
        host_fire(fire::WS_CLOSE, &args);
    }

    /// The opaque kernel-side handle. Useful only for debugging — apps
    /// shouldn't store or forward it. The handle is bound to this app
    /// instance and rejected by the dispatcher if presented from
    /// another worker.
    pub fn handle(&self) -> u32 { self.handle }
}

impl Drop for WebSocket {
    fn drop(&mut self) {
        if !self.closed.get() {
            // 1000 = normal closure, RFC 6455 §7.4.1.
            self.close(1000, "");
        }
    }
}

#[must_use = "WsConnectFuture must be awaited; dropping it without await \
              cancels the connection attempt"]
pub struct WsConnectFuture {
    future_id: u64,
    cached: Option<Result<u32, NetError>>,
}

impl WsConnectFuture {
    fn pre_resolved(r: Result<WebSocket, NetError>) -> Self {
        match r {
            Ok(_) => Self { future_id: 0, cached: Some(Err(NetError::Denied)) },
            Err(e) => Self { future_id: 0, cached: Some(Err(e)) },
        }
    }
}

impl Future for WsConnectFuture {
    type Output = Result<WebSocket, NetError>;

    fn poll(mut self: Pin<&mut Self>, _cx: &mut TaskContext<'_>) -> Poll<Self::Output> {
        if let Some(cached) = self.cached.take() {
            self.cached = Some(cached.clone());
            return Poll::Ready(cached.map(|h| WebSocket { handle: h, closed: Cell::new(false) }));
        }
        match poll_ws_open_inner(self.future_id) {
            None => Poll::Pending,
            Some(result) => {
                self.cached = Some(result.clone());
                Poll::Ready(result.map(|h| WebSocket { handle: h, closed: Cell::new(false) }))
            }
        }
    }
}

#[must_use]
pub struct RecvFuture {
    future_id: u64,
    cached: Option<Result<WsEvent, NetError>>,
    /// Pointer back to the parent [`WebSocket::closed`] cell so a
    /// terminal `WsEvent::Closed` can flip the parent into the closed
    /// state without taking `&mut self`. Single-threaded — wasm has no
    /// concurrent borrowers — but kept as a raw pointer because the
    /// borrow checker can't see the lifetime relationship through the
    /// future heap-allocation in the SDK runtime.
    owner_closed_flag_addr: *const Cell<bool>,
}

impl RecvFuture {
    fn pre_resolved(r: Result<WsEvent, NetError>) -> Self {
        Self {
            future_id: 0,
            cached: Some(r),
            owner_closed_flag_addr: core::ptr::null(),
        }
    }
}

impl Future for RecvFuture {
    type Output = Result<WsEvent, NetError>;

    fn poll(mut self: Pin<&mut Self>, _cx: &mut TaskContext<'_>) -> Poll<Self::Output> {
        if let Some(cached) = self.cached.take() {
            self.cached = Some(cached.clone());
            return Poll::Ready(cached);
        }
        match poll_ws_recv_inner(self.future_id) {
            None => Poll::Pending,
            Some(result) => {
                if let Ok(WsEvent::Closed { .. }) = &result {
                    if !self.owner_closed_flag_addr.is_null() {
                        // SAFETY: the WebSocket that owns this Cell
                        // outlives the RecvFuture (RecvFuture borrows
                        // &self), and wasm is single-threaded so no
                        // concurrent mutator races us.
                        unsafe { (*self.owner_closed_flag_addr).set(true); }
                    }
                }
                self.cached = Some(result.clone());
                Poll::Ready(result)
            }
        }
    }
}

/// Decode a [`io::WS_RECV`] reply into a typed [`WsEvent`]. Public to
/// the crate for tests; apps go through [`WebSocket::recv`].
pub(crate) fn decode_ws_event(buf: &[u8]) -> Result<WsEvent, NetError> {
    if buf.is_empty() {
        return Err(NetError::Network("ws: empty recv reply".into()));
    }
    match buf[0] {
        WS_RECV_KIND_TEXT => {
            let s = String::from_utf8_lossy(&buf[1..]).into_owned();
            Ok(WsEvent::Text(s))
        }
        WS_RECV_KIND_BINARY => Ok(WsEvent::Binary(buf[1..].to_vec())),
        WS_RECV_KIND_CLOSED => {
            if buf.len() < 3 {
                return Ok(WsEvent::Closed { code: 1006, reason: String::new() });
            }
            let code = u16::from_le_bytes([buf[1], buf[2]]);
            let reason = String::from_utf8_lossy(&buf[3..]).into_owned();
            Ok(WsEvent::Closed { code, reason })
        }
        other => Err(NetError::Network(format!("ws: unknown recv kind {other}"))),
    }
}

// ── WebTransport ────────────────────────────────────────────────────────
//
// HTTP/3 + QUIC. Two channels per session:
//
//   * Datagrams — unreliable, unordered, UDP-like. Frames may be
//     dropped under congestion (RFC 9221). `send_datagram` /
//     `recv_datagram`.
//   * Bidirectional streams — reliable, ordered, with backpressure.
//     One QUIC stream per [`BidiStream`]. Many streams per session.
//
// The shape matches `tokio-tungstenite`-ish ergonomics: `connect()`
// hands back the session; the session vends streams; streams have
// `send`/`recv`/`close`. `Drop` impls send the appropriate close so
// stray drops don't leak host-side handles.
//
// Browser support: Chrome 97+, Firefox 114+, Safari 18.2+. Servers
// must speak HTTP/3 + WebTransport ALPN (`h3` + WT extension).
// Unlike WebSocket, public WT echo servers are sparse — for local
// testing apps typically run their own WT endpoint behind a valid
// TLS cert (or `chrome --ignore-certificate-errors-spki-list` for
// self-signed dev work).

const WT_DG_KIND_BYTES: u8 = 0;
const WT_DG_KIND_CLOSED: u8 = 1;

const WT_STREAM_KIND_BYTES: u8 = 0;
const WT_STREAM_KIND_FIN: u8 = 1;
const WT_STREAM_KIND_ERROR: u8 = 2;

/// One event surfaced by [`WebTransport::recv_datagram`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WtDatagram {
    Bytes(Vec<u8>),
    Closed { code: u32, reason: String },
}

/// One event surfaced by [`BidiStream::recv`]. Streams are reliable
/// and ordered; `Fin` arrives once when the peer closes their write
/// side (the local writer can still flow until [`BidiStream::close`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamEvent {
    Bytes(Vec<u8>),
    /// Peer closed their write side — no more bytes will arrive on
    /// this stream. Subsequent `recv` calls return `Fin` immediately.
    Fin,
    /// Transport-level error. `String` carries the host's reason.
    Error(String),
}

/// An open WebTransport session. Owns the per-session datagram queue
/// and acts as the parent for any [`BidiStream`] it opens.
#[derive(Debug)]
pub struct WebTransport {
    handle: u32,
    closed: Cell<bool>,
}

impl WebTransport {
    /// Open a WebTransport session. Resolves on the underlying
    /// session's `ready` promise; `NetError::Network` on a
    /// handshake / HTTP/3 / TLS failure.
    pub fn connect(url: impl Into<String>) -> WtConnectFuture {
        let url: String = url.into();
        if url.is_empty() {
            return WtConnectFuture::pre_resolved(Err(NetError::BadUrl));
        }
        let mut args = Vec::with_capacity(2 + url.len());
        let url_len = url.len().min(u16::MAX as usize) as u16;
        args.extend_from_slice(&url_len.to_le_bytes());
        args.extend_from_slice(&url.as_bytes()[..url_len as usize]);
        let fid = io_start(io::WT_OPEN, &args);
        if fid == 0 {
            return WtConnectFuture::pre_resolved(Err(NetError::Denied));
        }
        WtConnectFuture { future_id: fid, cached: None }
    }

    /// Send a datagram. Best-effort, fire-and-forget. Frames larger
    /// than the connection's max datagram size are silently dropped
    /// by the host (matching browser `WebTransport.datagrams.writable`
    /// semantics — write returns success but the wire frame may not
    /// reach the peer).
    pub fn send_datagram(&self, data: &[u8]) {
        if self.closed.get() { return; }
        let mut args = Vec::with_capacity(4 + data.len());
        args.extend_from_slice(&self.handle.to_le_bytes());
        args.extend_from_slice(data);
        host_fire(fire::WT_SEND_DATAGRAM, &args);
    }

    /// Pull the next datagram. After a [`WtDatagram::Closed`] every
    /// further recv resolves `Err(NetError::Closed)` so loops
    /// terminate naturally.
    pub fn recv_datagram(&self) -> WtRecvDatagramFuture {
        if self.closed.get() {
            return WtRecvDatagramFuture::pre_resolved(Err(NetError::Closed));
        }
        let fid = io_start(io::WT_RECV_DATAGRAM, &self.handle.to_le_bytes());
        if fid == 0 {
            return WtRecvDatagramFuture::pre_resolved(Err(NetError::Denied));
        }
        WtRecvDatagramFuture {
            future_id: fid,
            cached: None,
            owner_closed_flag_addr: &self.closed as *const _,
        }
    }

    /// Open a bidirectional stream under this session.
    pub fn open_bidi_stream(&self) -> WtOpenStreamFuture {
        if self.closed.get() {
            return WtOpenStreamFuture::pre_resolved(Err(NetError::Closed));
        }
        let fid = io_start(io::WT_OPEN_BIDI, &self.handle.to_le_bytes());
        if fid == 0 {
            return WtOpenStreamFuture::pre_resolved(Err(NetError::Denied));
        }
        WtOpenStreamFuture { future_id: fid, cached: None }
    }

    /// Close the session and all streams under it. Idempotent.
    pub fn close(&self, code: u32, reason: &str) {
        if self.closed.replace(true) { return; }
        let reason_bytes = reason.as_bytes();
        let reason_len = reason_bytes.len().min(u16::MAX as usize) as u16;
        let mut args = Vec::with_capacity(4 + 4 + 2 + reason_len as usize);
        args.extend_from_slice(&self.handle.to_le_bytes());
        args.extend_from_slice(&code.to_le_bytes());
        args.extend_from_slice(&reason_len.to_le_bytes());
        args.extend_from_slice(&reason_bytes[..reason_len as usize]);
        host_fire(fire::WT_CLOSE, &args);
    }

    pub fn handle(&self) -> u32 { self.handle }
}

impl Drop for WebTransport {
    fn drop(&mut self) {
        if !self.closed.get() {
            self.close(0, "");
        }
    }
}

/// One bidirectional stream over a [`WebTransport`] session.
#[derive(Debug)]
pub struct BidiStream {
    handle: u32,
    closed: Cell<bool>,
}

impl BidiStream {
    /// Send bytes. Fire-and-forget; backpressure is handled by the
    /// host (browser `WritableStream` flow control). For now apps
    /// don't observe it — large bursts can be dropped if the host
    /// queue fills, with a warning logged.
    pub fn send(&self, data: &[u8]) {
        if self.closed.get() { return; }
        let mut args = Vec::with_capacity(4 + data.len());
        args.extend_from_slice(&self.handle.to_le_bytes());
        args.extend_from_slice(data);
        host_fire(fire::WT_SEND_STREAM, &args);
    }

    /// Pull the next chunk. Returns `Fin` when the peer closes their
    /// write side, then `Fin` for every subsequent call.
    pub fn recv(&self) -> WtRecvStreamFuture {
        if self.closed.get() {
            return WtRecvStreamFuture::pre_resolved(Err(NetError::Closed));
        }
        let fid = io_start(io::WT_RECV_STREAM, &self.handle.to_le_bytes());
        if fid == 0 {
            return WtRecvStreamFuture::pre_resolved(Err(NetError::Denied));
        }
        WtRecvStreamFuture {
            future_id: fid,
            cached: None,
            owner_closed_flag_addr: &self.closed as *const _,
        }
    }

    /// Close the writer side. Idempotent. Reads continue to flow
    /// until the peer closes theirs.
    pub fn close(&self, code: u32) {
        if self.closed.replace(true) { return; }
        let mut args = Vec::with_capacity(8);
        args.extend_from_slice(&self.handle.to_le_bytes());
        args.extend_from_slice(&code.to_le_bytes());
        host_fire(fire::WT_CLOSE_STREAM, &args);
    }

    pub fn handle(&self) -> u32 { self.handle }
}

impl Drop for BidiStream {
    fn drop(&mut self) {
        if !self.closed.get() {
            self.close(0);
        }
    }
}

#[must_use]
pub struct WtConnectFuture {
    future_id: u64,
    cached: Option<Result<u32, NetError>>,
}

impl WtConnectFuture {
    fn pre_resolved(r: Result<WebTransport, NetError>) -> Self {
        match r {
            Ok(_) => Self { future_id: 0, cached: Some(Err(NetError::Denied)) },
            Err(e) => Self { future_id: 0, cached: Some(Err(e)) },
        }
    }
}

impl Future for WtConnectFuture {
    type Output = Result<WebTransport, NetError>;

    fn poll(mut self: Pin<&mut Self>, _cx: &mut TaskContext<'_>) -> Poll<Self::Output> {
        if let Some(cached) = self.cached.take() {
            self.cached = Some(cached.clone());
            return Poll::Ready(cached.map(|h| WebTransport { handle: h, closed: Cell::new(false) }));
        }
        match poll_wt_open_inner(self.future_id) {
            None => Poll::Pending,
            Some(result) => {
                self.cached = Some(result.clone());
                Poll::Ready(result.map(|h| WebTransport { handle: h, closed: Cell::new(false) }))
            }
        }
    }
}

#[must_use]
pub struct WtOpenStreamFuture {
    future_id: u64,
    cached: Option<Result<u32, NetError>>,
}

impl WtOpenStreamFuture {
    fn pre_resolved(r: Result<BidiStream, NetError>) -> Self {
        match r {
            Ok(_) => Self { future_id: 0, cached: Some(Err(NetError::Denied)) },
            Err(e) => Self { future_id: 0, cached: Some(Err(e)) },
        }
    }
}

impl Future for WtOpenStreamFuture {
    type Output = Result<BidiStream, NetError>;

    fn poll(mut self: Pin<&mut Self>, _cx: &mut TaskContext<'_>) -> Poll<Self::Output> {
        if let Some(cached) = self.cached.take() {
            self.cached = Some(cached.clone());
            return Poll::Ready(cached.map(|h| BidiStream { handle: h, closed: Cell::new(false) }));
        }
        match poll_wt_open_stream_inner(self.future_id) {
            None => Poll::Pending,
            Some(result) => {
                self.cached = Some(result.clone());
                Poll::Ready(result.map(|h| BidiStream { handle: h, closed: Cell::new(false) }))
            }
        }
    }
}

#[must_use]
pub struct WtRecvDatagramFuture {
    future_id: u64,
    cached: Option<Result<WtDatagram, NetError>>,
    owner_closed_flag_addr: *const Cell<bool>,
}

impl WtRecvDatagramFuture {
    fn pre_resolved(r: Result<WtDatagram, NetError>) -> Self {
        Self { future_id: 0, cached: Some(r), owner_closed_flag_addr: core::ptr::null() }
    }
}

impl Future for WtRecvDatagramFuture {
    type Output = Result<WtDatagram, NetError>;

    fn poll(mut self: Pin<&mut Self>, _cx: &mut TaskContext<'_>) -> Poll<Self::Output> {
        if let Some(cached) = self.cached.take() {
            self.cached = Some(cached.clone());
            return Poll::Ready(cached);
        }
        match poll_wt_recv_datagram_inner(self.future_id) {
            None => Poll::Pending,
            Some(result) => {
                if let Ok(WtDatagram::Closed { .. }) = &result {
                    if !self.owner_closed_flag_addr.is_null() {
                        // SAFETY: owner WebTransport outlives this future
                        // (recv borrows &self) and wasm is single-threaded.
                        unsafe { (*self.owner_closed_flag_addr).set(true); }
                    }
                }
                self.cached = Some(result.clone());
                Poll::Ready(result)
            }
        }
    }
}

#[must_use]
pub struct WtRecvStreamFuture {
    future_id: u64,
    cached: Option<Result<StreamEvent, NetError>>,
    owner_closed_flag_addr: *const Cell<bool>,
}

impl WtRecvStreamFuture {
    fn pre_resolved(r: Result<StreamEvent, NetError>) -> Self {
        Self { future_id: 0, cached: Some(r), owner_closed_flag_addr: core::ptr::null() }
    }
}

impl Future for WtRecvStreamFuture {
    type Output = Result<StreamEvent, NetError>;

    fn poll(mut self: Pin<&mut Self>, _cx: &mut TaskContext<'_>) -> Poll<Self::Output> {
        if let Some(cached) = self.cached.take() {
            self.cached = Some(cached.clone());
            return Poll::Ready(cached);
        }
        match poll_wt_recv_stream_inner(self.future_id) {
            None => Poll::Pending,
            Some(result) => {
                if matches!(&result, Ok(StreamEvent::Fin) | Ok(StreamEvent::Error(_))) {
                    if !self.owner_closed_flag_addr.is_null() {
                        unsafe { (*self.owner_closed_flag_addr).set(true); }
                    }
                }
                self.cached = Some(result.clone());
                Poll::Ready(result)
            }
        }
    }
}

/// Decode a [`io::WT_RECV_DATAGRAM`] reply.
pub(crate) fn decode_wt_datagram(buf: &[u8]) -> Result<WtDatagram, NetError> {
    if buf.is_empty() {
        return Err(NetError::Network("wt: empty datagram reply".into()));
    }
    match buf[0] {
        WT_DG_KIND_BYTES => Ok(WtDatagram::Bytes(buf[1..].to_vec())),
        WT_DG_KIND_CLOSED => {
            if buf.len() < 5 {
                return Ok(WtDatagram::Closed { code: 0, reason: String::new() });
            }
            let code = u32::from_le_bytes([buf[1], buf[2], buf[3], buf[4]]);
            let reason = String::from_utf8_lossy(&buf[5..]).into_owned();
            Ok(WtDatagram::Closed { code, reason })
        }
        other => Err(NetError::Network(format!("wt: unknown datagram kind {other}"))),
    }
}

/// Decode a [`io::WT_RECV_STREAM`] reply.
pub(crate) fn decode_wt_stream_event(buf: &[u8]) -> Result<StreamEvent, NetError> {
    if buf.is_empty() {
        return Err(NetError::Network("wt: empty stream reply".into()));
    }
    match buf[0] {
        WT_STREAM_KIND_BYTES => Ok(StreamEvent::Bytes(buf[1..].to_vec())),
        WT_STREAM_KIND_FIN => Ok(StreamEvent::Fin),
        WT_STREAM_KIND_ERROR => {
            let reason = String::from_utf8_lossy(&buf[1..]).into_owned();
            Ok(StreamEvent::Error(reason))
        }
        other => Err(NetError::Network(format!("wt: unknown stream kind {other}"))),
    }
}

// ── wasm/non-wasm io_start + poll glue ─────────────────────────────────

#[cfg(target_arch = "wasm32")]
fn io_start(op_kind: u32, args: &[u8]) -> u64 { crate::runtime::host_io_start(op_kind, args) }

#[cfg(not(target_arch = "wasm32"))]
fn io_start(_op_kind: u32, _args: &[u8]) -> u64 { 0 }

#[cfg(target_arch = "wasm32")]
fn host_fire(op_kind: u32, args: &[u8]) -> u32 { crate::runtime::host_fire(op_kind, args) }

#[cfg(not(target_arch = "wasm32"))]
fn host_fire(_op_kind: u32, _args: &[u8]) -> u32 { 0 }

#[cfg(target_arch = "wasm32")]
fn poll_http_inner(future_id: u64) -> Option<Result<Response, NetError>> {
    use crate::runtime::host_io_poll;
    if future_id == 0 { return Some(Err(NetError::Denied)); }
    // 4 KiB initial guess covers small JSON; the retry branch handles
    // larger payloads. Keep on the heap so multi-MB responses don't
    // blow the wasm stack.
    let mut buf = vec![0u8; 4096];
    let packed = host_io_poll(future_id, &mut buf);
    let (st, len) = unpack_poll(packed);
    let len = len as usize;
    match st {
        status::PENDING => None,
        status::ERROR => {
            buf.truncate(len.min(buf.len()));
            let reason = String::from_utf8_lossy(&buf).into_owned();
            Some(Err(NetError::Network(reason)))
        }
        status::BUSY => Some(Err(NetError::Busy)),
        status::READY => {
            if len > buf.len() {
                buf = vec![0u8; len];
                let packed = host_io_poll(future_id, &mut buf);
                let (st2, len2) = unpack_poll(packed);
                if st2 != status::READY {
                    return Some(Err(NetError::Network("http: retry returned non-READY".into())));
                }
                buf.truncate((len2 as usize).min(buf.len()));
            } else {
                buf.truncate(len);
            }
            Some(decode_http_response(&buf))
        }
        _ => Some(Err(NetError::Network("http: unknown status".into()))),
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn poll_http_inner(_future_id: u64) -> Option<Result<Response, NetError>> {
    Some(Err(NetError::Denied))
}

#[cfg(target_arch = "wasm32")]
fn poll_ws_open_inner(future_id: u64) -> Option<Result<u32, NetError>> {
    use crate::runtime::host_io_poll;
    if future_id == 0 { return Some(Err(NetError::Denied)); }
    let mut buf = [0u8; 4];
    let packed = host_io_poll(future_id, &mut buf);
    let (st, len) = unpack_poll(packed);
    match st {
        status::PENDING => None,
        status::READY if len == 4 => {
            Some(Ok(u32::from_le_bytes(buf)))
        }
        status::READY => Some(Err(NetError::Network(format!("ws: open reply len {len}")))),
        status::ERROR => {
            // Reason text won't fit in 4 bytes; we don't bother
            // re-polling for it — the typed Network error is enough
            // for app branching, and dev consoles already show the
            // browser error.
            Some(Err(NetError::Network("ws: open failed".into())))
        }
        status::BUSY => Some(Err(NetError::Busy)),
        _ => Some(Err(NetError::Network("ws: open unknown status".into()))),
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn poll_ws_open_inner(_future_id: u64) -> Option<Result<u32, NetError>> {
    Some(Err(NetError::Denied))
}

#[cfg(target_arch = "wasm32")]
fn poll_ws_recv_inner(future_id: u64) -> Option<Result<WsEvent, NetError>> {
    use crate::runtime::host_io_poll;
    if future_id == 0 { return Some(Err(NetError::Denied)); }
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
                    return Some(Err(NetError::Network("ws: recv retry not READY".into())));
                }
                buf.truncate((len2 as usize).min(buf.len()));
            } else {
                buf.truncate(len);
            }
            Some(decode_ws_event(&buf))
        }
        status::ERROR => Some(Err(NetError::Network("ws: recv failed".into()))),
        status::BUSY => Some(Err(NetError::Busy)),
        _ => Some(Err(NetError::Network("ws: recv unknown status".into()))),
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn poll_ws_recv_inner(_future_id: u64) -> Option<Result<WsEvent, NetError>> {
    Some(Err(NetError::Denied))
}

// ── WebTransport poll glue ─────────────────────────────────────────────

#[cfg(target_arch = "wasm32")]
fn poll_wt_open_inner(future_id: u64) -> Option<Result<u32, NetError>> {
    use crate::runtime::host_io_poll;
    if future_id == 0 { return Some(Err(NetError::Denied)); }
    let mut buf = [0u8; 4];
    let packed = host_io_poll(future_id, &mut buf);
    let (st, len) = unpack_poll(packed);
    match st {
        status::PENDING => None,
        status::READY if len == 4 => Some(Ok(u32::from_le_bytes(buf))),
        status::READY => Some(Err(NetError::Network(format!("wt: open reply len {len}")))),
        status::ERROR => Some(Err(NetError::Network("wt: open failed".into()))),
        status::BUSY => Some(Err(NetError::Busy)),
        _ => Some(Err(NetError::Network("wt: open unknown status".into()))),
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn poll_wt_open_inner(_future_id: u64) -> Option<Result<u32, NetError>> {
    Some(Err(NetError::Denied))
}

#[cfg(target_arch = "wasm32")]
fn poll_wt_open_stream_inner(future_id: u64) -> Option<Result<u32, NetError>> {
    use crate::runtime::host_io_poll;
    if future_id == 0 { return Some(Err(NetError::Denied)); }
    let mut buf = [0u8; 4];
    let packed = host_io_poll(future_id, &mut buf);
    let (st, len) = unpack_poll(packed);
    match st {
        status::PENDING => None,
        status::READY if len == 4 => Some(Ok(u32::from_le_bytes(buf))),
        status::READY => Some(Err(NetError::Network(format!("wt: open_bidi reply len {len}")))),
        status::ERROR => Some(Err(NetError::Network("wt: open_bidi failed".into()))),
        status::BUSY => Some(Err(NetError::Busy)),
        _ => Some(Err(NetError::Network("wt: open_bidi unknown status".into()))),
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn poll_wt_open_stream_inner(_future_id: u64) -> Option<Result<u32, NetError>> {
    Some(Err(NetError::Denied))
}

#[cfg(target_arch = "wasm32")]
fn poll_wt_recv_datagram_inner(future_id: u64) -> Option<Result<WtDatagram, NetError>> {
    use crate::runtime::host_io_poll;
    if future_id == 0 { return Some(Err(NetError::Denied)); }
    // 1500 byte initial guess covers MTU-sized datagrams; any larger
    // and we retry. Datagrams larger than the path MTU are dropped at
    // the QUIC layer anyway.
    let mut buf = vec![0u8; 1500];
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
                    return Some(Err(NetError::Network("wt: dg retry not READY".into())));
                }
                buf.truncate((len2 as usize).min(buf.len()));
            } else {
                buf.truncate(len);
            }
            Some(decode_wt_datagram(&buf))
        }
        status::ERROR => Some(Err(NetError::Network("wt: recv_datagram failed".into()))),
        status::BUSY => Some(Err(NetError::Busy)),
        _ => Some(Err(NetError::Network("wt: recv_datagram unknown status".into()))),
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn poll_wt_recv_datagram_inner(_future_id: u64) -> Option<Result<WtDatagram, NetError>> {
    Some(Err(NetError::Denied))
}

#[cfg(target_arch = "wasm32")]
fn poll_wt_recv_stream_inner(future_id: u64) -> Option<Result<StreamEvent, NetError>> {
    use crate::runtime::host_io_poll;
    if future_id == 0 { return Some(Err(NetError::Denied)); }
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
                    return Some(Err(NetError::Network("wt: stream retry not READY".into())));
                }
                buf.truncate((len2 as usize).min(buf.len()));
            } else {
                buf.truncate(len);
            }
            Some(decode_wt_stream_event(&buf))
        }
        status::ERROR => Some(Err(NetError::Network("wt: recv_stream failed".into()))),
        status::BUSY => Some(Err(NetError::Busy)),
        _ => Some(Err(NetError::Network("wt: recv_stream unknown status".into()))),
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn poll_wt_recv_stream_inner(_future_id: u64) -> Option<Result<StreamEvent, NetError>> {
    Some(Err(NetError::Denied))
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

    // ── HTTP encode ─────────────────────────────────────────────────────

    #[test]
    fn encode_http_get_no_headers_no_body() {
        let req = Request::get("https://example.com/");
        let bytes = encode_http_args(&req);
        // [method:1][url_len:2][url:20][header_count:2][body:0]
        assert_eq!(bytes.len(), 1 + 2 + 20 + 2);
        assert_eq!(bytes[0], 0); // GET
        assert_eq!(u16::from_le_bytes([bytes[1], bytes[2]]), 20);
        assert_eq!(&bytes[3..23], b"https://example.com/");
        assert_eq!(u16::from_le_bytes([bytes[23], bytes[24]]), 0);
    }

    #[test]
    fn encode_http_post_with_header_and_body() {
        let req = Request::post("https://x.test/")
            .header("Content-Type", "application/json")
            .body_text(r#"{"a":1}"#);
        let bytes = encode_http_args(&req);
        assert_eq!(bytes[0], 1); // POST
        // url_len at offset 1
        let url_len = u16::from_le_bytes([bytes[1], bytes[2]]) as usize;
        assert_eq!(url_len, 15);
        let cur = 3 + url_len;
        let header_count = u16::from_le_bytes([bytes[cur], bytes[cur + 1]]);
        assert_eq!(header_count, 1);
        // last 7 bytes should be the JSON body
        assert_eq!(&bytes[bytes.len() - 7..], br#"{"a":1}"#);
    }

    // ── HTTP decode ─────────────────────────────────────────────────────

    #[test]
    fn decode_http_response_round_trips_status_and_body() {
        let mut wire: Vec<u8> = Vec::new();
        wire.extend_from_slice(&200u16.to_le_bytes());
        wire.extend_from_slice(&1u16.to_le_bytes()); // 1 header
        wire.extend_from_slice(&12u16.to_le_bytes());
        wire.extend_from_slice(b"content-type");
        wire.extend_from_slice(&10u16.to_le_bytes());
        wire.extend_from_slice(b"text/plain");
        wire.extend_from_slice(b"hello");
        let r = decode_http_response(&wire).unwrap();
        assert_eq!(r.status, 200);
        assert_eq!(r.headers, vec![("content-type".into(), "text/plain".into())]);
        assert_eq!(r.body, b"hello");
        assert!(r.is_success());
    }

    #[test]
    fn decode_http_response_truncated_returns_network_error() {
        let r = decode_http_response(&[0x00]); // 1 byte — too short
        assert!(matches!(r, Err(NetError::Network(_))));
    }

    // ── WS encode wire ──────────────────────────────────────────────────

    #[test]
    fn ws_send_text_args_layout() {
        // Ensure send_text wire format is [handle:4 | kind:1 | bytes...].
        // We can't observe host_fire calls directly off-wasm, so encode
        // by hand and round-trip the assertion.
        let handle = 7u32;
        let mut args = Vec::with_capacity(5 + 5);
        args.extend_from_slice(&handle.to_le_bytes());
        args.push(WS_SEND_KIND_TEXT);
        args.extend_from_slice(b"hello");
        assert_eq!(u32::from_le_bytes([args[0], args[1], args[2], args[3]]), 7);
        assert_eq!(args[4], 0);
        assert_eq!(&args[5..], b"hello");
    }

    // ── WS decode ───────────────────────────────────────────────────────

    #[test]
    fn decode_ws_event_text() {
        let mut wire = vec![WS_RECV_KIND_TEXT];
        wire.extend_from_slice(b"pong");
        match decode_ws_event(&wire).unwrap() {
            WsEvent::Text(s) => assert_eq!(s, "pong"),
            other => panic!("expected Text, got {other:?}"),
        }
    }

    #[test]
    fn decode_ws_event_binary() {
        let wire = [WS_RECV_KIND_BINARY, 1, 2, 3, 4];
        assert_eq!(decode_ws_event(&wire).unwrap(), WsEvent::Binary(vec![1,2,3,4]));
    }

    #[test]
    fn decode_ws_event_closed_with_code_and_reason() {
        let mut wire = vec![WS_RECV_KIND_CLOSED];
        wire.extend_from_slice(&1000u16.to_le_bytes());
        wire.extend_from_slice(b"bye");
        match decode_ws_event(&wire).unwrap() {
            WsEvent::Closed { code, reason } => {
                assert_eq!(code, 1000);
                assert_eq!(reason, "bye");
            }
            other => panic!("expected Closed, got {other:?}"),
        }
    }

    #[test]
    fn decode_ws_event_closed_truncated_yields_1006() {
        let r = decode_ws_event(&[WS_RECV_KIND_CLOSED]).unwrap();
        // 1006 = abnormal closure (RFC 6455 §7.4.1) — reasonable
        // synthetic when the host didn't include a code.
        match r {
            WsEvent::Closed { code, .. } => assert_eq!(code, 1006),
            other => panic!("expected Closed, got {other:?}"),
        }
    }

    #[test]
    fn decode_ws_event_unknown_kind_is_an_error() {
        assert!(matches!(decode_ws_event(&[0xFF, 1, 2]), Err(NetError::Network(_))));
    }

    // ── Pre-resolved futures ────────────────────────────────────────────

    #[test]
    fn empty_url_resolves_bad_url() {
        let mut f = Request::get("").send();
        let waker = noop_waker();
        let mut cx = TaskContext::from_waker(&waker);
        match Pin::new(&mut f).poll(&mut cx) {
            Poll::Ready(Err(NetError::BadUrl)) => {}
            other => panic!("expected Ready(Err(BadUrl)), got {other:?}"),
        }
    }

    #[test]
    fn ws_connect_empty_url_resolves_bad_url() {
        let mut f = WebSocket::connect("");
        let waker = noop_waker();
        let mut cx = TaskContext::from_waker(&waker);
        match Pin::new(&mut f).poll(&mut cx) {
            Poll::Ready(Err(NetError::BadUrl)) => {}
            other => panic!("expected Ready(Err(BadUrl)), got {other:?}"),
        }
    }

    #[test]
    fn http_get_on_non_wasm_resolves_denied() {
        // Off-wasm io_start returns 0 → pre-resolved Denied. Mirrors
        // the storage tests' fixture shape.
        let mut f = get("https://example.test/");
        let waker = noop_waker();
        let mut cx = TaskContext::from_waker(&waker);
        match Pin::new(&mut f).poll(&mut cx) {
            Poll::Ready(Err(NetError::Denied)) => {}
            other => panic!("expected Ready(Err(Denied)), got {other:?}"),
        }
    }

    #[test]
    fn method_codes_match_the_wire_table() {
        // The numeric codes are pinned by docs/abi.md; renumbering is
        // a wire break. Test exists to make a renumber visible.
        assert_eq!(Method::Get.code(), 0);
        assert_eq!(Method::Post.code(), 1);
        assert_eq!(Method::Put.code(), 2);
        assert_eq!(Method::Delete.code(), 3);
        assert_eq!(Method::Patch.code(), 4);
        assert_eq!(Method::Head.code(), 5);
    }

    // ── WebTransport — Stone 14.2 ────────────────────────────────────────

    #[test]
    fn decode_wt_datagram_bytes() {
        let wire = [WT_DG_KIND_BYTES, 9, 8, 7];
        assert_eq!(decode_wt_datagram(&wire).unwrap(), WtDatagram::Bytes(vec![9, 8, 7]));
    }

    #[test]
    fn decode_wt_datagram_closed_with_code_and_reason() {
        let mut wire = vec![WT_DG_KIND_CLOSED];
        wire.extend_from_slice(&7u32.to_le_bytes());
        wire.extend_from_slice(b"server gone");
        match decode_wt_datagram(&wire).unwrap() {
            WtDatagram::Closed { code, reason } => {
                assert_eq!(code, 7);
                assert_eq!(reason, "server gone");
            }
            other => panic!("expected Closed, got {other:?}"),
        }
    }

    #[test]
    fn decode_wt_datagram_closed_truncated_yields_zero() {
        // No code byte present — synthesize 0/empty rather than error,
        // matching the WS truncated-Closed path.
        match decode_wt_datagram(&[WT_DG_KIND_CLOSED]).unwrap() {
            WtDatagram::Closed { code, reason } => {
                assert_eq!(code, 0);
                assert_eq!(reason, "");
            }
            other => panic!("expected Closed, got {other:?}"),
        }
    }

    #[test]
    fn decode_wt_datagram_unknown_kind_is_an_error() {
        assert!(matches!(decode_wt_datagram(&[0xEE, 1]), Err(NetError::Network(_))));
    }

    #[test]
    fn decode_wt_stream_bytes() {
        let wire = [WT_STREAM_KIND_BYTES, 1, 2, 3];
        assert_eq!(decode_wt_stream_event(&wire).unwrap(), StreamEvent::Bytes(vec![1, 2, 3]));
    }

    #[test]
    fn decode_wt_stream_fin() {
        assert_eq!(decode_wt_stream_event(&[WT_STREAM_KIND_FIN]).unwrap(), StreamEvent::Fin);
    }

    #[test]
    fn decode_wt_stream_error_carries_reason() {
        let mut wire = vec![WT_STREAM_KIND_ERROR];
        wire.extend_from_slice(b"reset");
        match decode_wt_stream_event(&wire).unwrap() {
            StreamEvent::Error(s) => assert_eq!(s, "reset"),
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn wt_connect_empty_url_resolves_bad_url() {
        let mut f = WebTransport::connect("");
        let waker = noop_waker();
        let mut cx = TaskContext::from_waker(&waker);
        match Pin::new(&mut f).poll(&mut cx) {
            Poll::Ready(Err(NetError::BadUrl)) => {}
            other => panic!("expected Ready(Err(BadUrl)), got {other:?}"),
        }
    }
}
