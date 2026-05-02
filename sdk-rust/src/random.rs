//! Cryptographic random bytes (Stone 11.6.8 #1).
//!
//! `wasm` itself has no source of randomness. The host (a Web Worker
//! with `crypto.getRandomValues` available) does. This module exposes
//! that primitive to apps without going through the io / fire
//! channels — RNG is synchronous and infallible at the host's chosen
//! granularity, so a future would be ceremony.
//!
//! Use cases:
//!
//! - **UUIDs / session tokens** — generate v4 UUIDs, opaque session
//!   IDs, anti-CSRF nonces.
//! - **Crypto** — keys, IVs, salts. The host backing is
//!   `crypto.getRandomValues`, which is NIST SP 800-90A-compliant on
//!   every modern browser.
//! - **Game logic** — initial board state, deck shuffles. Use a
//!   seeded PRNG instead if you need reproducibility; this is for
//!   "I don't care, just give me bytes."
//!
//! Ungated — RNG is a primitive, not a resource. Same model as
//! `Math.random()` (without the bias problem).
//!
//! ```ignore
//! use akiva_sdk::random;
//!
//! let mut salt = [0u8; 16];
//! random::fill_bytes(&mut salt);
//!
//! let token = random::u64();
//! ```

/// Fill `buf` with cryptographic random bytes. Length is whatever the
/// caller chose; the host doesn't impose a maximum (chunks internally
/// for `crypto.getRandomValues`'s 64 KiB limit). Empty `buf` is a no-op.
#[cfg(target_arch = "wasm32")]
pub fn fill_bytes(buf: &mut [u8]) {
    if buf.is_empty() { return; }
    // SAFETY: the host writes exactly `len` bytes into the buffer,
    // bounded by the slice we just measured. Single-threaded wasm
    // means no concurrent reads can race with the fill.
    unsafe {
        crate::runtime::akiva_host_random_bytes(buf.as_mut_ptr(), buf.len() as u32);
    }
}

/// Non-wasm builds get a deterministic in-process counter. Tests can
/// rely on it being non-zero and varying across calls; security-
/// sensitive code obviously must not run outside wasm.
#[cfg(not(target_arch = "wasm32"))]
pub fn fill_bytes(buf: &mut [u8]) {
    use std::cell::Cell;
    thread_local! {
        static COUNTER: Cell<u64> = const { Cell::new(0xDEADBEEFCAFEBABE) };
    }
    COUNTER.with(|c| {
        let mut state = c.get();
        for slot in buf.iter_mut() {
            // xorshift step — varied, not random. For tests only.
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            *slot = (state & 0xFF) as u8;
        }
        c.set(state);
    });
}

/// Generate a random `u64` from cryptographic random bytes. Useful
/// for session IDs, hash-table seeds, etc.
pub fn u64() -> u64 {
    let mut buf = [0u8; 8];
    fill_bytes(&mut buf);
    core::primitive::u64::from_le_bytes(buf)
}

/// Generate a random `u32` from cryptographic random bytes.
pub fn u32() -> u32 {
    let mut buf = [0u8; 4];
    fill_bytes(&mut buf);
    core::primitive::u32::from_le_bytes(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fill_bytes_writes_non_zero_to_a_nonempty_buffer() {
        let mut buf = [0u8; 32];
        fill_bytes(&mut buf);
        // The non-wasm xorshift can't produce all-zero output for the
        // seed we use; the wasm path obviously can't either. Either
        // way, the test asserts the call left the buffer in a varied
        // state, not a literal "non-zero" guarantee on every byte.
        assert!(buf.iter().any(|&b| b != 0), "fill_bytes left buffer all-zero");
    }

    #[test]
    fn fill_bytes_empty_buffer_is_noop() {
        let mut buf: [u8; 0] = [];
        fill_bytes(&mut buf); // shouldn't panic
    }

    #[test]
    fn two_u64_calls_typically_differ() {
        // Probabilistic on the wasm host (1 in 2^64 of false negative);
        // deterministic-but-varying on the non-wasm xorshift stub.
        let a = u64();
        let b = u64();
        assert_ne!(a, b, "two consecutive u64() calls must differ");
    }

    #[test]
    fn u32_smoke() {
        let _ = u32();
    }
}
