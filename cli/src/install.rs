//! `akiva install <archive>` — unpack a `.akv` into the dev runtime.
//!
//! This is the CLI-side install path. It doesn't touch OPFS directly
//! (the browser owns that); instead it writes the archive's entries
//! into `runtime/web/dev-apps/<dir>/` where the worker's
//! `installDevApps` flow will pick them up on the next page load. The
//! set of keys mirrors exactly what `akiva package` produces, so
//! install is the clean inverse of package.
//!
//! Why not parse + validate the manifest properly via
//! `akiva-framework::PackageManager::install`? The ROADMAP's stone 8.3
//! notes that as the long-term shape, but pulling the framework in
//! would drag serde + toml + indexmap into the CLI (currently
//! zero-dep). The runtime already re-validates every manifest through
//! `PackageManager::install` at boot, so a malformed archive that
//! sneaks past the CLI fails cleanly in the browser. In Stone 8.4,
//! `DEV_APPS` and `installDevApps` go away entirely — at that point
//! the CLI takes over full validation and the framework import
//! becomes necessary anyway. Until then: match worker.js's tiny
//! `id = "..."` regex and move on.
//!
//! Directory naming: archives carry only the reverse-domain
//! `app.id` (e.g. `com.akiva.calculator`). The existing dev flow uses
//! short directory names (`calculator`), so we default to the last
//! dot-separated segment of the id. `--as <name>` overrides when two
//! apps collide on the short name or when the user wants a specific
//! layout.

use std::io;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use crate::archive::{Archive, ArchiveError};
use crate::workspace;

pub fn run(args: &[String]) -> ExitCode {
    let mut archives: Vec<PathBuf> = Vec::new();
    let mut dir_override: Option<String> = None;
    let mut runtime_root: Option<PathBuf> = None;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--help" | "-h" => { print_help(); return ExitCode::SUCCESS; }
            "--as" => {
                match args.get(i + 1) {
                    Some(s) => { dir_override = Some(s.clone()); i += 2; }
                    None => {
                        eprintln!("akiva install: --as requires a directory name");
                        return ExitCode::from(2);
                    }
                }
            }
            "--to" => {
                match args.get(i + 1) {
                    Some(s) => { runtime_root = Some(PathBuf::from(s)); i += 2; }
                    None => {
                        eprintln!("akiva install: --to requires a path");
                        return ExitCode::from(2);
                    }
                }
            }
            other if other.starts_with("--") => {
                eprintln!("akiva install: unknown flag '{other}'");
                return ExitCode::from(2);
            }
            path => { archives.push(PathBuf::from(path)); i += 1; }
        }
    }

    if archives.len() != 1 {
        eprintln!("akiva install: expected exactly one <archive>, got {}", archives.len());
        return ExitCode::from(2);
    }
    let archive_path = &archives[0];

    let runtime_root = match runtime_root {
        Some(p) => p,
        None => match workspace::find_root_from_cwd() {
            Ok(root) => root.join("runtime/web"),
            Err(e) => {
                eprintln!("akiva install: {e}");
                return ExitCode::FAILURE;
            }
        },
    };

    match install(archive_path, &runtime_root, dir_override.as_deref()) {
        Ok(report) => {
            println!(
                "akiva install: extracted {} ({}) → {}",
                report.package_id,
                plural_files(report.file_count),
                report.dest.display(),
            );
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("akiva install: {e}");
            ExitCode::FAILURE
        }
    }
}

fn print_help() {
    println!("akiva install — unpack a .akv archive into the dev runtime\n");
    println!("USAGE: akiva install <archive> [--as <dir>] [--to <runtime-web>]\n");
    println!("  <archive>         Path to a .akv file");
    println!("  --as <dir>        Dev-apps dir name (default: last segment of app.id)");
    println!("  --to <path>       Runtime web root (default: <workspace>/runtime/web)");
}

/// Outcome of a successful install — used by `run` for logging.
#[derive(Debug)]
struct Report {
    package_id: String,
    dest: PathBuf,
    file_count: usize,
}

fn plural_files(n: usize) -> String {
    if n == 1 { "1 file".into() } else { format!("{n} files") }
}

