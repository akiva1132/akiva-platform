//! Op-kind constants for the new io / fire ABI (Stone 11.3).
//!
//! Every host-mediated operation has a `u32` *op kind* that the app
//! passes as the first arg of [`akiva_host_io_start`] (data-returning
//! ops) or [`akiva_host_fire`] (fire-and-forget ops). The kind tells
//! the host which encoder to apply to the args blob and which
//! handler to dispatch.
//!
//! Fire vs io is the *transport*; the kind itself is just a stable
//! integer. Numbering keeps the two categories in disjoint ranges
//! so a corrupted blob that picks an io kind from a fire import (or
//! vice versa) is rejected loudly rather than silently mis-decoded.
//!
//! Layout choices:
//!
//! - **Fire range:** `0x0001..=0x00FF`. Headroom for ~250 fire ops;
//!   today we use 5.
//! - **Io range:**   `0x0101..=0x01FF`. Same headroom for io ops.
//! - **Reserved:**   `0x0200..=` for streams / pushed events when
//!   Stone 11.4+ wants them. Don't allocate from this range yet.
//!
//! Numbers are stable forever once assigned — they're a wire-format
//! contract between SDK and host. Renaming a constant is fine; renumbering
//! is a wire break.

/// Fire-and-forget operations. The host accepts the encoded args,
/// dispatches the side effect, and returns `0` on success or a
/// non-zero error code. No result data flows back.
pub mod fire {
    /// Args: `(level: u32, message: &str)`. Writes a log line.
    pub const LOG: u32 = 0x0001;

    /// Args: `(handle: u32)`. Marks the activity for tear-down at
    /// end-of-callback. See [`docs/activity-model.md`].
    pub const FINISH: u32 = 0x0002;

    /// Args: `(handle: u32, intent: encoded Intent)`. Resolve and
    /// launch the matching activity. See [`docs/intents.md`].
    pub const START_ACTIVITY: u32 = 0x0003;

    /// Args: `(handle: u32, intent: encoded Intent, request_code: u32)`.
    /// Same as `START_ACTIVITY` but the caller will receive a result
    /// via `akiva_app_activity_result` when the callee finishes.
    pub const START_ACTIVITY_FOR_RESULT: u32 = 0x0004;

    /// Args: `(handle: u32, result: encoded ActivityResult)`. Stash
    /// the result this activity wants to hand back to whoever
    /// launched it via `START_ACTIVITY_FOR_RESULT`.
    pub const SET_RESULT: u32 = 0x0005;

    /// Stone 14.1 — Send a frame on an open WebSocket.
    ///
    /// Args: `(ws_handle: u32, kind: u8, data: bytes...)` where
    /// `kind == 0` is a UTF-8 text frame and `kind == 1` is a
    /// binary frame. The handle was returned by [`io::WS_OPEN`];
    /// sending on a closed handle is silently dropped. Capability
    /// gated by `akiva.permission.NETWORK` — but the gate already
    /// fired at `WS_OPEN`, so an app without the grant has no way
    /// to obtain a valid handle in the first place.
    pub const WS_SEND: u32 = 0x0006;

    /// Stone 14.1 — Close a WebSocket. Idempotent; subsequent
    /// `WS_RECV` polls on the handle resolve immediately to
    /// `event_kind = 2` (closed).
    ///
    /// Args: `(ws_handle: u32, code: u16, reason: &str)`.
    pub const WS_CLOSE: u32 = 0x0007;

    /// Stone 14.2 — Send a WebTransport datagram. Datagrams are
    /// unreliable + unordered (UDP-style). Args:
    /// `(wt_handle: u32, data: bytes)`.
    pub const WT_SEND_DATAGRAM: u32 = 0x0008;

    /// Stone 14.2 — Send bytes on an open bidirectional stream.
    /// Args: `(stream_handle: u32, data: bytes)`.
    pub const WT_SEND_STREAM: u32 = 0x0009;

    /// Stone 14.2 — Close the writer side of a bidirectional
    /// stream. Args: `(stream_handle: u32, code: u32)`. Idempotent.
    pub const WT_CLOSE_STREAM: u32 = 0x000A;

