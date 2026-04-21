//! Intent router.
//!
//! Matches outgoing intents against intent filters declared by installed
//! packages and picks the resolved target. See docs/intents.md.

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
