//! Dev HTTP server.
//!
//! Serves `runtime/web/` as static files with the cross-origin isolation
//! headers (`Cross-Origin-Opener-Policy: same-origin`,
//! `Cross-Origin-Embedder-Policy: require-corp`) that OPFS
//! SyncAccessHandle and future threads support require. Pure-stdlib
//! implementation: listen on TCP, thread-per-connection, request-line +
//! Host header parse, static file response. No keepalive, no routes, no
//! compression — a dev server that matches our dev loop (refresh the
//! page, reload the wasm) and nothing more.
//!
//! Zero third-party dependencies on purpose. The moment we pull in axum
//! or tokio, the compile time jumps by ~30s and the dev environment
//! gains a Tokio runtime that has nothing to do with the OS under
//! development. For our needs (a handful of file fetches per page load)
//! a blocking thread per request is more than enough.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::thread;

use crate::workspace;

/// Default bind address. 127.0.0.1 (not 0.0.0.0) so the dev server
/// isn't exposed on the LAN without the developer opting in — the
/// cross-origin isolation it enables would be noisy leaked to peers.
const DEFAULT_ADDR: &str = "127.0.0.1:4173";

/// Serve `runtime/web/` until the process is killed. Logs each request
/// to stdout so `akiva run` tails look the way a dev server should. The
/// function never returns in the happy path; the caller can assume a
/// return value means the listener itself failed.
pub fn serve(addr: &str) -> io::Result<()> {
    let root = workspace::find_root_from_cwd()?;
    let web_root = root.join("runtime/web");
    if !web_root.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("runtime/web not found at {}", web_root.display()),
        ));
    }

    let listener = TcpListener::bind(addr)?;
    eprintln!("akiva devserver: serving http://{addr}/ from {}", web_root.display());
    eprintln!("akiva devserver: COOP=same-origin, COEP=require-corp (OPFS-ready)");

    for stream in listener.incoming() {
        let stream = match stream {
            Ok(s) => s,
            Err(e) => { eprintln!("accept error: {e}"); continue; }
        };
        let web_root = web_root.clone();
        // One thread per request — simple, bounded naturally by the
        // handful of assets a dev-mode page pulls. If a connection
        // handler panics, the other in-flight requests keep working.
        thread::spawn(move || {
            if let Err(e) = handle(stream, &web_root) {
                eprintln!("request error: {e}");
            }
        });
    }
    Ok(())
}

/// Handle a single connection: parse one request line, write one
/// response, close. Deliberately non-persistent — the browser opens a
/// new connection for each asset, which is fine for the scale we're at.
fn handle(mut stream: TcpStream, web_root: &Path) -> io::Result<()> {
    // Readers get their own clone of the socket; writes go on the
    // original stream. Splitting lets us read headers via BufReader
    // without the buffering affecting the write path.
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut request_line = String::new();
    let n = reader.read_line(&mut request_line)?;
    if n == 0 {
        // Peer closed immediately; nothing to serve.
        return Ok(());
    }

    // Drain remaining headers. We don't interpret them (no keepalive,
    // no range requests), but reading them out keeps the socket in a
    // clean state for the response write.
    loop {
        let mut line = String::new();
        let n = reader.read_line(&mut line)?;
        if n == 0 || line == "\r\n" || line == "\n" { break; }
    }

    let (method, target) = parse_request_line(&request_line);
    // Method filter: HEAD gets a body-less response; anything beyond
    // GET/HEAD isn't something a static server should attempt.
    let head_only = match method {
        "GET" => false,
        "HEAD" => true,
        _ => {
            write_status(&mut stream, 405, "Method Not Allowed", b"method not supported\n", "text/plain; charset=utf-8", false)?;
            log(method, target, 405);
            return Ok(());
        }
    };

    // Path resolution. Strip any query string, then map "/" to
    // "index.html" and reject ".." escapes so a request can't traverse
    // outside `web_root`. The dev server has the same-origin user
    // pointing a browser at it, but defensive traversal handling costs
    // nothing.
    let path_only = target.split('?').next().unwrap_or("/");
    let relative = match resolve_path(path_only) {
        Some(p) => p,
        None => {
            write_status(&mut stream, 400, "Bad Request", b"bad path\n", "text/plain; charset=utf-8", head_only)?;
            log(method, target, 400);
            return Ok(());
        }
    };
    let abs = web_root.join(&relative);
    // Canonicalise to collapse ".." and symlinks, then re-check it's
    // still under web_root. If the file doesn't exist the canonicalize
    // call itself errors — fall through to 404 in that case.
    let canonical = abs.canonicalize();
    let (body, status, content_type) = match canonical {
        Ok(p) if p.starts_with(web_root) && p.is_file() => {
            match std::fs::read(&p) {
                Ok(bytes) => (bytes, 200, mime_for(&p)),
                Err(_) => (b"read error\n".to_vec(), 500, "text/plain; charset=utf-8"),
            }
        }
        _ => (b"not found\n".to_vec(), 404, "text/plain; charset=utf-8"),
    };
    let reason = match status {
        200 => "OK",
        404 => "Not Found",
        _   => "Error",
    };
    write_status(&mut stream, status, reason, &body, content_type, head_only)?;
    log(method, target, status);
    Ok(())
}

