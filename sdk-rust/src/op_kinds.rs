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
}

/// I/O operations. Range `0x0101..=0x01FF`.
pub mod io {
    /// Args: `[size: f32 LE | text: utf8...]` -> `[width: f32 LE]`.
    pub const TEXT_WIDTH: u32 = 0x0101;
    /// Args: `[]` -> `pkgs_wire blob`. Gated by QUERY_PACKAGES.
    pub const QUERY_PACKAGES: u32 = 0x0102;
    /// Args: `[key: utf8...]` -> `Option<bytes>`.
    pub const STORAGE_GET: u32 = 0x0103;
    /// Args: `[key_len: u32 LE | key: utf8... | value: bytes...]` -> `[]`.
    pub const STORAGE_SET: u32 = 0x0104;
}

/// Status codes returned by `akiva_host_io_poll`'s low 32 bits.
pub mod status {
    pub const PENDING: u32 = 0;
    pub const READY:   u32 = 1;
    pub const ERROR:   u32 = 2;
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
        assert!(io::TEXT_WIDTH >= 0x0101 && io::TEXT_WIDTH <= 0x01FF);
        assert!(io::STORAGE_SET >= 0x0101 && io::STORAGE_SET <= 0x01FF);
        assert_eq!(status::PENDING, 0);
        assert_eq!(status::READY,   1);
        assert_eq!(status::ERROR,   2);
    }
}
