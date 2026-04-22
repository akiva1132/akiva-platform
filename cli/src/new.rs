//! App scaffolding.
//!
//! `akiva new <name>` creates a fresh app crate under `apps/<name>/`
//! with a minimal-but-runnable setup: Cargo.toml wired to the SDK, a
//! manifest.toml carrying a placeholder package id, and a `src/lib.rs`
//! that paints a centred greeting. The result is immediately buildable
//! via `akiva build <name>` and installable via `akiva run`.
//!
//! The goal is that an outsider can clone the repo, run
//! `akiva new notes`, and see a new "Notes" tile on the Shell within
//! the minute. Every file written here is what a seasoned akiva app
//! author would produce by hand — not a stub to be filled in later.

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use crate::workspace;

pub fn run(args: &[String]) -> ExitCode {
    let mut force = false;
    let mut name: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--force" | "-f" => { force = true; i += 1; }
            "--help" | "-h" => {
                println!("akiva new — scaffold a new app under apps/<name>/\n");
                println!("USAGE: akiva new <name> [--force]\n");
                println!("  <name>    Short name (lowercase, dash-or-underscore separated).");
                println!("            Used for the crate name (akiva-app-<name>),");
                println!("            the package id (com.example.<name>), and the");
                println!("            activity class (<Name>Main).\n");
                println!("  --force   Overwrite an existing apps/<name>/ directory.");
                return ExitCode::SUCCESS;
            }
            other if other.starts_with('-') => {
                eprintln!("akiva new: unknown flag '{other}'");
                return ExitCode::from(2);
            }
            other => {
                if name.is_some() {
                    eprintln!("akiva new: extra positional arg '{other}' (only one name allowed)");
                    return ExitCode::from(2);
                }
                name = Some(other.into());
                i += 1;
            }
        }
    }

    let Some(raw_name) = name else {
        eprintln!("akiva new: missing <name>. Try 'akiva new --help'.");
        return ExitCode::from(2);
    };

    // Validate + derive the four names the template substitutes. Reject
    // inputs that can't produce a valid crate/class/id rather than
    // silently munging — a bad name is almost always a typo the author
    // would rather fix now than later.
    let names = match Names::from_input(&raw_name) {
        Ok(n) => n,
        Err(msg) => { eprintln!("akiva new: {msg}"); return ExitCode::from(2); }
    };

    let root = match workspace::find_root_from_cwd() {
        Ok(r) => r,
        Err(e) => { eprintln!("akiva new: {e}"); return ExitCode::FAILURE; }
    };

    let app_dir = root.join("apps").join(&names.dir);
    if app_dir.exists() {
        if !force {
            eprintln!(
                "akiva new: {} already exists. Re-run with --force to overwrite.",
                app_dir.display(),
            );
            return ExitCode::from(2);
        }
        if let Err(e) = std::fs::remove_dir_all(&app_dir) {
            eprintln!("akiva new: could not remove existing dir: {e}");
            return ExitCode::FAILURE;
        }
    }

    match write_scaffold(&app_dir, &names) {
        Ok(()) => {
            println!("akiva new: created {}", app_dir.display());
            println!("  crate:       {}", names.crate_name);
            println!("  package id:  {}", names.package_id);
            println!("  class:       {}", names.class);
            println!();
            println!("Next:");
            println!("  akiva build {}", names.dir);
            println!("  akiva run              # builds everything + serves");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("akiva new: writing {}: {e}", app_dir.display());
            // Best-effort cleanup — leaving a half-written directory
            // makes the next `akiva new` fail confusingly. If removal
            // itself fails (permissions?), we swallow it because the
            // primary error is already reported.
            let _ = std::fs::remove_dir_all(&app_dir);
            ExitCode::FAILURE
        }
    }
}

/// Derived names used throughout the scaffold. Kept together so the
/// template never reaches for a transformation it couldn't do itself.
struct Names {
    /// Directory name under `apps/`. Same as raw input after
    /// normalisation: lowercase, dashes only.
    dir: String,
    /// Cargo crate name — `akiva-app-<dir>`. Dashes are Cargo's
    /// preferred separator.
    crate_name: String,
    /// Wasm artifact stem — `akiva_app_<underscore_name>`. Matches
    /// what cargo produces from a crate with dashes.
    wasm_stem: String,
    /// Android-style reverse-domain package id. We pick
    /// `com.example.<name>` as the placeholder; the author will
    /// usually change this to their own domain before publishing.
    package_id: String,
    /// Activity class name — CamelCase of the input plus `Main`.
    class: String,
    /// Human-facing app name for the manifest's `[app] name` and
    /// the Shell's launcher entry. Title-cased from the input.
    display_name: String,
}

