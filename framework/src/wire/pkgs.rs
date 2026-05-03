//! Wire format for the package-list mirror pushed to the wasm host
//! (Stone 11.0a).
//!
//! Stone 11.6.7 #2 — the codec proper (encode/decode + DecodeError +
//! `PackageRef` / `PackageEntry` types) lives in the
//! [`akiva_wire::pkgs`] crate. This module re-exports those names so
//! existing call sites (and in particular `framework::lib.rs::boot`,
//! which encodes a snapshot for `set_packages`) keep their import
//! shape. Adding a field to the wire format is now a one-place
//! change in `wire/src/pkgs.rs`; this file shouldn't grow.
//!
//! See [`akiva_wire::pkgs`] for the wire layout and rationale.

pub use akiva_wire::pkgs::{decode, encode, DecodeError, PackageEntry, PackageRef};

#[cfg(test)]
mod tests {
    use super::*;

    /// Stone 11.6.7 #2 — sanity check that the wrapper still surfaces
    /// the wire-crate types under the framework's old import path. The
    /// substantive coverage lives in `akiva-wire`'s own test module
    /// (10 tests including roundtrip, unicode, truncation, oversized
    /// strings); this test catches accidental re-export breakage.
    #[test]
    fn re_exports_compile_and_roundtrip() {
        let bytes = encode(&[PackageRef {
            id: "x.y", class: "C", name: "n", icon: "ic",
        }]);
        let decoded = decode(&bytes).unwrap();
        assert_eq!(decoded, vec![PackageEntry {
            id: "x.y".to_string(),
            class: "C".to_string(),
            name: "n".to_string(),
            icon: "ic".to_string(),
        }]);
        assert_eq!(decode(&[1, 2, 3]), Err(DecodeError::Truncated));
    }
}
