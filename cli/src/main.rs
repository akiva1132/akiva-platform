//! Akiva CLI.
//!
//! Subcommands:
//!   akiva new <name>       — scaffold a new app
//!   akiva build             — build the workspace (kernel → wasm, cli → native)
//!   akiva package <app>     — build an app + emit a .akiva archive
//!   akiva install <archive> — install an app into the local runtime
//!   akiva run               — build + launch the devserver
//!   akiva devserver         — serve runtime/web/ with the cross-origin
//!                             isolation headers OPFS + threads need

mod build;
mod devserver;
mod workspace;

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        None | Some("--help") | Some("-h") | Some("help") => { print_help(); ExitCode::SUCCESS }
        Some("--version") | Some("-V") | Some("version") => {
            println!("akiva {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Some("new") => cmd_new(&args[1..]),
        Some("build") => cmd_build(&args[1..]),
        Some("package") => cmd_package(&args[1..]),
        Some("install") => cmd_install(&args[1..]),
        Some("run") => cmd_run(&args[1..]),
        Some("devserver") => cmd_devserver(&args[1..]),
        Some(other) => {
            eprintln!("akiva: unknown subcommand '{other}'. Try 'akiva help'.");
            ExitCode::from(2)
        }
    }
}

fn print_help() {
    println!("akiva {} — platform CLI\n", env!("CARGO_PKG_VERSION"));
    println!("USAGE: akiva <command> [args...]\n");
    println!("COMMANDS:");
    println!("  new <name>          Scaffold a new app crate");
    println!("  build               Build the workspace");
    println!("  package <app>       Build an app and emit a .akiva archive");
    println!("  install <archive>   Install an app into the local runtime");
    println!("  run                 Build + launch the devserver");
    println!("  devserver           Serve runtime/web with hot reload");
    println!("  version             Print the CLI version");
    println!("  help                Show this message");
}

fn cmd_new(args: &[String]) -> ExitCode {
    let Some(name) = args.first() else {
        eprintln!("akiva new: missing <name>"); return ExitCode::from(2);
    };
    println!("[stub] would scaffold apps/{name}/ with manifest.toml + src/lib.rs");
    ExitCode::SUCCESS
}

fn cmd_build(args: &[String]) -> ExitCode {
    build::run(args)
}

fn cmd_package(args: &[String]) -> ExitCode {
    let Some(app) = args.first() else {
        eprintln!("akiva package: missing <app>"); return ExitCode::from(2);
    };
    println!("[stub] would package apps/{app}/ into {app}.akiva");
    ExitCode::SUCCESS
}

fn cmd_install(args: &[String]) -> ExitCode {
    let Some(archive) = args.first() else {
        eprintln!("akiva install: missing <archive>"); return ExitCode::from(2);
    };
    println!("[stub] would install {archive} into the runtime package store");
    ExitCode::SUCCESS
}

fn cmd_run(_args: &[String]) -> ExitCode {
    println!("[stub] would: build, then launch devserver on http://localhost:8765/");
    ExitCode::SUCCESS
}

fn cmd_devserver(args: &[String]) -> ExitCode {
    devserver::run(args)
}
