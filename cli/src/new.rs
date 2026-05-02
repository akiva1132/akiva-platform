//! App scaffolding.
//!
//! `akiva new <name>` creates a fresh app crate under `apps/<name>/`
//! with a minimal-but-runnable setup. Three templates ship today
//! (default, compose-style, game); the orchestration here parses
//! flags + derives names + picks an output directory, then defers all
//! file generation to the chosen [`Template`].
//!
//! The goal is that an outsider can clone the repo, run
//! `akiva new notes`, and see a new "Notes" tile on the Shell within
//! the minute. Every file the templates write is what a seasoned
//! akiva app author would produce by hand — not a stub to be filled in
//! later.

use std::process::ExitCode;

use crate::templates::Template;
use crate::workspace;

pub fn run(args: &[String]) -> ExitCode {
    let mut force = false;
    let mut name: Option<String> = None;
    let mut template = Template::Default;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--force" | "-f" => { force = true; i += 1; }
            "--template" | "-t" => {
                let Some(val) = args.get(i + 1) else {
                    eprintln!("akiva new: --template requires a value. Try 'akiva new --help'.");
                    return ExitCode::from(2);
                };
                template = match Template::parse(val) {
                    Ok(t) => t,
                    Err(msg) => { eprintln!("akiva new: {msg}"); return ExitCode::from(2); }
                };
                i += 2;
            }
            "--help" | "-h" => {
                print_help();
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

    // Validate + derive the names the templates substitute. Reject
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

    match template.write_scaffold(&app_dir, &names) {
        Ok(()) => {
            println!("akiva new: created {} (template: {})", app_dir.display(), template.slug());
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

fn print_help() {
    println!("akiva new — scaffold a new app under apps/<name>/\n");
    println!("USAGE: akiva new <name> [--template <name>] [--force]\n");
    println!("  <name>                  Short name (lowercase, dash-or-underscore separated).");
    println!("                          Used for the crate name (akiva-app-<name>),");
    println!("                          the package id (com.example.<name>), and the");
    println!("                          activity class (<Name>Main).\n");
    println!("  -t, --template <name>   Template to scaffold. Default: default.");
    for t in Template::ALL {
        println!("                            {:<14}  {}", t.slug(), t.description());
    }
    println!();
    println!("  -f, --force             Overwrite an existing apps/<name>/ directory.");
}

/// Derived names used throughout the scaffold. Kept together so the
/// template never reaches for a transformation it couldn't do itself.
pub struct Names {
    /// Directory name under `apps/`. Same as raw input after
    /// normalisation: lowercase, dashes only.
    pub dir: String,
    /// Cargo crate name — `akiva-app-<dir>`. Dashes are Cargo's
    /// preferred separator.
    pub crate_name: String,
    /// Wasm artifact stem — `akiva_app_<underscore_name>`. Matches
    /// what cargo produces from a crate with dashes.
    pub wasm_stem: String,
    /// Android-style reverse-domain package id. We pick
    /// `com.example.<name>` as the placeholder; the author will
    /// usually change this to their own domain before publishing.
    pub package_id: String,
    /// Activity class name — CamelCase of the input plus `Main`.
    pub class: String,
    /// Human-facing app name for the manifest's `[app] name` and
    /// the Shell's launcher entry. Title-cased from the input.
    pub display_name: String,
}

impl Names {
    pub fn from_input(raw: &str) -> Result<Self, String> {
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
