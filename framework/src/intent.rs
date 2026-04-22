//! Intent router.
//!
//! Matches outgoing intents against intent filters declared by installed
//! packages and picks the resolved target. See docs/intents.md.

use std::collections::HashMap;

/// Canonical action strings. Kept as module-level constants so callers
/// can't typo the wire names and so greps for "who launches HOME" land on
/// every use site.
pub mod action {
    pub const MAIN: &str = "akiva.intent.action.MAIN";
    pub const VIEW: &str = "akiva.intent.action.VIEW";
}

pub mod category {
    pub const LAUNCHER: &str = "akiva.intent.category.LAUNCHER";
    pub const HOME: &str = "akiva.intent.category.HOME";
    pub const DEFAULT: &str = "akiva.intent.category.DEFAULT";
}

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
    /// Build a fresh intent with the given action and every other field at
    /// its empty default. Chain `.with_*` to fill in categories / targets
    /// / extras.
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

    pub fn with_category(mut self, cat: impl Into<String>) -> Self {
        self.categories.push(cat.into());
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

    /// Shortcut for a fully-explicit launch: both `target_package` and
    /// `target_class` are set, so the router short-circuits the matcher.
    /// Uses action `MAIN` because explicit launches never read the action
    /// field anyway; callers that care can override it.
    pub fn explicit(package_id: impl Into<String>, class: impl Into<String>) -> Self {
        Self::new(action::MAIN)
            .with_target_package(package_id)
            .with_target_class(class)
    }

    // ── Wire codec (Stone 9.4) ───────────────────────────────────────────
    //
    // The app side (sdk-rust) encodes an `Intent` into a flat byte stream
    // with `akiva_host_start_activity(ptr, len)`, the worker stashes it on
    // the handle, and the kernel drains it via `hal_app_drain_intent`.
    // Framework decodes here before handing it to `IntentRouter`. The
    // format mirrors `SavedState`'s "flat, little-endian, tagged" style
    // and is deliberately narrow — extras/IntentValue entries ride an
    // `extras_count` slot that is always `0` for v0.2 and reserved for
    // Stone 9.5 so readers stay stable across the addition.
    //
    // Wire layout (all little-endian):
    //   u32 action_len + <action_len> UTF-8 bytes
    //   u32 category_count + [u32 len + UTF-8 bytes] × count
    //   for each of data_uri, mime_type, target_package, target_class:
    //     u8 flag (0 = absent, 1 = present) then if present: u32 len + bytes
    //   u32 extras_count (must be 0 — reserved for 9.5)

    /// Upper bound on a single encoded intent blob. 16 KiB is generous
    /// for a MAIN+LAUNCHER + target_package intent (the common case is
    /// ~100 bytes) while still bounding host copies from a buggy app.
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
            // v0.2 doesn't carry extras. A non-zero count from an SDK
            // that doesn't know the extension yet would mean the reader
            // misaligns on every subsequent byte — refuse outright rather
            // than feed garbage into `IntentRouter`.
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

#[derive(Debug, Clone)]
pub struct IntentFilter {
    pub package_id: String,
    pub activity_class: String,
    pub actions: Vec<String>,
    pub categories: Vec<String>,
    pub data_schemes: Vec<String>,
    pub mime_types: Vec<String>,
}

#[derive(Debug)]
pub enum ResolveError {
    NoMatch,
    AmbiguousMatch(Vec<(String, String)>),
}

pub struct IntentRouter {
    filters: Vec<IntentFilter>,
}

impl IntentRouter {
    pub fn new() -> Self { Self { filters: Vec::new() } }

    pub fn register_filter(&mut self, filter: IntentFilter) {
        self.filters.push(filter);
    }

    pub fn clear_package(&mut self, package_id: &str) {
        self.filters.retain(|f| f.package_id != package_id);
    }

    /// Match an intent against all known filters. Returns the resolved
    /// `(package_id, activity_class)` or an error.
    pub fn resolve(&self, intent: &Intent) -> Result<(String, String), ResolveError> {
        if let (Some(p), Some(c)) = (intent.target_package.as_ref(), intent.target_class.as_ref()) {
            return Ok((p.clone(), c.clone()));
        }
        let mut matches: Vec<&IntentFilter> = self.filters.iter()
            .filter(|f| matches_intent(f, intent))
            .collect();
        match matches.len() {
            0 => Err(ResolveError::NoMatch),
            1 => {
                let f = matches.pop().unwrap();
                Ok((f.package_id.clone(), f.activity_class.clone()))
            }
            _ => Err(ResolveError::AmbiguousMatch(
                matches.iter().map(|f| (f.package_id.clone(), f.activity_class.clone())).collect()
            )),
        }
    }
}

fn matches_intent(f: &IntentFilter, i: &Intent) -> bool {
    // A `target_package` on the intent narrows matching to filters owned
    // by that package. The Shell uses this to say "launch MAIN+LAUNCHER
    // of *this* package" without knowing the entry class name.
    if let Some(tp) = &i.target_package {
        if &f.package_id != tp { return false; }
    }
    if !f.actions.iter().any(|a| a == &i.action) { return false; }
    // Categories must all be declared by the filter (AOSP semantics).
    if !i.categories.iter().all(|c| f.categories.iter().any(|fc| fc == c)) {
        return false;
    }
    if let Some(mime) = &i.mime_type {
        if !f.mime_types.is_empty() && !f.mime_types.iter().any(|m| mime_matches(m, mime)) {
            return false;
        }
    }
    true
}

fn mime_matches(pattern: &str, value: &str) -> bool {
    if pattern == "*/*" { return true; }
    if let Some(prefix) = pattern.strip_suffix("/*") {
        return value.starts_with(prefix) && value[prefix.len()..].starts_with('/');
    }
    pattern == value
}

#[cfg(test)]
mod tests {
    use super::*;

    fn launcher_filter(pkg: &str, class: &str) -> IntentFilter {
        IntentFilter {
            package_id: pkg.into(),
            activity_class: class.into(),
            actions: vec![action::MAIN.into()],
            categories: vec![category::LAUNCHER.into()],
            data_schemes: vec![],
            mime_types: vec![],
        }
    }

    #[test]
    fn explicit_intent_short_circuits_matcher() {
        // No filters registered; explicit target should still resolve —
        // the router trusts fully-targeted intents without consulting the
        // filter table.
        let router = IntentRouter::new();
        let intent = Intent::explicit("com.example.app", "Main");
        let (pkg, class) = router.resolve(&intent).unwrap();
        assert_eq!(pkg, "com.example.app");
        assert_eq!(class, "Main");
    }

    #[test]
    fn implicit_launcher_intent_resolves_to_single_match() {
        let mut router = IntentRouter::new();
        router.register_filter(launcher_filter("com.app.a", "MainA"));
        router.register_filter(IntentFilter {
            package_id: "com.app.b".into(),
            activity_class: "Other".into(),
            actions: vec!["some.other.action".into()],
            categories: vec![],
            data_schemes: vec![],
            mime_types: vec![],
        });
        let intent = Intent::new(action::MAIN).with_category(category::LAUNCHER);
        let (pkg, class) = router.resolve(&intent).unwrap();
        assert_eq!(pkg, "com.app.a");
        assert_eq!(class, "MainA");
    }

    #[test]
    fn target_package_narrows_matching() {
        // Two apps both expose MAIN+LAUNCHER. An intent that targets one
        // package must resolve only to that package's activity, even
        // though both filters would otherwise match.
        let mut router = IntentRouter::new();
        router.register_filter(launcher_filter("com.app.a", "MainA"));
        router.register_filter(launcher_filter("com.app.b", "MainB"));
        let intent = Intent::new(action::MAIN)
            .with_category(category::LAUNCHER)
            .with_target_package("com.app.b");
        let (pkg, class) = router.resolve(&intent).unwrap();
        assert_eq!(pkg, "com.app.b");
        assert_eq!(class, "MainB");
    }

    #[test]
    fn ambiguous_match_reported() {
        let mut router = IntentRouter::new();
        router.register_filter(launcher_filter("com.app.a", "MainA"));
        router.register_filter(launcher_filter("com.app.b", "MainB"));
        let intent = Intent::new(action::MAIN).with_category(category::LAUNCHER);
        assert!(matches!(router.resolve(&intent), Err(ResolveError::AmbiguousMatch(_))));
    }

    #[test]
    fn missing_category_fails_to_match() {
        let mut router = IntentRouter::new();
        router.register_filter(launcher_filter("com.app.a", "MainA"));
        // Intent requires HOME; filter only declares LAUNCHER — no match.
        let intent = Intent::new(action::MAIN).with_category(category::HOME);
        assert!(matches!(router.resolve(&intent), Err(ResolveError::NoMatch)));
    }

    // ── Wire codec (Stone 9.4) ──────────────────────────────────────────

    #[test]
    fn encoded_intent_roundtrips_all_fields() {
        let mut source = Intent::new(action::VIEW)
            .with_category(category::DEFAULT)
            .with_target_package("com.example.browser")
            .with_target_class("Viewer");
        source.data_uri = Some("https://example.com/page".into());
        source.mime_type = Some("text/html".into());

        let bytes = source.encode();
        let decoded = Intent::decode(&bytes).unwrap();

        assert_eq!(decoded.action, source.action);
        assert_eq!(decoded.categories, source.categories);
        assert_eq!(decoded.data_uri, source.data_uri);
        assert_eq!(decoded.mime_type, source.mime_type);
        assert_eq!(decoded.target_package, source.target_package);
        assert_eq!(decoded.target_class, source.target_class);
        assert!(decoded.extras.is_empty());
    }

    #[test]
    fn encoded_intent_roundtrips_minimal() {
        // Just an action — every other field at its default. The flag
        // bytes still fire (0 each); decode must not mis-interpret them.
        let source = Intent::new(action::MAIN);
        let bytes = source.encode();
        let decoded = Intent::decode(&bytes).unwrap();
        assert_eq!(decoded.action, action::MAIN);
        assert!(decoded.categories.is_empty());
        assert_eq!(decoded.data_uri, None);
        assert_eq!(decoded.mime_type, None);
        assert_eq!(decoded.target_package, None);
        assert_eq!(decoded.target_class, None);
    }

    #[test]
    fn decode_rejects_truncated_stream() {
        let mut bytes = Intent::new(action::MAIN).encode();
        bytes.truncate(bytes.len() - 1);
        assert!(matches!(Intent::decode(&bytes), Err(IntentDecodeError::Truncated)));
    }

    #[test]
    fn decode_rejects_nonzero_extras_count() {
        // Encode a minimal intent then overwrite the final u32
        // (extras_count) with 1. v0.2 has no extras payload reader —
        // refuse rather than misread the next blob as one.
        let mut bytes = Intent::new(action::MAIN).encode();
        let tail_start = bytes.len() - 4;
        bytes[tail_start..].copy_from_slice(&1u32.to_le_bytes());
        assert!(matches!(Intent::decode(&bytes), Err(IntentDecodeError::ExtrasNotSupported)));
    }

    #[test]
    fn decode_rejects_oversized_blob() {
        let bytes = vec![0u8; Intent::MAX_WIRE_BYTES + 1];
        assert!(matches!(Intent::decode(&bytes), Err(IntentDecodeError::TooLarge)));
    }

    #[test]
    fn decode_rejects_bad_flag() {
        // Minimal encode + flip the first flag byte (after the category
        // count) to an illegal value. Built by hand so the byte layout
        // stays obvious to a future reader.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&1u32.to_le_bytes());  // action_len = 1
        bytes.push(b'x');
        bytes.extend_from_slice(&0u32.to_le_bytes());  // category_count = 0
        bytes.push(7);  // data_uri flag — not 0 or 1
        assert!(matches!(Intent::decode(&bytes), Err(IntentDecodeError::BadFlag(7))));
    }

    #[test]
    fn clear_package_drops_only_that_packages_filters() {
        let mut router = IntentRouter::new();
        router.register_filter(launcher_filter("com.app.a", "MainA"));
        router.register_filter(launcher_filter("com.app.b", "MainB"));
        router.clear_package("com.app.a");
        let intent = Intent::new(action::MAIN).with_category(category::LAUNCHER);
        let (pkg, _) = router.resolve(&intent).unwrap();
        assert_eq!(pkg, "com.app.b");
    }
}
