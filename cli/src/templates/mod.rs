//! Scaffold templates for `akiva new --template <name>`.
//!
//! Each template is a self-contained renderer: given the derived
//! [`Names`] (crate name, package id, class, ...) it produces the full
//! file tree for a runnable starter app. Three flavors ship today:
//!
//!   * [`Template::Default`] — minimal counter using the programmatic
//!     `Ui` builder. Same shape as `apps/hello`. Good first read.
//!   * [`Template::ComposeStyle`] — declarative AKML layout with
//!     `Bindings`, `@color/` / `@string/` resource lookups, and a
//!     `values-night/` overlay so dark mode flips automatically.
//!     Good first read for app authors who already know Compose / SwiftUI.
//!   * [`Template::Game`] — animation loop on the raw [`Canvas`] with
//!     pointer-driven physics. No widget toolkit. Good first read for
//!     game / canvas-y workloads.
//!
//! Adding a fourth template is two changes: append a variant, point
//! [`Template::write_scaffold`] at a new submodule. The CLI's `--help`
//! text iterates [`Template::ALL`] so the new option appears for free.

use std::io::{self, Write};
use std::path::Path;

use crate::new::Names;

mod compose_style;
mod default_;
mod game;

/// Discriminator for the three scaffold flavours `akiva new` can emit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Template {
    Default,
    ComposeStyle,
    Game,
}

impl Template {
    /// Templates exposed in the `--template` CLI flag, in display order.
    /// `Default` is first so help output lists it as the recommendation.
    pub const ALL: &'static [Template] = &[
        Template::Default,
        Template::ComposeStyle,
        Template::Game,
    ];

    /// CLI flag name. Stable contract — appears in `--help` and in
    /// generated README snippets, so tests pin the spelling.
    pub fn slug(self) -> &'static str {
        match self {
            Template::Default      => "default",
            Template::ComposeStyle => "compose-style",
            Template::Game         => "game",
        }
    }

    /// One-line summary printed in `akiva new --help` next to each
    /// template name. Two screens of text would help nobody — anyone
    /// who needs more reads the generated README.
    pub fn description(self) -> &'static str {
        match self {
            Template::Default      => "Minimal counter (programmatic Ui builder).",
            Template::ComposeStyle => "Declarative AKML layout + resource theming + dark mode.",
            Template::Game         => "Animation loop on raw Canvas with pointer physics.",
        }
    }

    /// Parse `--template <s>`. Accepts the slugs in [`Template::ALL`];
    /// every other input is rejected with an error that lists the
    /// valid options. Underscores are tolerated as a friendly synonym
    /// for dashes (`compose_style` ≡ `compose-style`).
    pub fn parse(s: &str) -> Result<Self, String> {
        let normalised = s.replace('_', "-");
        for t in Self::ALL {
            if t.slug().eq_ignore_ascii_case(&normalised) {
                return Ok(*t);
            }
        }
        let valid: Vec<&str> = Self::ALL.iter().map(|t| t.slug()).collect();
        Err(format!(
            "unknown template '{s}'. Valid: {}",
            valid.join(", "),
        ))
    }

    /// Write the template's files under `app_dir`. The caller owns the
    /// pre-existence check and `--force` semantics; this fn assumes
    /// `app_dir` exists or can be created and that any prior contents
    /// have already been removed.
    pub fn write_scaffold(self, app_dir: &Path, n: &Names) -> io::Result<()> {
        match self {
            Template::Default      => default_::write(app_dir, n),
            Template::ComposeStyle => compose_style::write(app_dir, n),
            Template::Game         => game::write(app_dir, n),
        }
    }
}

/// Shared `Cargo.toml` body — every template consumes the SDK by path
/// and emits a `cdylib`. Description is templated per app so `cargo
/// publish` (eventual) doesn't trip over duplicates.
pub(crate) fn render_cargo_toml(n: &Names, body_one_liner: &str) -> String {
    format!(
r#"[package]
name = "{crate_name}"
version = "0.1.0"
edition = "2021"
rust-version = "1.82"
license = "MIT"
description = "{display} — {body}"

# The workspace excludes apps/, so this is a standalone crate. It still
# pulls akiva-sdk by path — that's the dev-loop shape; a registry path
# (akiva-sdk = "0.1") lands when there's a registry to pull from.
[lib]
crate-type = ["cdylib"]

[dependencies]
akiva-sdk = {{ path = "../../sdk-rust" }}

[profile.release]
opt-level = "z"
lto = true
codegen-units = 1
panic = "abort"
strip = true
"#,
        crate_name = n.crate_name,
        display = n.display_name,
        body = body_one_liner,
    )
}

/// Shared `manifest.toml` skeleton. Templates pass in any extra
/// `[permissions]` they need (storage, etc.); the rest is identical.
pub(crate) fn render_manifest(n: &Names, required_perms: &[&str]) -> String {
    let perms = if required_perms.is_empty() {
        "required = []".to_string()
    } else {
        let quoted: Vec<String> = required_perms.iter().map(|p| format!("\"{p}\"")).collect();
        format!("required = [{}]", quoted.join(", "))
    };
    format!(
r#"[app]
id = "{package_id}"
name = "{display}"
version = "0.1.0"
version_code = 1
min_abi_version = "0.4.0"
description = "Scaffold created by akiva new."
category = "utilities"

[runtime]
wasm = "{wasm_stem}.wasm"
min_memory_mb = 1
max_memory_mb = 4

[[activities]]
id = "main"
class = "{class}"
entry = true
launch_mode = "singleTop"
exported = true

[[activities.intent_filters]]
actions = ["akiva.intent.action.MAIN"]
categories = ["akiva.intent.category.LAUNCHER"]

[permissions]
{perms}
optional = []

[display]
default_width_dp = 400
default_height_dp = 600
resizable = true
"#,
        package_id = n.package_id,
        display = n.display_name,
        wasm_stem = n.wasm_stem,
        class = n.class,
    )
}

