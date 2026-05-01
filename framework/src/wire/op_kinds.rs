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

    /// Args: `(key: &str) -> Option<Vec<u8>>`. Read from the package's
    /// sandboxed key/value store. Stone 11.6 lifts this onto OPFS
    /// readonly handles in the app worker; in 11.3 it still routes
    /// through the kernel.
    pub const STORAGE_GET: u32 = 0x0103;

    /// Args: `(key: &str, value: &[u8]) -> ()`. Write to the package's
    /// sandboxed store. Always routed through the kernel (single
    /// writer per OPFS handle).
    pub const STORAGE_SET: u32 = 0x0104;
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
            io::TEXT_WIDTH, io::QUERY_PACKAGES, io::STORAGE_GET, io::STORAGE_SET,
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
            io::TEXT_WIDTH, io::QUERY_PACKAGES, io::STORAGE_GET, io::STORAGE_SET,
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
        assert_eq!(status::PENDING,                 sdk::status::PENDING);
        assert_eq!(status::READY,                   sdk::status::READY);
        assert_eq!(status::ERROR,                   sdk::status::ERROR);
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
