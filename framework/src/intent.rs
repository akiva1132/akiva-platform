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
