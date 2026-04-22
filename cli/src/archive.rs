//! `.akv` archive format (AKPK v1).
//!
//! The wire format and rationale live in [`docs/archive-format.md`]. This
//! module is stdlib-only by design — the CLI doesn't pull in `zip` just to
//! bundle three files per app. If compression or random access ever
//! matter, we bump the version byte and switch the implementation; the
//! callers (`akiva package`, `akiva install`) wouldn't change.
//!
//! Shape:
//!   magic "AKPK" | version u8 | count u32 | entries*
//!   entry: path_len u16 | path UTF-8 | data_len u32 | data bytes
//!
//! All integers little-endian. Paths use `/` separators and are validated
//! against traversal on decode — an `.akv` archive cannot name a file
//! outside its own tree.

// `encode` + `add` + `new` are used by `akiva package` (Stone 8.2); the
// reader half (`decode`, `entries`, accessors) lands its first caller in
// Stone 8.3 (`akiva install`). Until then keep them public without the
// warning noise rather than pollute the API with `pub(crate)` just to
// suppress a transient signal.
#![allow(dead_code)]

use std::fmt;

/// Magic bytes at offset 0. Chosen to not collide with ZIP (`PK\x03\x04`),
/// gzip (`1F 8B`), or wasm (`\0asm`). See [`docs/archive-format.md §11`].
pub const MAGIC: [u8; 4] = *b"AKPK";

/// Current wire format version. Bump on any incompatible change.
pub const VERSION: u8 = 1;

/// Size of the fixed header: magic + version + count.
const HEADER_LEN: usize = 4 + 1 + 4;

/// Entries whose presence is required for a valid archive. `decode`
/// rejects archives missing any of these; `encode` also rejects, so a
/// caller can't accidentally publish an incomplete archive.
pub const REQUIRED_ENTRIES: &[&str] = &["manifest.toml", "app.wasm"];

/// One file inside the archive. Paths use `/` separators, are validated
/// by [`validate_path`] on both encode and decode, and are unique.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub path: String,
    pub data: Vec<u8>,
}

/// In-memory representation of an `.akv` archive. Built by
/// repeatedly calling [`add`][Self::add], then serialised via
/// [`encode`][Self::encode]; or decoded from bytes with
/// [`decode`][Self::decode].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Archive {
    entries: Vec<Entry>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ArchiveError {
    /// Header didn't start with `AKPK`.
    BadMagic,
    /// Version byte wasn't [`VERSION`].
    UnsupportedVersion(u8),
    /// A length header runs past the end of the buffer.
    Truncated,
    /// Bytes remain after the last entry — archive is corrupt.
    TrailingBytes,
    /// An entry's path wasn't valid UTF-8.
    NonUtf8Path,
    /// The path violates the rules in [`validate_path`] (empty, absolute,
    /// traversal, ...).
    InvalidPath(String),
    /// Two entries with the same path.
    DuplicatePath(String),
    /// A required entry (see [`REQUIRED_ENTRIES`]) is missing.
    MissingRequired(&'static str),
    /// Path length exceeds `u16::MAX`.
    PathTooLong(String),
    /// Data length exceeds `u32::MAX`.
    FileTooLarge(String),
}

impl fmt::Display for ArchiveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ArchiveError::BadMagic => write!(f, "not an Akiva archive (bad magic)"),
            ArchiveError::UnsupportedVersion(v) => {
                write!(f, "unsupported archive version: {v} (expected {VERSION})")
            }
            ArchiveError::Truncated => write!(f, "archive is truncated"),
            ArchiveError::TrailingBytes => write!(f, "archive has trailing garbage"),
            ArchiveError::NonUtf8Path => write!(f, "archive contains a non-UTF-8 path"),
            ArchiveError::InvalidPath(p) => write!(f, "invalid path in archive: {p:?}"),
            ArchiveError::DuplicatePath(p) => write!(f, "duplicate path in archive: {p:?}"),
            ArchiveError::MissingRequired(name) => {
                write!(f, "archive missing required entry: {name}")
            }
            ArchiveError::PathTooLong(p) => write!(f, "path too long ({} bytes): {p:?}", p.len()),
            ArchiveError::FileTooLarge(p) => {
                write!(f, "entry exceeds 4 GiB: {p:?}")
            }
        }
    }
}

impl std::error::Error for ArchiveError {}

impl Archive {
    pub fn new() -> Self { Self::default() }

    pub fn entries(&self) -> &[Entry] { &self.entries }

    pub fn get(&self, path: &str) -> Option<&Entry> {
        self.entries.iter().find(|e| e.path == path)
    }

    /// The `manifest.toml` bytes. Safe to call on any archive produced
    /// by [`encode`][Self::encode] or returned from [`decode`][Self::decode]
    /// — both guarantee this entry exists.
    pub fn manifest_toml(&self) -> &[u8] {
        &self.get("manifest.toml")
            .expect("validated: manifest.toml is required on every archive")
            .data
    }

