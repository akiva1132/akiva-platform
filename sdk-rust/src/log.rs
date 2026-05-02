//! App-side logging.
//!
//! Stone 11.3c-4 — forwards through the unified-ABI
//! `host_fire(FIRE::LOG, [level | msg])` rather than the legacy
//! per-op `akiva_host_log` import. The host dispatcher decodes the
//! args and routes the same `_log()` callback the kernel hooked up,
//! so log lines still land with the `[package-id]` prefix and the
//! standard level mapping. Level codes match `kernel/src/host.rs::LogLevel`.
//!
//! # Examples
//!
//! Apps reach for the `log_*!` macros, which mirror `format!`:
//!
//! ```ignore
//! use akiva_sdk::prelude::*;
//!
//! log_info!("activity started ({} ms)", 42);
//! log_warn!("retrying after {} failures", 3);
//! log_error!("giving up: {}", "out of slots");
//! ```
//!
//! `log_trace!` and `log_debug!` are filtered out by default in release
//! builds of the host; the SDK still emits them, the kernel drops them.

/// Stack-trace level. Filtered out of release builds host-side.
pub const LEVEL_TRACE: u32 = 0;
/// Verbose-but-useful level for app development. Default-off in
/// release builds host-side.
pub const LEVEL_DEBUG: u32 = 1;
/// Standard "this happened, fyi" level. The default audible level.
pub const LEVEL_INFO:  u32 = 2;
/// Recoverable issue worth surfacing. Highlighted in console output.
pub const LEVEL_WARN:  u32 = 3;
/// Unrecoverable issue or assertion failure. Always emitted.
pub const LEVEL_ERROR: u32 = 4;

/// Implementation hook used by the `log_*!` macros. Apps that want to
/// log without going through `format!` (e.g. to avoid the allocation
/// in a hot path) can call this directly with one of the `LEVEL_*`
/// constants.
///
/// On non-wasm targets the call is a no-op so unit tests don't trip
/// the host-import wall.
#[doc(hidden)]
pub fn __emit(level: u32, msg: &str) {
    // On wasm the host's dispatcher resolves FIRE::LOG by decoding the
    // first 4 bytes as a u32 LE level and the remainder as UTF-8 message
    // bytes. On non-wasm (cargo test, docs) we swallow the message so
    // apps' log macros don't block unit tests.
    #[cfg(target_arch = "wasm32")]
    {
        use crate::op_kinds::fire;
        use crate::runtime::host_fire;
        let bytes = msg.as_bytes();
        let mut args = Vec::with_capacity(4 + bytes.len());
        args.extend_from_slice(&level.to_le_bytes());
        args.extend_from_slice(bytes);
        // Return code is intentionally ignored: a logging failure
        // shouldn't propagate to call sites that may themselves be in
        // a panic-handler chain. The dispatcher logs its own warning
        // on unknown op_kinds, so a silent rc check here adds nothing.
        let _ = host_fire(fire::LOG, &args);
    }
    #[cfg(not(target_arch = "wasm32"))]
    { let _ = (level, msg); }
}

/// Log at [`LEVEL_TRACE`]. Filtered out by default in release host
/// builds; useful for tracing without recompiling. Same syntax as
/// `format!`.
#[macro_export]
macro_rules! log_trace { ($($t:tt)*) => { $crate::log::__emit($crate::log::LEVEL_TRACE, &format!($($t)*)) } }
/// Log at [`LEVEL_DEBUG`]. Default-off in release; default-on in dev.
#[macro_export]
macro_rules! log_debug { ($($t:tt)*) => { $crate::log::__emit($crate::log::LEVEL_DEBUG, &format!($($t)*)) } }
/// Log at [`LEVEL_INFO`]. The audible default level.
#[macro_export]
macro_rules! log_info  { ($($t:tt)*) => { $crate::log::__emit($crate::log::LEVEL_INFO,  &format!($($t)*)) } }
/// Log at [`LEVEL_WARN`]. Surfaces in the console, doesn't halt.
#[macro_export]
macro_rules! log_warn  { ($($t:tt)*) => { $crate::log::__emit($crate::log::LEVEL_WARN,  &format!($($t)*)) } }
/// Log at [`LEVEL_ERROR`]. Always emitted, including in release.
#[macro_export]
macro_rules! log_error { ($($t:tt)*) => { $crate::log::__emit($crate::log::LEVEL_ERROR, &format!($($t)*)) } }
