//! `akiva package <app>` — pack a built app into a `.akv` archive.
//!
//! Reads the staged artifacts under `runtime/web/dev-apps/<app>/` (the
//! same tree `akiva build` produces) and serialises them through the
//! AKPK v1 format from [`crate::archive`]. The output is a single
//! self-contained file suitable for `akiva install`.
//!
//! By default, we build the app first so the archive reflects current
//! source. `--no-build` skips that — useful for CI where a separate job
//! has already staged the wasm.
//!
//! Wire format guarantees (see [`docs/archive-format.md`]):
//!   - `manifest.toml` and `app.wasm` are required.
//!   - `res/values/strings.toml` / `colors.toml` are copied if present.
//!   - No other files are included in v1; layout/drawable etc. are
//!     compile-time baked into the wasm (see `apps/*/src/lib.rs`).
//!
//! This matches exactly what [`framework::PackageManager`] reads at install
//! time — keeping the set minimal means fewer surprises when the runtime
//! loader evolves.
//!
//! Layout of staged dir (authored by `akiva build`):
//!   runtime/web/dev-apps/<app>/
//!     ├── app.wasm
//!     ├── manifest.toml
//!     └── res/values/{strings,colors}.toml    (optional)

use std::io;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use crate::archive::{Archive, ArchiveError};
use crate::{build, workspace};

pub fn run(args: &[String]) -> ExitCode {
    let mut names: Vec<String> = Vec::new();
    let mut out: Option<PathBuf> = None;
    let mut no_build = false;
    let mut debug = false;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--help" | "-h" => { print_help(); return ExitCode::SUCCESS; }
            "--out" | "-o" => {
                match args.get(i + 1) {
                    Some(p) => { out = Some(PathBuf::from(p)); i += 2; }
                    None => {
                        eprintln!("akiva package: --out requires a path");
                        return ExitCode::from(2);
                    }
                }
            }
            "--no-build" => { no_build = true; i += 1; }
            "--debug" => { debug = true; i += 1; }
            other if other.starts_with("--") => {
                eprintln!("akiva package: unknown flag '{other}'");
                return ExitCode::from(2);
            }
            name => { names.push(name.into()); i += 1; }
        }
    }

    if names.len() != 1 {
        eprintln!("akiva package: expected exactly one <app>, got {}", names.len());
        return ExitCode::from(2);
    }
    // --out <path> only makes sense with a single app — we enforce
    // single-app above, so no further ambiguity here.
    let name = &names[0];

    let root = match workspace::find_root_from_cwd() {
        Ok(r) => r,
        Err(e) => { eprintln!("akiva package: {e}"); return ExitCode::FAILURE; }
    };

    if !no_build {
        // Re-use the build pipeline so staging rules stay in one place.
        // If the user has a stale artifact, this is the cheap way to
        // guarantee it gets refreshed.
        let mut build_args: Vec<String> = vec![name.clone()];
        if debug { build_args.push("--debug".into()); }
        if !build::run_bool(&build_args) {
            eprintln!("akiva package: build failed for {name}");
            return ExitCode::FAILURE;
        }
    }

    let staged = root.join("runtime/web/dev-apps").join(name);
    let out_path = out.unwrap_or_else(|| PathBuf::from(format!("{name}.akv")));

    match package(&staged, &out_path) {
        Ok(size) => {
            println!("akiva package: wrote {} ({} bytes)", out_path.display(), size);
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("akiva package: {e}");
            ExitCode::FAILURE
        }
    }
}

fn print_help() {
    println!("akiva package — pack a built app into a .akv archive\n");
    println!("USAGE: akiva package <app> [--out <path>] [--no-build] [--debug]\n");
    println!("  <app>           App directory name under apps/");
    println!("  --out <path>    Output file (default: <app>.akv in cwd)");
    println!("  --no-build      Skip the pre-package build step");
    println!("  --debug         Build in debug profile (ignored with --no-build)");
}

/// Build the archive from a staged dev-apps directory. Returns the size
/// in bytes of the output file. The caller owns flag parsing and CWD
/// resolution; this function is pure I/O + format.
fn package(staged: &Path, out_path: &Path) -> io::Result<u64> {
    if !staged.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("no staged build at {} (run `akiva build` first, or drop --no-build)", staged.display()),
        ));
    }

    let mut archive = Archive::new();
    add_required(&mut archive, staged, "manifest.toml")?;
    add_required(&mut archive, staged, "app.wasm")?;

    // Resources are optional — an app with no user-visible strings or
    // custom palette still packages fine. Iterate the known set
    // explicitly: there's no glob of res/values*/ because the framework
    // only reads these specific files (see package_manager.rs).
    //
    // Stone 11.6.9 #3 — `values-night/` is the dark-mode overlay; same
    // include-or-skip rule.
    for leaf in [
        "res/values/strings.toml",
        "res/values/colors.toml",
        "res/values-night/strings.toml",
        "res/values-night/colors.toml",
    ] {
        let src = staged.join(leaf);
        if src.is_file() {
            let data = std::fs::read(&src)?;
            archive.add(leaf, data).map_err(archive_io_err)?;
        }
    }

    let bytes = archive.encode().map_err(archive_io_err)?;
    // Ensure parent dir exists if --out points at a nested path the
    // user hasn't created yet. Matches `mkdir -p` semantics.
    if let Some(parent) = out_path.parent() {
        if !parent.as_os_str().is_empty() && !parent.exists() {
            std::fs::create_dir_all(parent)?;
        }
    }
    std::fs::write(out_path, &bytes)?;
    Ok(bytes.len() as u64)
}

