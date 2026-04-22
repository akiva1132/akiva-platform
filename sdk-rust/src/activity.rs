//! The `Activity` trait app authors implement.
//!
//! An activity is one screen worth of UI + interaction. Over the course
//! of its life the runtime drives it through lifecycle callbacks
//! (`on_create` → `on_resume` → `on_pause` → ...) interleaved with input
//! events and render requests. All callbacks have default no-op impls so
//! a minimal app overrides only what it cares about — typically just
//! `on_render`.
//!
//! The trait is deliberately flat: no sub-traits, no associated types.
//! More structure (widget-tree composition, retained-mode UI, background
//! tasks) is a library concern that can be built on top without touching
//! the ABI.
//!
//! See [docs/activity-model.md] for the state-machine rules and [docs/abi.md §2.2]
//! for how each callback maps to a raw wasm export.

use std::collections::HashMap;

use crate::canvas::Canvas;

// ── Public data types ─────────────────────────────────────────────────

/// Surface the activity is painting onto. Filled in by the runtime before
/// every callback; activities read it from `Context::metrics`.
#[derive(Debug, Default, Clone, Copy)]
pub struct Metrics {
    pub width_px: u32,
    pub height_px: u32,
    /// Device pixel ratio. `1.0` on a 160dpi reference surface.
    pub density: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PointerPhase { Down, Move, Up, Cancel }

#[derive(Debug, Clone, Copy)]
pub struct PointerEvent {
    pub x: f32,
    pub y: f32,
    pub button: u8,
    pub phase: PointerPhase,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyPhase { Down, Repeat, Up }

#[derive(Debug, Clone, Copy)]
pub struct KeyEvent {
    pub code: u32,
    pub modifiers: u8,
    pub phase: KeyPhase,
}

/// Per-callback context the runtime constructs fresh each tick. Carries
/// display metrics, a frame timestamp, and (when relevant) the most
/// recent pointer state — so callbacks that need to look at the pointer
/// outside `on_pointer` (e.g. during render) don't need to thread state.
///
/// `class` is the activity class name the host registered this handle
/// under (the string from the manifest's `[[activities]].class`). Useful
/// when one Rust type backs multiple declared activity classes.
pub struct Context<'a> {
    pub metrics: Metrics,
    pub timestamp_ms: f64,
    pub pointer: Option<PointerEvent>,
    pub class: &'a str,
    /// Handle the runtime passed to the current callback. Kept
    /// crate-private because it's only useful for host imports that need
    /// to name the activity (see [`Context::finish`]); apps shouldn't
    /// have to thread it around or compare two handles themselves.
    pub(crate) handle: u32,
}

impl<'a> Context<'a> {
    /// Ask the platform to tear this activity down. Fires after the
    /// current callback returns — the framework runs
    /// `on_pause → on_stop → on_destroy` in order (or, if called from
    /// inside `on_create`, jumps straight to `on_destroy` via the
    /// `Created → Destroyed` shortcut documented in
    /// [docs/activity-model.md §2](../../docs/activity-model.md)).
    ///
    /// No-op on non-wasm targets so unit tests that build with the SDK
    /// can call it without a host.
    pub fn finish(&self) {
        crate::runtime::host_finish(self.handle);
    }
}

// ── The trait ─────────────────────────────────────────────────────────

pub trait Activity: 'static {
    // ── Lifecycle (docs/activity-model.md §2) ──────────────────────────
    fn on_create(&mut self, ctx: &mut Context<'_>)  { let _ = ctx; }
    fn on_start(&mut self, ctx: &mut Context<'_>)   { let _ = ctx; }
    fn on_resume(&mut self, ctx: &mut Context<'_>)  { let _ = ctx; }
    fn on_pause(&mut self, ctx: &mut Context<'_>)   { let _ = ctx; }
    fn on_stop(&mut self, ctx: &mut Context<'_>)    { let _ = ctx; }
    fn on_destroy(&mut self, ctx: &mut Context<'_>) { let _ = ctx; }

    // ── Input ───────────────────────────────────────────────────────────
    fn on_pointer(&mut self, ctx: &mut Context<'_>, ev: PointerEvent) {
        let _ = (ctx, ev);
    }
    fn on_key(&mut self, ctx: &mut Context<'_>, ev: KeyEvent) {
        let _ = (ctx, ev);
    }
    fn on_resize(&mut self, ctx: &mut Context<'_>, metrics: Metrics) {
        let _ = (ctx, metrics);
    }

    // ── Render ──────────────────────────────────────────────────────────
    /// Paint one frame. Called at the runtime's frame rate (rAF, typically
    /// 60 Hz). Must be fast — all ops are serialised immediately.
    fn on_render(&mut self, ctx: &mut Context<'_>, canvas: &mut Canvas<'_>) {
        let _ = (ctx, canvas);
    }

    // ── Saved state — forward-compat stubs for Phase 3 ─────────────────
    /// UI-state snapshot called when the system is about to release the
    /// activity. Budget: 64 KiB — use `storage` for larger data. Not wired
    /// up in v0; kept so app code can start using it today and keep
    /// working when the runtime honours it.
    fn on_save_state(&self, out: &mut SavedState) { let _ = out; }
    fn on_restore_state(&mut self, saved: &SavedState) { let _ = saved; }
}

// ── Saved state ────────────────────────────────────────────────────────
//
// Small typed key/value bag an activity hands back across a process kill.
// Wire format (all little-endian):
//
//   u32  count
//   for each entry:
//       u32  key_len
//       <key_len> UTF-8 bytes
//       u8   tag      (see `tag` module below)
//       <payload per tag>
//
// Kept intentionally small — the framework treats the whole thing as an
// opaque blob and never inspects individual entries. Decoding happens on
// the other side of the ABI, inside the app's own wasm.

mod tag {
    pub const NULL:   u8 = 0;
    pub const BOOL:   u8 = 1;
    pub const INT:    u8 = 2;
    pub const FLOAT:  u8 = 3;
    pub const STRING: u8 = 4;
    pub const BYTES:  u8 = 5;
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct SavedState {
    pub entries: HashMap<String, SavedValue>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SavedValue {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    String(String),
    Bytes(Vec<u8>),
}

/// Errors that can come out of [`SavedState::decode`]. Exposed as an enum
/// so callers distinguish "stream truncated" (probably a host bug) from
/// "tag we don't recognise" (probably a version mismatch).
#[derive(Debug, PartialEq, Eq)]
pub enum SavedStateDecodeError {
    Truncated,
    BadUtf8,
    UnknownTag(u8),
    /// Encoded blob would exceed [`SavedState::MAX_BYTES`]; refused to
    /// avoid unbounded app memory growth on untrusted input.
    TooLarge,
}

impl SavedState {
    /// Budget the runtime honours. 64 KiB matches Android's pragmatic
    /// upper bound on `onSaveInstanceState` — large enough for UI state
    /// (scroll offsets, text buffers) and small enough to keep writes
    /// cheap. Apps that need more should use persistent storage instead.
    pub const MAX_BYTES: usize = 64 * 1024;

    // ── Typed putters ────────────────────────────────────────────────

    pub fn put_bool(&mut self, key: &str, v: bool) {
        self.entries.insert(key.into(), SavedValue::Bool(v));
    }
    pub fn put_int(&mut self, key: &str, v: i64) {
        self.entries.insert(key.into(), SavedValue::Int(v));
    }
    pub fn put_float(&mut self, key: &str, v: f64) {
        self.entries.insert(key.into(), SavedValue::Float(v));
    }
    pub fn put_string(&mut self, key: &str, v: impl Into<String>) {
        self.entries.insert(key.into(), SavedValue::String(v.into()));
    }
    pub fn put_bytes(&mut self, key: &str, v: impl Into<Vec<u8>>) {
        self.entries.insert(key.into(), SavedValue::Bytes(v.into()));
    }

    // ── Typed getters ────────────────────────────────────────────────

    pub fn get_bool(&self, key: &str) -> Option<bool> {
        match self.entries.get(key)? { SavedValue::Bool(v) => Some(*v), _ => None }
    }
    pub fn get_int(&self, key: &str) -> Option<i64> {
        match self.entries.get(key)? { SavedValue::Int(v) => Some(*v), _ => None }
    }
    pub fn get_float(&self, key: &str) -> Option<f64> {
        match self.entries.get(key)? { SavedValue::Float(v) => Some(*v), _ => None }
    }
    pub fn get_string(&self, key: &str) -> Option<&str> {
        match self.entries.get(key)? { SavedValue::String(v) => Some(v), _ => None }
    }
    pub fn get_bytes(&self, key: &str) -> Option<&[u8]> {
        match self.entries.get(key)? { SavedValue::Bytes(v) => Some(v), _ => None }
    }

    pub fn is_empty(&self) -> bool { self.entries.is_empty() }

    // ── Wire codec ───────────────────────────────────────────────────

    /// Serialise to the wire format documented above. Key iteration order
    /// is sorted so the output is deterministic — useful for tests, and
    /// means an app that writes the same logical state twice produces
    /// byte-identical blobs (friendlier to storage dedup).
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(32 + self.entries.len() * 16);
        out.extend_from_slice(&(self.entries.len() as u32).to_le_bytes());
        let mut keys: Vec<&String> = self.entries.keys().collect();
        keys.sort();
        for key in keys {
            let value = &self.entries[key];
            let bytes = key.as_bytes();
            out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
            out.extend_from_slice(bytes);
            match value {
                SavedValue::Null => out.push(tag::NULL),
                SavedValue::Bool(b) => {
                    out.push(tag::BOOL);
                    out.push(if *b { 1 } else { 0 });
                }
                SavedValue::Int(i) => {
                    out.push(tag::INT);
                    out.extend_from_slice(&i.to_le_bytes());
                }
                SavedValue::Float(f) => {
                    out.push(tag::FLOAT);
                    out.extend_from_slice(&f.to_le_bytes());
                }
                SavedValue::String(s) => {
                    out.push(tag::STRING);
                    let sb = s.as_bytes();
                    out.extend_from_slice(&(sb.len() as u32).to_le_bytes());
                    out.extend_from_slice(sb);
                }
                SavedValue::Bytes(b) => {
                    out.push(tag::BYTES);
                    out.extend_from_slice(&(b.len() as u32).to_le_bytes());
                    out.extend_from_slice(b);
                }
            }
        }
        out
    }

    /// Decode from the wire format. The inverse of [`encode`].
    pub fn decode(bytes: &[u8]) -> Result<Self, SavedStateDecodeError> {
        if bytes.len() > Self::MAX_BYTES {
            return Err(SavedStateDecodeError::TooLarge);
        }
        let mut r = Reader { buf: bytes };
        let count = r.read_u32()?;
        let mut entries = HashMap::with_capacity(count as usize);
        for _ in 0..count {
            let key_len = r.read_u32()? as usize;
            let key_bytes = r.read_bytes(key_len)?;
            let key = core::str::from_utf8(key_bytes)
                .map_err(|_| SavedStateDecodeError::BadUtf8)?
                .to_string();
            let t = r.read_u8()?;
            let value = match t {
                tag::NULL => SavedValue::Null,
                tag::BOOL => SavedValue::Bool(r.read_u8()? != 0),
                tag::INT => SavedValue::Int(i64::from_le_bytes(
                    r.read_array::<8>()?,
                )),
                tag::FLOAT => SavedValue::Float(f64::from_le_bytes(
                    r.read_array::<8>()?,
                )),
                tag::STRING => {
                    let n = r.read_u32()? as usize;
                    let sb = r.read_bytes(n)?;
                    SavedValue::String(
                        core::str::from_utf8(sb)
                            .map_err(|_| SavedStateDecodeError::BadUtf8)?
                            .to_string(),
                    )
                }
                tag::BYTES => {
                    let n = r.read_u32()? as usize;
                    SavedValue::Bytes(r.read_bytes(n)?.to_vec())
                }
                other => return Err(SavedStateDecodeError::UnknownTag(other)),
            };
            entries.insert(key, value);
        }
        Ok(SavedState { entries })
    }
}

struct Reader<'a> {
    buf: &'a [u8],
}

impl<'a> Reader<'a> {
    fn read_u8(&mut self) -> Result<u8, SavedStateDecodeError> {
        let b = *self.buf.first().ok_or(SavedStateDecodeError::Truncated)?;
        self.buf = &self.buf[1..];
        Ok(b)
    }
    fn read_array<const N: usize>(&mut self) -> Result<[u8; N], SavedStateDecodeError> {
        if self.buf.len() < N { return Err(SavedStateDecodeError::Truncated); }
        let (head, tail) = self.buf.split_at(N);
        self.buf = tail;
        Ok(head.try_into().expect("len checked"))
    }
    fn read_u32(&mut self) -> Result<u32, SavedStateDecodeError> {
        Ok(u32::from_le_bytes(self.read_array::<4>()?))
    }
    fn read_bytes(&mut self, n: usize) -> Result<&'a [u8], SavedStateDecodeError> {
        if self.buf.len() < n { return Err(SavedStateDecodeError::Truncated); }
        let (head, tail) = self.buf.split_at(n);
        self.buf = tail;
        Ok(head)
    }
}

// ── Backward-compat alias ─────────────────────────────────────────────
// The original scaffold called the context `ActivityContext`. Keeping the
// alias means the old name in `lib.rs`'s prelude still resolves during
// the transition.
pub type ActivityContext<'a> = Context<'a>;

// ── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_state_roundtrips() {
        let state = SavedState::default();
        let bytes = state.encode();
        assert_eq!(bytes, vec![0, 0, 0, 0]); // count=0
        let decoded = SavedState::decode(&bytes).unwrap();
        assert!(decoded.is_empty());
    }

    #[test]
    fn all_variants_roundtrip() {
        let mut state = SavedState::default();
        state.entries.insert("null".into(), SavedValue::Null);
        state.put_bool("flag", true);
        state.put_int("counter", -42);
        state.put_float("ratio", 1.5);
        state.put_string("title", "שלום world");
        state.put_bytes("blob", vec![0x00, 0xff, 0x42]);

        let bytes = state.encode();
        let decoded = SavedState::decode(&bytes).unwrap();
        assert_eq!(decoded.entries.len(), 6);
        assert_eq!(decoded.entries.get("null"), Some(&SavedValue::Null));
        assert_eq!(decoded.get_bool("flag"), Some(true));
        assert_eq!(decoded.get_int("counter"), Some(-42));
        assert_eq!(decoded.get_float("ratio"), Some(1.5));
        assert_eq!(decoded.get_string("title"), Some("שלום world"));
        assert_eq!(decoded.get_bytes("blob"), Some(&[0x00u8, 0xff, 0x42][..]));
    }

    #[test]
    fn encoding_is_deterministic_across_insertion_order() {
        let mut a = SavedState::default();
        a.put_int("x", 1);
        a.put_int("y", 2);
        let mut b = SavedState::default();
        b.put_int("y", 2);
        b.put_int("x", 1);
        assert_eq!(a.encode(), b.encode());
    }

    #[test]
    fn decode_rejects_truncated_header() {
        assert_eq!(
            SavedState::decode(&[0, 0]),
            Err(SavedStateDecodeError::Truncated),
        );
    }

    #[test]
    fn decode_rejects_truncated_string_payload() {
        // count=1, key_len=3 "abc", tag=STRING, str_len=10, only 2 bytes
        let mut bytes = vec![];
        bytes.extend_from_slice(&1u32.to_le_bytes());
        bytes.extend_from_slice(&3u32.to_le_bytes());
        bytes.extend_from_slice(b"abc");
        bytes.push(tag::STRING);
        bytes.extend_from_slice(&10u32.to_le_bytes());
        bytes.extend_from_slice(&[0xaa, 0xbb]);
        assert_eq!(
            SavedState::decode(&bytes),
            Err(SavedStateDecodeError::Truncated),
        );
    }

    #[test]
    fn decode_rejects_unknown_tag() {
        let mut bytes = vec![];
        bytes.extend_from_slice(&1u32.to_le_bytes());
        bytes.extend_from_slice(&1u32.to_le_bytes());
        bytes.push(b'k');
        bytes.push(0x7f);
        assert_eq!(
            SavedState::decode(&bytes),
            Err(SavedStateDecodeError::UnknownTag(0x7f)),
        );
    }

    #[test]
    fn decode_rejects_bad_utf8_key() {
        let mut bytes = vec![];
        bytes.extend_from_slice(&1u32.to_le_bytes());
        bytes.extend_from_slice(&2u32.to_le_bytes());
        bytes.extend_from_slice(&[0xff, 0xfe]);
        bytes.push(tag::NULL);
        assert_eq!(
            SavedState::decode(&bytes),
            Err(SavedStateDecodeError::BadUtf8),
        );
    }

    #[test]
    fn decode_refuses_blob_larger_than_max() {
        let bytes = vec![0u8; SavedState::MAX_BYTES + 1];
        assert_eq!(
            SavedState::decode(&bytes),
            Err(SavedStateDecodeError::TooLarge),
        );
    }
}
