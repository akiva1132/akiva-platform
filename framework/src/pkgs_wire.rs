//! Wire format for the package-list mirror pushed to the wasm host
//! (Stone 11.0a).
//!
//! The host snapshots `(id, class, name)` for every installed package so
//! apps holding `akiva.permission.QUERY_PACKAGES` can enumerate them via
//! the `akiva_host_query_packages` import without crossing back into the
//! kernel. Length-prefixed binary — not LF/TAB — because app names are
//! free-form UTF-8 and can contain newlines or tabs.
//!
//! Wire layout:
//!
//! ```text
//! u32 LE  count
//! repeat count times:
//!   u16 LE  id_len      ; id bytes (UTF-8)
//!   u16 LE  class_len   ; class bytes (UTF-8)
//!   u16 LE  name_len    ; name bytes (UTF-8)
//! ```
//!
//! The SDK mirrors this decoder in `sdk-rust/src/pkgs.rs`. Both sides must
//! stay byte-for-byte in sync; a roundtrip test in each crate pins the
//! shape.
//!
//! Per-string cap is 64 KiB (u16 length). Manifests already cap id+name
//! well below that — the ceiling exists to keep one bad upgrade from
//! producing an unparseable blob without a crisp error.

/// One installed package, in the order the framework iterates
/// `PackageManager::list()`. Borrowed at encode time so we don't need an
/// extra clone of the strings off `PackageRecord`.
#[derive(Debug)]
pub struct PackageRef<'a> {
    pub id: &'a str,
    pub class: &'a str,
    pub name: &'a str,
}

/// Encode a snapshot of the installed packages. `count` is capped at
/// `u32::MAX` (effectively unbounded for our purposes); each string is
/// truncated at `u16::MAX` bytes — over-long names degrade gracefully
/// rather than producing an unparseable blob.
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

/// Decoder kept close to the encoder for the framework's own roundtrip
/// test. Production decoding happens in the SDK (apps), not the
/// framework — but pinning the shape here means the encoder can't drift
/// without the test catching it.
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
        // Mirrors the host contract: an app calling query_packages on a
        // freshly-booted system with no installed packages gets a zero-
        // length blob, not a 4-byte zero-count blob. Both must decode to
        // the empty vec — we own the receiving end via the SDK decoder.
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
        // App names are user-facing and free-form. The whole reason we
        // chose length-prefixed over LF/TAB is to not corrupt names with
        // separator bytes inside them.
        let pkgs = vec![
            PackageRef { id: "x.y", class: "C", name: "שלום\tworld\n!" },
        ];
        let decoded = decode(&encode(&pkgs)).unwrap();
        assert_eq!(decoded[0].name, "שלום\tworld\n!");
    }

    #[test]
    fn truncated_count_header() {
        assert_eq!(decode(&[1, 2, 3]), Err(DecodeError::Truncated));
    }

    #[test]
    fn truncated_string_payload() {
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
}
