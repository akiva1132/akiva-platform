//! Resource tables — per-package lookup of `@string/name` and
//! `@color/name` references from `res/values/*.toml`.
//!
//! MVP scope (April 2026):
//!   - Strings and colors only. Dimens and drawables deferred.
//!   - Single default set per package (no qualifier matching — no
//!     `values-he/`, `values-night/`, `values-land/`). The structure
//!     here lets us add qualifiers later as a second indexing key
//!     without changing the public API.
//!
//! The framework parses and owns a Rust-side copy so future
//! framework code (e.g. Shell showing `@string/app_name`) can query
//! directly. The runtime's host-imports layer (worker.js) parses the
//! same TOML independently for fast per-call lookup; divergence is
//! impossible because both derive from the single OPFS-resident
//! source. See `docs/resources.md` for the long-term design.

use std::collections::HashMap;

/// A single package's values resources. Name is deliberately boring —
/// future qualifier support will add a second `HashMap<Qualifiers, ...>`
/// layer around this, not replace it.
#[derive(Debug, Default, Clone)]
pub struct ResourceTable {
    strings: HashMap<String, String>,
    colors: HashMap<String, u32>,
}

impl ResourceTable {
    /// Parse a `values/strings.toml` + `values/colors.toml` pair into a
    /// ready-to-serve table. Either file may be absent (pass `None`) —
    /// an app that declares no strings or colors is legal.
    pub fn parse(
        strings_toml: Option<&str>,
        colors_toml: Option<&str>,
    ) -> Result<Self, ResourceError> {
        let strings = match strings_toml {
            Some(src) => parse_strings_toml(src)?,
            None => HashMap::new(),
        };
        let colors = match colors_toml {
            Some(src) => parse_colors_toml(src)?,
            None => HashMap::new(),
        };
        Ok(Self { strings, colors })
    }

    pub fn string(&self, name: &str) -> Option<&str> {
        self.strings.get(name).map(String::as_str)
    }

    pub fn color(&self, name: &str) -> Option<u32> {
        self.colors.get(name).copied()
    }

    /// Iterate `(name, value)` pairs. Used by the runtime-bridge layer
    /// that mirrors the table into worker.js at app instantiation.
    pub fn iter_strings(&self) -> impl Iterator<Item = (&str, &str)> {
        self.strings.iter().map(|(k, v)| (k.as_str(), v.as_str()))
    }

    pub fn iter_colors(&self) -> impl Iterator<Item = (&str, u32)> {
        self.colors.iter().map(|(k, v)| (k.as_str(), *v))
    }
}

/// Registry of tables keyed by installed package id. Rust-side counterpart
/// of the JS-side `AppRegistry._resources`. Populated at boot by
/// `PackageManager::load_installed`; queried by the Shell and by any
/// framework code that wants a user-facing string without reaching into
/// the host-imports layer.
#[derive(Debug, Default)]
pub struct ResourceManager {
    tables: HashMap<String, ResourceTable>,
}

impl ResourceManager {
    pub fn new() -> Self { Self::default() }

    pub fn register(&mut self, package_id: &str, table: ResourceTable) {
        self.tables.insert(package_id.to_string(), table);
    }

    pub fn remove(&mut self, package_id: &str) {
        self.tables.remove(package_id);
    }

    pub fn table(&self, package_id: &str) -> Option<&ResourceTable> {
        self.tables.get(package_id)
    }

    /// Convenience: resolve `@string/name` for a given package.
    pub fn string(&self, package_id: &str, name: &str) -> Option<&str> {
        self.tables.get(package_id).and_then(|t| t.string(name))
    }

    /// Convenience: resolve `@color/name` for a given package.
    pub fn color(&self, package_id: &str, name: &str) -> Option<u32> {
        self.tables.get(package_id).and_then(|t| t.color(name))
    }
}

#[derive(Debug)]
pub enum ResourceError {
    /// TOML tokenizer / grammar failure. Pass-through so tooling can
    /// point at a line.
    Syntax(String),
    /// A value in `strings.toml` wasn't a string (e.g. the author wrote
    /// `foo = 42` by mistake). We don't auto-coerce — the resource
    /// system promises typed lookups.
    NotAString { key: String },
    /// A color value wasn't parseable as `#RGB`, `#RRGGBB`, `#AARRGGBB`,
    /// or `0xAARRGGBB`.
    BadColor { key: String, value: String },
}

impl core::fmt::Display for ResourceError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            ResourceError::Syntax(msg) => write!(f, "resource syntax error: {msg}"),
            ResourceError::NotAString { key } => {
                write!(f, "strings.toml: '{key}' is not a string")
            }
            ResourceError::BadColor { key, value } => {
                write!(f, "colors.toml: '{key}' = '{value}' isn't a valid color")
            }
        }
    }
}

// ── Parsers ──────────────────────────────────────────────────────────
//
// Both values files share the same trivial shape:
//   key = "value"  # comments allowed
// Nested tables aren't meaningful in MVP scope — every entry sits at
// the top level. The inflater calls are `@string/app_name`, not
// `@string/app.name`, so hierarchical keys would just make lookup
// ambiguous for no gain.

fn parse_strings_toml(src: &str) -> Result<HashMap<String, String>, ResourceError> {
    let table: toml::Table = toml::from_str(src)
        .map_err(|e| ResourceError::Syntax(e.to_string()))?;
    let mut out = HashMap::with_capacity(table.len());
    for (key, value) in table {
        let s = value.as_str()
            .ok_or(ResourceError::NotAString { key: key.clone() })?;
        out.insert(key, s.to_string());
    }
    Ok(out)
}

