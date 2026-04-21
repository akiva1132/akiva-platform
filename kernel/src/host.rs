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

    // ── App runtime (Phase 2b.2) ──────────────────────────────────────────
    //
    // The host (worker.js) instantiates each installed app's wasm module
    // alongside the kernel. These imports let the kernel drive a chosen
    // activity instance inside a given app without the kernel ever holding
    // the app's memory. See docs/abi.md §2.1 for the raw-export contract
    // on the app side.
    //
    // `hal_app_register(package_id, class)` returns a non-zero handle the
    // kernel reuses for the rest of the activity's lifetime, or `0` if
    // the package/class isn't installed or the wasm failed to instantiate.
    // The handle is only meaningful paired with this host — the kernel
    // must not persist it.
    //
    // `hal_app_render` copies at most `out_cap` paint-op bytes into the
    // kernel buffer and returns the total number of bytes the app
    // produced. If the return value exceeds `out_cap` the buffer was
    // truncated and the kernel should retry with a larger allocation.

    pub fn hal_app_register(
        package_id_ptr: *const u8, package_id_len: usize,
        class_ptr: *const u8, class_len: usize,
    ) -> u32;
    pub fn hal_app_unregister(handle: u32);

    /// `transition` matches `framework::wasm_activity::LifecycleTransition`.
    pub fn hal_app_lifecycle(handle: u32, transition: u32);

    pub fn hal_app_pointer(
        handle: u32,
        x: f32, y: f32,
        button: u32, phase: u32,
        ts_ms: f64,
    );
    pub fn hal_app_key(
        handle: u32,
        code: u32, modifiers: u32, phase: u32,
        ts_ms: f64,
    );
    pub fn hal_app_resize(handle: u32, width_px: u32, height_px: u32, density: f32);

    pub fn hal_app_render(
        handle: u32,
        ts_ms: f64,
        out_ptr: *mut u8,
        out_cap: u32,
    ) -> u32;
}

/// Safe wrapper for hal_log that never panics.
#[inline]
pub fn log(level: LogLevel, msg: &str) {
    let bytes = msg.as_bytes();
    // SAFETY: slice pointer is valid for `bytes.len()`, HAL reads only.
    unsafe { hal_log(level as u32, bytes.as_ptr(), bytes.len()) }
}