/// Read + decode the archive, pick a destination, and drop each entry
/// in place. Pure I/O + format — `run` owns arg parsing and workspace
/// discovery.
fn install(archive_path: &Path, runtime_root: &Path, dir_override: Option<&str>) -> io::Result<Report> {
    if !archive_path.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("archive not found: {}", archive_path.display()),
        ));
    }
    let bytes = std::fs::read(archive_path)?;
    let archive = Archive::decode(&bytes).map_err(archive_io_err)?;

    // `Archive::decode` guarantees manifest.toml is present. Still
    // validate UTF-8 up front so we can surface a targeted error
    // rather than letting utf8 failures bubble up as generic InvalidData.
    // `Archive::decode` guarantees manifest.toml is in the entry set
    // (MissingRequired would have fired otherwise), so the `.expect` is
    // a genuine invariant, not a hidden unwrap.
    let manifest_bytes = &archive
        .get("manifest.toml")
        .expect("archive decoder guarantees manifest.toml is present")
        .data;
    let manifest_text = std::str::from_utf8(manifest_bytes).map_err(|_| {
        io::Error::new(io::ErrorKind::InvalidData, "manifest.toml is not valid UTF-8")
    })?;

    let package_id = extract_app_id(manifest_text).ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidData, "manifest.toml has no `id = \"...\"` under [app]")
    })?;

    let dir_name = match dir_override {
        Some(name) => name.to_string(),
        None => short_name_from_id(&package_id),
    };

    // `dev-apps/` is the existing bridge location; worker.js's
    // installDevApps fetches it on boot. Stone 8.4 replaces this
    // with a proper installed/ tree + index.
    let dest = runtime_root.join("dev-apps").join(&dir_name);
    std::fs::create_dir_all(&dest)?;

    let mut file_count = 0usize;
    for entry in archive.entries() {
        let out_path = dest.join(&entry.path);
        if let Some(parent) = out_path.parent() {
            if !parent.as_os_str().is_empty() && !parent.exists() {
                std::fs::create_dir_all(parent)?;
            }
        }
        std::fs::write(&out_path, &entry.data)?;
        file_count += 1;
    }

    Ok(Report { package_id, dest, file_count })
}

