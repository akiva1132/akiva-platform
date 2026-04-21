//! Kernel logging. Forwards to the HAL which decides routing (console,
//! in-app viewer, file sink) per docs/observability.md.

use crate::host::{self, LogLevel};

#[inline]
pub fn trace(msg: &str) { host::log(LogLevel::Trace, msg) }
#[inline]
pub fn debug(msg: &str) { host::log(LogLevel::Debug, msg) }
#[inline]
pub fn info(msg: &str)  { host::log(LogLevel::Info, msg) }
#[inline]
pub fn warn(msg: &str)  { host::log(LogLevel::Warn, msg) }
#[inline]
pub fn error(msg: &str) { host::log(LogLevel::Error, msg) }

/// Log with a short tag. Tag + ": " + msg is emitted as a single line so the
/// HAL does not have to reassemble multi-part records.
pub fn tagged(level: LogLevel, tag: &str, msg: &str) {
    let mut buf = String::with_capacity(tag.len() + 2 + msg.len());
    buf.push_str(tag);
    buf.push_str(": ");
    buf.push_str(msg);
    host::log(level, &buf);
}
