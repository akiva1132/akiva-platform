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

/// Two-phase buffer dance: ask once with a 1 KiB guess, retry with the
/// reported size if it didn't fit. 1 KiB covers ~25 packages with
/// average id+class+name lengths of ~30 bytes each, which is well above
/// any plausible MVP install count.
#[cfg(target_arch = "wasm32")]
fn read_blob() -> Vec<u8> {
    use crate::runtime::akiva_host_query_packages;

    let mut buf = vec![0u8; 1024];
    let needed = unsafe {
        akiva_host_query_packages(buf.as_mut_ptr(), buf.len() as u32)
    } as usize;
    if needed == 0 {
        return Vec::new();
    }
    if needed > buf.len() {
        buf = vec![0u8; needed];
        let got = unsafe {
            akiva_host_query_packages(buf.as_mut_ptr(), buf.len() as u32)
        } as usize;
        buf.truncate(got.min(buf.len()));
    } else {
        buf.truncate(needed);
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