    /// Stone 14.2 — Close a WebTransport session and every stream
    /// under it. Args: `(wt_handle: u32, code: u32, reason: &str)`.
    /// Idempotent.
    pub const WT_CLOSE: u32 = 0x000B;

    /// Stone 14.3 — Close a system notification by id. Args:
    /// `(notify_id: u32)`. Idempotent. Capability gated by
    /// `akiva.permission.NOTIFICATIONS`. The notification is
    /// physically dismissed by the host; the app may still hold
    /// the handle but `close` is a no-op after the first call.
    pub const NOTIFY_CLOSE: u32 = 0x000C;
    /// Stone 14.5 — Trigger a haptic vibration pattern.
    /// Args: `[count: u16 LE | (duration_ms: u32 LE){count}]`.
    /// Empty pattern (count==0) cancels any in-progress vibration
    /// (matches `navigator.vibrate(0)` semantics). Capability
    /// gated by `akiva.permission.VIBRATE`.
    pub const VIBRATE: u32 = 0x000D;
}

/// I/O operations — the app calls `akiva_host_io_start(op_kind, args…)`
/// to get a `future_id`, then `akiva_host_io_poll(future_id, …)` until
/// the future resolves to `Ready` (with a result blob) or `Error`.
pub mod io {
    /// Args: `(text: &str, size: f32) -> f32`. Returns the rendered
    /// width in pixels using the host's measureText against the same
    /// font the renderer uses. In 11.3 single-worker the future is
    /// always `Ready` on first poll; isolation in 11.4 makes this
    /// genuinely async.
    pub const TEXT_WIDTH: u32 = 0x0101;

    /// Args: `()` -> `Vec<u8>` (length-prefixed package list, see
    /// [`crate::wire::pkgs`]). Snapshot of installed packages the
    /// caller is granted to see (gated by
    /// `akiva.permission.QUERY_PACKAGES`).
    pub const QUERY_PACKAGES: u32 = 0x0102;

    /// Storage GET. Read from the package's sandboxed key/value
    /// store. The host namespaces every user-supplied key under
    /// `pkgs/{package_id}/` so apps can't see each other's data.
    ///
    /// Args:   `[key_bytes: utf8...]` (the whole args buffer is the
    ///         key — no length prefix needed because the caller
    ///         already supplies the byte length).
    /// Result: `[present: u8, value_bytes...]` where `present` is
    ///         `1` when the key existed (value follows, possibly
    ///         empty) and `0` for "not found" (no value bytes).
    ///         Error blob carries a UTF-8 reason on STATUS::ERROR
    ///         (typically a quota/IO failure or empty-key reject).
    ///
    /// Stone 11.6 wires the kernel-routed handler. A future
    /// optimization will hand readonly OPFS handles directly to
    /// the app-worker so reads skip the round-trip; writes always
    /// proxy through the kernel for single-writer discipline.
    pub const STORAGE_GET: u32 = 0x0103;

    /// Storage SET. Write to the package's sandboxed store.
    ///
    /// Args:   `[key_len: u32 LE, key_bytes..., value_bytes...]`.
    ///         Length-prefixed key because the value can be any
    ///         bytes (no separator is safe). Empty key is rejected.
    /// Result: empty payload on success (STATUS::READY, len=0).
    ///         Error blob carries a UTF-8 reason on STATUS::ERROR.
    pub const STORAGE_SET: u32 = 0x0104;

    /// Storage DELETE. Remove a key from the package's sandboxed
    /// store. Idempotent — deleting a missing key is a no-op
    /// success (the SDK doesn't surface a "not present" failure
    /// because the user's intent was "make sure it's gone").
    ///
    /// Args:   `[key_bytes: utf8...]`. Same shape as STORAGE_GET.
    /// Result: empty (STATUS::READY).
    pub const STORAGE_DELETE: u32 = 0x0105;

    /// Stone 13.0prep — Timer sleep. Park a future for `delay_ms` of
    /// wall-clock time, then resolve READY with empty payload.
    ///
    /// Args:   `[delay_ms: u32 LE]`.
    /// Result: empty (STATUS::READY).
    ///
    /// Best-effort: subject to JS event-loop scheduling and tab
    /// background-throttling. The host caps `delay_ms` at the
    /// equivalent of one frame (16ms) for backgrounded tabs to avoid
    /// wakeup storms.
    pub const TIMER_SLEEP: u32 = 0x0106;

