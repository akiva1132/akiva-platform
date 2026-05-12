//! Op-kind constants for the unified ABI (Stone 11.3).
//!
//! Mirror of `framework/src/wire/op_kinds.rs`. The two MUST stay
//! numerically identical — the host-side dispatcher routes by these
//! integers, so a drift here means the SDK calls one op and the host
//! runs another.
//!
//! sdk-rust deliberately does NOT depend on `akiva-framework` (which
//! is the kernel-side crate and would pull half the platform into
//! every app's wasm). The duplication is the price of the layering.
//! A workspace-level Vitest test pins the JS mirror; for the Rust
//! side, see the `cross_crate_op_kinds_match` test in
//! `framework/src/wire/op_kinds.rs` once it lands (deferred — would
//! require a dev-dep cycle today).
//!
//! Numbering is locked forever once assigned: a renumbering is a wire
//! break that requires every app to be rebuilt against the new SDK.

/// Fire-and-forget operations. Range `0x0001..=0x00FF`.
pub mod fire {
    /// Args: `[level: u32 LE | message: utf8...]`. Writes a log line.
    pub const LOG: u32 = 0x0001;
    /// Args: `[handle: u32 LE]`. Marks the activity for tear-down.
    pub const FINISH: u32 = 0x0002;
    /// Args: `[handle: u32 LE | intent: encoded Intent...]`.
    pub const START_ACTIVITY: u32 = 0x0003;
    /// Args: `[handle: u32 LE | request_code: u32 LE | intent...]`.
    pub const START_ACTIVITY_FOR_RESULT: u32 = 0x0004;
    /// Args: `[handle: u32 LE | result: encoded ActivityResult...]`.
    pub const SET_RESULT: u32 = 0x0005;
    /// Stone 14.1 — Send a frame on an open WebSocket. Args:
    /// `[ws_handle: u32 LE | kind: u8 | data: bytes...]` where
    /// `kind == 0` is a UTF-8 text frame and `kind == 1` is a
    /// binary frame. The handle was returned by [`io::WS_OPEN`].
    /// Sending on a closed handle is silently dropped (the
    /// followup `WS_RECV` will surface the close).
    pub const WS_SEND: u32 = 0x0006;
    /// Stone 14.1 — Close a WebSocket. Args:
    /// `[ws_handle: u32 LE | code: u16 LE | reason_len: u16 LE
    ///   | reason: utf8...]`. Idempotent; subsequent ops on the
    /// handle are no-ops.
    pub const WS_CLOSE: u32 = 0x0007;
    /// Stone 14.2 — WebTransport datagram (unreliable, unordered,
    /// the closest a browser exposes to UDP). Args:
    /// `[wt_handle: u32 LE | data: bytes...]`. Silent no-op on a
    /// closed handle. Capability gated by `NETWORK` at the dispatcher.
    pub const WT_SEND_DATAGRAM: u32 = 0x0008;
    /// Stone 14.2 — Send bytes on an open bidirectional stream.
    /// Args: `[stream_handle: u32 LE | data: bytes...]`. The handle
    /// was returned by [`io::WT_OPEN_BIDI`].
    pub const WT_SEND_STREAM: u32 = 0x0009;
    /// Stone 14.2 — Close a WebTransport bidirectional stream
    /// (writer side). Args: `[stream_handle: u32 LE | code: u32 LE]`.
    /// Idempotent. The peer's writer can still flow until they
    /// close their side; `WT_RECV_STREAM` keeps draining until then.
    pub const WT_CLOSE_STREAM: u32 = 0x000A;
    /// Stone 14.2 — Close a WebTransport session (and every
    /// stream under it). Args:
    /// `[wt_handle: u32 LE | code: u32 LE | reason_len: u16 LE
    ///   | reason: utf8...]`. Idempotent.
    pub const WT_CLOSE: u32 = 0x000B;
    /// Stone 14.3 — Close a system notification by id. Args:
    /// `[notify_id: u32 LE]`. Idempotent. Capability gated by
    /// `akiva.permission.NOTIFICATIONS`.
    pub const NOTIFY_CLOSE: u32 = 0x000C;
    /// Stone 14.5 — Trigger device haptic vibration. Args:
    /// `[count: u16 LE | duration_ms: u32 LE; count]`. An empty
    /// pattern (`count == 0`) cancels any in-progress vibration.
    /// Capability gated by `akiva.permission.VIBRATE`.
    pub const VIBRATE: u32 = 0x000D;
    /// Phase 15.4 — set the WebGPU compositor's per-Activity surface
    /// effects (backdrop blur, drop shadow, colour tint). Args:
    /// `[handle: u32 LE | SurfaceEffects::ENCODED_LEN bytes]` —
    /// 4 + 24 = 28 bytes. The effects persist on the framework side
    /// until the next call (or until the activity is destroyed); apps
    /// don't need to re-set them every frame. Canvas2D backends
    /// silently ignore. No grant required — the effects only apply
    /// to the calling activity's own surface.
    pub const SET_SURFACE_EFFECTS: u32 = 0x000E;
    /// Test-infra phase — publish the activity's semantic tree for
    /// inspector / Playwright projection. Args:
    /// `[handle: u32 LE | SemanticTree::encode bytes...]`. Cheap
    /// short-circuit on the host (a `Map.set`) so always-on is the
    /// shipped default; tabs project to hidden ARIA-decorated DOM only
    /// when `?inspector=1` is set in the URL. No grant required — a
    /// package can only describe its own widget tree, which is its
    /// own paint stream from a different angle.
    pub const PUBLISH_SEMANTICS: u32 = 0x000F;
}

