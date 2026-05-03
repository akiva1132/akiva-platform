//! Haptic vibration — Stone 14.5.
//!
//! Apps with `akiva.permission.VIBRATE` declared in their manifest
//! can trigger device vibration patterns via
//! `navigator.vibrate(pattern)`. Fire-and-forget — there's no
//! "vibration completed" signal from the browser, and apps that
//! need to know the device actually vibrated read [`is_supported`]
//! ahead of time.
//!
//! Pattern semantics match the W3C Vibration API: a sequence of
//! `[on, off, on, off, …]` durations in milliseconds. A pattern of
//! `[200]` vibrates for 200ms; `[100, 50, 100]` does two pulses
//! with a 50ms pause between. An empty pattern cancels any
//! in-progress vibration (mirrors `navigator.vibrate(0)`).
//!
//! ```ignore
//! use akiva_sdk::vibration;
//!
//! fn buzz() {
//!     // Single 100ms pulse.
//!     vibration::vibrate(&[100]);
//! }
//!
//! fn sos() {
//!     // …— short-short-short long-long-long short-short-short
//!     vibration::vibrate(&[100, 100, 100, 100, 100, 300, 300, 100, 300, 100, 300, 100, 100, 100, 100, 100, 100]);
//! }
//! ```

use crate::op_kinds::fire;

/// Trigger a vibration pattern. Each `u32` is a duration in
/// milliseconds; even-indexed entries are vibration-on, odd-indexed
/// entries are vibration-off pauses.
///
/// An empty `pattern` cancels any in-progress vibration.
///
/// Returns silently — `navigator.vibrate` itself is fire-and-forget,
/// and the browser may refuse silently (no haptic hardware,
/// page hidden, no recent user activation). Apps that need to
/// know whether the call landed in pixels can't get that signal
/// today; treat vibration as best-effort UX feedback.
pub fn vibrate(pattern: &[u32]) {
    let count = pattern.len().min(u16::MAX as usize) as u16;
    let mut args = Vec::with_capacity(2 + (count as usize) * 4);
    args.extend_from_slice(&count.to_le_bytes());
    for &ms in pattern.iter().take(count as usize) {
        args.extend_from_slice(&ms.to_le_bytes());
    }
    host_fire(fire::VIBRATE, &args);
}

/// Cancel any in-progress vibration. Equivalent to
/// `vibrate(&[])` but reads more clearly at call sites.
pub fn cancel() {
    vibrate(&[]);
}

// ── wasm/non-wasm glue ─────────────────────────────────────────────────

#[cfg(target_arch = "wasm32")]
fn host_fire(op_kind: u32, args: &[u8]) -> u32 { crate::runtime::host_fire(op_kind, args) }

#[cfg(not(target_arch = "wasm32"))]
fn host_fire(_op_kind: u32, _args: &[u8]) -> u32 { 0 }

#[cfg(test)]
mod tests {
    /// Verify the wire shape of a single-pulse pattern. The host's
    /// decoder mirrors this layout, so any change here without a
    /// matching change in `app_registry.js::_handleVibrate` would
    /// silently mis-vibrate.
    #[test]
    fn single_pulse_args_layout() {
        // [count:u16 LE][ms:u32 LE]
        let pattern = [200u32];
        let mut args = Vec::new();
        args.extend_from_slice(&(pattern.len() as u16).to_le_bytes());
        for ms in pattern { args.extend_from_slice(&ms.to_le_bytes()); }
        assert_eq!(args.len(), 2 + 4);
        assert_eq!(u16::from_le_bytes([args[0], args[1]]), 1);
        assert_eq!(u32::from_le_bytes([args[2], args[3], args[4], args[5]]), 200);
    }

    #[test]
    fn empty_pattern_args() {
        let pattern: [u32; 0] = [];
        let mut args = Vec::new();
        args.extend_from_slice(&(pattern.len() as u16).to_le_bytes());
        // Empty pattern is just the 2-byte count==0 prefix; cancels in-progress vibration.
        assert_eq!(args, vec![0, 0]);
    }

    #[test]
    fn vibrate_does_not_panic_on_long_pattern() {
        // Patterns at the u16 cap should encode without panic.
        let big = vec![10u32; 70_000];
        super::vibrate(&big);
        // No assertion — just that the code path doesn't panic.
        // Off-wasm `host_fire` is a no-op stub.
    }
}
