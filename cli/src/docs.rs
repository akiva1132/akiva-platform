//! `akiva docs` — surface the SDK's API reference.
//!
//! Wraps `cargo doc -p akiva-sdk --no-deps` and points the user at the
//! rendered HTML. By default it builds + opens the index page; flags
//! cover the dev loops we care about:
//!
//!   * `--no-open` — CI / scripted use; print the path instead of
//!     spawning a browser.
//!   * `--module <name>` — open straight at one module's page (e.g.
//!     `akiva docs --module ui` for the widget toolkit).
//!   * `--no-build` — skip the cargo invocation if you just want the
//!     URL of the most recent build.
//!
//! Why a wrapper at all when `cargo doc --open` exists. Two reasons:
//! the `-p akiva-sdk --no-deps` invocation is the right one for app
//! authors (skips host-side crates they shouldn't read), and a stable
//! `akiva docs` command is what the README and other docs can link to
//! without pinning the underlying cargo invocation.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use crate::workspace;

pub fn run(args: &[String]) -> ExitCode {
    let mut open = true;
    let mut build = true;
    let mut module: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--no-open"  => { open = false;  i += 1; }
            "--no-build" => { build = false; i += 1; }
            "--module" | "-m" => {
                let Some(val) = args.get(i + 1) else {
                    eprintln!("akiva docs: --module requires a value");
                    return ExitCode::from(2);
                };
                if !is_valid_module(val) {
                    eprintln!("akiva docs: --module '{val}' must be a Rust identifier");
                    return ExitCode::from(2);
                }
                module = Some(val.clone());
                i += 2;
            }
            "--help" | "-h" => { print_help(); return ExitCode::SUCCESS; }
            other if other.starts_with('-') => {
                eprintln!("akiva docs: unknown flag '{other}'");
                return ExitCode::from(2);
            }
            other => {
                eprintln!("akiva docs: unexpected positional arg '{other}'");
                return ExitCode::from(2);
            }
        }
    }

    let root = match workspace::find_root_from_cwd() {
        Ok(r) => r,
        Err(e) => { eprintln!("akiva docs: {e}"); return ExitCode::FAILURE; }
    };

    if build {
        println!("[akiva docs] cargo doc -p akiva-sdk --no-deps");
        let status = Command::new("cargo")
            .args(["doc", "-p", "akiva-sdk", "--no-deps"])
            .current_dir(&root)
            .status();
        match status {
            Ok(s) if s.success() => {}
            Ok(s) => {
                eprintln!("akiva docs: cargo doc failed (exit {})", s.code().unwrap_or(-1));
                return ExitCode::FAILURE;
            }
            Err(e) => {
                eprintln!("akiva docs: could not invoke cargo: {e}");
                return ExitCode::FAILURE;
            }
        }
    }

    let index_path = doc_index_path(&root, module.as_deref());
    if !index_path.exists() {
        eprintln!(
            "akiva docs: rendered index missing at {}\n\
            (re-run without --no-build, or check that module name '{}' exists)",
            index_path.display(),
            module.as_deref().unwrap_or(""),
        );
        return ExitCode::FAILURE;
    }

    let url = path_to_file_url(&index_path);
    if open {
        if let Err(e) = open_in_browser(&url) {
            // Falling back to printing the URL keeps the command useful
            // on headless / sandboxed boxes where xdg-open and friends
            // aren't installed.
            eprintln!("[akiva docs] could not open browser ({e}); URL below");
            println!("{url}");
        }
    } else {
        println!("{url}");
    }
    ExitCode::SUCCESS
}

fn print_help() {
    println!("akiva docs — open the akiva-sdk API reference\n");
    println!("USAGE: akiva docs [--module <name>] [--no-open] [--no-build]\n");
    println!("  -m, --module <name>   Open at a specific module page (e.g. ui, canvas, storage).");
    println!("      --no-open         Print the file:// URL instead of spawning a browser.");
    println!("      --no-build        Skip cargo doc; assume the artifact already exists.");
}

fn is_valid_module(s: &str) -> bool {
    !s.is_empty()
        && s.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Where rustdoc places `akiva-sdk`'s index.html. Cargo writes to the
/// workspace's `target/doc/<crate-name-with-underscores>/index.html`;
/// the SDK's crate name is `akiva-sdk`, so the directory is `akiva_sdk`.
/// A `--module <m>` deepens the path by one level.
fn doc_index_path(root: &Path, module: Option<&str>) -> PathBuf {
    let mut p = root.join("target").join("doc").join("akiva_sdk");
    if let Some(m) = module {
        p.push(m);
    }
    p.push("index.html");
    p
}

fn path_to_file_url(p: &Path) -> String {
    // Crude but adequate for local paths. Rustdoc never emits paths
    // with spaces or non-ASCII characters under target/doc/, so we don't
    // need percent-encoding here. If it ever does, this helper grows up.
    format!("file://{}", p.display())
}

#[cfg(target_os = "linux")]
fn open_in_browser(url: &str) -> std::io::Result<()> {
    let status = Command::new("xdg-open").arg(url).status()?;
    if status.success() { Ok(()) } else {
        Err(std::io::Error::new(std::io::ErrorKind::Other, "xdg-open exited non-zero"))
    }
}

#[cfg(target_os = "macos")]
fn open_in_browser(url: &str) -> std::io::Result<()> {
    let status = Command::new("open").arg(url).status()?;
    if status.success() { Ok(()) } else {
        Err(std::io::Error::new(std::io::ErrorKind::Other, "open exited non-zero"))
    }
}

#[cfg(target_os = "windows")]
fn open_in_browser(url: &str) -> std::io::Result<()> {
    // `cmd /C start` is the canonical "open with default handler" on
    // Windows. The empty title arg is required because `start` treats
    // the first quoted arg as a window title.
    let status = Command::new("cmd").args(["/C", "start", "", url]).status()?;
    if status.success() { Ok(()) } else {
        Err(std::io::Error::new(std::io::ErrorKind::Other, "start exited non-zero"))
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn open_in_browser(_url: &str) -> std::io::Result<()> {
    Err(std::io::Error::new(std::io::ErrorKind::Unsupported, "no browser-open shim for this OS"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn module_name_validation_accepts_simple_idents() {
        assert!(is_valid_module("ui"));
        assert!(is_valid_module("akml"));
        assert!(is_valid_module("storage"));
        assert!(is_valid_module("_underscored"));
        assert!(is_valid_module("with_digits9"));
    }

    #[test]
    fn module_name_validation_rejects_garbage() {
        assert!(!is_valid_module(""));
        assert!(!is_valid_module("9starts-with-digit"));
        assert!(!is_valid_module("has dash-here"));
        assert!(!is_valid_module("path/like"));
        assert!(!is_valid_module("../etc/passwd"));
    }

    #[test]
    fn doc_index_path_default_targets_sdk_index() {
        let p = doc_index_path(Path::new("/work/akiva"), None);
        assert!(p.ends_with("target/doc/akiva_sdk/index.html"));
    }

    #[test]
    fn doc_index_path_with_module_descends_into_module() {
        let p = doc_index_path(Path::new("/work/akiva"), Some("ui"));
        assert!(p.ends_with("target/doc/akiva_sdk/ui/index.html"));
    }

    #[test]
    fn path_to_file_url_emits_file_scheme() {
        let url = path_to_file_url(Path::new("/tmp/index.html"));
        assert_eq!(url, "file:///tmp/index.html");
    }
}