/// I/O operations. Range `0x0101..=0x01FF`.
pub mod io {
    /// Args: `[size: f32 LE | text: utf8...]` -> `[width: f32 LE]`.
    pub const TEXT_WIDTH: u32 = 0x0101;
    /// Args: `[]` -> `pkgs_wire blob`. Gated by QUERY_PACKAGES.
    pub const QUERY_PACKAGES: u32 = 0x0102;
    /// Args: `[key: utf8...]` -> `[present: u8, value_bytes...]`.
    pub const STORAGE_GET: u32 = 0x0103;
    /// Args: `[key_len: u32 LE | key: utf8... | value: bytes...]` -> `[]`.
    pub const STORAGE_SET: u32 = 0x0104;
    /// Args: `[key: utf8...]` -> `[]`. Idempotent (missing key = no-op success).
    pub const STORAGE_DELETE: u32 = 0x0105;
    /// Stone 13.0prep — timer arm. Args: `[delay_ms: u32 LE]` -> `[]`.
    /// The future resolves READY after the host's wall-clock has
    /// advanced by ≥ `delay_ms` (best-effort; subject to JS event loop
    /// scheduling). No cancel-on-drop yet — a dropped Sleep future
    /// leaves a stale entry in the worker's futures map until the
    /// worker dies. Document as known limitation; revisit if practice
    /// shows leaking.
    pub const TIMER_SLEEP: u32 = 0x0106;
    /// Stone 13.2 — system dialog overlay. Args: see
    /// [`crate::dialog`] for the wire format. Resolves with `[kind:
    /// u8, btn_idx: u8]` once the user taps a button or outside the
    /// dialog box. The framework owns the dialog surface and input
    /// shielding — the app worker just awaits the resolution.
    pub const SHOW_DIALOG: u32 = 0x0107;
    /// Stone 14.1 — HTTP request, gated by
    /// `akiva.permission.NETWORK`. The host calls `fetch()`
    /// from the kernel worker; CORS rules apply. Args:
    /// ```text
    /// [method: u8]
    /// [url_len: u16 LE | url: utf8...]
    /// [header_count: u16 LE]
    /// repeat header_count:
    ///   [name_len: u16 LE | name: utf8...]
    ///   [val_len:  u16 LE | val:  utf8...]
    /// [body: bytes...]   // remainder of args
    /// ```
    /// Method codes: 0=GET, 1=POST, 2=PUT, 3=DELETE, 4=PATCH, 5=HEAD.
    /// Result:
    /// ```text
    /// [status: u16 LE]
    /// [header_count: u16 LE]
    /// repeat header_count: same shape as above
    /// [body: bytes...]
    /// ```
    /// On network/CORS failure → STATUS::ERROR with a UTF-8 reason.
    pub const HTTP_FETCH: u32 = 0x0108;
    /// Stone 14.1 — Open a WebSocket. Gated by
    /// `akiva.permission.NETWORK`. Args:
    /// `[url_len: u16 LE | url: utf8...
    ///   | proto_count: u8 | (proto_len: u16 LE | proto: utf8...)*]`.
    /// Resolves READY with `[ws_handle: u32 LE]` once the WS
    /// `open` event fires; ERROR with a UTF-8 reason if the
    /// handshake fails. The handle is opaque, allocated by the
    /// host, and only valid for the lifetime of this app instance.
    pub const WS_OPEN: u32 = 0x0109;
    /// Stone 14.1 — Receive the next event on a WebSocket.
    /// Args: `[ws_handle: u32 LE]`. Resolves READY with:
    /// ```text
    /// [event_kind: u8 | data: bytes...]
    /// ```
    /// `event_kind == 0` text message (data is utf8);
    /// `event_kind == 1` binary message;
    /// `event_kind == 2` closed (`data` = `[code: u16 LE
    ///   | reason: utf8...]`). After a close event every
    /// further `WS_RECV` resolves immediately as closed.
    pub const WS_RECV: u32 = 0x010A;
    /// Stone 14.2 — Open a WebTransport session. Gated by
    /// `akiva.permission.NETWORK`. Args:
    /// `[url_len: u16 LE | url: utf8...]`. Resolves READY with
    /// `[wt_handle: u32 LE]` on the underlying transport's `ready`
    /// promise; ERROR with utf8 reason on a handshake failure
    /// (HTTP/3 negotiation refused, server unavailable, TLS).
    pub const WT_OPEN: u32 = 0x010B;
    /// Stone 14.2 — Pull the next datagram or transport-level event.
    /// Args: `[wt_handle: u32 LE]`. Result:
    /// ```text
    /// [event_kind: u8 | data: bytes...]
    /// ```
    /// `event_kind == 0` datagram (data = the frame bytes).
    /// `event_kind == 1` closed (data = `[code: u32 LE
    ///   | reason: utf8...]`); every further recv on the handle
    /// resolves immediately as closed.
    pub const WT_RECV_DATAGRAM: u32 = 0x010C;
    /// Stone 14.2 — Open a bidirectional stream under an open
    /// WebTransport session. Args: `[wt_handle: u32 LE]`. Result:
    /// `[stream_handle: u32 LE]`. The stream stays open until
    /// either side calls [`fire::WT_CLOSE_STREAM`] or the parent
    /// session closes.
    pub const WT_OPEN_BIDI: u32 = 0x010D;
    /// Stone 14.2 — Pull the next chunk off a bidirectional stream.
    /// Args: `[stream_handle: u32 LE]`. Result:
    /// ```text
    /// [event_kind: u8 | data: bytes...]
    /// ```
    /// `event_kind == 0` bytes (data = the chunk; chunk size is up
    /// to the host's pump).
    /// `event_kind == 1` fin (peer closed their write side; data
    /// is empty). Subsequent recvs return fin.
    /// `event_kind == 2` error (data = utf8 reason).
    pub const WT_RECV_STREAM: u32 = 0x010E;
    /// Stone 14.3 — Request the browser-level notification
    /// permission. Args: `[]` (empty). Result: `[status: u8]`
    /// where 0 = Default (user has not yet decided), 1 = Granted,
    /// 2 = Denied. Must be triggered from a user-gesture handler;
    /// if called outside one, browsers may treat the request as
    /// implicit-denied. Gated by `akiva.permission.NOTIFICATIONS`
    /// at the system layer (separate from the browser permission).
    pub const NOTIFY_REQUEST_PERMISSION: u32 = 0x010F;
    /// Stone 14.3 — Post a system notification. Args:
    /// ```text
    /// [title_len: u16 LE | title: utf8...]
    /// [body_len:  u16 LE | body:  utf8...]
    /// [tag_len:   u16 LE | tag:   utf8...]
    /// ```
    /// Result: `[notify_id: u32 LE]` once the browser accepts the
    /// notification. ERROR with utf8 reason on
    /// `permission != "granted"`, browser refusal, or any
    /// `Notification` constructor throw. The `tag` collapses
    /// duplicates: posting twice with the same tag updates the
    /// existing notification rather than spawning a second.
    /// Gated by `akiva.permission.NOTIFICATIONS`.
    pub const NOTIFY_POST: u32 = 0x0110;
    /// Stone 14.4 — Read clipboard text. Args: `[]`. Result:
    /// `[text: utf8...]`. ERROR on permission denial or no text on
    /// the clipboard. Capability gated by `akiva.permission.CLIPBOARD`.
    /// The browser also enforces user-activation + page-focus;
    /// calls outside those constraints surface as ERROR with the
    /// browser's reason text.
    pub const CLIPBOARD_READ_TEXT: u32 = 0x0111;
    /// Stone 14.4 — Write clipboard text. Args: `[text: utf8...]`.
    /// Result: empty (READY = success). Capability gated by
    /// `akiva.permission.CLIPBOARD`. Same browser activation rules
    /// as `CLIPBOARD_READ_TEXT`.
    pub const CLIPBOARD_WRITE_TEXT: u32 = 0x0112;
    /// Stone 14.6 — One-shot device geolocation read. Args: `[]`.
    /// Result:
    /// ```text
    /// [lat: f64 LE | lon: f64 LE | accuracy_m: f64 LE
    ///  | has_alt: u8 | (alt: f64 LE)?
    ///  | has_speed: u8 | (speed: f64 LE)?
    ///  | has_heading: u8 | (heading: f64 LE)?
    ///  | timestamp_ms: f64 LE]
    /// ```
    /// ERROR with the browser's reason on permission denial,
    /// position-unavailable, or timeout. Capability gated by
    /// `akiva.permission.LOCATION`.
    pub const GEO_GET_POSITION: u32 = 0x0113;
    /// Foundation gap — list keys in the per-package storage sandbox,
    /// optionally filtered by a prefix. Args: `[prefix: utf8...]`
    /// (empty bytes = list every key the package owns). Result:
    /// ```text
    /// [count: u32 LE]
    /// repeat count: [key_len: u16 LE | key: utf8...]
    /// ```
    /// Required for any app that wants to enumerate its own data
    /// (notes, files, history, drafts) without maintaining a manual
    /// index key. Capability: same as STORAGE_GET — gated by
    /// `akiva.permission.STORAGE_SANDBOX`. The host caps the response
    /// payload at 64 KiB; if the directory listing exceeds that the
    /// host returns ERROR with reason "too many keys" (apps with that
    /// many keys should paginate by prefix). Order is the host's
    /// directory iteration order — apps that want a stable order sort
    /// the result themselves.
    pub const STORAGE_LIST: u32 = 0x0114;
}