    /// Stone 13.2 — system dialog overlay.
    ///
    /// Args:
    /// `[handle: u32 LE | title_len: u16 LE | title: utf8...
    ///   | body_len: u16 LE | body: utf8...
    ///   | btn_count: u8 | (btn_len: u16 LE | btn: utf8...)*]`
    ///
    /// Result: `[kind: u8, btn_idx: u8]` — `kind == 0` →
    /// `Button(btn_idx)`, `kind == 1` → `Dismissed` (`btn_idx`
    /// ignored).
    ///
    /// The framework owns the dialog surface and input shielding; the
    /// caller's worker just awaits the resolution. See
    /// [`crate::window_manager::dialog`] for the framework-side
    /// implementation.
    pub const SHOW_DIALOG: u32 = 0x0107;

    /// Stone 14.1 — HTTP request, gated by
    /// `akiva.permission.NETWORK`.
    ///
    /// Args:
    /// ```text
    /// [method: u8]
    /// [url_len: u16 LE | url: utf8...]
    /// [header_count: u16 LE]
    /// repeat header_count:
    ///   [name_len: u16 LE | name: utf8...]
    ///   [val_len:  u16 LE | val:  utf8...]
    /// [body: bytes...]   // remainder
    /// ```
    /// Method codes: 0=GET, 1=POST, 2=PUT, 3=DELETE, 4=PATCH, 5=HEAD.
    ///
    /// Result:
    /// ```text
    /// [status: u16 LE]
    /// [header_count: u16 LE]
    /// repeat header_count: same shape
    /// [body: bytes...]
    /// ```
    /// On network or CORS failure the future resolves
    /// `STATUS::ERROR` with a UTF-8 reason. The browser's CORS
    /// rules apply transparently — apps targeting servers that
    /// don't expose CORS headers must proxy through their own
    /// backend, exactly like a sandboxed mobile app on a
    /// network without root certs would.
    pub const HTTP_FETCH: u32 = 0x0108;

    /// Stone 14.1 — Open a WebSocket. Gated by
    /// `akiva.permission.NETWORK`.
    ///
    /// Args: `[url_len: u16 LE | url: utf8...
    ///   | proto_count: u8 | (proto_len: u16 LE | proto: utf8...)*]`.
    ///
    /// Resolves READY with `[ws_handle: u32 LE]` once the
    /// underlying WebSocket emits its `open` event; resolves
    /// ERROR with a UTF-8 reason on handshake failure. The
    /// handle is opaque, allocated by the host's `NetManager`,
    /// and tied to the calling package — passing it from
    /// another package's worker is rejected at the dispatcher.
    pub const WS_OPEN: u32 = 0x0109;

    /// Stone 14.1 — Pull the next event off an open WebSocket.
    ///
    /// Args: `[ws_handle: u32 LE]`.
    ///
    /// Result:
    /// ```text
    /// [event_kind: u8 | data: bytes...]
    /// ```
    /// where `event_kind`:
    /// - `0` text message (data is utf8 of the frame),
    /// - `1` binary message,
    /// - `2` closed (data = `[code: u16 LE | reason: utf8...]`).
    ///
    /// The host buffers up to a small bounded queue per handle;
    /// if the queue overflows the connection is force-closed
    /// and the next `WS_RECV` surfaces a `2` event with code
    /// `1009` ("message too big") in keeping with RFC 6455.
    /// After a close event every further `WS_RECV` on the
    /// handle resolves immediately as closed.
    pub const WS_RECV: u32 = 0x010A;

    /// Stone 14.2 — Open a WebTransport (HTTP/3) session. Gated
    /// by `akiva.permission.NETWORK`.
    ///
    /// Args: `[url_len: u16 LE | url: utf8...]`.
    ///
    /// Resolves READY with `[wt_handle: u32 LE]` once the
    /// underlying session's `ready` promise fulfills; ERROR with
    /// a UTF-8 reason on handshake failure (HTTP/3 negotiation
    /// refused, server unavailable, TLS, or `closed` rejecting
    /// before `ready`). Browsers gate WebTransport behind
    /// HTTPS-only and require the server to advertise HTTP/3 +
    /// WebTransport ALPN.
    pub const WT_OPEN: u32 = 0x010B;

