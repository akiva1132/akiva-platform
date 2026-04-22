//! Workspace discovery.
//!
//! Most CLI commands need to resolve paths relative to the akiva-platform
//! checkout, not wherever the user ran `akiva` from. The simple
//! convention: walk up from CWD looking for a `Cargo.toml` whose content
//! names the workspace members we expect. That keeps the CLI usable from
//! anywhere inside the tree (app subdirectories included) without a
//! config flag, and fails loudly if invoked outside a checkout.

use std::io;
use std::path::{Path, PathBuf};

/// Walk up from `start` until a directory containing a workspace
/// `Cargo.toml` is found. Returns the directory path (the workspace
/// root), or an error if none is found before reaching the filesystem
/// root. "Workspace" is identified by the literal `[workspace]` table
/// header — our Cargo.toml unambiguously has one, and checking the
/// membership list would couple this helper to the package layout.
pub fn find_root(start: &Path) -> io::Result<PathBuf> {
    let mut cur: Option<&Path> = Some(start);
    while let Some(dir) = cur {
        let candidate = dir.join("Cargo.toml");
        if candidate.is_file() {
            // Cheap fingerprint — read the file and look for the
            // workspace table. Any file small enough to be a Cargo.toml
            // fits in one go; we don't need a proper TOML parser just
            // to check whether a single line exists.
            let text = std::fs::read_to_string(&candidate)?;
            if text.lines().any(|l| l.trim_start().starts_with("[workspace]")) {
                return Ok(dir.to_path_buf());
            }
        }
        cur = dir.parent();
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        "could not find akiva-platform workspace root (no [workspace] Cargo.toml from here to /)",
    ))
}

/// Convenience: discover starting from the process's CWD. Used by every
/// subcommand; centralised so error messages stay consistent.
pub fn find_root_from_cwd() -> io::Result<PathBuf> {
    let cwd = std::env::current_dir()?;
    find_root(&cwd)
}
