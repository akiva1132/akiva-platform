//! App-side logging. Forwards to the runtime log interface.

#[doc(hidden)]
pub fn __emit(level: u32, msg: &str) {
    // Phase 2: forward through WIT-bound log interface. For now, eprintln
    // so host-side unit tests can see output.
    let tag = match level { 0 => "T", 1 => "D", 2 => "I", 3 => "W", _ => "E" };
    eprintln!("[{tag}] {msg}");
}

#[macro_export]
macro_rules! log_trace { ($($t:tt)*) => { $crate::log::__emit(0, &format!($($t)*)) } }
#[macro_export]
macro_rules! log_debug { ($($t:tt)*) => { $crate::log::__emit(1, &format!($($t)*)) } }
#[macro_export]
macro_rules! log_info  { ($($t:tt)*) => { $crate::log::__emit(2, &format!($($t)*)) } }
#[macro_export]
macro_rules! log_warn  { ($($t:tt)*) => { $crate::log::__emit(3, &format!($($t)*)) } }
#[macro_export]
macro_rules! log_error { ($($t:tt)*) => { $crate::log::__emit(4, &format!($($t)*)) } }