    /// Stone 14.2 — Pull the next datagram (or transport close
    /// notification) off a session.
    ///
    /// Args: `[wt_handle: u32 LE]`. Result:
    /// `[event_kind: u8 | data: bytes...]` where:
    /// - `0` datagram (data = frame bytes; ≤ ~1200 bytes typical),
    /// - `1` closed (data = `[code: u32 LE | reason: utf8]`).
    ///
    /// Datagrams are unreliable + unordered: the host pump may
    /// drop frames under backpressure (RFC 9221 — datagrams "MAY
    /// be dropped") rather than block; the SDK treats lost
    /// frames as fact-of-life UDP-style behaviour.
    pub const WT_RECV_DATAGRAM: u32 = 0x010C;

    /// Stone 14.2 — Open a bidirectional stream under an open
    /// session. Streams are reliable + ordered (TCP-style on top
    /// of QUIC streams).
    ///
    /// Args: `[wt_handle: u32 LE]`. Result:
    /// `[stream_handle: u32 LE]`.
    pub const WT_OPEN_BIDI: u32 = 0x010D;

    /// Stone 14.2 — Pull the next chunk off a bidirectional stream.
    ///
    /// Args: `[stream_handle: u32 LE]`. Result:
    /// `[event_kind: u8 | data: bytes...]`:
    /// - `0` bytes (data = chunk; size up to host pump),
    /// - `1` fin (peer closed their write side; data empty),
    /// - `2` error (data = utf8 reason).
    ///
    /// After fin or error every further recv resolves with the
    /// same event so app loops terminate naturally.
    pub const WT_RECV_STREAM: u32 = 0x010E;

    /// Stone 14.3 — Request the browser-level Notification
    /// permission. Args: `[]`. Result: `[status: u8]` —
    /// 0 = Default, 1 = Granted, 2 = Denied. Must be triggered
    /// from a user-gesture path or browsers may auto-deny.
    /// Gated by `akiva.permission.NOTIFICATIONS` at the system
    /// layer (separate from the browser permission, which is
    /// what this op surfaces).
    pub const NOTIFY_REQUEST_PERMISSION: u32 = 0x010F;

    /// Stone 14.3 — Post a system notification.
    ///
    /// Args:
    /// ```text
    /// [title_len: u16 LE | title: utf8...]
    /// [body_len:  u16 LE | body:  utf8...]
    /// [tag_len:   u16 LE | tag:   utf8...]
    /// ```
    /// Result: `[notify_id: u32 LE]` once the browser accepts
    /// the notification; ERROR with a utf8 reason on
    /// `permission != "granted"` or any browser refusal.
    ///
    /// The `tag` collapses duplicates per browser convention —
    /// posting twice with the same tag updates the existing
    /// notification rather than spawning a second. Apps that
    /// want every event to surface separately should leave
    /// `tag` empty.
    pub const NOTIFY_POST: u32 = 0x0110;

    /// Stone 14.4 — Read clipboard text. Args: `[]`. Result:
    /// `[text: utf8...]`. Capability gated by
    /// `akiva.permission.CLIPBOARD`. ERROR on browser refusal
    /// (no user activation, no focus, no text content).
    pub const CLIPBOARD_READ_TEXT: u32 = 0x0111;

    /// Stone 14.4 — Write clipboard text. Args: `[text: utf8...]`.
    /// Result: empty (READY = success). Same browser activation
    /// rules as the read.
    pub const CLIPBOARD_WRITE_TEXT: u32 = 0x0112;

    /// Stone 14.6 — One-shot geolocation read. Args: `[]`.
    /// Result wire format:
    /// ```text
    /// [lat: f64 LE | lon: f64 LE | accuracy_m: f64 LE
    ///  | has_alt: u8 | (alt: f64 LE)?
    ///  | has_speed: u8 | (speed: f64 LE)?
    ///  | has_heading: u8 | (heading: f64 LE)?
    ///  | timestamp_ms: f64 LE]
    /// ```
    /// `has_*` is the present byte; the f64 follows only if it
    /// equals `1`. Optional fields collapse to absent when the
    /// browser reports `null` (e.g. browsers without GPS rarely
    /// return altitude). ERROR with utf8 reason on permission
    /// denial, position-unavailable, or timeout. Capability
    /// gated by `akiva.permission.LOCATION`.
    pub const GEO_GET_POSITION: u32 = 0x0113;

