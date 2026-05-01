//! Package enumeration (Stone 11.0a).
//!
//! Apps that hold `akiva.permission.QUERY_PACKAGES` can call
//! [`list()`](self::list) to discover everything else installed on the
//! system. Used by the system Shell to populate its launcher; ordinary
//! apps almost never need this.
//!
//! Without the grant the host's deny-stub returns zero, which the
//! decoder reads as an empty list — the same shape an app sees on a
//! freshly-booted system with nothing installed. That ambiguity is by
//! design: the gate is the manifest, not a runtime branch.
//!
//! # Wire format
//!
//! Mirror of `framework::pkgs_wire`:
//!
//! ```text
//! u32 LE  count
//! repeat count times:
//!   u16 LE  id_len      ; id bytes (UTF-8)
//!   u16 LE  class_len   ; class bytes (UTF-8)
//!   u16 LE  name_len    ; name bytes (UTF-8)
//! ```

/// One installed package's launcher-relevant metadata. The `entry_class`
/// is the activity the system will launch by default (matches the
/// manifest's first activity).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageEntry {
    pub id: String,
    pub entry_class: String,
    pub name: String,
}

/// Enumerate installed packages. Returns an empty `Vec` when no
/// packages are installed, when the grant is missing (deny-stub path),
/// or when the host's blob is malformed (degrade silently — a Shell
/// can't usefully recover by panicking, and the wire format is owned
/// entirely by the framework on the other side of the ABI).
pub fn list() -> Vec<PackageEntry> {
    let bytes = read_blob();
    decode(&bytes).unwrap_or_default()
}

/// Two-phase buffer dance: poll once with a 1 KiB guess; if the host
/// reports a longer payload, realloc and poll again. The host's
/// FutureRegistry retains the entry across an undersized poll so the
/// retry sees the same data — single-shot eviction only happens once
/// the buffer fits. 1 KiB covers ~25 packages with average id+class+name
/// lengths of ~30 bytes each, which is well above any plausible MVP
/// install count.
///
/// Stone 11.3c-3 — migrated to the unified ABI. Args are empty (`[]`);
/// a non-zero `future_id` means "started", `0` means denied or unknown
/// op_kind. ERROR/PENDING from poll degrades to an empty list — same
/// shape an app sees on a freshly-booted system, matching the existing
/// "missing grant ⇒ empty list" contract.
#[cfg(target_arch = "wasm32")]
fn read_blob() -> Vec<u8> {
    use crate::op_kinds::{io, status, unpack_poll};
    use crate::runtime::{host_io_poll, host_io_start};

    let future_id = host_io_start(io::QUERY_PACKAGES, &[]);
    if future_id == 0 {
        // Denied (no grant), unknown op_kind, or malformed args. The
        // legacy deny-stub returned 0 here; we keep that shape so the
        // grant-less Shell behaves identically to before the migration.
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

/// Non-wasm builds (cargo test) link without a real host. Returning an
/// empty blob means `list()` silently yields `[]`, which is the same
/// observable behaviour as a freshly-booted system with no packages —
/// fine for SDK unit tests that don't care about discovery.
#[cfg(not(target_arch = "wasm32"))]
fn read_blob() -> Vec<u8> {
    Vec::new()
}

#[derive(Debug, PartialEq, Eq)]
enum DecodeError {
    Truncated,
    BadUtf8,
}

fn decode(bytes: &[u8]) -> Result<Vec<PackageEntry>, DecodeError> {
    if bytes.is_empty() {
        return Ok(Vec::new());
    }
    if bytes.len() < 4 {
        return Err(DecodeError::Truncated);
    }
    let count = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize;
    let mut cur = 4usize;
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        let id = take_str(bytes, &mut cur)?;
        let entry_class = take_str(bytes, &mut cur)?;
        let name = take_str(bytes, &mut cur)?;
        out.push(PackageEntry { id, entry_class, name });
    }
    Ok(out)
}

fn take_str(bytes: &[u8], cur: &mut usize) -> Result<String, DecodeError> {
    if *cur + 2 > bytes.len() {
        return Err(DecodeError::Truncated);
    }
    let len = u16::from_le_bytes([bytes[*cur], bytes[*cur + 1]]) as usize;
    *cur += 2;
    if *cur + len > bytes.len() {
        return Err(DecodeError::Truncated);
    }
    let s = core::str::from_utf8(&bytes[*cur..*cur + len])
        .map_err(|_| DecodeError::BadUtf8)?
        .to_string();
    *cur += len;
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a wire-format blob the same way the framework's
    /// `pkgs_wire::encode` does, so the SDK decoder is exercised against
    /// the canonical shape without taking a dependency on the framework
    /// crate. If this drifts from `framework/src/pkgs_wire.rs` the
    /// integration test in that crate fires.
    fn encode(pkgs: &[(&str, &str, &str)]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&(pkgs.len() as u32).to_le_bytes());
        for (id, class, name) in pkgs {
            for s in [id, class, name] {
                let b = s.as_bytes();
                out.extend_from_slice(&(b.len() as u16).to_le_bytes());
                out.extend_from_slice(b);
            }
        }
        out
    }

    #[test]
    fn empty_blob_decodes_to_empty_list() {
        assert_eq!(decode(&[]).unwrap(), vec![]);
    }

    #[test]
    fn zero_count_blob_decodes_to_empty_list() {
        assert_eq!(decode(&[0, 0, 0, 0]).unwrap(), vec![]);
    }

    #[test]
    fn three_entries_roundtrip() {
        let bytes = encode(&[
            ("com.akiva.hello", "MainActivity", "Hello"),
            ("com.akiva.calc",  "Calc",         "Calculator"),
            ("com.akiva.ttt",   "Game",         "Tic-Tac-Toe"),
        ]);
        let decoded = decode(&bytes).unwrap();
        assert_eq!(decoded.len(), 3);
        assert_eq!(decoded[0], PackageEntry {
            id: "com.akiva.hello".into(),
            entry_class: "MainActivity".into(),
            name: "Hello".into(),
        });
        assert_eq!(decoded[2].name, "Tic-Tac-Toe");
    }

    #[test]
    fn unicode_name_survives() {
        let bytes = encode(&[("x.y", "C", "שלום")]);
        assert_eq!(decode(&bytes).unwrap()[0].name, "שלום");
    }

    #[test]
    fn truncated_count_returns_truncated() {
        assert_eq!(decode(&[1, 2, 3]), Err(DecodeError::Truncated));
    }

    #[test]
    fn truncated_payload_returns_truncated() {
        // count=1, id_len=10, only 4 bytes follow.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&1u32.to_le_bytes());
        bytes.extend_from_slice(&10u16.to_le_bytes());
        bytes.extend_from_slice(b"abcd");
        assert_eq!(decode(&bytes), Err(DecodeError::Truncated));
    }

    #[test]
    fn bad_utf8_is_rejected() {
        // count=1, id_len=2, two invalid UTF-8 bytes.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&1u32.to_le_bytes());
        bytes.extend_from_slice(&2u16.to_le_bytes());
        bytes.push(0xff);
        bytes.push(0xfe);
        assert_eq!(decode(&bytes), Err(DecodeError::BadUtf8));
    }

    #[test]
    fn list_on_non_wasm_returns_empty() {
        // The host import isn't linked outside wasm32 — `list()` falls
        // back to an empty blob and decodes to an empty list. A test
        // that wants to inject a fake should pass bytes through
        // `decode` directly rather than calling `list`.
        assert_eq!(list(), vec![]);
    }
}
