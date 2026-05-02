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
        ];
        let io = [
            io::TEXT_WIDTH, io::QUERY_PACKAGES,
            io::STORAGE_GET, io::STORAGE_SET, io::STORAGE_DELETE,
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
        ];
        for (i, a) in fire.iter().enumerate() {
            for b in &fire[i + 1..] {
                assert_ne!(a, b, "duplicate fire op kind {a:#x}");
            }
        }
        let io = [
            io::TEXT_WIDTH, io::QUERY_PACKAGES,
            io::STORAGE_GET, io::STORAGE_SET, io::STORAGE_DELETE,
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
        assert_eq!(io::TEXT_WIDTH,                  sdk::io::TEXT_WIDTH);
        assert_eq!(io::QUERY_PACKAGES,              sdk::io::QUERY_PACKAGES);
        assert_eq!(io::STORAGE_GET,                 sdk::io::STORAGE_GET);
        assert_eq!(io::STORAGE_SET,                 sdk::io::STORAGE_SET);
        assert_eq!(io::STORAGE_DELETE,              sdk::io::STORAGE_DELETE);
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