    /// Foundation gap — list keys in the per-package storage sandbox,
    /// optionally filtered by a UTF-8 prefix. Args: `[prefix: utf8...]`
    /// (empty bytes mean "list every key the package owns"). Result:
    /// ```text
    /// [count: u32 LE]
    /// repeat count: [key_len: u16 LE | key: utf8...]
    /// ```
    /// The host caps the result payload at 64 KiB; oversize listings
    /// resolve `STATUS::ERROR` with reason `"too many keys"` so apps
    /// can paginate by prefix. Order is the host's directory iteration
    /// order — not lexicographic; sort caller-side if you care. Same
    /// gate as STORAGE_GET (`akiva.permission.STORAGE_SANDBOX`).
    pub const STORAGE_LIST: u32 = 0x0114;
}

/// Stone 11.4b — host→app dispatch op kinds.
///
/// The single export `akiva_app_dispatch(cmd_kind, payload_ptr,
/// payload_len) -> u64` replaces the 13 per-callback exports
/// (`akiva_app_create`, `..._start`, `..._render`, …). The host packs
/// the per-callback args into the payload and the wasm app demuxes by
/// `cmd_kind`. Result bytes (render, save_state) flow back via a
/// separate `akiva_app_get_result(future_id, out_len_ptr) -> *const u8`
/// export (Stone 11.4b-2).
///
/// Layout:
/// - **Lifecycle:** `0x01..=0x0F` — six transitions; only `CREATE`
///   carries a payload (the activity class string).
/// - **Input:**     `0x10..=0x1F` — pointer/key/resize, fixed-size
///   payloads.
/// - **Render:**    `0x20..=0x2F`.
/// - **State:**     `0x30..=0x3F` — save/restore.
/// - **Intent:**    `0x40..=0x4F` — new_intent + activity_result.
/// - **Reserved:**  `0x50..=` for streams / pushed events when 11.5+
///   wants them.
///
/// Disjoint from the host-direction `fire::*` (`0x0001..=0x00FF`) and
/// `io::*` (`0x0101..=0x01FF`) ranges by construction — the
/// `akiva_app_dispatch` import and the `akiva_host_*` imports are
/// separate functions, so a corrupted blob can't cross-route, but the
/// disjoint ranges keep the constants searchable in one grep without
/// false matches.
///
/// Numbering is locked forever once assigned — wire format. See
/// [`docs/abi.md §2.2`](../../../../docs/abi.md).
pub mod cmd {
    /// Args: `[handle: u32 LE, class: utf8...]`. Class string is
    /// `payload[4..]`. Mirrors the old `akiva_app_create(handle,
    /// class_ptr, class_len)`.
    pub const CREATE: u32 = 0x01;
    /// Args: `[handle: u32 LE]`. Mirrors `akiva_app_start(handle)`.
    pub const START: u32 = 0x02;
    /// Args: `[handle: u32 LE]`. Mirrors `akiva_app_resume(handle)`.
    pub const RESUME: u32 = 0x03;
    /// Args: `[handle: u32 LE]`. Mirrors `akiva_app_pause(handle)`.
    pub const PAUSE: u32 = 0x04;
    /// Args: `[handle: u32 LE]`. Mirrors `akiva_app_stop(handle)`.
    pub const STOP: u32 = 0x05;
    /// Args: `[handle: u32 LE]`. Mirrors `akiva_app_destroy(handle)`.
    pub const DESTROY: u32 = 0x06;