impl Names {
    fn from_input(raw: &str) -> Result<Self, String> {
        if raw.is_empty() {
            return Err("name is empty".into());
        }
        // Accept `-` and `_` as separators; reject anything else.
        // Start character must be a letter — matches the crate-name
        // rules and keeps the generated identifiers unambiguous.
        let first = raw.chars().next().unwrap();
        if !first.is_ascii_alphabetic() {
            return Err(format!("name must start with a letter (got {first:?})"));
        }
        for c in raw.chars() {
            let ok = c.is_ascii_alphanumeric() || c == '-' || c == '_';
            if !ok {
                return Err(format!("name contains invalid character {c:?} — only letters, digits, '-' and '_' allowed"));
            }
        }

        // Lowercase, single-separator canonical form for the directory
        // name. Cargo is happy with dashes; we pick them consistently.
        let dir: String = raw.to_ascii_lowercase().replace('_', "-");
        let crate_name = format!("akiva-app-{dir}");
        let wasm_stem = format!("akiva_app_{}", dir.replace('-', "_"));
        let package_id = format!("com.example.{}", dir.replace('-', "."));
        let class = to_camel(&dir) + "Main";
        let display_name = to_title(&dir);

        Ok(Self { dir, crate_name, wasm_stem, package_id, class, display_name })
    }
}

/// Convert `kebab-or-snake_case` to `CamelCase`. Drops the separator
/// and capitalises the next letter — the standard Rust identifier
/// transform.
fn to_camel(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut capitalise_next = true;
    for c in s.chars() {
        if c == '-' || c == '_' {
            capitalise_next = true;
        } else if capitalise_next {
            out.extend(c.to_ascii_uppercase().to_string().chars());
            capitalise_next = false;
        } else {
            out.push(c);
        }
    }
    out
}

/// Human-readable variant of `to_camel` — separates words with spaces
/// instead of joining them. Used for the manifest's `[app] name`.
fn to_title(s: &str) -> String {
    let words: Vec<String> = s
        .split(|c: char| c == '-' || c == '_')
        .filter(|w| !w.is_empty())
        .map(|w| {
            let mut it = w.chars();
            match it.next() {
                Some(c) => c.to_ascii_uppercase().to_string() + it.as_str(),
                None => String::new(),
            }
        })
        .collect();
    words.join(" ")
}

fn write_scaffold(app_dir: &Path, n: &Names) -> io::Result<()> {
    std::fs::create_dir_all(app_dir.join("src"))?;
    write_file(&app_dir.join("Cargo.toml"), &render_cargo_toml(n))?;
    write_file(&app_dir.join("manifest.toml"), &render_manifest(n))?;
    write_file(&app_dir.join("src/lib.rs"), &render_lib_rs(n))?;
    // `.gitignore` for the app-local `target/` — Cargo creates it on
    // first build and we don't want it tracked. One line, one reason.
    write_file(&app_dir.join(".gitignore"), "target/\n")?;
    Ok(())
}

fn write_file(path: &PathBuf, content: &str) -> io::Result<()> {
    let mut f = std::fs::File::create(path)?;
    f.write_all(content.as_bytes())?;
    Ok(())
}

fn render_cargo_toml(n: &Names) -> String {
    // Mirrors apps/hello/Cargo.toml — the release profile lives in the
    // app crate (not the workspace) because apps are excluded from the
    // workspace and need their own wasm-optimised settings. Keeping
    // them local also means an outsider's fork has one file to tune.
    format!(
r#"[package]
name = "{crate_name}"
version = "0.1.0"
edition = "2021"
rust-version = "1.82"
license = "MIT OR Apache-2.0"
description = "{display} — Akiva app."

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
    )
}