    /// The `app.wasm` bytes. Same existence guarantee as
    /// [`manifest_toml`][Self::manifest_toml].
    pub fn app_wasm(&self) -> &[u8] {
        &self.get("app.wasm")
            .expect("validated: app.wasm is required on every archive")
            .data
    }

    /// Add an entry. Validates the path and rejects duplicates. The
    /// required-entry check happens in [`encode`][Self::encode] — an
    /// archive-under-construction is allowed to be incomplete.
    pub fn add(&mut self, path: impl Into<String>, data: Vec<u8>) -> Result<(), ArchiveError> {
        let path = path.into();
        validate_path(&path)?;
        if self.entries.iter().any(|e| e.path == path) {
            return Err(ArchiveError::DuplicatePath(path));
        }
        self.entries.push(Entry { path, data });
        Ok(())
    }

    /// Serialise to the AKPK wire format. Fails if a required entry
    /// is missing or if any individual entry violates a size cap.
    pub fn encode(&self) -> Result<Vec<u8>, ArchiveError> {
        for name in REQUIRED_ENTRIES {
            if self.get(name).is_none() {
                return Err(ArchiveError::MissingRequired(name));
            }
        }

        // Size hint: header + per-entry (2 + path + 4 + data). Off by a
        // bit is fine — just saves reallocs.
        let est: usize = HEADER_LEN
            + self.entries.iter().map(|e| 2 + e.path.len() + 4 + e.data.len()).sum::<usize>();
        let mut out = Vec::with_capacity(est);

        out.extend_from_slice(&MAGIC);
        out.push(VERSION);
        out.extend_from_slice(&(self.entries.len() as u32).to_le_bytes());

        for e in &self.entries {
            if e.path.len() > u16::MAX as usize {
                return Err(ArchiveError::PathTooLong(e.path.clone()));
            }
            if e.data.len() > u32::MAX as usize {
                return Err(ArchiveError::FileTooLarge(e.path.clone()));
            }
            out.extend_from_slice(&(e.path.len() as u16).to_le_bytes());
            out.extend_from_slice(e.path.as_bytes());
            out.extend_from_slice(&(e.data.len() as u32).to_le_bytes());
            out.extend_from_slice(&e.data);
        }

        Ok(out)
    }

    /// Parse an AKPK-format byte slice. Returns an error on any
    /// structural issue — callers that want partial recovery should
    /// build their own parser, this one bails at the first problem.
    pub fn decode(bytes: &[u8]) -> Result<Self, ArchiveError> {
        if bytes.len() < HEADER_LEN { return Err(ArchiveError::Truncated); }
        if bytes[0..4] != MAGIC { return Err(ArchiveError::BadMagic); }
        if bytes[4] != VERSION { return Err(ArchiveError::UnsupportedVersion(bytes[4])); }
        let count = u32::from_le_bytes([bytes[5], bytes[6], bytes[7], bytes[8]]) as usize;

        let mut pos = HEADER_LEN;
        let mut entries: Vec<Entry> = Vec::with_capacity(count.min(16 * 1024));

        for _ in 0..count {
            // path_len
            if pos + 2 > bytes.len() { return Err(ArchiveError::Truncated); }
            let plen = u16::from_le_bytes([bytes[pos], bytes[pos + 1]]) as usize;
            pos += 2;
            // path
            if pos + plen > bytes.len() { return Err(ArchiveError::Truncated); }
            let path = std::str::from_utf8(&bytes[pos..pos + plen])
                .map_err(|_| ArchiveError::NonUtf8Path)?
                .to_string();
            validate_path(&path)?;
            pos += plen;
            // data_len
            if pos + 4 > bytes.len() { return Err(ArchiveError::Truncated); }
            let dlen = u32::from_le_bytes([
                bytes[pos], bytes[pos + 1], bytes[pos + 2], bytes[pos + 3],
            ]) as usize;
            pos += 4;
            // data
            if pos + dlen > bytes.len() { return Err(ArchiveError::Truncated); }
            let data = bytes[pos..pos + dlen].to_vec();
            pos += dlen;

            if entries.iter().any(|e| e.path == path) {
                return Err(ArchiveError::DuplicatePath(path));
            }
            entries.push(Entry { path, data });
        }

        if pos != bytes.len() { return Err(ArchiveError::TrailingBytes); }

        for name in REQUIRED_ENTRIES {
            if !entries.iter().any(|e| e.path == *name) {
                return Err(ArchiveError::MissingRequired(name));
            }
        }

        Ok(Archive { entries })
    }
}

