//! Intent construction + response handling.
//!
//! Mirrors the kernel-side `IntentRouter` types but in app-safe form: no
//! direct access to routing state, just a builder that the SDK serialises
//! across the WIT boundary.
//!
//! # Examples
//!
//! Build an explicit intent (skips filter matching, lands directly at
//! the named activity):
//!
//! ```
//! use akiva_sdk::Intent;
//! let i = Intent::explicit("com.akiva.calculator", "CalculatorMain");
//! assert_eq!(i.target_package.as_deref(), Some("com.akiva.calculator"));
//! ```
//!
//! Implicit intent — the framework's `IntentRouter` picks the activity
//! whose declared `<intent_filter>` matches:
//!
//! ```
//! use akiva_sdk::Intent;
//! let i = Intent::new("akiva.intent.action.VIEW")
//!     .with_category("akiva.intent.category.DEFAULT");
//! # let _ = i;
//! ```

use std::collections::HashMap;

/// A pending request to launch an activity. Mirrors the Android intent
/// model: `action` + `categories` describe the operation; optional
/// `data_uri` / `mime_type` carry the payload; `extras` carries
/// app-specific key/value pairs.
///
/// `target_package` / `target_class` make the intent *explicit* — the
/// router ignores filter matching and dispatches straight to the named
/// activity. Use [`Intent::explicit`] for that path; otherwise leave
/// them `None` and let `IntentRouter` resolve.
#[derive(Debug, Clone)]
pub struct Intent {
    /// Required action verb (e.g. `akiva.intent.action.MAIN`,
    /// `akiva.intent.action.VIEW`). Must match the activity's
    /// `<intent_filter>` action list for implicit dispatch.
    pub action: String,
    /// Optional category filters (e.g.
    /// `akiva.intent.category.LAUNCHER`). The activity's filter must
    /// declare every category present here for a match.
    pub categories: Vec<String>,
    /// Optional URI describing the payload to operate on.
    pub data_uri: Option<String>,
    /// Optional MIME type for `data_uri`. Used for filter narrowing
    /// when `data_uri`'s scheme alone isn't enough.
    pub mime_type: Option<String>,
    /// App-defined named extras. Key/value pairs round-trip through
    /// the wire codec; serialisation is reserved for Stone 9.5.
    pub extras: HashMap<String, IntentValue>,
    /// Set together with `target_class` for an explicit launch. The
    /// framework's `IntentRouter` short-circuits filter matching when
    /// both are present.
    pub target_package: Option<String>,
    /// Set together with `target_package` for an explicit launch.
    pub target_class: Option<String>,
}

impl Intent {
    /// Build a fresh intent with the given action and every other field
    /// at its empty default. Chain `.with_category(...)` /
    /// `.with_target_package(...)` to fill in more.
    pub fn new(action: impl Into<String>) -> Self {
        Self {
            action: action.into(),
            categories: Vec::new(),
            data_uri: None,
            mime_type: None,
            extras: HashMap::new(),
            target_package: None,
            target_class: None,
        }
    }

    /// Append `c` to the category list. Categories narrow which
    /// `<intent_filter>` will match — every category here must be
    /// declared on the filter.
    pub fn with_category(mut self, c: impl Into<String>) -> Self {
        self.categories.push(c.into());
        self
    }

    /// Pin the explicit `target_package` field. Combine with
    /// [`with_target_class`](Self::with_target_class) for fully-explicit
    /// dispatch — or use [`explicit`](Self::explicit) for the common case.
    pub fn with_target_package(mut self, pkg: impl Into<String>) -> Self {
        self.target_package = Some(pkg.into());
        self
    }

    /// Pin the explicit `target_class` field. See
    /// [`with_target_package`](Self::with_target_package).
    pub fn with_target_class(mut self, class: impl Into<String>) -> Self {
        self.target_class = Some(class.into());
        self
    }

    /// Fully-explicit launch: framework's `IntentRouter` short-circuits
    /// its filter matcher and routes straight to `(package, class)`.
    /// Action defaults to `akiva.intent.action.MAIN`; override with
    /// `.action = ...` afterwards if a different verb is needed.
    pub fn explicit(package_id: impl Into<String>, class: impl Into<String>) -> Self {
        Self::new("akiva.intent.action.MAIN")
            .with_target_package(package_id)
            .with_target_class(class)
    }