/// Extract METHOD and target (URI) from the HTTP request line. Trims
/// the trailing CRLF. On a malformed line we return `("", "")` — the
/// caller rejects empty method with a 405, which is as graceful as a
/// one-shot server needs to be.
fn parse_request_line(line: &str) -> (&str, &str) {
    let line = line.trim_end_matches(['\r', '\n']);
    let mut it = line.split_whitespace();
    let method = it.next().unwrap_or("");
    let target = it.next().unwrap_or("");
    (method, target)
}

/// Turn a request path into a safe relative path under web root, or
/// `None` if the path tries to escape. Returns `"index.html"` for `/`.
fn resolve_path(target: &str) -> Option<PathBuf> {
    let stripped = target.strip_prefix('/').unwrap_or(target);
    let candidate = if stripped.is_empty() { "index.html" } else { stripped };
    // Reject explicit traversal. Component-level check is robust against
    // URL-decoded or mixed separators; we also reject absolute components
    // which the dev server has no business serving.
    for component in Path::new(candidate).components() {
        use std::path::Component;
        match component {
            Component::Normal(_) | Component::CurDir => {}
            _ => return None,  // ParentDir / RootDir / Prefix
        }
    }
    Some(PathBuf::from(candidate))
}

/// Minimal content-type table. The dev server exists to serve a handful
/// of known extensions — exhaustive MIME coverage is a distraction.
/// Unknown extensions fall back to `application/octet-stream`, which
/// browsers handle without fuss.
fn mime_for(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()).unwrap_or("") {
        "html" | "htm" => "text/html; charset=utf-8",
        "js" | "mjs"   => "application/javascript; charset=utf-8",
        "css"          => "text/css; charset=utf-8",
        "json"         => "application/json",
        "toml"         => "text/plain; charset=utf-8",
        "wasm"         => "application/wasm",
        "png"          => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "svg"          => "image/svg+xml",
        "ico"          => "image/x-icon",
        "txt" | "md"   => "text/plain; charset=utf-8",
        _              => "application/octet-stream",
    }
}

/// Write an entire response (status line + headers + body) to the
/// socket. COOP/COEP/CORP headers ride on every response — even 404s —
/// so the page's cross-origin isolation doesn't flicker between loads.
/// `head_only` suppresses the body for HEAD requests but keeps the
/// Content-Length accurate.
fn write_status(
    stream: &mut TcpStream,
    status: u16, reason: &str,
    body: &[u8], content_type: &str,
    head_only: bool,
) -> io::Result<()> {
    let mut head = format!(
        "HTTP/1.1 {status} {reason}\r\n\
         Content-Type: {content_type}\r\n\
         Content-Length: {}\r\n\
         Cross-Origin-Opener-Policy: same-origin\r\n\
         Cross-Origin-Embedder-Policy: require-corp\r\n\
         Cross-Origin-Resource-Policy: same-origin\r\n\
         Cache-Control: no-store\r\n\
         Connection: close\r\n",
        body.len(),
    );
    head.push_str("\r\n");
    stream.write_all(head.as_bytes())?;
    if !head_only {
        stream.write_all(body)?;
    }
    stream.flush()?;
    // Drain any leftover request data the peer queued; without this the
    // peer occasionally sees a connection reset before its write
    // returns, which shows up as a spurious error in browser devtools.
    let _ = stream.read(&mut [0u8; 64]);
    Ok(())
}

