//! `manifest.toml` parser + validator.
//!
//! Parses the schema documented in `docs/manifest.md §3` into typed
//! structs, applies the defaults from §5, then enforces the validation
//! rules from §6 that can be checked without cross-package context
//! (resource reference resolution lives in `ResourceManager` and runs
//! later).
//!
//! What we deliberately do **not** do here:
//!   - Verify `runtime.wasm` exists on disk (the PackageManager does that
//!     when it wires the manifest to its bytes).
//!   - Verify AKML / string / drawable references — ResourceManager's job.
//!   - Check `min_abi_version` against the live runtime ABI — PackageManager
//!     does this during install.

use serde::Deserialize;

use crate::activity_manager::LaunchMode;

/// Current framework ABI version string, bumped in lock-step with
/// `docs/abi.md` major+minor. MVP guests targeting 0.1.x pass the check.
pub const RUNTIME_ABI_VERSION: &str = "0.1.0";

// ── Typed model ──────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    pub app: AppInfo,
    pub runtime: RuntimeInfo,
    pub activities: Vec<ActivityDecl>,
    pub permissions: PermissionDecl,
    pub display: DisplayDecl,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppInfo {
    pub id: String,
    pub name: String,
    pub version: String,
    pub version_code: u64,
    pub min_abi_version: String,
    pub authors: Vec<String>,
    pub description: String,
    pub icon: Option<String>,
    pub category: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeInfo {
    pub wasm: String,
    pub min_memory_mb: u32,
    pub max_memory_mb: u32,
    /// Stone 11.6.7 #5 — per-package watchdog timeout override
    /// (ms). `None` falls back to the global `WATCHDOG.TIMEOUT_MS`
    /// in `runtime/web/config.js` (or its
    /// `globalThis.__akivaConfig.watchdog.TIMEOUT_MS` override).
    /// Apps with long-running synchronous compute (cold starts,
    /// large state restores) can lift the limit; nothing should
    /// lower it below 200ms or the warn threshold (`WARN_MS`)
    /// can't fire reliably.
    pub watchdog_timeout_ms: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivityDecl {
    pub id: String,
    pub class: String,
    pub entry: bool,
    pub launch_mode: LaunchMode,
    pub task_affinity: String,
    pub exported: bool,
    pub config_changes: Vec<String>,
    pub intent_filters: Vec<IntentFilterDecl>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct IntentFilterDecl {
    pub actions: Vec<String>,
    pub categories: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PermissionDecl {
    pub required: Vec<String>,
    pub optional: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisplayDecl {
    pub default_width_dp: u32,
    pub default_height_dp: u32,
    pub resizable: bool,
    pub orientation: String,
}

impl Manifest {
    /// Convenience: the activity with `entry = true`. Guaranteed to exist
    /// on a validated manifest.
    pub fn entry_activity(&self) -> &ActivityDecl {
        self.activities.iter().find(|a| a.entry).expect("validated manifest has entry activity")
    }
}

// ── Errors ───────────────────────────────────────────────────────────

#[derive(Debug)]
pub enum ManifestError {
    /// TOML tokenizer / grammar failure. Message is pass-through from the
    /// parser so tooling can point at a line.
    Syntax(String),
    /// Required field missing or of the wrong type.
    MissingField(&'static str),
    InvalidField { field: &'static str, reason: String },
    /// No activity had `entry = true`.
    NoEntryActivity,
    /// Two or more activities share an `id`.
    DuplicateActivityId(String),
    /// An intent filter declared an empty `actions` list.
    EmptyIntentFilter { activity_id: String },
}

impl core::fmt::Display for ManifestError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            ManifestError::Syntax(m) => write!(f, "manifest syntax error: {m}"),
            ManifestError::MissingField(name) => write!(f, "missing field: {name}"),
            ManifestError::InvalidField { field, reason } => {
                write!(f, "invalid {field}: {reason}")
            }
            ManifestError::NoEntryActivity => {
                write!(f, "no [[activities]] has entry = true")
            }
            ManifestError::DuplicateActivityId(id) => {
                write!(f, "duplicate activity id: {id}")
            }
            ManifestError::EmptyIntentFilter { activity_id } => {
                write!(f, "activity {activity_id}: intent_filter has no actions")
            }
        }
    }
}

// ── Wire schema (serde) ──────────────────────────────────────────────
//
// These mirror the TOML shape 1:1 so `toml::from_str` does the heavy
// lifting. We pull the validated, typed `Manifest` out of the wire
// schema below in `normalize()`.

#[derive(Debug, Deserialize)]
struct WireManifest {
    app: WireApp,
    runtime: WireRuntime,
    #[serde(default)]
    activities: Vec<WireActivity>,
    #[serde(default)]
    permissions: WirePermissions,
    #[serde(default)]
    display: Option<WireDisplay>,
}

#[derive(Debug, Deserialize)]
struct WireApp {
    id: String,
    name: String,
    version: String,
    version_code: u64,
    min_abi_version: String,
    #[serde(default)]
    authors: Vec<String>,
    #[serde(default)]
    description: String,
    #[serde(default)]
    icon: Option<String>,
    #[serde(default = "default_category")]
    category: String,
}

fn default_category() -> String { "other".into() }

#[derive(Debug, Deserialize)]
struct WireRuntime {
    wasm: String,
    #[serde(default = "default_min_mem")]
    min_memory_mb: u32,
    #[serde(default = "default_max_mem")]
    max_memory_mb: u32,
    #[serde(default)]
    watchdog_timeout_ms: Option<u32>,
}

fn default_min_mem() -> u32 { 1 }
fn default_max_mem() -> u32 { 32 }

#[derive(Debug, Deserialize)]
struct WireActivity {
    id: String,
    #[serde(default)]
    class: Option<String>,
    #[serde(default)]
    entry: bool,
    #[serde(default)]
    launch_mode: Option<String>,
    #[serde(default)]
    task_affinity: Option<String>,
    #[serde(default)]
    exported: Option<bool>,
    #[serde(default)]
    config_changes: Vec<String>,
    #[serde(default)]
    intent_filters: Vec<WireIntentFilter>,
}

#[derive(Debug, Deserialize, Default)]
struct WireIntentFilter {
    #[serde(default)]
    actions: Vec<String>,
    #[serde(default)]
    categories: Vec<String>,
}

#[derive(Debug, Deserialize, Default)]
struct WirePermissions {
    #[serde(default)]
    required: Vec<String>,
    #[serde(default)]
    optional: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct WireDisplay {
    #[serde(default = "default_width")]
    default_width_dp: u32,
    #[serde(default = "default_height")]
    default_height_dp: u32,
    #[serde(default = "default_resizable")]
    resizable: bool,
    #[serde(default = "default_orientation")]
    orientation: String,
}

fn default_width() -> u32 { 360 }
fn default_height() -> u32 { 640 }
fn default_resizable() -> bool { true }
fn default_orientation() -> String { "any".into() }

// ── Parsing entry point ──────────────────────────────────────────────

/// Parse + validate a `manifest.toml` body. Returns a fully-defaulted,
/// invariant-checked `Manifest`, or the first error encountered.
pub fn parse(toml_src: &str) -> Result<Manifest, ManifestError> {
    let wire: WireManifest =
        toml::from_str(toml_src).map_err(|e| ManifestError::Syntax(e.to_string()))?;
    normalize(wire)
}

fn normalize(w: WireManifest) -> Result<Manifest, ManifestError> {
    validate_app_id(&w.app.id)?;
    validate_semver(&w.app.version, "app.version")?;
    validate_semver(&w.app.min_abi_version, "app.min_abi_version")?;

    if w.runtime.wasm.is_empty() {
        return Err(ManifestError::MissingField("runtime.wasm"));
    }
    if w.runtime.max_memory_mb < w.runtime.min_memory_mb {
        return Err(ManifestError::InvalidField {
            field: "runtime.max_memory_mb",
            reason: "must be >= min_memory_mb".into(),
        });
    }

    let activities = normalize_activities(&w.app.id, w.activities)?;
    if !activities.iter().any(|a| a.entry) {
        return Err(ManifestError::NoEntryActivity);
    }

    let display = match w.display {
        Some(d) => DisplayDecl {
            default_width_dp: d.default_width_dp,
            default_height_dp: d.default_height_dp,
            resizable: d.resizable,
            orientation: d.orientation,
        },
        None => DisplayDecl {
            default_width_dp: default_width(),
            default_height_dp: default_height(),
            resizable: default_resizable(),
            orientation: default_orientation(),
        },
    };

    Ok(Manifest {
        app: AppInfo {
            id: w.app.id,
            name: w.app.name,
            version: w.app.version,
            version_code: w.app.version_code,
            min_abi_version: w.app.min_abi_version,
            authors: w.app.authors,
            description: w.app.description,
            icon: w.app.icon,
            category: w.app.category,
        },
        runtime: RuntimeInfo {
            wasm: w.runtime.wasm,
            min_memory_mb: w.runtime.min_memory_mb,
            max_memory_mb: w.runtime.max_memory_mb,
            watchdog_timeout_ms: w.runtime.watchdog_timeout_ms,
        },
        activities,
        permissions: PermissionDecl {
            required: w.permissions.required,
            optional: w.permissions.optional,
        },
        display,
    })
}

fn normalize_activities(
    app_id: &str,
    wire: Vec<WireActivity>,
) -> Result<Vec<ActivityDecl>, ManifestError> {
    let mut out = Vec::with_capacity(wire.len());
    let mut seen = std::collections::HashSet::new();
    for a in wire {
        if !seen.insert(a.id.clone()) {
            return Err(ManifestError::DuplicateActivityId(a.id));
        }
        let launch_mode = match a.launch_mode.as_deref() {
            None | Some("standard") => LaunchMode::Standard,
            Some("singleTop") => LaunchMode::SingleTop,
            Some("singleTask") => LaunchMode::SingleTask,
            Some("singleInstance") => LaunchMode::SingleInstance,
            Some(other) => return Err(ManifestError::InvalidField {
                field: "activity.launch_mode",
                reason: format!("unknown value: {other}"),
            }),
        };
        // Default for `exported`: true iff the activity has any intent
        // filter declared. Apps without a filter are assumed internal.
        let exported = a.exported.unwrap_or(!a.intent_filters.is_empty());
        let class = a.class.clone().unwrap_or_else(|| a.id.clone());
        let task_affinity = a.task_affinity.unwrap_or_else(|| app_id.to_string());

        let intent_filters = a.intent_filters
            .into_iter()
            .map(|f| {
                if f.actions.is_empty() {
                    return Err(ManifestError::EmptyIntentFilter { activity_id: a.id.clone() });
                }
                Ok(IntentFilterDecl { actions: f.actions, categories: f.categories })
            })
            .collect::<Result<Vec<_>, _>>()?;

        out.push(ActivityDecl {
            id: a.id,
            class,
            entry: a.entry,
            launch_mode,
            task_affinity,
            exported,
            config_changes: a.config_changes,
            intent_filters,
        });
    }
    Ok(out)
}

// ── Field-level validation ───────────────────────────────────────────

fn validate_app_id(id: &str) -> Result<(), ManifestError> {
    // Reverse-domain-lite: non-empty, at least one dot, each segment is
    // ASCII [a-z0-9_-] and doesn't start with a digit. This matches the
    // set Android uses for `applicationId`, relaxed to allow hyphens.
    if id.is_empty() {
        return Err(ManifestError::MissingField("app.id"));
    }
    if !id.contains('.') {
        return Err(ManifestError::InvalidField {
            field: "app.id",
            reason: "must be reverse-domain (at least one '.')".into(),
        });
    }
    for seg in id.split('.') {
        if seg.is_empty() {
            return Err(ManifestError::InvalidField {
                field: "app.id",
                reason: "empty segment".into(),
            });
        }
        let first = seg.chars().next().unwrap();
        if first.is_ascii_digit() {
            return Err(ManifestError::InvalidField {
                field: "app.id",
                reason: format!("segment '{seg}' starts with a digit"),
            });
        }
        if !seg.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-') {
            return Err(ManifestError::InvalidField {
                field: "app.id",
                reason: format!("segment '{seg}' has illegal chars (allow: a-z 0-9 _ -)"),
            });
        }
    }
    Ok(())
}

fn validate_semver(s: &str, field: &'static str) -> Result<(), ManifestError> {
    // Intentional: we accept MAJOR.MINOR.PATCH with optional -prerelease.
    // Leaves out build metadata (`+...`) to keep parsing trivial; we can
    // expand later when any app needs it.
    let (core, _pre) = match s.split_once('-') {
        Some((c, p)) if !p.is_empty() => (c, Some(p)),
        _ => (s, None),
    };
    let parts: Vec<&str> = core.split('.').collect();
    if parts.len() != 3 {
        return Err(ManifestError::InvalidField {
            field,
            reason: format!("not semver MAJOR.MINOR.PATCH: '{s}'"),
        });
    }
    for part in parts {
        if part.is_empty() || !part.chars().all(|c| c.is_ascii_digit()) {
            return Err(ManifestError::InvalidField {
                field,
                reason: format!("non-numeric version component in '{s}'"),
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const FULL: &str = r#"
[app]
id = "com.akiva.calculator"
name = "Calculator"
version = "0.1.0"
version_code = 1
min_abi_version = "0.1.0"
authors = ["Akiva Team"]
description = "Four-function calculator"
category = "utilities"

[runtime]
wasm = "calculator.wasm"
min_memory_mb = 1
max_memory_mb = 16

[[activities]]
id = "main"
class = "CalculatorMain"
entry = true
launch_mode = "singleTop"

[[activities.intent_filters]]
actions = ["akiva.intent.action.MAIN"]
categories = ["akiva.intent.category.LAUNCHER"]

[[activities]]
id = "settings"
class = "CalculatorSettings"

[permissions]
required = []
optional = []

[display]
default_width_dp = 320
default_height_dp = 520
resizable = true
orientation = "any"
"#;

    #[test]
    fn full_manifest_parses() {
        let m = parse(FULL).expect("valid manifest");
        assert_eq!(m.app.id, "com.akiva.calculator");
        assert_eq!(m.app.version_code, 1);
        assert_eq!(m.runtime.wasm, "calculator.wasm");
        assert_eq!(m.activities.len(), 2);
        assert_eq!(m.activities[0].launch_mode, LaunchMode::SingleTop);
        assert!(m.activities[0].entry);
        assert_eq!(m.activities[0].intent_filters.len(), 1);
        // Default for exported: false when activity has no filter.
        assert!(!m.activities[1].exported);
        // Task affinity defaults to package id.
        assert_eq!(m.activities[1].task_affinity, "com.akiva.calculator");
    }

    #[test]
    fn entry_activity_required() {
        let src = r#"
[app]
id = "com.akiva.x"
name = "X"
version = "0.1.0"
version_code = 1
min_abi_version = "0.1.0"
[runtime]
wasm = "x.wasm"
[[activities]]
id = "main"
"#;
        assert!(matches!(parse(src), Err(ManifestError::NoEntryActivity)));
    }

    #[test]
    fn duplicate_activity_id_rejected() {
        let src = r#"
[app]
id = "com.akiva.x"
name = "X"
version = "0.1.0"
version_code = 1
min_abi_version = "0.1.0"
[runtime]
wasm = "x.wasm"
[[activities]]
id = "main"
entry = true
[[activities]]
id = "main"
"#;
        assert!(matches!(parse(src), Err(ManifestError::DuplicateActivityId(_))));
    }

    #[test]
    fn bad_app_id_rejected() {
        let bad = ["", "com", "com..x", "Com.Akiva.X", "1com.akiva", "com akiva"];
        for id in bad {
            let src = format!(
                r#"
[app]
id = "{id}"
name = "x"
version = "0.1.0"
version_code = 1
min_abi_version = "0.1.0"
[runtime]
wasm = "x.wasm"
[[activities]]
id = "m"
entry = true
"#
            );
            assert!(parse(&src).is_err(), "id '{id}' should be rejected");
        }
    }

    #[test]
    fn bad_semver_rejected() {
        let src = r#"
[app]
id = "com.akiva.x"
name = "X"
version = "0.1"
version_code = 1
min_abi_version = "0.1.0"
[runtime]
wasm = "x.wasm"
[[activities]]
id = "m"
entry = true
"#;
        assert!(matches!(parse(src), Err(ManifestError::InvalidField { field: "app.version", .. })));
    }

    #[test]
    fn empty_intent_filter_rejected() {
        let src = r#"
[app]
id = "com.akiva.x"
name = "X"
version = "0.1.0"
version_code = 1
min_abi_version = "0.1.0"
[runtime]
wasm = "x.wasm"
[[activities]]
id = "m"
entry = true
[[activities.intent_filters]]
actions = []
"#;
        assert!(matches!(parse(src), Err(ManifestError::EmptyIntentFilter { .. })));
    }

    #[test]
    fn launch_mode_defaults_to_standard() {
        let src = r#"
[app]
id = "com.akiva.x"
name = "X"
version = "0.1.0"
version_code = 1
min_abi_version = "0.1.0"
[runtime]
wasm = "x.wasm"
[[activities]]
id = "m"
entry = true
"#;
        let m = parse(src).unwrap();
        assert_eq!(m.activities[0].launch_mode, LaunchMode::Standard);
    }

    #[test]
    fn unknown_launch_mode_rejected() {
        let src = r#"
[app]
id = "com.akiva.x"
name = "X"
version = "0.1.0"
version_code = 1
min_abi_version = "0.1.0"
[runtime]
wasm = "x.wasm"
[[activities]]
id = "m"
entry = true
launch_mode = "weird"
"#;
        assert!(matches!(parse(src), Err(ManifestError::InvalidField { field: "activity.launch_mode", .. })));
    }

    #[test]
    fn prerelease_semver_accepted() {
        let src = r#"
[app]
id = "com.akiva.x"
name = "X"
version = "0.1.0-alpha"
version_code = 1
min_abi_version = "0.1.0"
[runtime]
wasm = "x.wasm"
[[activities]]
id = "m"
entry = true
"#;
        parse(src).expect("prerelease semver should parse");
    }
}
