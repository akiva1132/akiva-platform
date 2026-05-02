//! Package enumeration (Stone 11.0a).
//!
//! Apps that hold `akiva.permission.QUERY_PACKAGES` can call
//! [`list()`](self::list) to discover everything else installed on
//! the system. Used by the system Shell to populate its launcher;
//! ordinary apps almost never need this.
//!
//! Without the grant the host's deny-stub returns zero, which the
//! decoder reads as an empty list — the same shape an app sees on a
//! freshly-booted system with nothing installed. That ambiguity is
//! by design: the gate is the manifest, not a runtime branch.
//!
//! Stone 11.6.7 #2 — the codec lives in the shared `akiva-wire`
//! crate. This module re-exports `PackageEntry` from there and keeps
//! only the `read_blob` host-import dance + the public `list()`.

pub use akiva_wire::pkgs::PackageEntry;
use akiva_wire::pkgs::decode;

/// Enumerate installed packages. Returns an empty `Vec` when no
/// packages are installed, when the grant is missing (deny-stub
/// path), or when the host's blob is malformed (degrade silently —
/// a Shell can't usefully recover by panicking, and the wire format
/// is owned entirely by the framework on the other side of the ABI).
pub fn list() -> Vec<PackageEntry> {
    let bytes = read_blob();
    decode(&bytes).unwrap_or_default()
}

/// Two-phase buffer dance: poll once with a 1 KiB guess; if the host
/// reports a longer payload, realloc and poll again. The host's
/// FutureRegistry retains the entry across an undersized poll so the
/// retry sees the same data — single-shot eviction only happens once
/// the buffer fits. 1 KiB covers ~25 packages with average
/// id+class+name lengths of ~30 bytes each, well above any plausible
/// MVP install count.
#[cfg(target_arch = "wasm32")]
fn read_blob() -> Vec<u8> {
    use crate::op_kinds::{io, status, unpack_poll};
    use crate::runtime::{host_io_poll, host_io_start};

    let future_id = host_io_start(io::QUERY_PACKAGES, &[]);
    if future_id == 0 {
        // Denied (no grant), unknown op_kind, or malformed args.
        return Vec::new();
    }
    let mut buf = vec![0u8; 1024];
    let packed = host_io_poll(future_id, &mut buf);
    let (st, len) = unpack_poll(packed);
    if st != status::READY {
        return Vec::new();
    }
    let len = len as usize;
    if len == 0 {
        return Vec::new();
    }
    if len > buf.len() {
        // Host retained the entry; retry with the right-sized buffer.
        buf = vec![0u8; len];
        let packed = host_io_poll(future_id, &mut buf);
        let (st2, len2) = unpack_poll(packed);
        if st2 != status::READY {
            return Vec::new();
        }
        buf.truncate((len2 as usize).min(buf.len()));
    } else {
        buf.truncate(len);
    }
    buf
}

/// Non-wasm builds (cargo test) link without a real host. Returning
/// an empty blob means `list()` silently yields `[]`, which is the
/// same observable behaviour as a freshly-booted system with no
/// packages — fine for SDK unit tests that don't care about
/// discovery.
#[cfg(not(target_arch = "wasm32"))]
fn read_blob() -> Vec<u8> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_on_non_wasm_returns_empty() {
        // The host import isn't linked outside wasm32 — `list()`
        // falls back to an empty blob and decodes to an empty list.
        // A test that wants to inject a fake should pass bytes
        // through `akiva_wire::pkgs::decode` directly rather than
        // going via `list()`.
        assert_eq!(list(), vec![]);
    }

    #[test]
    fn package_entry_is_re_exported_from_wire() {
        // Pin the public surface: PackageEntry is the wire crate's
        // type, accessible via `akiva_sdk::pkgs::PackageEntry` for
        // app code that doesn't care where it came from.
        let e = PackageEntry {
            id: "com.akiva.hello".to_string(),
            class: "MainActivity".to_string(),
            name: "Hello".to_string(),
        };
        assert_eq!(e.id, "com.akiva.hello");
        assert_eq!(e.name, "Hello");
    }
}
