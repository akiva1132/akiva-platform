//! `akiva uninstall <id-or-dir>` — remove a staged app from the dev runtime.
//!
//! The counterpart to [`crate::install`]. Scans subdirectories of
//! `runtime/web/dev-apps/`, reads each one's `manifest.toml` to extract
//! the package id, then removes whichever directory matches the user's
//! argument — either by short directory name (e.g. `calculator`) or by
//! reverse-domain id (`com.akiva.calculator`). Exactly one match has to
//! resolve: zero means nothing to remove, more than one would be an
//! ambiguous operation the CLI refuses to guess about.
//!
//! Scope note: this removes the dev-apps stage only. Packages the
//! browser already pulled into OPFS stay installed until the user
//! clears site storage, the browser console invokes
//! `PackageManager::uninstall` via a kernel-side bridge, or a future
//! runtime bootstrapping step reconciles OPFS with `dev-apps/index.json`
//! and drops missing entries. For now, the common dev flow (remove the
//! staged dir → refresh → Shell stops offering the app) works because
//! the discovery list is derived from `dev-apps/` at boot.

use std::io;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use crate::workspace;

pub fn run(args: &[String]) -> ExitCode {
    let mut targets: Vec<String> = Vec::new();
    let mut runtime_root: Option<PathBuf> = None;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--help" | "-h" => { print_help(); return ExitCode::SUCCESS; }
            "--to" => {
                match args.get(i + 1) {
                    Some(s) => { runtime_root = Some(PathBuf::from(s)); i += 2; }
                    None => {
                        eprintln!("akiva uninstall: --to requires a path");
                        return ExitCode::from(2);
                    }
                }
            }
            other if other.starts_with("--") => {
                eprintln!("akiva uninstall: unknown flag '{other}'");
                return ExitCode::from(2);
            }
            name => { targets.push(name.into()); i += 1; }
        }
    }

    if targets.len() != 1 {
        eprintln!("akiva uninstall: expected exactly one <id-or-dir>, got {}", targets.len());
        return ExitCode::from(2);
    }
    let target = &targets[0];

    let runtime_root = match runtime_root {
        Some(p) => p,
        None => match workspace::find_root_from_cwd() {
            Ok(root) => root.join("runtime/web"),
            Err(e) => {
                eprintln!("akiva uninstall: {e}");
                return ExitCode::FAILURE;
            }
        },
    };

    match uninstall(&runtime_root, target) {
        Ok(removed) => {
            println!(
                "akiva uninstall: removed {} (package id: {}) from {}",
                removed.dir_name,
                removed.package_id.as_deref().unwrap_or("<unknown>"),
                removed.dest.display(),
            );
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("akiva uninstall: {e}");
            ExitCode::FAILURE
        }
    }
}

fn print_help() {
    println!("akiva uninstall — remove a staged app from runtime/web/dev-apps/\n");
    println!("USAGE: akiva uninstall <id-or-dir> [--to <runtime-web>]\n");
    println!("  <id-or-dir>   Reverse-domain id (com.akiva.calculator) or dir name (calculator)");
    println!("  --to <path>   Runtime web root (default: <workspace>/runtime/web)");
}

#[derive(Debug)]
struct Removed {
    dir_name: String,
    package_id: Option<String>,
    dest: PathBuf,
}

fn uninstall(runtime_root: &Path, target: &str) -> io::Result<Removed> {
    let dev_apps = runtime_root.join("dev-apps");
    if !dev_apps.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("no dev-apps tree at {}", dev_apps.display()),
        ));
    }

    let mut matches: Vec<(PathBuf, String, Option<String>)> = Vec::new();
    for entry in std::fs::read_dir(&dev_apps)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() { continue; }
        let dir_name = match entry.file_name().into_string() {
            Ok(s) => s,
            Err(_) => continue,  // non-UTF8 dir name — ignore, can't address it anyway
        };
        // Skip hidden dirs so `.cache` and similar never become
        // uninstall targets by accident.
        if dir_name.starts_with('.') { continue; }

        let dir_path = entry.path();
        let package_id = read_manifest_id(&dir_path.join("manifest.toml"));

        // Match either form. Short-circuit on the first hit per dir so
        // an apparent "both dir and id match" counts once.
        let hits =
            dir_name == target
            || package_id.as_deref() == Some(target);
        if hits {
            matches.push((dir_path, dir_name, package_id));
        }
    }

    match matches.len() {
        0 => Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("no dev-app matches {target:?} under {}", dev_apps.display()),
        )),
        1 => {
            let (dir_path, dir_name, package_id) = matches.pop().unwrap();
            std::fs::remove_dir_all(&dir_path)?;
            Ok(Removed { dir_name, package_id, dest: dir_path })
        }
        n => {
            // Build a useful hint: list every ambiguous candidate so the
            // user can pick a more specific argument.
            let mut listing = String::new();
            for (p, name, id) in &matches {
                listing.push_str(&format!(
                    "\n  - {} (id: {}) at {}",
                    name,
                    id.as_deref().unwrap_or("<unknown>"),
                    p.display(),
                ));
            }
            Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{target:?} matches {n} dev-apps — be more specific:{listing}"),
            ))
        }
    }
}