fn render_manifest(n: &Names) -> String {
    // Minimum runnable manifest: one entry activity filtered to
    // MAIN+LAUNCHER so the Shell picks it up. Default memory quotas
    // match hello's. The `wasm` field under [runtime] is cosmetic for
    // the dev-install path (the file always lands as `app.wasm`), but
    // keeping it honest lets future package formats reuse the name.
    format!(
r#"[app]
id = "{package_id}"
name = "{display}"
version = "0.1.0"
version_code = 1
min_abi_version = "0.1.0"
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
required = []
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

fn render_lib_rs(n: &Names) -> String {
    // The scaffold shows the three things every new author will want
    // to change first: state on the struct, click dispatch in
    // on_render, and the on_save_state/restore plumbing. Keeping them
    // all present (even as no-ops for the counter) means the first
    // meaningful edit doesn't require learning the rest of the API at
    // once.
    format!(
r#"//! {display} — scaffolded by `akiva new`.
//!
//! The Activity trait is your contract with the runtime. Minimum
//! override is `on_render`; everything else has sensible defaults.

use akiva_sdk::prelude::*;

#[derive(Default)]
pub struct {class} {{
    ui: UiState,
    count: i32,
}}

impl Activity for {class} {{
    fn on_create(&mut self, ctx: &mut Context<'_>) {{
        log_info!("{class} created ({{}}x{{}} @ {{:.1}}x)",
            ctx.metrics.width_px, ctx.metrics.height_px, ctx.metrics.density);
    }}

    fn on_pointer(&mut self, _ctx: &mut Context<'_>, ev: PointerEvent) {{
        // Feed the UI state so it can correlate pointer-down events
        // with the widgets laid out during the next `present()`.
        self.ui.feed_pointer(ev);
    }}

    fn on_render(&mut self, ctx: &mut Context<'_>, c: &mut Canvas<'_>) {{
        c.clear(0xff0f1420);
        let mut ui = Ui::new(&mut self.ui, ctx, c);

        let title = ui.text("{display}").size(28.0).color(0xfff0f4fa).build();
        let counter = ui.text(&format!("count = {{}}", self.count))
            .size(20.0).color(0xffe0e6f0).build();
        let inc = ui.button("inc", "+1");

        let root = ui.v_stack(&[title, counter, inc])
            .gap(16.0).padding(24.0).build();
        ui.present(root);

        if ui.was_clicked("inc") {{ self.count += 1; }}
    }}

    fn on_save_state(&self, state: &mut SavedState) {{
        state.put_int("count", self.count as i64);
    }}
    fn on_restore_state(&mut self, state: &SavedState) {{
        if let Some(v) = state.get_int("count") {{
            self.count = v as i32;
        }}
    }}
}}

akiva_sdk::akiva_app!({class});
"#,
        display = n.display_name,
        class = n.class,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_from_dashed_input() {
        let n = Names::from_input("my-notes").unwrap();
        assert_eq!(n.dir,          "my-notes");
        assert_eq!(n.crate_name,   "akiva-app-my-notes");
        assert_eq!(n.wasm_stem,    "akiva_app_my_notes");
        assert_eq!(n.package_id,   "com.example.my.notes");
        assert_eq!(n.class,        "MyNotesMain");
        assert_eq!(n.display_name, "My Notes");
    }

    #[test]
    fn names_from_underscored_input_normalises_to_dashes() {
        let n = Names::from_input("my_app").unwrap();
        assert_eq!(n.dir,        "my-app");
        assert_eq!(n.crate_name, "akiva-app-my-app");
    }

    #[test]
    fn names_rejects_leading_digit() {
        assert!(Names::from_input("1app").is_err());
    }

    #[test]
    fn names_rejects_empty() {
        assert!(Names::from_input("").is_err());
    }

    #[test]
    fn names_rejects_special_characters() {
        assert!(Names::from_input("my app").is_err());
        assert!(Names::from_input("my.app").is_err());
        assert!(Names::from_input("my/app").is_err());
    }

    #[test]
    fn to_camel_capitalises_every_word() {
        assert_eq!(to_camel("my-notes"), "MyNotes");
        assert_eq!(to_camel("one"),      "One");
        assert_eq!(to_camel("a-b-c"),    "ABC");
    }

    #[test]
    fn to_title_is_space_separated_camel() {
        assert_eq!(to_title("my-notes"), "My Notes");
        assert_eq!(to_title("hello"),    "Hello");
    }
}