    /// Args: `[handle: u32 LE, x: f32 LE, y: f32 LE, button: u32 LE,
    /// phase: u32 LE, ts_ms: f64 LE]`. 28 bytes.
    pub const POINTER: u32 = 0x10;
    /// Args: `[handle: u32 LE, code: u32 LE, modifiers: u32 LE,
    /// phase: u32 LE, ts_ms: f64 LE]`. 24 bytes.
    pub const KEY: u32 = 0x11;
    /// Args: `[handle: u32 LE, width_px: u32 LE, height_px: u32 LE,
    /// density: f32 LE]`. 16 bytes.
    pub const RESIZE: u32 = 0x12;

    /// Args: `[handle: u32 LE, ts_ms: f64 LE]`. 12 bytes. Result
    /// bytes (paint stream) fetched via `akiva_app_get_result`.
    pub const RENDER: u32 = 0x20;

    /// Args: `[handle: u32 LE]`. Result bytes (encoded `SavedState`)
    /// fetched via `akiva_app_get_result`.
    pub const SAVE_STATE: u32 = 0x30;
    /// Args: `[handle: u32 LE, state: bytes...]`. State is
    /// `payload[4..]`.
    pub const RESTORE_STATE: u32 = 0x31;

    /// Args: `[handle: u32 LE, intent: encoded Intent...]`. Intent is
    /// `payload[4..]`.
    pub const NEW_INTENT: u32 = 0x40;
    /// Args: `[handle: u32 LE, request_code: u32 LE,
    /// result: encoded ActivityResult...]`. Result bytes are
    /// `payload[8..]`.
    pub const ACTIVITY_RESULT: u32 = 0x41;
}

/// Status codes returned by [`akiva_host_io_poll`].
pub mod status {
    /// Future is not yet resolved. App should yield (return Pending
    /// from its own poll) and try again on the next wake.
    pub const PENDING: u32 = 0;
    /// Future resolved successfully. The result blob has been written
    /// to the out buffer; the high bits of the return value carry the
    /// payload length (TBD wire detail in 11.3b).
    pub const READY: u32 = 1;
    /// Future resolved with an error. Out buffer carries an
    /// encoded error message (UTF-8 string).
    pub const ERROR: u32 = 2;
    /// Stone 11.6.8 #2 — backpressure: host refused the request
    /// because the app's slot is at `MAX_PENDING_PER_SLOT`. Distinct
    /// from `ERROR` so SDK code can branch on it (typically retry
    /// after a yield) without conflating with permanent failures.
    pub const BUSY: u32 = 3;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fire_ops_disjoint_from_io_ops() {
        // Cross-check the disjoint-range invariant by listing every
        // assigned constant. If the test compiles after a future op
        // is added without touching this list, the new op silently
        // skips the invariant — so we reference each one explicitly
        // and let the assertion enforce the ranges.
        let fire = [
            fire::LOG, fire::FINISH, fire::START_ACTIVITY,
            fire::START_ACTIVITY_FOR_RESULT, fire::SET_RESULT,
            fire::WS_SEND, fire::WS_CLOSE,
            fire::WT_SEND_DATAGRAM, fire::WT_SEND_STREAM,
            fire::WT_CLOSE_STREAM, fire::WT_CLOSE,
            fire::NOTIFY_CLOSE,
            fire::VIBRATE,
        ];
        let io = [
            io::TEXT_WIDTH, io::QUERY_PACKAGES,
            io::STORAGE_GET, io::STORAGE_SET, io::STORAGE_DELETE,
            io::TIMER_SLEEP, io::SHOW_DIALOG,
            io::HTTP_FETCH, io::WS_OPEN, io::WS_RECV,
            io::WT_OPEN, io::WT_RECV_DATAGRAM,
            io::WT_OPEN_BIDI, io::WT_RECV_STREAM,
            io::NOTIFY_REQUEST_PERMISSION, io::NOTIFY_POST,
            io::CLIPBOARD_READ_TEXT, io::CLIPBOARD_WRITE_TEXT,
            io::GEO_GET_POSITION,
            io::STORAGE_LIST,
        ];
        for k in fire { assert!((0x0001..=0x00FF).contains(&k), "fire op {k:#x} out of range"); }
        for k in io   { assert!((0x0101..=0x01FF).contains(&k), "io op {k:#x} out of range"); }
        for f in fire {
            for i in io {
                assert_ne!(f, i, "fire and io op kinds must be disjoint ({f:#x} vs {i:#x})");
            }
        }
    }