/// Pull the `id = "..."` field out of a manifest.toml if present. Same
/// approach used in [`crate::install`] — a real TOML parser is overkill
/// for a single field we control the format of. Missing file or absent
/// id returns None; we never error out of discovery because one
/// malformed neighbour shouldn't block uninstalling a healthy one.
fn read_manifest_id(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    for line in text.lines() {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("id") {
            let rest = rest.trim_start().trim_start_matches('=').trim();
            if let Some(stripped) = rest.strip_prefix('"').and_then(|s| s.strip_suffix('"')) {
                if !stripped.is_empty() {
                    return Some(stripped.to_string());
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch_runtime(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("akiva-cli-uninstall-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn stage_app(runtime_root: &Path, dir_name: &str, id: &str) {
        let d = runtime_root.join("dev-apps").join(dir_name);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(
            d.join("manifest.toml"),
            format!("[app]\nid = \"{id}\"\nname = \"x\"\n"),
        ).unwrap();
        std::fs::write(d.join("app.wasm"), b"\0asm\x01\x00\x00\x00").unwrap();
    }

    #[test]
    fn uninstall_by_dir_name() {
        let rt = scratch_runtime("dir");
        stage_app(&rt, "calculator", "com.akiva.calculator");
        stage_app(&rt, "hello", "com.akiva.hello");

        let removed = uninstall(&rt, "calculator").unwrap();
        assert_eq!(removed.dir_name, "calculator");
        assert_eq!(removed.package_id.as_deref(), Some("com.akiva.calculator"));
        assert!(!rt.join("dev-apps/calculator").exists());
        // The other staged app is untouched.
        assert!(rt.join("dev-apps/hello/manifest.toml").is_file());

        let _ = std::fs::remove_dir_all(&rt);
    }

    #[test]
    fn uninstall_by_package_id() {
        let rt = scratch_runtime("id");
        stage_app(&rt, "calc", "com.akiva.calculator");
        stage_app(&rt, "hello", "com.akiva.hello");

        let removed = uninstall(&rt, "com.akiva.calculator").unwrap();
        assert_eq!(removed.dir_name, "calc");
        assert!(!rt.join("dev-apps/calc").exists());
        assert!(rt.join("dev-apps/hello").exists());
        let _ = std::fs::remove_dir_all(&rt);
    }

    #[test]
    fn uninstall_missing_returns_not_found() {
        let rt = scratch_runtime("missing");
        stage_app(&rt, "hello", "com.akiva.hello");
        let err = uninstall(&rt, "nothing-like-this").unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
        // The unrelated app wasn't touched.
        assert!(rt.join("dev-apps/hello").exists());
        let _ = std::fs::remove_dir_all(&rt);
    }

    #[test]
    fn uninstall_ambiguous_is_rejected() {
        // Two dirs where one's dir_name equals the other's id tail —
        // here, passing "calculator" matches dir A by name and dir B
        // via its id. The CLI refuses to guess.
        let rt = scratch_runtime("ambig");
        stage_app(&rt, "calculator", "com.akiva.calculator-alt");
        stage_app(&rt, "other-dir", "calculator");  // unusual id, but valid for the test
        let err = uninstall(&rt, "calculator").unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        // Neither dir was touched after an ambiguous-match abort.
        assert!(rt.join("dev-apps/calculator").exists());
        assert!(rt.join("dev-apps/other-dir").exists());
        let _ = std::fs::remove_dir_all(&rt);
    }

    #[test]
    fn uninstall_missing_dev_apps_errors() {
        // Fresh checkout with no apps staged at all.
        let rt = scratch_runtime("empty");
        let err = uninstall(&rt, "calculator").unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
        let _ = std::fs::remove_dir_all(&rt);
    }

    #[test]
    fn uninstall_ignores_hidden_dirs_and_malformed_manifests() {
        // `.cache` must never be uninstalled; a dir without a
        // manifest.toml is still addressable by its directory name
        // (common while an `akiva build` is mid-flight).
        let rt = scratch_runtime("edge");
        stage_app(&rt, "real", "com.akiva.real");
        std::fs::create_dir_all(rt.join("dev-apps/.cache")).unwrap();
        std::fs::create_dir_all(rt.join("dev-apps/broken")).unwrap();
        // No manifest.toml in `broken/`.

        // Asking for `.cache` should fail with NotFound (not silently
        // remove it).
        let err = uninstall(&rt, ".cache").unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
        assert!(rt.join("dev-apps/.cache").exists());

        // Asking for `broken` works — the id lookup returned None but
        // the dir-name match still holds.
        let removed = uninstall(&rt, "broken").unwrap();
        assert_eq!(removed.dir_name, "broken");
        assert!(removed.package_id.is_none());

        let _ = std::fs::remove_dir_all(&rt);
    }
}
