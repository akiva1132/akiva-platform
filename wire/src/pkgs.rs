//! Wire format for the package-list mirror pushed to the wasm host
//! (Stone 11.0a, deduplicated in Stone 11.6.7 #2).
//!
//! The host snapshots `(id, class, name)` for every installed package
//! so apps holding `akiva.permission.QUERY_PACKAGES` can enumerate
//! them via the `akiva_host_query_packages` import without crossing
//! back into the kernel. Length-prefixed binary — not LF/TAB —
//! because app names are free-form UTF-8 and can contain newlines or
//! tabs.
//!
//! ## Wire layout
//!
//! ```text
//! u32 LE  count
//! repeat count times:
//!   u16 LE  id_len      ; id bytes (UTF-8)
//!   u16 LE  class_len   ; class bytes (UTF-8)
//!   u16 LE  name_len    ; name bytes (UTF-8)
//! ```
//!
//! Per-string cap is 64 KiB (u16 length). Manifests already cap
//! id+name well below that — the ceiling exists to keep one bad
//! upgrade from producing an unparseable blob without a crisp error.
//!
//! ## JS mirror
//!
//! The JS host's `app_registry.js::_warmupWorkers` reads this format
//! to populate per-worker `packagesBlob` snapshots. The JS code does
//! NOT depend on this crate (it's a separate language); a
//! `// SCHEMA: …` comment in `app_registry.js` points back here so
//! drift is visible at code-review time when the layout changes.

/// One installed package, in the order the framework iterates
/// `PackageManager::list()`. Borrowed at encode time so we don't
/// need an extra clone of the strings off `PackageRecord`.
#[derive(Debug)]
pub struct PackageRef<'a> {
    pub id: &'a str,
    pub class: &'a str,
    pub name: &'a str,
}

/// One decoded record. Owned strings — the decoder is expected to be
/// called on a freshly-copied buffer in the host plane, not on a
/// borrowed reference into kernel memory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageEntry {
    pub id: String,
    pub class: String,
    pub name: String,
}

#[derive(Debug, PartialEq, Eq)]
pub enum DecodeError {
    Truncated,
    BadUtf8,
}

/// Encode a snapshot of installed packages. Strings are truncated at
/// `u16::MAX` bytes — over-long names degrade gracefully rather than
/// producing an unparseable blob.
pub fn encode(packages: &[PackageRef<'_>]) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + packages.len() * 32);
    out.extend_from_slice(&(packages.len() as u32).to_le_bytes());
    for p in packages {
        push_str(&mut out, p.id);
        push_str(&mut out, p.class);
        push_str(&mut out, p.name);
    }
    out
}

fn push_str(out: &mut Vec<u8>, s: &str) {
    let bytes = s.as_bytes();
    let len = bytes.len().min(u16::MAX as usize);
    out.extend_from_slice(&(len as u16).to_le_bytes());
    out.extend_from_slice(&bytes[..len]);
}

/// Decode a snapshot produced by [`encode`]. An empty input is the
/// freshly-booted-system shape — yields `Ok(empty)` rather than an
/// error. Anything past 4 bytes that doesn't parse cleanly returns
/// `DecodeError::{Truncated, BadUtf8}`.
pub fn decode(bytes: &[u8]) -> Result<Vec<PackageEntry>, DecodeError> {
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
        let class = take_str(bytes, &mut cur)?;
        let name = take_str(bytes, &mut cur)?;
        out.push(PackageEntry { id, class, name });
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

    #[test]
    fn empty_list_roundtrips() {
        let bytes = encode(&[]);
        assert_eq!(bytes, vec![0, 0, 0, 0]);
        assert_eq!(decode(&bytes).unwrap(), Vec::<PackageEntry>::new());
    }

    #[test]
    fn empty_payload_decodes_as_empty() {
        // A freshly-booted system with no installed packages returns a
        // zero-length blob, NOT a 4-byte zero-count blob. Both must
        // decode to the empty vec.
        assert_eq!(decode(&[]).unwrap(), Vec::<PackageEntry>::new());
    }

    #[test]
    fn three_entries_roundtrip() {
        let pkgs = vec![
            PackageRef { id: "com.akiva.hello", class: "MainActivity", name: "Hello" },
            PackageRef { id: "com.akiva.calc", class: "Calc", name: "Calculator" },
            PackageRef { id: "com.akiva.ttt", class: "Game", name: "Tic-Tac-Toe" },
        ];
        let bytes = encode(&pkgs);
        let decoded = decode(&bytes).unwrap();
        assert_eq!(decoded.len(), 3);
        assert_eq!(decoded[0].id, "com.akiva.hello");
        assert_eq!(decoded[1].name, "Calculator");
        assert_eq!(decoded[2].class, "Game");
    }

    #[test]
    fn unicode_name_survives_roundtrip() {
        // App names are user-facing and free-form. Length-prefixed
        // (vs. LF/TAB) is what lets names safely contain separator
        // bytes inside them.
        let pkgs = vec![
            PackageRef { id: "x.y", class: "C", name: "שלום\tworld\n!" },
        ];
        let decoded = decode(&encode(&pkgs)).unwrap();
        assert_eq!(decoded[0].name, "שלום\tworld\n!");
    }

    #[test]
    fn truncated_count_header_is_an_error() {
        assert_eq!(decode(&[1, 2, 3]), Err(DecodeError::Truncated));
    }

    #[test]
    fn truncated_string_payload_is_an_error() {
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
    fn over_long_string_is_truncated_to_u16_max() {
        // The push_str fast path silently truncates beyond u16::MAX.
        // Regression-pin: reading the encoded bytes back recovers the
        // truncated prefix, not an error.
        let big = "x".repeat(70_000);  // > u16::MAX = 65_535
        let pkgs = vec![PackageRef { id: &big, class: "c", name: "n" }];
        let decoded = decode(&encode(&pkgs)).unwrap();
        assert_eq!(decoded[0].id.len(), u16::MAX as usize);
    }
}