/// Convenience for templates that share the trivial gitignore.
pub(crate) const GITIGNORE: &str = "target/\nCargo.lock\n";

/// Shared file-writer used by every template renderer. Creates parent
/// dirs implicitly via the caller's `create_dir_all`; this fn just
/// stamps the bytes. Errors propagate; the caller is responsible for
/// best-effort cleanup if a partial write needs to be undone.
pub(crate) fn write_file(path: &Path, content: &str) -> io::Result<()> {
    let mut f = std::fs::File::create(path)?;
    f.write_all(content.as_bytes())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn scratch_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "akiva-cli-tpl-{tag}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn names(dir: &str) -> Names {
        Names::from_input(dir).unwrap()
    }

    #[test]
    fn parse_accepts_all_slugs() {
        assert_eq!(Template::parse("default").unwrap(), Template::Default);
        assert_eq!(Template::parse("compose-style").unwrap(), Template::ComposeStyle);
        assert_eq!(Template::parse("game").unwrap(), Template::Game);
    }

    #[test]
    fn parse_accepts_underscore_synonym() {
        assert_eq!(Template::parse("compose_style").unwrap(), Template::ComposeStyle);
    }

    #[test]
    fn parse_is_case_insensitive() {
        assert_eq!(Template::parse("Default").unwrap(), Template::Default);
        assert_eq!(Template::parse("GAME").unwrap(), Template::Game);
    }

    #[test]
    fn parse_rejects_unknown_with_listing() {
        let err = Template::parse("react").unwrap_err();
        assert!(err.contains("react"));
        assert!(err.contains("default"));
        assert!(err.contains("compose-style"));
        assert!(err.contains("game"));
    }

    #[test]
    fn slug_round_trips_through_parse() {
        for t in Template::ALL {
            assert_eq!(Template::parse(t.slug()).unwrap(), *t);
        }
    }

    /// Default template lays down the minimal four-file tree
    /// (Cargo.toml + manifest.toml + src/lib.rs + .gitignore + README.md)
    /// and every file substitutes the derived names.
    #[test]
    fn default_template_writes_minimal_tree() {
        let dir = scratch_dir("default");
        let n = names("my-app");
        Template::Default.write_scaffold(&dir, &n).unwrap();

        for f in ["Cargo.toml", "manifest.toml", "src/lib.rs", ".gitignore", "README.md"] {
            assert!(dir.join(f).exists(), "default template missing {f}");
        }

        let cargo = std::fs::read_to_string(dir.join("Cargo.toml")).unwrap();
        assert!(cargo.contains("akiva-app-my-app"));
        let manifest = std::fs::read_to_string(dir.join("manifest.toml")).unwrap();
        assert!(manifest.contains("com.example.my.app"));
        assert!(manifest.contains("MyAppMain"));
        let lib = std::fs::read_to_string(dir.join("src/lib.rs")).unwrap();
        assert!(lib.contains("struct MyAppMain"));
        assert!(lib.contains("akiva_sdk::akiva_app!(MyAppMain)"));
        // Default template uses the programmatic Ui builder, not AKML.
        assert!(!lib.contains("akml::inflate"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Compose-style template adds res/{layout,values,values-night} and
    /// the lib.rs uses the AKML inflater.
    #[test]
    fn compose_style_template_lays_down_resource_dirs() {
        let dir = scratch_dir("compose");
        let n = names("notes");
        Template::ComposeStyle.write_scaffold(&dir, &n).unwrap();

        for f in [
            "Cargo.toml", "manifest.toml", "src/lib.rs", ".gitignore", "README.md",
            "res/layout/main.akml",
            "res/values/colors.toml",
            "res/values/strings.toml",
            "res/values-night/colors.toml",
        ] {
            assert!(dir.join(f).exists(), "compose-style missing {f}");
        }

        let lib = std::fs::read_to_string(dir.join("src/lib.rs")).unwrap();
        assert!(lib.contains("akml::inflate_str_with"));
        assert!(lib.contains("Bindings::new()"));
        assert!(lib.contains("res::color"));

        let strings = std::fs::read_to_string(dir.join("res/values/strings.toml")).unwrap();
        assert!(strings.contains("app_name"));
        assert!(strings.contains("Notes"));

        // Manifest grants STORAGE_SANDBOX so saved-state survives — the
        // template's selling point.
        let manifest = std::fs::read_to_string(dir.join("manifest.toml")).unwrap();
        assert!(manifest.contains("STORAGE_SANDBOX"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Game template skips res/ entirely and the lib.rs paints directly
    /// to the canvas inside an animation loop.
    #[test]
    fn game_template_omits_resource_dirs() {
        let dir = scratch_dir("game");
        let n = names("pong");
        Template::Game.write_scaffold(&dir, &n).unwrap();

        for f in ["Cargo.toml", "manifest.toml", "src/lib.rs", ".gitignore", "README.md"] {
            assert!(dir.join(f).exists(), "game missing {f}");
        }
        assert!(!dir.join("res").exists(), "game template should not lay down res/");

        let lib = std::fs::read_to_string(dir.join("src/lib.rs")).unwrap();
        // Animation hallmarks: timestamp_ms read, dt-driven physics,
        // pointer-down impulse, no widget toolkit.
        assert!(lib.contains("ctx.timestamp_ms"));
        assert!(lib.contains("PointerPhase::Down"));
        assert!(lib.contains("c.fill_round_rect"));
        assert!(!lib.contains("Ui::new"));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