fn add_required(archive: &mut Archive, staged: &Path, name: &str) -> io::Result<()> {
    let src = staged.join(name);
    if !src.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("staged artifact missing: {} (did the build succeed?)", src.display()),
        ));
    }
    let data = std::fs::read(&src)?;
    archive.add(name, data).map_err(archive_io_err)
}

/// Bridge ArchiveError into io::Error so the caller's error type stays
/// single. The archive errors already carry good messages via Display.
fn archive_io_err(e: ArchiveError) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::archive::Archive;

    /// Build a fake staged dir and exercise the pure `package()` entry
    /// point. This avoids invoking cargo — the build-pipeline coupling
    /// is exercised at the `run` level in integration tests (not here,
    /// cargo in cargo is too heavy for unit tests).
    fn scratch_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("akiva-cli-package-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_staged_minimal(staged: &Path) {
        std::fs::write(staged.join("manifest.toml"), b"id = \"com.example.x\"").unwrap();
        std::fs::write(staged.join("app.wasm"), b"\0asm\x01\x00\x00\x00").unwrap();
    }

    #[test]
    fn package_minimal_roundtrips() {
        let staged = scratch_dir("min");
        write_staged_minimal(&staged);
        let out = staged.join("out.akv");
        let size = package(&staged, &out).unwrap();
        assert!(size > 0);

        let bytes = std::fs::read(&out).unwrap();
        let archive = Archive::decode(&bytes).unwrap();
        assert_eq!(archive.get("manifest.toml").unwrap().data, b"id = \"com.example.x\"");
        assert_eq!(archive.get("app.wasm").unwrap().data, b"\0asm\x01\x00\x00\x00");
        assert_eq!(archive.entries().len(), 2);
        let _ = std::fs::remove_dir_all(&staged);
    }

    #[test]
    fn package_includes_present_resources() {
        let staged = scratch_dir("res");
        write_staged_minimal(&staged);
        std::fs::create_dir_all(staged.join("res/values")).unwrap();
        std::fs::write(staged.join("res/values/strings.toml"), b"greet = \"hi\"").unwrap();
        std::fs::write(staged.join("res/values/colors.toml"), b"bg = \"#ff000000\"").unwrap();

        let out = staged.join("out.akv");
        package(&staged, &out).unwrap();
        let archive = Archive::decode(&std::fs::read(&out).unwrap()).unwrap();
        assert!(archive.get("res/values/strings.toml").is_some());
        assert!(archive.get("res/values/colors.toml").is_some());
        assert_eq!(archive.entries().len(), 4);
        let _ = std::fs::remove_dir_all(&staged);
    }

    #[test]
    fn package_skips_absent_resources_silently() {
        // Apps with no strings/colors.toml at all must still package —
        // only manifest+wasm are required.
        let staged = scratch_dir("noval");
        write_staged_minimal(&staged);
        let out = staged.join("out.akv");
        package(&staged, &out).unwrap();
        let archive = Archive::decode(&std::fs::read(&out).unwrap()).unwrap();
        assert_eq!(archive.entries().len(), 2);
        let _ = std::fs::remove_dir_all(&staged);
    }

    #[test]
    fn package_errors_when_staged_dir_missing() {
        let missing = std::env::temp_dir().join("akiva-cli-package-nope-xyz");
        let _ = std::fs::remove_dir_all(&missing);
        let out = std::env::temp_dir().join("akiva-cli-package-nope.akv");
        let err = package(&missing, &out).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn package_errors_when_manifest_missing() {
        let staged = scratch_dir("nomanifest");
        // Only wasm — manifest is required, must fail cleanly.
        std::fs::write(staged.join("app.wasm"), b"\0asm\x01\x00\x00\x00").unwrap();
        let out = staged.join("out.akv");
        let err = package(&staged, &out).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
        let _ = std::fs::remove_dir_all(&staged);
    }

    #[test]
    fn package_creates_output_parent_directory() {
        // --out foo/bar/app.akv should mkdir -p foo/bar/ for the user.
        let staged = scratch_dir("mkdirp");
        write_staged_minimal(&staged);
        let out = staged.join("nested/a/b/out.akv");
        package(&staged, &out).unwrap();
        assert!(out.is_file());
        let _ = std::fs::remove_dir_all(&staged);
    }
}
