//! App-side logging. Stone 11.3c-4 — forwards through the unified-ABI
//! `host_fire(FIRE::LOG, [level | msg])` rather than the legacy
//! per-op `akiva_host_log` import. The host dispatcher decodes the
//! args and routes the same `_log()` callback the kernel hooked up,
//! so log lines still land with the `[package-id]` prefix and the
//! standard level mapping. Level codes match `kernel/src/host.rs::LogLevel`.

/// Numeric level codes shared with the host. Exposed so apps that want
/// to log without `format!`-ing can call `__emit` directly.
pub const LEVEL_TRACE: u32 = 0;
pub const LEVEL_DEBUG: u32 = 1;
pub const LEVEL_INFO:  u32 = 2;
pub const LEVEL_WARN:  u32 = 3;
pub const LEVEL_ERROR: u32 = 4;

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

#[macro_export]
macro_rules! log_trace { ($($t:tt)*) => { $crate::log::__emit($crate::log::LEVEL_TRACE, &format!($($t)*)) } }
#[macro_export]
macro_rules! log_debug { ($($t:tt)*) => { $crate::log::__emit($crate::log::LEVEL_DEBUG, &format!($($t)*)) } }
#[macro_export]
macro_rules! log_info  { ($($t:tt)*) => { $crate::log::__emit($crate::log::LEVEL_INFO,  &format!($($t)*)) } }
#[macro_export]
macro_rules! log_warn  { ($($t:tt)*) => { $crate::log::__emit($crate::log::LEVEL_WARN,  &format!($($t)*)) } }
#[macro_export]
macro_rules! log_error { ($($t:tt)*) => { $crate::log::__emit($crate::log::LEVEL_ERROR, &format!($($t)*)) } }
