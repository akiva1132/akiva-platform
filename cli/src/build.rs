//! App build pipeline.
//!
//! Replaces the per-app `build.sh` scripts with one command. Given an
//! app directory under `apps/`, runs `cargo build --target
//! wasm32-unknown-unknown --release` and stages the output under
//! `runtime/web/dev-apps/{name}/` where `worker.js`'s dev-install hook
//! picks it up at next boot.
//!
//! The build.sh files were nearly identical — only the app name and
//! package name varied. Centralising here means new apps don't have to
//! copy a 40-line shell script, and the staging rules (which files move
//! where, how `res/values/` is refreshed) live in one place where a
//! bugfix lands once.

use std::io;
use std::path::Path;
use std::process::{Command, ExitCode};

use crate::workspace;

/// CLI entry point for `akiva build [names...] [--debug] [--all]`.
/// Wraps [`run_bool`] with the top-level `ExitCode` conversion; other
/// subcommands (`akiva run`) call `run_bool` directly so they can
/// branch on success without poking at `ExitCode`, which isn't
/// comparable.
pub fn run(args: &[String]) -> ExitCode {
    if run_bool(args) { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}

/// Positional args are app directory names (e.g. `hello`,
/// `calculator`). With no names and no `--all`, we default to building
/// every app under `apps/` — in practice the most useful shape since
/// the calculator's AKML or SDK change almost always wants every
/// dev-app re-staged.
///
/// Returns `true` on full success, `false` if any step failed. Help
/// and arg-error exits are also `true` (they're user-facing termination
/// states, not build failures the caller should chain past).
pub fn run_bool(args: &[String]) -> bool {
    let mut debug = false;
    let mut all = false;
    let mut names: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--debug" => { debug = true; i += 1; }
            "--release" => { debug = false; i += 1; }  // explicit, matches cargo default
            "--all" => { all = true; i += 1; }
            "--help" | "-h" => {
                println!("akiva build — compile apps to wasm and stage into runtime/web/dev-apps/\n");
                println!("USAGE: akiva build [names...] [--debug] [--all]\n");
                println!("Positional names are app directory names under apps/.");
                println!("With no names (and no --all), every app under apps/ is built.\n");
                println!("  --debug    Use the `dev` cargo profile (faster compile, larger wasm)");
                println!("  --release  Explicit opposite of --debug (this is the default)");
                println!("  --all      Equivalent to passing every app name explicitly");
                return true;
            }
            other if other.starts_with("--") => {
                eprintln!("akiva build: unknown flag '{other}'");
                return false;
            }
            name => { names.push(name.into()); i += 1; }
        }
    }

    let root = match workspace::find_root_from_cwd() {
        Ok(r) => r,
        Err(e) => { eprintln!("akiva build: {e}"); return false; }
    };

    // Resolve the final app list: explicit names if any, else everything.
    // `--all` is allowed together with names but behaves the same as the
    // name-less case (ignores positional args) — simpler than erroring.
    let apps_root = root.join("apps");
    let targets: Vec<String> = if names.is_empty() || all {
        match discover_apps(&apps_root) {
            Ok(v) if v.is_empty() => {
                eprintln!("akiva build: no apps found under {}", apps_root.display());
                return false;
            }
            Ok(v) => v,
            Err(e) => {
                eprintln!("akiva build: scanning {}: {e}", apps_root.display());
                return false;
            }
        }
    } else {
        names
    };

    let mut any_failed = false;
    for name in &targets {
        match build_one(&root, name, debug) {
            Ok(()) => {}
            Err(e) => {
                eprintln!("akiva build: {name}: {e}");
                any_failed = true;
            }
        }
    }
    !any_failed
}

/// List every directory under `apps/` that looks like a buildable crate.
/// The heuristic is "contains a Cargo.toml" — cheap and matches the
/// workspace convention exactly (`apps/*/Cargo.toml`).
fn discover_apps(apps_root: &Path) -> io::Result<Vec<String>> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(apps_root)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() { continue; }
        if entry.path().join("Cargo.toml").is_file() {
            if let Some(name) = entry.file_name().to_str() {
                out.push(name.to_string());
            }
        }
    }
    out.sort();  // stable ordering so logs diff cleanly across runs
    Ok(out)
}