    // ── Wire codec (Stone 9.4) ───────────────────────────────────────────
    //
    // Mirror of `framework::intent::Intent::encode`/`decode` — the two
    // sides of the ABI. Same layout; MUST stay byte-for-byte identical,
    // so any change here is a v0-breaking event. See docs/abi.md §2.5.

    /// Maximum encoded blob size the framework accepts. Mirror of
    /// `framework::intent::Intent::MAX_WIRE_BYTES`. Larger payloads
    /// belong in storage with a key passed through `data_uri`.
    pub const MAX_WIRE_BYTES: usize = 16 * 1024;

    /// Serialise to the v0 wire format. Always succeeds; the 16 KiB
    /// cap is enforced on the decode side. See `docs/abi.md §2.5` for
    /// the byte layout — symmetric with framework's decoder.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(32 + self.action.len());
        let action = self.action.as_bytes();
        out.extend_from_slice(&(action.len() as u32).to_le_bytes());
        out.extend_from_slice(action);
        out.extend_from_slice(&(self.categories.len() as u32).to_le_bytes());
        for c in &self.categories {
            let cb = c.as_bytes();
            out.extend_from_slice(&(cb.len() as u32).to_le_bytes());
            out.extend_from_slice(cb);
        }
        for field in [&self.data_uri, &self.mime_type, &self.target_package, &self.target_class] {
            match field {
                Some(s) => {
                    out.push(1);
                    let sb = s.as_bytes();
                    out.extend_from_slice(&(sb.len() as u32).to_le_bytes());
                    out.extend_from_slice(sb);
                }
                None => out.push(0),
            }
        }
        // extras_count = 0 (reserved for Stone 9.5).
        out.extend_from_slice(&0u32.to_le_bytes());
        out
    }

    /// Parse the v0 wire format. Returns
    /// [`IntentDecodeError::Truncated`] for short buffers,
    /// [`IntentDecodeError::TooLarge`] for buffers exceeding
    /// [`MAX_WIRE_BYTES`](Self::MAX_WIRE_BYTES), or
    /// [`IntentDecodeError::ExtrasNotSupported`] when an old encoder
    /// emits non-empty extras (reserved for 9.5).
    pub fn decode(bytes: &[u8]) -> Result<Self, IntentDecodeError> {
        if bytes.len() > Self::MAX_WIRE_BYTES {
            return Err(IntentDecodeError::TooLarge);
        }
        let mut r = WireReader { buf: bytes };
        let action = r.read_string()?;
        let cat_count = r.read_u32()? as usize;
        let mut categories = Vec::with_capacity(cat_count);
        for _ in 0..cat_count {
            categories.push(r.read_string()?);
        }
        let data_uri = r.read_opt_string()?;
        let mime_type = r.read_opt_string()?;
        let target_package = r.read_opt_string()?;
        let target_class = r.read_opt_string()?;
        let extras_count = r.read_u32()?;
        if extras_count != 0 {
            return Err(IntentDecodeError::ExtrasNotSupported);
        }
        Ok(Intent {
            action,
            categories,
            data_uri,
            mime_type,
            extras: HashMap::new(),
            target_package,
            target_class,
        })
    }
}

/// Reasons [`Intent::decode`] can refuse to parse a buffer.
#[derive(Debug, PartialEq, Eq)]
pub enum IntentDecodeError {
    /// Buffer ended before the layout said it should — usually a
    /// length prefix promising more bytes than the buffer holds.
    Truncated,
    /// One of the string fields wasn't valid UTF-8.
    BadUtf8,
    /// Optional-field flag byte was something other than `0` (None) or
    /// `1` (Some). Wire-format violation.
    BadFlag(u8),
    /// Encoded blob declared a non-zero `extras_count`. Extras are
    /// reserved for Stone 9.5; until then, both sides emit zero.
    ExtrasNotSupported,
    /// Encoded blob exceeded [`Intent::MAX_WIRE_BYTES`].
    TooLarge,
}

struct WireReader<'a> {
    buf: &'a [u8],
}

