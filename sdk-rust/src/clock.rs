//! Wall-clock time (Stone 11.6.8 #4).
//!
//! Two distinct concepts of time exist in any OS, and Akiva exposes
//! both — but apps that confuse them produce subtle bugs. This
//! module is the wall-clock primitive; the monotonic counterpart is
//! [`crate::activity::Context::timestamp_ms`].
//!
//! | concept            | monotonic              | wall-clock                |
//! | ------------------ | ---------------------- | ------------------------- |
//! | source             | `performance.now()`    | `Date.now()`              |
//! | reference epoch    | navigation-start       | UNIX epoch (1970-01-01 UTC)|
//! | NTP / DST          | unaffected             | jumps                     |
//! | usage              | elapsed-time, timeouts | timestamps, "created at"  |
//! | resolution         | sub-ms                 | ms                        |
//!
//! **Use [`wall_clock_ms`] when the value is shown to a human or
//! persisted across boots — "this note was created at 2026-05-02
//! 14:32." Use `ctx.timestamp_ms` when the value is compared to
//! another timestamp from the same boot — "this animation has been
//! running for 800 ms."** Confusing the two means animations that
//! glitch when the user crosses a DST boundary, or "created at"
//! stamps that drift after an NTP correction.
//!
//! ```ignore
//! use akiva_sdk::clock;
//!
//! struct Note {
//!     text: String,
//!     created_ms: u64,    // wall clock — survives the user reopening the app
//! }
//!
//! impl Note {
//!     fn new(text: String) -> Self {
//!         Note { text, created_ms: clock::wall_clock_ms() }
//!     }
//! }
//! ```
//!
//! Ungated — wall-clock time is a primitive, not a resource. No app
//! has ever asked the user for permission to know what year it is.

/// Milliseconds since the UNIX epoch (1970-01-01 UTC). Wall-clock
/// time, **not** a monotonic counter — see the table at the top of
/// the module.
#[cfg(target_arch = "wasm32")]
pub fn wall_clock_ms() -> u64 {
    // SAFETY: the host returns a non-negative u64 derived from
    // `Date.now()`. No allocations, no buffers, no UB.
    unsafe { crate::runtime::akiva_host_wall_clock_ms() }
}

/// Non-wasm fallback. Returns the host's `SystemTime` reading so unit
/// tests on the developer's machine see a reasonable value. Fails
/// fast (panics) if the host clock is set before the UNIX epoch —
/// that's not a wasm runtime concern, but a sanity check on the
/// developer's machine.
#[cfg(not(target_arch = "wasm32"))]
pub fn wall_clock_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is before UNIX epoch")
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn returns_a_recent_value() {
        // 2026-01-01 UTC = 1_767_225_600_000 ms. Anything earlier
        // means our test machine is running a wildly wrong clock.
        let now = wall_clock_ms();
        assert!(now >= 1_767_225_600_000, "clock returned {now} (before 2026)");
    }

    #[test]
    fn second_call_is_not_strictly_earlier() {
        // Wall clock can step backwards on NTP/DST, but the test
        // machine isn't doing that during a 1-µs window. Two
        // consecutive calls yield a non-decreasing value.
        let a = wall_clock_ms();
        let b = wall_clock_ms();
        assert!(b >= a, "wall_clock_ms went backwards: {a} -> {b}");
    }
}