    #[test]
    fn op_kinds_are_unique_within_each_category() {
        let fire = [
            fire::LOG, fire::FINISH, fire::START_ACTIVITY,
            fire::START_ACTIVITY_FOR_RESULT, fire::SET_RESULT,
            fire::WS_SEND, fire::WS_CLOSE,
            fire::WT_SEND_DATAGRAM, fire::WT_SEND_STREAM,
            fire::WT_CLOSE_STREAM, fire::WT_CLOSE,
            fire::NOTIFY_CLOSE,
            fire::VIBRATE,
        ];
        for (i, a) in fire.iter().enumerate() {
            for b in &fire[i + 1..] {
                assert_ne!(a, b, "duplicate fire op kind {a:#x}");
            }
        }
        let io = [
            io::TEXT_WIDTH, io::QUERY_PACKAGES,
            io::STORAGE_GET, io::STORAGE_SET, io::STORAGE_DELETE,
            io::TIMER_SLEEP, io::SHOW_DIALOG,
            io::HTTP_FETCH, io::WS_OPEN, io::WS_RECV,
            io::WT_OPEN, io::WT_RECV_DATAGRAM,
            io::WT_OPEN_BIDI, io::WT_RECV_STREAM,
            io::NOTIFY_REQUEST_PERMISSION, io::NOTIFY_POST,
            io::CLIPBOARD_READ_TEXT, io::CLIPBOARD_WRITE_TEXT,
            io::GEO_GET_POSITION,
            io::STORAGE_LIST,
        ];
        for (i, a) in io.iter().enumerate() {
            for b in &io[i + 1..] {
                assert_ne!(a, b, "duplicate io op kind {a:#x}");
            }
        }
    }