/// Single-line access log to stderr (so the server log stays separate
/// from stdout in case someone pipes it). Compact format for eyeballing
/// during a dev session — we're not building production telemetry.
fn log(method: &str, target: &str, status: u16) {
    eprintln!("{status} {method} {target}");
}

/// CLI entry point. Parses `--port N` / `--addr HOST:PORT`; everything
/// else falls back to [`DEFAULT_ADDR`]. Kept in this module (rather
/// than `main.rs`) so the parser lives next to the thing it configures.
pub fn run(args: &[String]) -> std::process::ExitCode {
    let mut addr: String = DEFAULT_ADDR.to_string();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--addr" => {
                if i + 1 >= args.len() {
                    eprintln!("akiva devserver: --addr expects HOST:PORT");
                    return std::process::ExitCode::from(2);
                }
                addr = args[i + 1].clone();
                i += 2;
            }
            "--port" => {
                if i + 1 >= args.len() {
                    eprintln!("akiva devserver: --port expects a number");
                    return std::process::ExitCode::from(2);
                }
                addr = format!("127.0.0.1:{}", args[i + 1]);
                i += 2;
            }
            "--help" | "-h" => {
                println!("akiva devserver — static file server for runtime/web/\n");
                println!("USAGE: akiva devserver [--addr HOST:PORT | --port PORT]\n");
                println!("Defaults to {DEFAULT_ADDR}. Serves with COOP/COEP headers so OPFS");
                println!("SyncAccessHandle and future threads support work.");
                return std::process::ExitCode::SUCCESS;
            }
            other => {
                eprintln!("akiva devserver: unknown flag '{other}'");
                return std::process::ExitCode::from(2);
            }
        }
    }
    match serve(&addr) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("akiva devserver: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_path_maps_root_to_index() {
        assert_eq!(resolve_path("/"), Some(PathBuf::from("index.html")));
        assert_eq!(resolve_path(""),  Some(PathBuf::from("index.html")));
    }

    #[test]
    fn resolve_path_strips_leading_slash() {
        assert_eq!(resolve_path("/worker.js"), Some(PathBuf::from("worker.js")));
    }

    #[test]
    fn resolve_path_rejects_parent_traversal() {
        assert_eq!(resolve_path("/../etc/passwd"), None);
        assert_eq!(resolve_path("/a/../../b"),     None);
    }

    #[test]
    fn resolve_path_rejects_absolute_paths() {
        // Leading-slash requests strip to relative; additional root
        // components (Windows "C:\" prefixes) should still be rejected.
        assert_eq!(resolve_path("//etc/passwd"), None);  // second slash → RootDir component
    }

    #[test]
    fn parse_request_line_extracts_method_and_target() {
        assert_eq!(parse_request_line("GET / HTTP/1.1\r\n"), ("GET", "/"));
        assert_eq!(parse_request_line("GET /worker.js HTTP/1.1"), ("GET", "/worker.js"));
    }

    #[test]
    fn parse_request_line_on_empty_returns_empty_method() {
        assert_eq!(parse_request_line(""), ("", ""));
    }

    #[test]
    fn mime_maps_common_extensions() {
        assert_eq!(mime_for(Path::new("a.html")), "text/html; charset=utf-8");
        assert_eq!(mime_for(Path::new("a.js")),   "application/javascript; charset=utf-8");
        assert_eq!(mime_for(Path::new("a.wasm")), "application/wasm");
        assert_eq!(mime_for(Path::new("a.toml")), "text/plain; charset=utf-8");
        assert_eq!(mime_for(Path::new("unknown.xyz")), "application/octet-stream");
    }
}
