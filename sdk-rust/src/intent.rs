//! Intent construction + response handling.
//!
//! Mirrors the kernel-side `IntentRouter` types but in app-safe form: no
//! direct access to routing state, just a builder that the SDK serialises
//! across the WIT boundary.

use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct Intent {
    pub action: String,
    pub categories: Vec<String>,
    pub data_uri: Option<String>,
    pub mime_type: Option<String>,
    pub extras: HashMap<String, IntentValue>,
    pub target_package: Option<String>,
    pub target_class: Option<String>,
}

impl Intent {
    /// Build a fresh intent with the given action and every other field
    /// at its empty default.
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

    pub fn with_category(mut self, c: impl Into<String>) -> Self {
        self.categories.push(c.into());
        self
    }

    pub fn with_target_package(mut self, pkg: impl Into<String>) -> Self {
        self.target_package = Some(pkg.into());
        self
    }

    pub fn with_target_class(mut self, class: impl Into<String>) -> Self {
        self.target_class = Some(class.into());
        self
    }

    /// Fully-explicit launch: framework's `IntentRouter` short-circuits
    /// its filter matcher and routes straight to `(package, class)`.
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
    /// `framework::intent::Intent::MAX_WIRE_BYTES`.
    pub const MAX_WIRE_BYTES: usize = 16 * 1024;

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

#[derive(Debug, PartialEq, Eq)]
pub enum IntentDecodeError {
    Truncated,
    BadUtf8,
    BadFlag(u8),
    ExtrasNotSupported,
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

#[derive(Debug, Clone)]
pub enum IntentValue {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    String(String),
    Bytes(Vec<u8>),
}

#[derive(Debug, Default)]
pub struct IntentBuilder {
    inner: Option<Intent>,
}

impl IntentBuilder {
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

    pub fn category(mut self, c: impl Into<String>) -> Self {
        if let Some(i) = self.inner.as_mut() { i.categories.push(c.into()); }
        self
    }

    pub fn data(mut self, uri: impl Into<String>) -> Self {
        if let Some(i) = self.inner.as_mut() { i.data_uri = Some(uri.into()); }
        self
    }

    pub fn extra(mut self, key: impl Into<String>, v: IntentValue) -> Self {
        if let Some(i) = self.inner.as_mut() { i.extras.insert(key.into(), v); }
        self
    }

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