/// Build a single app. Mirrors the old `apps/<name>/build.sh`:
///   1. `cargo build --target wasm32-unknown-unknown --profile (release|dev)`
///   2. Copy the produced `.wasm` to `runtime/web/dev-apps/<name>/app.wasm`.
///   3. Copy `manifest.toml` alongside it.
///   4. Refresh `res/values/{strings,colors}.toml` if present.
///
/// Each step produces a user-visible log line; failures are returned as
/// an `io::Error` so the caller's error-aggregation path handles them.
fn build_one(root: &Path, name: &str, debug: bool) -> io::Result<()> {
    let app_dir = root.join("apps").join(name);
    if !app_dir.join("Cargo.toml").is_file() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("no Cargo.toml at {}", app_dir.join("Cargo.toml").display()),
        ));
    }

    // Package name lives in the crate's Cargo.toml and becomes the
    // wasm artifact name (`package.name` with '-' → '_'). Cheap
    // line-scan parser — same rationale as workspace::find_root.
    let pkg_name = read_cargo_package_name(&app_dir.join("Cargo.toml"))?;
    let wasm_leaf = format!("{}.wasm", pkg_name.replace('-', "_"));

    let (profile_flag, profile_dir) = if debug { ("dev", "debug") } else { ("release", "release") };
    println!("[{name}] cargo build ({profile_flag})");

    let status = Command::new("cargo")
        .args([
            "build",
            "--target", "wasm32-unknown-unknown",
            "--profile", profile_flag,
        ])
        .current_dir(&app_dir)
        .status()?;
    if !status.success() {
        return Err(io::Error::new(io::ErrorKind::Other, "cargo build failed"));
    }

    // Apps excluded from the workspace carry their own target/ inside
    // the app directory (see root Cargo.toml). This mirrors the path
    // the old build.sh used — if cargo's layout changes upstream, this
    // is the single place to fix it.
    let wasm_path = app_dir
        .join("target").join("wasm32-unknown-unknown").join(profile_dir).join(&wasm_leaf);
    if !wasm_path.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("expected wasm artifact at {}", wasm_path.display()),
        ));
    }

    let dest_dir = root.join("runtime/web/dev-apps").join(name);
    std::fs::create_dir_all(&dest_dir)?;
    std::fs::copy(&wasm_path, dest_dir.join("app.wasm"))?;

    // Manifest is required; a missing one points at an incomplete
    // scaffold rather than a build glitch, so the error message says so.
    let manifest_src = app_dir.join("manifest.toml");
    if !manifest_src.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("{name}: missing manifest.toml (scaffold incomplete?)"),
        ));
    }
    std::fs::copy(&manifest_src, dest_dir.join("manifest.toml"))?;

    // Refresh res/values/ so deletions in the source tree propagate:
    // remove any existing staged copies first, then re-copy whichever
    // of strings/colors.toml actually exist. Mirrors the build.sh rule
    // that calculator added in its own file.
    let src_values = app_dir.join("res/values");
    let dst_values = dest_dir.join("res/values");
    if src_values.is_dir() {
        std::fs::create_dir_all(&dst_values)?;
        for leaf in ["strings.toml", "colors.toml"] {
            let dst = dst_values.join(leaf);
            if dst.exists() { std::fs::remove_file(&dst)?; }
            let src = src_values.join(leaf);
            if src.is_file() { std::fs::copy(&src, &dst)?; }
        }
    } else if dst_values.is_dir() {
        // App removed its res/values/ entirely — clean the stage dir
        // so stale strings/colors don't keep getting installed.
        let _ = std::fs::remove_dir_all(&dst_values);
    }

    let staged = dest_dir.join("app.wasm");
    let size = std::fs::metadata(&staged)?.len();
    println!("[{name}] staged {} ({} bytes)", staged.display(), size);
    Ok(())
}

/// Minimal Cargo.toml reader: pick `name = "…"` out of the `[package]`
/// table. A real TOML parser would cost a dependency we don't need for
/// a field we always control.
fn read_cargo_package_name(path: &Path) -> io::Result<String> {
    let text = std::fs::read_to_string(path)?;
    let mut in_package = false;
    for raw in text.lines() {
        let line = raw.trim();
        if line.starts_with('#') || line.is_empty() { continue; }
        if line.starts_with('[') {
            in_package = line == "[package]";
            continue;
        }
        if !in_package { continue; }
        // Expect `name = "value"`. Anything else in [package] (version,
        // edition, …) is ignored.
        if let Some(rest) = line.strip_prefix("name") {
            let rest = rest.trim_start().trim_start_matches('=').trim();
            if let Some(stripped) = rest.strip_prefix('"').and_then(|s| s.strip_suffix('"')) {
                return Ok(stripped.to_string());
            }
        }
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        format!("{}: no [package] name", path.display()),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_cargo_name_picks_package_name() {
        let dir = std::env::temp_dir().join("akiva-cli-build-test-1");
        let _ = std::fs::create_dir_all(&dir);
        let p = dir.join("Cargo.toml");
        std::fs::write(&p, r##"[package]
name = "my-app"
version = "0.1.0"

[dependencies]
foo = "1"
"##).unwrap();
        assert_eq!(read_cargo_package_name(&p).unwrap(), "my-app");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn read_cargo_name_ignores_name_outside_package_table() {
        // A `name` field inside `[dependencies]` (or any other table)
        // must NOT be picked up — only the `[package] name` counts.
        let dir = std::env::temp_dir().join("akiva-cli-build-test-2");
        let _ = std::fs::create_dir_all(&dir);
        let p = dir.join("Cargo.toml");
        std::fs::write(&p, r##"[dependencies]
name = "decoy"

[package]
name = "real-app"
"##).unwrap();
        assert_eq!(read_cargo_package_name(&p).unwrap(), "real-app");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn read_cargo_name_errors_when_missing() {
        let dir = std::env::temp_dir().join("akiva-cli-build-test-3");
        let _ = std::fs::create_dir_all(&dir);
        let p = dir.join("Cargo.toml");
        std::fs::write(&p, "[package]\nversion = \"0.1.0\"\n").unwrap();
        assert!(read_cargo_package_name(&p).is_err());
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn discover_apps_lists_only_directories_with_cargo_toml() {
        // Build a tiny fake `apps/` tree with one real app and one
        // "directory without Cargo.toml" to make sure the filter works.
        let dir = std::env::temp_dir().join("akiva-cli-discover-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("real-app")).unwrap();
        std::fs::write(dir.join("real-app/Cargo.toml"), "[package]\nname=\"x\"\n").unwrap();
        std::fs::create_dir_all(dir.join("junk-dir")).unwrap();
        let found = discover_apps(&dir).unwrap();
        assert_eq!(found, vec!["real-app".to_string()]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