    #[test]
    fn cross_crate_op_kinds_match_sdk_mirror() {
        // sdk-rust duplicates these constants in `akiva_sdk::op_kinds`
        // because it can't depend on `akiva-framework` (kernel-side
        // crate, would bloat every app). This test pins that the two
        // copies stay numerically equal — the dispatcher in
        // `runtime/web/app_registry.js` routes by these integers, so
        // a drift means the SDK calls one op and the host runs another.
        use akiva_sdk::op_kinds as sdk;
        assert_eq!(fire::LOG,                       sdk::fire::LOG);
        assert_eq!(fire::FINISH,                    sdk::fire::FINISH);
        assert_eq!(fire::START_ACTIVITY,            sdk::fire::START_ACTIVITY);
        assert_eq!(fire::START_ACTIVITY_FOR_RESULT, sdk::fire::START_ACTIVITY_FOR_RESULT);
        assert_eq!(fire::SET_RESULT,                sdk::fire::SET_RESULT);
        assert_eq!(fire::WS_SEND,                   sdk::fire::WS_SEND);
        assert_eq!(fire::WS_CLOSE,                  sdk::fire::WS_CLOSE);
        assert_eq!(fire::WT_SEND_DATAGRAM,          sdk::fire::WT_SEND_DATAGRAM);
        assert_eq!(fire::WT_SEND_STREAM,            sdk::fire::WT_SEND_STREAM);
        assert_eq!(fire::WT_CLOSE_STREAM,           sdk::fire::WT_CLOSE_STREAM);
        assert_eq!(fire::WT_CLOSE,                  sdk::fire::WT_CLOSE);
        assert_eq!(fire::NOTIFY_CLOSE,              sdk::fire::NOTIFY_CLOSE);
        assert_eq!(fire::VIBRATE,                   sdk::fire::VIBRATE);
        assert_eq!(io::TEXT_WIDTH,                  sdk::io::TEXT_WIDTH);
        assert_eq!(io::QUERY_PACKAGES,              sdk::io::QUERY_PACKAGES);
        assert_eq!(io::STORAGE_GET,                 sdk::io::STORAGE_GET);
        assert_eq!(io::STORAGE_SET,                 sdk::io::STORAGE_SET);
        assert_eq!(io::STORAGE_DELETE,              sdk::io::STORAGE_DELETE);
        assert_eq!(io::TIMER_SLEEP,                 sdk::io::TIMER_SLEEP);
        assert_eq!(io::SHOW_DIALOG,                 sdk::io::SHOW_DIALOG);
        assert_eq!(io::HTTP_FETCH,                  sdk::io::HTTP_FETCH);
        assert_eq!(io::WS_OPEN,                     sdk::io::WS_OPEN);
        assert_eq!(io::WS_RECV,                     sdk::io::WS_RECV);
        assert_eq!(io::WT_OPEN,                     sdk::io::WT_OPEN);
        assert_eq!(io::WT_RECV_DATAGRAM,            sdk::io::WT_RECV_DATAGRAM);
        assert_eq!(io::WT_OPEN_BIDI,                sdk::io::WT_OPEN_BIDI);
        assert_eq!(io::WT_RECV_STREAM,              sdk::io::WT_RECV_STREAM);
        assert_eq!(io::NOTIFY_REQUEST_PERMISSION,   sdk::io::NOTIFY_REQUEST_PERMISSION);
        assert_eq!(io::NOTIFY_POST,                 sdk::io::NOTIFY_POST);
        assert_eq!(io::CLIPBOARD_READ_TEXT,         sdk::io::CLIPBOARD_READ_TEXT);
        assert_eq!(io::CLIPBOARD_WRITE_TEXT,        sdk::io::CLIPBOARD_WRITE_TEXT);
        assert_eq!(io::GEO_GET_POSITION,            sdk::io::GEO_GET_POSITION);
        assert_eq!(io::STORAGE_LIST,                sdk::io::STORAGE_LIST);
        assert_eq!(status::PENDING,                 sdk::status::PENDING);
        assert_eq!(status::READY,                   sdk::status::READY);
        assert_eq!(status::ERROR,                   sdk::status::ERROR);
        // Stone 11.4b — host→app dispatch kinds.
        assert_eq!(cmd::CREATE,           sdk::cmd::CREATE);
        assert_eq!(cmd::START,            sdk::cmd::START);
        assert_eq!(cmd::RESUME,           sdk::cmd::RESUME);
        assert_eq!(cmd::PAUSE,            sdk::cmd::PAUSE);
        assert_eq!(cmd::STOP,             sdk::cmd::STOP);
        assert_eq!(cmd::DESTROY,          sdk::cmd::DESTROY);
        assert_eq!(cmd::POINTER,          sdk::cmd::POINTER);
        assert_eq!(cmd::KEY,              sdk::cmd::KEY);
        assert_eq!(cmd::RESIZE,           sdk::cmd::RESIZE);
        assert_eq!(cmd::RENDER,           sdk::cmd::RENDER);
        assert_eq!(cmd::SAVE_STATE,       sdk::cmd::SAVE_STATE);
        assert_eq!(cmd::RESTORE_STATE,    sdk::cmd::RESTORE_STATE);
        assert_eq!(cmd::NEW_INTENT,       sdk::cmd::NEW_INTENT);
        assert_eq!(cmd::ACTIVITY_RESULT,  sdk::cmd::ACTIVITY_RESULT);
    }

    #[test]
    fn cmd_kinds_are_unique() {
        let cmds = [
            cmd::CREATE, cmd::START, cmd::RESUME, cmd::PAUSE, cmd::STOP, cmd::DESTROY,
            cmd::POINTER, cmd::KEY, cmd::RESIZE,
            cmd::RENDER,
            cmd::SAVE_STATE, cmd::RESTORE_STATE,
            cmd::NEW_INTENT, cmd::ACTIVITY_RESULT,
        ];
        for k in cmds {
            assert!((0x01..=0x4F).contains(&k), "cmd op {k:#x} out of range");
        }
        for (i, a) in cmds.iter().enumerate() {
            for b in &cmds[i + 1..] {
                assert_ne!(a, b, "duplicate cmd op kind {a:#x}");
            }
        }
    }

    #[test]
    fn status_codes_match_the_design() {
        // PENDING/READY/ERROR encode 0/1/2 — locked because
        // app-side poll loops check for `>= 1` (resolved) vs `== 0`
        // (still pending), and `== 2` for error. Reordering breaks
        // the SDK glue.
        assert_eq!(status::PENDING, 0);
        assert_eq!(status::READY,   1);
        assert_eq!(status::ERROR,   2);
    }
}
