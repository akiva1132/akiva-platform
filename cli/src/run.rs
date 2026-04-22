//! One-shot dev loop entry point.
//!
//! `akiva run` is the convenience command for "I just want to see the
//! platform running". It sequences:
//!
//!   1. Build the kernel wasm (`cargo build -p akiva-kernel --target
//!      wasm32-unknown-unknown --release`) and copy the output to
//!      `runtime/web/akiva-kernel.wasm`.
//!   2. Build every app under `apps/` via [`crate::build`] so their
//!      staged `.wasm`/manifest/resources land in
//!      `runtime/web/dev-apps/<name>/`.
//!   3. Launch the dev HTTP server on 127.0.0.1:4173.
//!
//! Steps 1 and 2 are skip-if-current: if the user asked for `--no-build`
//! (or used `akiva devserver` directly) we jump straight to the server.
//! The kernel is treated separately from apps because it's a special
//! crate in the workspace — one binary, fixed path, no staging rules
//! beyond the copy.

use std::io;
use std::path::Path;
use std::process::{Command, ExitCode};

use crate::{build, devserver, workspace};

/// Default listen address, matching [`devserver`]. Duplicated rather
/// than re-imported so the two commands' defaults can diverge if we
/// ever want them to — today they don't.
const DEFAULT_ADDR: &str = "127.0.0.1:4173";

pub fn run(args: &[String]) -> ExitCode {
    let mut addr = DEFAULT_ADDR.to_string();
    let mut skip_build = false;
    let mut debug = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--addr" => {
                if i + 1 >= args.len() {
                    eprintln!("akiva run: --addr expects HOST:PORT");
                    return ExitCode::from(2);
                }
                addr = args[i + 1].clone();
                i += 2;
            }
            "--port" => {
                if i + 1 >= args.len() {
                    eprintln!("akiva run: --port expects a number");
                    return ExitCode::from(2);
                }
                addr = format!("127.0.0.1:{}", args[i + 1]);
                i += 2;
            }
            "--no-build" => { skip_build = true; i += 1; }
            "--debug" => { debug = true; i += 1; }
            "--help" | "-h" => {
                println!("akiva run — build everything, then serve runtime/web/\n");
                println!("USAGE: akiva run [--no-build] [--debug] [--addr HOST:PORT | --port PORT]\n");
                println!("  --no-build        Skip the kernel + apps build step");
                println!("  --debug           Use the `dev` cargo profile for the app builds");
                println!("  --addr HOST:PORT  Listen address (default {DEFAULT_ADDR})");
                println!("  --port PORT       Shorthand for --addr 127.0.0.1:PORT");
                return ExitCode::SUCCESS;
            }
            other => {
                eprintln!("akiva run: unknown flag '{other}'");
                return ExitCode::from(2);
            }
        }
    }

    let root = match workspace::find_root_from_cwd() {
        Ok(r) => r,
        Err(e) => { eprintln!("akiva run: {e}"); return ExitCode::FAILURE; }
    };

    if !skip_build {
        // Kernel first: if it fails the apps have no host to run
        // against, and the cached kernel.wasm would be misleading.
        if let Err(e) = build_kernel(&root, debug) {
            eprintln!("akiva run: kernel: {e}");
            return ExitCode::FAILURE;
        }
        // Then apps, via the same code path `akiva build` drives —
        // keeps the "one command, one rule" story honest. `build_bool`
        // returns false on any per-app failure; `akiva run` aborts
        // rather than launching the devserver against a broken app set
        // (that would manifest as "the apps I just changed didn't
        // reload", which is worse than a visible build error).
        let build_args: Vec<String> = if debug { vec!["--debug".into()] } else { vec![] };
        if !build::run_bool(&build_args) {
            return ExitCode::FAILURE;
        }
    }

    // Hand off to the devserver — this call doesn't return in the
    // happy path (it's a listen-forever loop). Failure to bind is the
    // only way back, and we propagate it.
    match devserver::serve(&addr) {
        Ok(()) => ExitCode::SUCCESS,  // unreachable today; kept for symmetry
        Err(e) => {
            eprintln!("akiva run: devserver: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Build the kernel and copy the output wasm into
/// `runtime/web/akiva-kernel.wasm`, replacing the manual
/// `cp target/.../akiva_kernel.wasm runtime/web/...` step from the
/// pre-CLI workflow.
fn build_kernel(root: &Path, debug: bool) -> io::Result<()> {
    let (profile_flag, profile_dir) = if debug { ("dev", "debug") } else { ("release", "release") };
    println!("[kernel] cargo build ({profile_flag})");

    let status = Command::new("cargo")
        .args([
            "build",
            "-p", "akiva-kernel",
            "--target", "wasm32-unknown-unknown",
            "--profile", profile_flag,
        ])
        .current_dir(root)
        .status()?;
    if !status.success() {
        return Err(io::Error::new(io::ErrorKind::Other, "cargo build -p akiva-kernel failed"));
    }

    let src = root
        .join("target").join("wasm32-unknown-unknown").join(profile_dir)
        .join("akiva_kernel.wasm");
    let dst = root.join("runtime/web/akiva-kernel.wasm");
    if !src.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("kernel wasm missing at {}", src.display()),
        ));
    }
    std::fs::copy(&src, &dst)?;
    let size = std::fs::metadata(&dst)?.len();
    println!("[kernel] staged {} ({} bytes)", dst.display(), size);
    Ok(())
}
