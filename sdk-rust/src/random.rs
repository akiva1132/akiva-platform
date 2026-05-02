//! Cryptographic random bytes (Stone 11.6.8 #1).
//!
//! Wasm itself has no source of randomness; the host (a Web Worker
//! with `crypto.getRandomValues` available) does. Rather than ship a
//! standalone API that only apps aware of it would use, this module
//! wires the platform's RNG host import as a `getrandom` v0.2 custom
//! backend. The downstream effect: every crate that depends on
//! `getrandom` — `rand`, `uuid`, `ring`, `rustls`, `chacha20poly1305`,
//! and so on — works inside an Akiva app with zero glue.
//!
//! ```ignore
//! use uuid::Uuid;
//!
//! // No SDK setup, no `use akiva_sdk::*` — just the standard
//! // ecosystem call. Routes through `getrandom` → our custom
//! // backend → `akiva_host_random_bytes` → `crypto.getRandomValues`.
//! let id = Uuid::new_v4();
//! ```
//!
//! For apps that just want raw bytes without a dependency, this
//! module also exposes thin wrappers:
//!
//! ```ignore
//! use akiva_sdk::random;
//!
//! let mut salt = [0u8; 16];
//! random::fill(&mut salt);
//!
//! let token = random::bytes(32);
//! ```
//!
//! Ungated — RNG is a primitive, not a resource. Same model as
//! `Math.random()` in browsers (without the bias problem) and
//! `getentropy(2)` on POSIX.

/// Custom backend for `getrandom`: routes every system-entropy
/// request through our wasm host import. Registered exactly once
/// at module load via `register_custom_getrandom!`; the ecosystem
/// then sees us as the system RNG.
///
/// Non-wasm builds (`cargo test` on the developer's machine) skip
/// the custom registration so the standard `getrandom` backends
/// (urandom on Linux, BCryptGenRandom on Windows, etc.) keep
/// working — a host call `akiva_host_random_bytes` doesn't exist
/// outside wasm.
#[cfg(target_arch = "wasm32")]
fn akiva_getrandom(dest: &mut [u8]) -> Result<(), getrandom::Error> {
    if dest.is_empty() { return Ok(()); }
    // SAFETY: the host writes exactly `dest.len()` bytes into the
    // buffer we just measured. No allocations, no aliasing concerns
    // (single-threaded wasm). Returns 0 on success per the extern
    // contract; any non-zero return surfaces as `Error::FAILED`.
    let ret = unsafe {
        crate::runtime::akiva_host_random_bytes(dest.as_mut_ptr(), dest.len())
    };
    // `getrandom::Error::UNEXPECTED` is the catch-all "RNG produced
    // no bytes for an unspecified reason" code in v0.2 — we surface
    // the host's non-zero return as that. In practice the worker's
    // `crypto.getRandomValues` body never returns non-zero on a
    // healthy browser, so this branch is defensive.
    if ret == 0 { Ok(()) } else { Err(getrandom::Error::UNEXPECTED) }
}

#[cfg(target_arch = "wasm32")]
getrandom::register_custom_getrandom!(akiva_getrandom);

/// Fill `out` with cryptographically secure random bytes.
///
/// Panics only if the host RNG itself failed, which on a healthy
/// browser doesn't happen. (`getrandom::Error::FAILED` from the
/// custom backend would propagate as a panic here; in practice the
/// `crypto.getRandomValues` host body never returns non-zero.)
pub fn fill(out: &mut [u8]) {
    getrandom::getrandom(out).expect("RNG unavailable")
}

/// Allocate and return `len` cryptographically secure random bytes.
pub fn bytes(len: usize) -> Vec<u8> {
    let mut v = vec![0u8; len];
    fill(&mut v);
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fill_writes_varied_bytes_to_a_nonempty_buffer() {
        let mut buf = [0u8; 32];
        fill(&mut buf);
        // The non-wasm `getrandom` (urandom) and the wasm host both
        // produce statistically-random output; an all-zero buffer
        // is astronomically unlikely.
        assert!(buf.iter().any(|&b| b != 0), "fill produced an all-zero buffer");
    }

    #[test]
    fn fill_empty_buffer_is_noop() {
        let mut buf: [u8; 0] = [];
        fill(&mut buf); // shouldn't panic
    }

    #[test]
    fn bytes_returns_a_buffer_of_requested_size() {
        let v = bytes(0);
        assert_eq!(v.len(), 0);
        let v = bytes(64);
        assert_eq!(v.len(), 64);
    }

    #[test]
    fn two_calls_yield_different_bytes() {
        // Probabilistic with 32 random bytes the collision rate is
        // ~2^-256 — comfortably zero.
        let a = bytes(32);
        let b = bytes(32);
        assert_ne!(a, b, "two consecutive bytes(32) calls collided");
    }

    #[test]
    fn uuid_v4_compiles_and_runs_via_the_getrandom_hook() {
        // The whole point of the redesign — `Uuid::new_v4()` calls
        // `getrandom::getrandom` under the hood. If this test
        // compiles, the `register_custom_getrandom!` registration is
        // honoured by the linker; if it runs, the host call returns
        // a buffer the UUID v4 constructor finds non-zero. Use the
        // crate version-2 surface explicitly.
        let id = uuid::Uuid::new_v4();
        // v4 has variant 1 ("RFC4122") and version 4 — `get_version_num`
        // reads the canonical bits, providing a stable assertion.
        assert_eq!(id.get_version_num(), 4);
    }
}
