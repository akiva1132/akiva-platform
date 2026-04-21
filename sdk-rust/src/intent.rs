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