/// Mirror of worker.js's id regex — accepts the first `id = "..."` line
/// in the manifest. We skip the `[app]` table header check because
/// manifests produced by Akiva always put `id` in `[app]` anyway, and
/// a malformed manifest with `id` somewhere else is rejected at the
/// runtime's real parse. This is a best-effort identifier extract.
fn extract_app_id(manifest: &str) -> Option<String> {
    for line in manifest.lines() {
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

/// `com.akiva.calculator` → `calculator`. If the id has no dots (which
/// should never happen — the manifest validator requires at least one
/// — but we handle it defensively), use the whole string.
fn short_name_from_id(id: &str) -> String {
    id.rsplit_once('.').map_or(id.to_string(), |(_, tail)| tail.to_string())
}

fn archive_io_err(e: ArchiveError) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::archive::Archive;

    fn scratch_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("akiva-cli-install-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn minimal_archive_bytes(id: &str) -> Vec<u8> {
        let manifest = format!("[app]\nid = \"{id}\"\nname = \"x\"\n");
        let mut a = Archive::new();
        a.add("manifest.toml", manifest.into_bytes()).unwrap();
        a.add("app.wasm", b"\0asm\x01\x00\x00\x00".to_vec()).unwrap();
        a.encode().unwrap()
    }

    #[test]
    fn extract_app_id_finds_quoted_value() {
        assert_eq!(extract_app_id("[app]\nid = \"com.x.y\"\n"), Some("com.x.y".into()));
        assert_eq!(extract_app_id("id=\"a.b\""), Some("a.b".into()));
    }

    #[test]
    fn extract_app_id_returns_none_when_missing() {
        assert_eq!(extract_app_id("[app]\nname = \"x\"\n"), None);
    }

    #[test]
    fn extract_app_id_ignores_empty_string() {
        // `id = ""` is syntactically valid TOML but not a real id —
        // treat it as missing so the caller emits a useful error.
        assert_eq!(extract_app_id("id = \"\""), None);
    }

    #[test]
    fn short_name_peels_last_segment() {
        assert_eq!(short_name_from_id("com.akiva.calculator"), "calculator");
        assert_eq!(short_name_from_id("a.b"), "b");
        // Defensive fallback for manifests that skip the dot (shouldn't
        // happen in practice — validator rejects them — but a CLI
        // crash here would be a bad day).
        assert_eq!(short_name_from_id("loose"), "loose");
    }

    #[test]
    fn install_extracts_required_files() {
        let tmp = scratch_dir("min");
        let archive_path = tmp.join("app.akv");
        std::fs::write(&archive_path, minimal_archive_bytes("com.example.calc")).unwrap();

        let runtime_root = tmp.join("runtime/web");
        let report = install(&archive_path, &runtime_root, None).unwrap();
        assert_eq!(report.package_id, "com.example.calc");
        assert_eq!(report.file_count, 2);
        // Short name derived from id: "calc".
        assert_eq!(report.dest, runtime_root.join("dev-apps/calc"));
        assert!(report.dest.join("manifest.toml").is_file());
        assert!(report.dest.join("app.wasm").is_file());

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn install_respects_as_override() {
        let tmp = scratch_dir("as");
        let archive_path = tmp.join("app.akv");
        std::fs::write(&archive_path, minimal_archive_bytes("com.x.widget")).unwrap();

        let runtime_root = tmp.join("runtime/web");
        let report = install(&archive_path, &runtime_root, Some("custom-dir")).unwrap();
        assert_eq!(report.dest, runtime_root.join("dev-apps/custom-dir"));
        assert!(report.dest.join("manifest.toml").is_file());

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn install_preserves_binary_wasm() {
        let tmp = scratch_dir("bin");
        let wasm: Vec<u8> = (0u8..=255).cycle().take(4096).collect();
        let mut a = Archive::new();
        a.add("manifest.toml", b"[app]\nid = \"com.x.bin\"".to_vec()).unwrap();
        a.add("app.wasm", wasm.clone()).unwrap();
        let archive_path = tmp.join("app.akv");
        std::fs::write(&archive_path, a.encode().unwrap()).unwrap();

        let runtime_root = tmp.join("runtime/web");
        let report = install(&archive_path, &runtime_root, None).unwrap();
        let staged = std::fs::read(report.dest.join("app.wasm")).unwrap();
        assert_eq!(staged, wasm);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn install_writes_nested_resource_paths() {
        // res/values/strings.toml lives two levels deep — ensure the
        // extractor mkdirs -p and doesn't trip on the path separator.
        let tmp = scratch_dir("res");
        let mut a = Archive::new();
        a.add("manifest.toml", b"[app]\nid = \"com.x.res\"".to_vec()).unwrap();
        a.add("app.wasm", b"\0asm\x01\x00\x00\x00".to_vec()).unwrap();
        a.add("res/values/strings.toml", b"greet = \"hi\"".to_vec()).unwrap();
        a.add("res/values/colors.toml", b"bg = \"#ff000000\"".to_vec()).unwrap();
        let archive_path = tmp.join("app.akv");
        std::fs::write(&archive_path, a.encode().unwrap()).unwrap();

        let runtime_root = tmp.join("runtime/web");
        let report = install(&archive_path, &runtime_root, None).unwrap();
        assert_eq!(report.file_count, 4);
        assert!(report.dest.join("res/values/strings.toml").is_file());
        assert!(report.dest.join("res/values/colors.toml").is_file());

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn install_rejects_missing_archive() {
        let missing = std::env::temp_dir().join("akiva-cli-install-missing-xyz.akv");
        let _ = std::fs::remove_file(&missing);
        let err = install(&missing, &std::env::temp_dir(), None).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn install_rejects_bad_magic() {
        let tmp = scratch_dir("magic");
        let archive_path = tmp.join("app.akv");
        std::fs::write(&archive_path, b"NOTA REAL ARCHIVE").unwrap();
        let err = install(&archive_path, &tmp, None).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn install_rejects_manifest_without_id() {
        // Build a manifest-less-id archive by hand — Archive::add
        // accepts any bytes for manifest.toml, the id-extract step
        // is what catches this.
        let tmp = scratch_dir("noid");
        let mut a = Archive::new();
        a.add("manifest.toml", b"[app]\nname = \"no id here\"".to_vec()).unwrap();
        a.add("app.wasm", b"\0asm\x01\x00\x00\x00".to_vec()).unwrap();
        let archive_path = tmp.join("app.akv");
        std::fs::write(&archive_path, a.encode().unwrap()).unwrap();

        let err = install(&archive_path, &tmp.join("runtime/web"), None).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
