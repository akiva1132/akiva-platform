//! Panic handling.
//!
//! Installs a panic hook that formats the payload and forwards it through
//! the HAL. In release builds `panic = "abort"` in `Cargo.toml` still
//! traps the wasm; the hook just ensures the user sees a real error
//! string on the crash screen before the trap fires.

use crate::host;

/// Called once from `akiva_boot`.
pub fn install() {
    std::panic::set_hook(Box::new(|info| {
        let location = info
            .location()
            .map(|l| format!("{}:{}", l.file(), l.line()))
            .unwrap_or_else(|| "<unknown>".into());

        let msg = if let Some(s) = info.payload().downcast_ref::<&'static str>() {
            (*s).to_string()
        } else if let Some(s) = info.payload().downcast_ref::<String>() {
            s.clone()
        } else {
            "panic payload is not a string".to_string()
        };

        let line = format!("panic at {}: {}", location, msg);
        let bytes = line.as_bytes();
        // SAFETY: ptr/len describe a live slice; hal_panic does not return.
        unsafe { host::hal_panic(bytes.as_ptr(), bytes.len()) }
    }));
}
