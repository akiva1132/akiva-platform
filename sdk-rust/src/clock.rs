//! Clocks (Stone 11.6.8 #4).
//!
//! Two distinct concepts of time exist in any OS, and Akiva exposes
//! both — but apps that confuse them produce subtle bugs. This
//! module gives them named slots in a single ABI primitive that
//! mirrors POSIX `clock_gettime(clockid_t, ...)`.
//!
//! | clock                    | source             | jumps?            | use for                                |
//! | ------------------------ | ------------------ | ----------------- | -------------------------------------- |
//! | [`ClockId::Monotonic`]   | `performance.now`  | never             | elapsed time, timeouts, animations     |
//! | [`ClockId::RealTime`]    | `Date.now`         | NTP, DST, manual  | "created at" timestamps, log entries   |
//!
//! There is also a third reading of "monotonic time" that lives
//! outside this module: [`crate::activity::Context::timestamp_ms`]
//! is a frame-stable monotonic value that the framework hands you
//! as a callback argument. It's the same source as
//! `clock_gettime_ms(Monotonic)` but **frozen for the duration of
//! the frame** — every `on_render` in the same frame sees an
//! identical value, which is what you want for animation curves.
//! `clock_gettime_ms(Monotonic)` is call-site-fresh: it advances
//! between two reads inside the same `on_render`. Pick the one that
//! matches your intent.
//!
//! ```ignore
//! use akiva_sdk::clock::{clock_gettime_ms, ClockId};
//!
//! struct Note {
//!     text: String,
//!     created_ms: u64,    // wall clock — survives boots
//! }
//!
//! impl Note {
//!     fn new(text: String) -> Self {
//!         Note { text, created_ms: clock_gettime_ms(ClockId::RealTime) }
//!     }
//! }
//!
//! // Elapsed-time measurement (e.g. detecting a long-press).
//! let started = clock_gettime_ms(ClockId::Monotonic);
//! // ... time passes ...
//! let elapsed = clock_gettime_ms(ClockId::Monotonic) - started;
//! ```
//!
//! Ungated — clocks are primitives, not resources. No app has ever
//! asked the user for permission to know what time it is.

/// Which clock to query. `#[repr(u32)]` so `id as u32` is the wire
/// representation directly — no conversion table to maintain.
/// Mirrors POSIX `clockid_t` constants (`CLOCK_MONOTONIC`,
/// `CLOCK_REALTIME`).
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClockId {
    /// Monotonic time since an unspecified epoch (process start in
    /// practice). Never jumps backward. Backed by
    /// `performance.now()` on the host. Use for elapsed-time
    /// measurement, timeouts, and animation timing.
    Monotonic = 0,
    /// Wall-clock time in milliseconds since the UNIX epoch
    /// (1970-01-01 UTC). May jump due to NTP corrections, DST,
    /// or manual user clock changes. Backed by `Date.now()`.
    /// Use for user-visible "created at" timestamps, log entries,
    /// and anything that must agree with the human calendar.
    RealTime = 1,
}

/// Read the current value of `id` as milliseconds. The single
/// extern keeps the ABI extensible — future clock types
/// (`ProcessCputimeId`, `ThreadCputimeId`, …) can land without a
/// fresh import or an ABI bump.
#[cfg(target_arch = "wasm32")]
pub fn clock_gettime_ms(id: ClockId) -> u64 {
    // SAFETY: the host returns a u64, no allocations, no aliasing.
    unsafe { crate::runtime::akiva_host_clock_ms(id as u32) }
}

/// Non-wasm fallback so unit tests on the developer machine see a
/// reasonable value. `Monotonic` returns ms since some unspecified
/// instant (we use `Instant::now()` minus a fixed anchor); `RealTime`
/// returns ms since UNIX epoch via `SystemTime::now()`.
#[cfg(not(target_arch = "wasm32"))]
pub fn clock_gettime_ms(id: ClockId) -> u64 {
    use std::sync::OnceLock;
    use std::time::{Instant, SystemTime, UNIX_EPOCH};
    match id {
        ClockId::Monotonic => {
            static ANCHOR: OnceLock<Instant> = OnceLock::new();
            let anchor = ANCHOR.get_or_init(Instant::now);
            anchor.elapsed().as_millis() as u64
        }
        ClockId::RealTime => SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock is before UNIX epoch")
            .as_millis() as u64,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn realtime_returns_a_value_after_2026() {
        // 2026-01-01 UTC = 1_767_225_600_000 ms. Anything earlier
        // means the test machine clock is wildly off.
        let now = clock_gettime_ms(ClockId::RealTime);
        assert!(now >= 1_767_225_600_000, "RealTime returned {now} (before 2026)");
    }

    #[test]
    fn realtime_is_non_decreasing_across_back_to_back_reads() {
        // Wall clock can step backwards on NTP/DST, but not in a
        // microsecond window. `>=` rather than `>` because both
        // reads can land in the same millisecond bucket.
        let a = clock_gettime_ms(ClockId::RealTime);
        let b = clock_gettime_ms(ClockId::RealTime);
        assert!(b >= a, "RealTime went backwards: {a} -> {b}");
    }

    #[test]
    fn monotonic_starts_at_or_near_zero_and_advances() {
        let a = clock_gettime_ms(ClockId::Monotonic);
        // Spin briefly so the second read advances past the first.
        // On the non-wasm fallback the OnceLock anchor is set on the
        // first call so `a` is zero (or very small); `b` is whatever
        // the test machine took to run the spin.
        let mut spin = 0u64;
        for i in 0..100_000 { spin = spin.wrapping_add(i); }
        let b = clock_gettime_ms(ClockId::Monotonic);
        assert!(b >= a, "Monotonic went backwards: {a} -> {b}");
        // Touch `spin` so the loop isn't dead-code-eliminated under
        // optimisation.
        std::hint::black_box(spin);
    }

    #[test]
    fn clock_id_repr_matches_wire_format() {
        // The SDK passes `id as u32` to the host. Lock the values so
        // a future enum reorder fails this test instead of silently
        // miswiring the host.
        assert_eq!(ClockId::Monotonic as u32, 0);
        assert_eq!(ClockId::RealTime as u32, 1);
    }
}
