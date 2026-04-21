//! Kernel → HAL imports.
//!
//! The JS HAL provides these functions at instantiation time. This is the
//! internal runtime ABI between the kernel wasm and the worker's JS host —
//! it is NOT the user-facing app ABI (that one lives in `wit/akiva.wit` and
//! is Component Model).
//!
//! All pointers refer into kernel memory. Strings are UTF-8, not null-terminated;
//! length is always passed explicitly.

/// Log level shared with the HAL. Matches the `log` interface in the WIT.
#[repr(u32)]
#[derive(Copy, Clone, Debug)]
pub enum LogLevel {
    Trace = 0,
    Debug = 1,
    Info = 2,
    Warn = 3,
    Error = 4,
}

extern "C" {
    /// Emit a log record. HAL routes it to console / in-app viewer / file sink
    /// per observability policy (see docs/observability.md).
    pub fn hal_log(level: u32, ptr: *const u8, len: usize);

    /// Report a panic — must not return. HAL should display a crash screen
    /// and notify the activity manager.
    pub fn hal_panic(ptr: *const u8, len: usize) -> !;

    /// Monotonic wall-clock in milliseconds. Matches `performance.now()` on
    /// the host side.
    pub fn hal_now_ms() -> f64;

    // ── Display / renderer backend ────────────────────────────────────────
    // Phase 1a uses a flat immediate-mode API. Phase 2 replaces this with a
    // submit(frame_commands) call against the RendererBackend trait, as
    // documented in ARCHITECTURE.md §10.

    pub fn hal_surface_width() -> u32;
    pub fn hal_surface_height() -> u32;
    pub fn hal_surface_density() -> f32;

    pub fn hal_draw_clear(rgba: u32);
    pub fn hal_draw_rect(x: f32, y: f32, w: f32, h: f32, rgba: u32);
    pub fn hal_draw_text(x: f32, y: f32, size_px: f32, rgba: u32, ptr: *const u8, len: usize);
    pub fn hal_draw_present();

    // ── Storage ───────────────────────────────────────────────────────────
    // Backed by OPFS + SyncAccessHandle inside the worker. MVP uses string
    // keys / byte values; see docs/observability.md and ARCHITECTURE.md
    // for the full persistence story.

    pub fn hal_storage_read(key_ptr: *const u8, key_len: usize, out_len: *mut usize) -> *mut u8;
    pub fn hal_storage_write(key_ptr: *const u8, key_len: usize, val_ptr: *const u8, val_len: usize) -> u32;
    pub fn hal_storage_delete(key_ptr: *const u8, key_len: usize) -> u32;

    // ── Process / leader coordination ─────────────────────────────────────
    // Post a message to a follower tab. Used by the window manager to push
    // frame deltas to each display (one OffscreenCanvas per tab).
    pub fn hal_post_to_display(display_id: u32, ptr: *const u8, len: usize);
}

/// Safe wrapper for hal_log that never panics.
#[inline]
pub fn log(level: LogLevel, msg: &str) {
    let bytes = msg.as_bytes();
    // SAFETY: slice pointer is valid for `bytes.len()`, HAL reads only.
    unsafe { hal_log(level as u32, bytes.as_ptr(), bytes.len()) }
}