/// Stone 11.4b — host→app dispatch op kinds. Mirror of
/// `framework::wire::op_kinds::cmd`. Used by the `akiva_app_dispatch`
/// glue inside the `akiva_app!` macro to demux a single export back
/// into the right `Activity` callback.
///
/// Numbering is locked forever once assigned — wire format. See
/// [`docs/abi.md §2.2`](../../../../docs/abi.md).
pub mod cmd {
    pub const CREATE: u32 = 0x01;
    pub const START: u32 = 0x02;
    pub const RESUME: u32 = 0x03;
    pub const PAUSE: u32 = 0x04;
    pub const STOP: u32 = 0x05;
    pub const DESTROY: u32 = 0x06;

    pub const POINTER: u32 = 0x10;
    pub const KEY: u32 = 0x11;
    pub const RESIZE: u32 = 0x12;
    /// Foundation gap — typed text. Distinct from KEY because key
    /// events carry the deprecated `KeyboardEvent.keyCode`, which
    /// can't represent a character. Text events carry the committed
    /// chunk (`e.key` for single-char keydown, `compositionend.data`
    /// for IME). Apps that want typed input override
    /// [`crate::Activity::on_text_input`].
    pub const TEXT_INPUT: u32 = 0x13;
    /// Wheel / scroll delta. Distinct from POINTER because wheels
    /// don't have phases (Down/Move/Up) and carry no x/y position of
    /// their own — they're "delta at the last-known pointer". Apps
    /// that want scroll input override [`crate::Activity::on_wheel`];
    /// the SDK's `Ui::feed_wheel` consumes the same event to drive
    /// `Node::Scroll` / `Node::LazyColumn`.
    pub const WHEEL: u32 = 0x14;

