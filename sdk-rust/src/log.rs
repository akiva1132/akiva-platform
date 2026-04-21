//! App-side logging. Forwards to the runtime's `akiva_host_log` import,
//! which the worker's `AppRegistry` routes to the kernel log channel with
//! a `[package-id]` prefix. Level codes match `kernel/src/host.rs::LogLevel`.

/// Numeric level codes shared with the host. Exposed so apps that want
/// to log without `format!`-ing can call `__emit` directly.
pub const LEVEL_TRACE: u32 = 0;
pub const LEVEL_DEBUG: u32 = 1;
pub const LEVEL_INFO:  u32 = 2;
pub const LEVEL_WARN:  u32 = 3;
pub const LEVEL_ERROR: u32 = 4;

#[doc(hidden)]
pub fn __emit(level: u32, msg: &str) {
    // On wasm the host supplies `akiva_host_log`. On non-wasm (cargo
    // test, docs) the extern isn't in scope — we swallow the message
    // so apps' log macros don't block unit tests. Level is unused in
    // the fallback; tag it so the compiler doesn't warn.
    #[cfg(target_arch = "wasm32")]
    unsafe {
        let bytes = msg.as_bytes();
        crate::runtime::akiva_host_log(level, bytes.as_ptr(), bytes.len() as u32);
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