impl<'a> WireReader<'a> {
    fn read_u8(&mut self) -> Result<u8, IntentDecodeError> {
        let b = *self.buf.first().ok_or(IntentDecodeError::Truncated)?;
        self.buf = &self.buf[1..];
        Ok(b)
    }
    fn read_u32(&mut self) -> Result<u32, IntentDecodeError> {
        if self.buf.len() < 4 { return Err(IntentDecodeError::Truncated); }
        let (head, tail) = self.buf.split_at(4);
        self.buf = tail;
        Ok(u32::from_le_bytes([head[0], head[1], head[2], head[3]]))
    }
    fn read_bytes(&mut self, n: usize) -> Result<&'a [u8], IntentDecodeError> {
        if self.buf.len() < n { return Err(IntentDecodeError::Truncated); }
        let (head, tail) = self.buf.split_at(n);
        self.buf = tail;
        Ok(head)
    }
    fn read_string(&mut self) -> Result<String, IntentDecodeError> {
        let len = self.read_u32()? as usize;
        let bytes = self.read_bytes(len)?;
        core::str::from_utf8(bytes)
            .map(|s| s.to_string())
            .map_err(|_| IntentDecodeError::BadUtf8)
    }
    fn read_opt_string(&mut self) -> Result<Option<String>, IntentDecodeError> {
        match self.read_u8()? {
            0 => Ok(None),
            1 => Ok(Some(self.read_string()?)),
            other => Err(IntentDecodeError::BadFlag(other)),
        }
    }
}

/// Typed value held by [`Intent::extras`]. Reserved for Stone 9.5;
/// today both encode and decode require the extras map to be empty.
/// The variants are listed now so app code can write extras without
/// having to retype the API once 9.5 lands.
#[derive(Debug, Clone)]
pub enum IntentValue {
    /// Explicit absence — useful for distinguishing "no value" from
    /// "key not present" in serialised form.
    Null,
    /// Boolean.
    Bool(bool),
    /// Signed 64-bit integer.
    Int(i64),
    /// 64-bit float.
    Float(f64),
    /// UTF-8 string.
    String(String),
    /// Opaque byte payload — for thumbnails, encoded protos, etc.
    Bytes(Vec<u8>),
}

/// Lazy intent builder for chains where the action isn't decided up
/// front. Compare with [`Intent::new`] which insists on the action at
/// construction. [`build`](Self::build) returns `None` if no field
/// was ever set — guards a "did the user actually intend an intent"
/// state.
#[derive(Debug, Default)]
pub struct IntentBuilder {
    inner: Option<Intent>,
}

impl IntentBuilder {
    /// Set or override the action. Creates the underlying [`Intent`]
    /// on first call.
    pub fn action(mut self, action: impl Into<String>) -> Self {
        self.inner.get_or_insert_with(|| Intent {
            action: String::new(),
            categories: Vec::new(),
            data_uri: None,
            mime_type: None,
            extras: HashMap::new(),
            target_package: None,
            target_class: None,
        }).action = action.into();
        self
    }

    /// Append a category. No-op if the builder hasn't seen an action
    /// yet — categories without an action wouldn't form a valid intent.
    pub fn category(mut self, c: impl Into<String>) -> Self {
        if let Some(i) = self.inner.as_mut() { i.categories.push(c.into()); }
        self
    }

    /// Set the `data_uri` field. No-op if no action has been set.
    pub fn data(mut self, uri: impl Into<String>) -> Self {
        if let Some(i) = self.inner.as_mut() { i.data_uri = Some(uri.into()); }
        self
    }

    /// Insert / overwrite an extra. No-op if no action has been set.
    pub fn extra(mut self, key: impl Into<String>, v: IntentValue) -> Self {
        if let Some(i) = self.inner.as_mut() { i.extras.insert(key.into(), v); }
        self
    }

    /// Finalise. `None` if no field was ever set; `Some(intent)`
    /// otherwise — caller is responsible for verifying the intent has
    /// an action before passing it to a launcher.
    pub fn build(self) -> Option<Intent> { self.inner }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_codec_roundtrips_common_fields() {
        // Encode-then-decode inside the SDK is already covered by the
        // framework-side test; this one pins the symmetric direction —
        // an SDK-encoded blob must decode back to the same fields here,
        // because on_new_intent goes SDK→host→app and the app decodes.
        let intent = Intent::explicit("com.akiva.calculator", "Main")
            .with_category("akiva.intent.category.LAUNCHER");
        let bytes = intent.encode();
        let round = Intent::decode(&bytes).unwrap();
        assert_eq!(round.action, intent.action);
        assert_eq!(round.categories, intent.categories);
        assert_eq!(round.target_package, intent.target_package);
        assert_eq!(round.target_class, intent.target_class);
    }
}