fn parse_colors_toml(src: &str) -> Result<HashMap<String, u32>, ResourceError> {
    let table: toml::Table = toml::from_str(src)
        .map_err(|e| ResourceError::Syntax(e.to_string()))?;
    let mut out = HashMap::with_capacity(table.len());
    for (key, value) in table {
        let raw = value.as_str()
            .ok_or(ResourceError::NotAString { key: key.clone() })?;
        let argb = parse_color_literal(raw)
            .ok_or_else(|| ResourceError::BadColor { key: key.clone(), value: raw.to_string() })?;
        out.insert(key, argb);
    }
    Ok(out)
}

/// Parse one of `#RGB`, `#RRGGBB`, `#AARRGGBB`, `0xAARRGGBB` into the
/// canonical 0xAARRGGBB wire format used throughout the compositor. The
/// one place in the framework that cares about colour syntax — AKML's
/// inflater has its own copy of the same rules (in sdk-rust), which is
/// fine because the input space is tiny and frozen.
fn parse_color_literal(s: &str) -> Option<u32> {
    let trimmed = s.trim();
    let (hex, has_alpha_in_source) = if let Some(rest) = trimmed.strip_prefix("0x") {
        (rest, true)  // 0xAARRGGBB always carries alpha
    } else if let Some(rest) = trimmed.strip_prefix('#') {
        (rest, rest.len() == 8)
    } else {
        return None;
    };
    match hex.len() {
        3 => {
            // #RGB → #RRGGBB with alpha = 0xff.
            let r = expand_nybble(hex.as_bytes()[0])?;
            let g = expand_nybble(hex.as_bytes()[1])?;
            let b = expand_nybble(hex.as_bytes()[2])?;
            Some(0xff000000 | ((r as u32) << 16) | ((g as u32) << 8) | b as u32)
        }
        6 => {
            let v = u32::from_str_radix(hex, 16).ok()?;
            Some(0xff000000 | v)
        }
        8 => {
            let v = u32::from_str_radix(hex, 16).ok()?;
            // `#AARRGGBB` has alpha-first already; so does `0xAARRGGBB`.
            // The `has_alpha_in_source` flag is here just to document
            // intent — both forms land in the same place.
            let _ = has_alpha_in_source;
            Some(v)
        }
        _ => None,
    }
}

fn expand_nybble(byte: u8) -> Option<u8> {
    let v = (byte as char).to_digit(16)? as u8;
    Some((v << 4) | v)
}

// ── Tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strings_parse_flat_keys() {
        let src = r#"
            app_name = "Calculator"
            error = "Error"
        "#;
        let m = parse_strings_toml(src).unwrap();
        assert_eq!(m.get("app_name").unwrap(), "Calculator");
        assert_eq!(m.get("error").unwrap(), "Error");
    }

    #[test]
    fn strings_reject_non_string_values() {
        let src = "answer = 42";
        assert!(matches!(parse_strings_toml(src), Err(ResourceError::NotAString { .. })));
    }

    #[test]
    fn colors_parse_all_supported_forms() {
        // Outer `r##` lets the inner `"#rgb"` literals keep their hash.
        let src = r##"
            short = "#f0a"
            rgb = "#ff15172a"
            rrggbb = "#15172a"
            zerox = "0xff15172a"
        "##;
        let m = parse_colors_toml(src).unwrap();
        // #f0a → #ffff00aa
        assert_eq!(m["short"], 0xffff00aa);
        assert_eq!(m["rgb"], 0xff15172a);
        // #RRGGBB gets alpha=0xff prepended
        assert_eq!(m["rrggbb"], 0xff15172a);
        assert_eq!(m["zerox"], 0xff15172a);
    }

    #[test]
    fn colors_reject_garbage() {
        let src = r#"bad = "not-a-colour""#;
        assert!(matches!(
            parse_colors_toml(src),
            Err(ResourceError::BadColor { .. })
        ));
    }

    #[test]
    fn resource_table_absent_files_mean_empty_maps() {
        let t = ResourceTable::parse(None, None).unwrap();
        assert!(t.string("anything").is_none());
        assert!(t.color("anything").is_none());
    }

    #[test]
    fn resource_manager_scopes_by_package() {
        let mut mgr = ResourceManager::new();
        let a = ResourceTable::parse(
            Some(r#"greeting = "hello from a""#),
            Some(r##"bg = "#ff111111""##),
        ).unwrap();
        let b = ResourceTable::parse(
            Some(r#"greeting = "hello from b""#),
            Some(r##"bg = "#ff222222""##),
        ).unwrap();
        mgr.register("com.app.a", a);
        mgr.register("com.app.b", b);
        assert_eq!(mgr.string("com.app.a", "greeting"), Some("hello from a"));
        assert_eq!(mgr.string("com.app.b", "greeting"), Some("hello from b"));
        assert_eq!(mgr.color("com.app.a", "bg"), Some(0xff111111));
        assert_eq!(mgr.color("com.app.b", "bg"), Some(0xff222222));
        // Unknown package returns None, not a default-value fallback.
        assert_eq!(mgr.string("com.app.missing", "greeting"), None);
    }

    #[test]
    fn resource_manager_remove_cleans_up() {
        let mut mgr = ResourceManager::new();
        mgr.register("com.app.a", ResourceTable::parse(
            Some(r#"x = "y""#), None,
        ).unwrap());
        assert!(mgr.table("com.app.a").is_some());
        mgr.remove("com.app.a");
        assert!(mgr.table("com.app.a").is_none());
    }
}