/// Path rules enforced on encode and decode. Keeps archive-safe paths —
/// no absolute paths, no `..`, no empty components, no backslashes or
/// NULs. Extracting with a naïve `Path::join` is still safe because of
/// this.
fn validate_path(path: &str) -> Result<(), ArchiveError> {
    if path.is_empty()
        || path.starts_with('/')
        || path.ends_with('/')
        || path.contains('\\')
        || path.contains('\0')
    {
        return Err(ArchiveError::InvalidPath(path.into()));
    }
    for comp in path.split('/') {
        if comp.is_empty() || comp == "." || comp == ".." {
            return Err(ArchiveError::InvalidPath(path.into()));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn minimal() -> Archive {
        let mut a = Archive::new();
        a.add("manifest.toml", b"id=\"x\"".to_vec()).unwrap();
        a.add("app.wasm", b"\0asm\x01\x00\x00\x00".to_vec()).unwrap();
        a
    }

    // ── Encode → decode roundtrip ─────────────────────────────────────

    #[test]
    fn roundtrip_minimal() {
        let a = minimal();
        let bytes = a.encode().unwrap();
        let b = Archive::decode(&bytes).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn roundtrip_with_resources() {
        let mut a = minimal();
        a.add("res/values/strings.toml", b"greet=\"hi\"".to_vec()).unwrap();
        a.add("res/values/colors.toml", br##"bg="#ff000000""##.to_vec()).unwrap();
        let bytes = a.encode().unwrap();
        let b = Archive::decode(&bytes).unwrap();
        assert_eq!(a.entries().len(), 4);
        assert_eq!(a, b);
    }

    #[test]
    fn accessors_return_required_entries() {
        let a = minimal();
        let bytes = a.encode().unwrap();
        let b = Archive::decode(&bytes).unwrap();
        assert_eq!(b.manifest_toml(), b"id=\"x\"");
        assert_eq!(b.app_wasm(), b"\0asm\x01\x00\x00\x00");
    }

    // ── Header + version guards ───────────────────────────────────────

    #[test]
    fn empty_buffer_is_truncated() {
        assert_eq!(Archive::decode(&[]), Err(ArchiveError::Truncated));
    }

    #[test]
    fn wrong_magic_rejected() {
        let mut bytes = minimal().encode().unwrap();
        bytes[0] = b'Z';
        assert_eq!(Archive::decode(&bytes), Err(ArchiveError::BadMagic));
    }

    #[test]
    fn unknown_version_rejected_without_crash() {
        let mut bytes = minimal().encode().unwrap();
        bytes[4] = 99;
        assert_eq!(Archive::decode(&bytes), Err(ArchiveError::UnsupportedVersion(99)));
    }

    // ── Length / framing guards ───────────────────────────────────────

    #[test]
    fn truncated_in_middle_of_entry() {
        let bytes = minimal().encode().unwrap();
        // Chop off the last few bytes — should land mid-data of app.wasm.
        let short = &bytes[..bytes.len() - 3];
        assert_eq!(Archive::decode(short), Err(ArchiveError::Truncated));
    }

    #[test]
    fn trailing_bytes_rejected() {
        let mut bytes = minimal().encode().unwrap();
        bytes.push(0xAB);
        bytes.push(0xCD);
        assert_eq!(Archive::decode(&bytes), Err(ArchiveError::TrailingBytes));
    }

    // ── Path validation ───────────────────────────────────────────────

    #[test]
    fn absolute_path_rejected_on_add() {
        let mut a = minimal();
        let err = a.add("/etc/passwd", vec![0]).unwrap_err();
        assert!(matches!(err, ArchiveError::InvalidPath(_)));
    }

    #[test]
    fn traversal_path_rejected_on_add() {
        let mut a = minimal();
        let err = a.add("res/../../etc/passwd", vec![0]).unwrap_err();
        assert!(matches!(err, ArchiveError::InvalidPath(_)));
    }

    #[test]
    fn empty_path_component_rejected() {
        let mut a = minimal();
        assert!(matches!(
            a.add("foo//bar", vec![0]).unwrap_err(),
            ArchiveError::InvalidPath(_),
        ));
    }

    #[test]
    fn backslash_rejected() {
        let mut a = minimal();
        assert!(matches!(
            a.add("res\\values\\x.toml", vec![0]).unwrap_err(),
            ArchiveError::InvalidPath(_),
        ));
    }

    #[test]
    fn nul_in_path_rejected() {
        let mut a = minimal();
        assert!(matches!(
            a.add("res/\0/x.toml", vec![0]).unwrap_err(),
            ArchiveError::InvalidPath(_),
        ));
    }

    #[test]
    fn trailing_slash_rejected() {
        let mut a = minimal();
        assert!(matches!(
            a.add("res/values/", vec![0]).unwrap_err(),
            ArchiveError::InvalidPath(_),
        ));
    }

    #[test]
    fn dotted_filenames_accepted() {
        // `.` and `..` are rejected as *components*. Files whose names
        // contain dots (hidden files, extension-rich names) must not
        // trip the traversal guard.
        let mut a = Archive::new();
        a.add("manifest.toml", vec![0]).unwrap();
        a.add("app.wasm", vec![0]).unwrap();
        a.add(".gitignore", vec![0]).unwrap();
        a.add("res/values/strings.v2.toml", vec![0]).unwrap();
        a.add("a.b.c.d", vec![0]).unwrap();
        assert_eq!(a.entries().len(), 5);
    }

    #[test]
    fn path_too_long_rejected_on_encode() {
        // `add` accepts paths up to any String length — the u16 cap is
        // a wire-format limit that `encode` enforces. A 65 537-byte path
        // exceeds u16::MAX (65 535) by two, so encode must reject.
        let mut a = Archive::new();
        a.add("manifest.toml", vec![0]).unwrap();
        a.add("app.wasm", vec![0]).unwrap();
        let huge = "a".repeat(u16::MAX as usize + 2);
        a.add(huge, vec![0]).unwrap();
        assert!(matches!(a.encode().unwrap_err(), ArchiveError::PathTooLong(_)));
    }

    #[test]
    fn decode_rejects_invalid_path_even_if_encoder_bypassed() {
        // Hand-build bytes with a "../evil" path to prove the decoder
        // re-validates; we can't reach this through the Archive API.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&MAGIC);
        bytes.push(VERSION);
        bytes.extend_from_slice(&1u32.to_le_bytes());
        let bad_path = b"../evil";
        bytes.extend_from_slice(&(bad_path.len() as u16).to_le_bytes());
        bytes.extend_from_slice(bad_path);
        bytes.extend_from_slice(&0u32.to_le_bytes());
        assert!(matches!(
            Archive::decode(&bytes).unwrap_err(),
            ArchiveError::InvalidPath(_),
        ));
    }

    // ── Required entries ──────────────────────────────────────────────

    #[test]
    fn encode_rejects_missing_manifest() {
        let mut a = Archive::new();
        a.add("app.wasm", vec![0]).unwrap();
        assert_eq!(a.encode(), Err(ArchiveError::MissingRequired("manifest.toml")));
    }

    #[test]
    fn encode_rejects_missing_wasm() {
        let mut a = Archive::new();
        a.add("manifest.toml", vec![0]).unwrap();
        assert_eq!(a.encode(), Err(ArchiveError::MissingRequired("app.wasm")));
    }

    // ── Duplicate paths ───────────────────────────────────────────────

    #[test]
    fn add_rejects_duplicate_path() {
        let mut a = minimal();
        let err = a.add("manifest.toml", vec![1]).unwrap_err();
        assert!(matches!(err, ArchiveError::DuplicatePath(_)));
    }

    #[test]
    fn decode_rejects_duplicate_path_in_stream() {
        // Hand-build an archive with two `manifest.toml` entries.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&MAGIC);
        bytes.push(VERSION);
        bytes.extend_from_slice(&2u32.to_le_bytes());
        for _ in 0..2 {
            let p = b"manifest.toml";
            bytes.extend_from_slice(&(p.len() as u16).to_le_bytes());
            bytes.extend_from_slice(p);
            bytes.extend_from_slice(&1u32.to_le_bytes());
            bytes.push(b'x');
        }
        assert!(matches!(
            Archive::decode(&bytes).unwrap_err(),
            ArchiveError::DuplicatePath(_),
        ));
    }

    // ── UTF-8 ─────────────────────────────────────────────────────────

    #[test]
    fn decode_rejects_non_utf8_path() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&MAGIC);
        bytes.push(VERSION);
        bytes.extend_from_slice(&1u32.to_le_bytes());
        let bad = [0xff, 0xfe, 0xfd];
        bytes.extend_from_slice(&(bad.len() as u16).to_le_bytes());
        bytes.extend_from_slice(&bad);
        bytes.extend_from_slice(&0u32.to_le_bytes());
        assert_eq!(Archive::decode(&bytes), Err(ArchiveError::NonUtf8Path));
    }

    // ── Large data roundtrip ──────────────────────────────────────────

    #[test]
    fn roundtrip_preserves_binary_wasm() {
        // Use a non-trivial wasm-shaped payload to catch any sneaky
        // text-mode corruption (unlikely on Rust, but cheap insurance).
        let wasm: Vec<u8> = (0u8..=255).cycle().take(10_000).collect();
        let mut a = Archive::new();
        a.add("manifest.toml", b"id=\"x\"".to_vec()).unwrap();
        a.add("app.wasm", wasm.clone()).unwrap();
        let bytes = a.encode().unwrap();
        let b = Archive::decode(&bytes).unwrap();
        assert_eq!(b.app_wasm(), wasm.as_slice());
    }
}