    pub const RENDER: u32 = 0x20;

    pub const SAVE_STATE: u32 = 0x30;
    pub const RESTORE_STATE: u32 = 0x31;

    pub const NEW_INTENT: u32 = 0x40;
    pub const ACTIVITY_RESULT: u32 = 0x41;
}

/// Status codes returned by `akiva_host_io_poll`'s low 32 bits.
pub mod status {
    pub const PENDING: u32 = 0;
    pub const READY:   u32 = 1;
    pub const ERROR:   u32 = 2;
    /// Stone 11.6.8 #2 — backpressure. The host returned BUSY because
    /// the app's slot is at `MAX_PENDING_PER_SLOT` concurrent in-flight
    /// dispatches; the future resolves immediately to `Err(_::Busy)`
    /// so the app can back off (e.g. spam-loop) rather than queue
    /// unbounded.
    pub const BUSY:    u32 = 3;
}

/// Decode the packed `(status, length)` u64 returned by
/// `akiva_host_io_poll`. Inverse of the host-side `packPoll`.
#[inline]
pub fn unpack_poll(packed: u64) -> (u32, u32) {
    let status = (packed & 0xFFFF_FFFF) as u32;
    let length = (packed >> 32) as u32;
    (status, length)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unpack_poll_round_trips() {
        // Pack like the JS host does: status low, length high.
        let packed = (status::READY as u64) | ((42u64) << 32);
        assert_eq!(unpack_poll(packed), (status::READY, 42));
    }

    #[test]
    fn ranges_are_locked() {
        // Bumping any of these would silently break SDK<->host wire compat
        // (an in-flight wasm app baked the old number into its bytecode).
        // Failure here means the constant moved out of its declared range.
        assert!(fire::LOG <= 0x00FF);
        assert!(fire::SET_RESULT <= 0x00FF);
        assert!(fire::WS_SEND <= 0x00FF);
        assert!(fire::WS_CLOSE <= 0x00FF);
        assert!(fire::WT_SEND_DATAGRAM <= 0x00FF);
        assert!(fire::WT_SEND_STREAM <= 0x00FF);
        assert!(fire::WT_CLOSE_STREAM <= 0x00FF);
        assert!(fire::WT_CLOSE <= 0x00FF);
        assert!(fire::NOTIFY_CLOSE <= 0x00FF);
        assert!(fire::VIBRATE <= 0x00FF);
        assert!(fire::SET_SURFACE_EFFECTS <= 0x00FF);
        assert!(fire::PUBLISH_SEMANTICS <= 0x00FF);
        assert!(io::TEXT_WIDTH >= 0x0101 && io::TEXT_WIDTH <= 0x01FF);
        assert!(io::STORAGE_SET >= 0x0101 && io::STORAGE_SET <= 0x01FF);
        assert!(io::STORAGE_DELETE >= 0x0101 && io::STORAGE_DELETE <= 0x01FF);
        assert!(io::SHOW_DIALOG >= 0x0101 && io::SHOW_DIALOG <= 0x01FF);
        assert!(io::HTTP_FETCH >= 0x0101 && io::HTTP_FETCH <= 0x01FF);
        assert!(io::WS_OPEN >= 0x0101 && io::WS_OPEN <= 0x01FF);
        assert!(io::WS_RECV >= 0x0101 && io::WS_RECV <= 0x01FF);
        assert!(io::WT_OPEN >= 0x0101 && io::WT_OPEN <= 0x01FF);
        assert!(io::WT_RECV_DATAGRAM >= 0x0101 && io::WT_RECV_DATAGRAM <= 0x01FF);
        assert!(io::WT_OPEN_BIDI >= 0x0101 && io::WT_OPEN_BIDI <= 0x01FF);
        assert!(io::WT_RECV_STREAM >= 0x0101 && io::WT_RECV_STREAM <= 0x01FF);
        assert!(io::NOTIFY_REQUEST_PERMISSION >= 0x0101 && io::NOTIFY_REQUEST_PERMISSION <= 0x01FF);
        assert!(io::NOTIFY_POST >= 0x0101 && io::NOTIFY_POST <= 0x01FF);
        assert!(io::CLIPBOARD_READ_TEXT >= 0x0101 && io::CLIPBOARD_READ_TEXT <= 0x01FF);
        assert!(io::CLIPBOARD_WRITE_TEXT >= 0x0101 && io::CLIPBOARD_WRITE_TEXT <= 0x01FF);
        assert!(io::GEO_GET_POSITION >= 0x0101 && io::GEO_GET_POSITION <= 0x01FF);
        assert!(io::STORAGE_LIST >= 0x0101 && io::STORAGE_LIST <= 0x01FF);
        assert_eq!(status::PENDING, 0);
        assert_eq!(status::READY,   1);
        assert_eq!(status::ERROR,   2);
    }

    #[test]
    fn cmd_kinds_are_unique_and_in_range() {
        // Stone 11.4b — host→app dispatch op kinds live in `0x01..=0x4F`,
        // disjoint from fire (`0x0001..=0x00FF`) and io
        // (`0x0101..=0x01FF`). The numerical disjointness isn't load-
        // bearing (different export, different import set) but a single
        // grep for `0x01` should never confuse the two without a `cmd::`
        // / `fire::` qualifier — keep the ranges visibly separate.
        let cmds = [
            cmd::CREATE, cmd::START, cmd::RESUME, cmd::PAUSE, cmd::STOP, cmd::DESTROY,
            cmd::POINTER, cmd::KEY, cmd::RESIZE, cmd::TEXT_INPUT, cmd::WHEEL,
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
}
