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

    // ── Stone 11.4b: unified host→app dispatch ────────────────────────────
    //
    // `hal_app_dispatch` replaces the 12 per-callback imports
    // (`hal_app_lifecycle`, `hal_app_pointer`, …, `hal_app_activity_result`)
    // with a single multiplexer keyed by `cmd_kind`. The JS host reads the
    // `[handle: u32 LE, ...]` prefix from `payload`, looks up the wasm
    // process that owns the handle, copies the payload into app memory,
    // and forwards to the app's `akiva_app_dispatch` export. Returns the
    // app's `future_id`:
    //   - `0`        — sync, no result bytes (every ack-only cmd_kind).
    //   - non-zero   — sync result bytes available; kernel calls
    //                  `hal_app_get_result(future_id, ...)` next.
    //
    // The legacy per-callback imports below are still declared (and the
    // JS env still maps them) so the migration can land without breaking
    // mid-air. Stone 11.4b-5 removes them.
    //
    // CREATE special case: the kernel passes only `[handle]` in the
    // payload; the JS host augments with the slot's class string before
    // forwarding to `akiva_app_dispatch` — same split as the legacy
    // path where `_dispatchCreate` looks up the class on the JS side.
    pub fn hal_app_dispatch(
        cmd_kind: u32,
        payload_ptr: *const u8,
        payload_len: u32,
    ) -> u64;

    /// Companion to `hal_app_dispatch`: fetch the result bytes for a
    /// non-zero `future_id` returned by a RENDER or SAVE_STATE
    /// dispatch. Two-phase buffer convention (matches `hal_app_render`
    /// pre-11.4b): kernel passes `out_ptr` + `out_cap`, host copies up
    /// to `out_cap` bytes, returns the *needed* length. If the return
    /// exceeds `out_cap` the kernel reallocates and retries.
    pub fn hal_app_get_result(
        future_id: u64,
        out_ptr: *mut u8,
        out_cap: u32,
    ) -> u32;

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

    // ── Saved state (Phase 6) ─────────────────────────────────────────────
    //
    // Symmetric with `hal_app_render`: the app encodes its `SavedState` into
    // its own wire buffer, the host copies up to `out_cap` bytes into
    // `out_ptr`, and returns the total length the app produced. If the
    // return value exceeds `out_cap` the kernel reallocates and retries.
    //
    // `hal_app_restore_state` goes the other direction: the kernel hands
    // previously-persisted bytes back, the host copies them into the app's
    // memory via `akiva_app_alloc`, and the app decodes inside
    // `akiva_app_restore_state`. Returns 0 on success, non-zero if the
    // handle is unknown or the host-side copy failed.
    pub fn hal_app_save_state(
        handle: u32,
        out_ptr: *mut u8,
        out_cap: u32,
    ) -> u32;
    pub fn hal_app_restore_state(
        handle: u32,
        bytes_ptr: *const u8,
        bytes_len: u32,
    ) -> u32;

    // ── App → host signals (Stone 9.3) ────────────────────────────────────
    //
    // The app imports `akiva_host_finish()` to ask the platform to tear it
    // down. The host records one pending flag per handle; the kernel drains
    // it after each callback via `hal_app_drain_finish` and flips the
    // framework's `ActivityContext.finish_requested`, matching the in-kernel
    // Shell's behaviour (see `framework/src/wasm_activity.rs::WasmActivity`).
    //
    // Returns 1 if a finish was pending (and resets the flag), 0 otherwise.
    pub fn hal_app_drain_finish(handle: u32) -> u32;

    // ── Intent delivery (Stone 9.4) ───────────────────────────────────────
    //
    // `hal_app_drain_intent` pairs with `hal_app_drain_finish`: the app
    // raised a `start_activity` request via `akiva_host_start_activity`,
    // we drain it here into the kernel buffer the framework can feed to
    // `IntentRouter`. Two-phase buffer (same shape as `hal_app_save_state`):
    // the return value is the total blob size; if it exceeds `out_cap`
    // nothing is copied and the kernel retries with a larger buffer.
    // A zero return means "no pending intent" — not the same as "empty
    // intent", which is impossible (the encoder always emits a minimum
    // of 17 bytes even for the empty action).
    pub fn hal_app_drain_intent(
        handle: u32,
        out_ptr: *mut u8,
        out_cap: u32,
    ) -> u32;

    // `hal_app_new_intent` fires the app's `akiva_app_new_intent` export.
    // Payload is a fresh `Intent::encode` blob the framework produced.
    // Returns 0 on success, non-zero if the handle is unknown or the
    // host-side copy failed (same convention as `hal_app_restore_state`).
    pub fn hal_app_new_intent(
        handle: u32,
        bytes_ptr: *const u8,
        bytes_len: u32,
    ) -> u32;

    // ── Activity result (Stone 9.5) ───────────────────────────────────────
    //
    // Three primitives, same shapes as their Stone 9.4 counterparts:
    //
    //   `hal_app_drain_start_for_result` — two-phase drain of a pending
    //   `start_activity_for_result` request. Returns total payload size
    //   (`intent.encode` bytes + trailing `u32` request_code, so at least
    //   21 bytes when present); writes `request_code` into `*rc_out` on
    //   a successful copy. Zero return = nothing pending.
    //
    //   `hal_app_drain_set_result` — two-phase drain of whatever bytes
    //   the app stashed via `akiva_host_set_result`. Payload is an
    //   encoded `ActivityResult` (i32 code + u32 data_len + data).
    //   Zero return = nothing stashed.
    //
    //   `hal_app_activity_result` — fires the app's
    //   `akiva_app_activity_result` export with the caller-chosen
    //   `request_code` and the encoded `ActivityResult` bytes. 0 on
    //   success, non-zero on handle-miss or host-copy failure.
    pub fn hal_app_drain_start_for_result(
        handle: u32,
        out_ptr: *mut u8,
        out_cap: u32,
        rc_out: *mut u32,
    ) -> u32;
    pub fn hal_app_drain_set_result(
        handle: u32,
        out_ptr: *mut u8,
        out_cap: u32,
    ) -> u32;
    pub fn hal_app_activity_result(
        handle: u32,
        request_code: u32,
        bytes_ptr: *const u8,
        bytes_len: u32,
    ) -> u32;

    // ── Permission grants (Stone 10.3b) ───────────────────────────────────
    //
    // Push the list of permissions granted to `package_id` to the host so
    // it can filter the `akiva_host_*` import map at `_instantiateProcess`
    // time. Called once per installed package at the end of
    // `framework::boot`, before any activity is launched.
    //
    // `grants_ptr..grants_len` is a UTF-8, LF-separated list of permission
    // names (e.g. "akiva.permission.STORAGE_SANDBOX\nakiva.permission.NETWORK").
    // An empty payload means "no grants" — the app gets only the auto set.
    // Last-write-wins semantics: a second call for the same `package_id`
    // replaces the earlier list, matching how `PackageManager::install`
    // rewrites the full set on upgrade.
    pub fn hal_host_set_grants(
        package_id_ptr: *const u8, package_id_len: usize,
        grants_ptr: *const u8, grants_len: usize,
    );

    // ── Package list mirror (Stone 11.0a) ─────────────────────────────────
    //
    // Push the snapshot of installed packages to the host so apps that hold
    // `akiva.permission.QUERY_PACKAGES` can enumerate them via
    // `akiva_host_query_packages`. Called once at the end of
    // `framework::boot` after rehydration; future package install/uninstall
    // flows will re-push the full set (last-write-wins, same shape as
    // `hal_host_set_grants`).
    //
    // `bytes_ptr..bytes_len` is the framework's `pkgs_wire::encode` output:
    //
    //   [u32 LE count] then for each record
    //     [u16 LE id_len][id bytes][u16 LE class_len][class bytes]
    //     [u16 LE name_len][name bytes]
    //
    // All strings are UTF-8. An empty payload (`bytes_len == 0`) means "no
    // packages installed", and `akiva_host_query_packages` returns an empty
    // blob to apps. Length-prefixed (rather than LF-separated like grants)
    // because app names can contain whitespace and arbitrary unicode.
    pub fn hal_host_set_packages(bytes_ptr: *const u8, bytes_len: usize);
}

/// Safe wrapper for hal_log that never panics.
#[inline]
pub fn log(level: LogLevel, msg: &str) {
    let bytes = msg.as_bytes();
    // SAFETY: slice pointer is valid for `bytes.len()`, HAL reads only.
    unsafe { hal_log(level as u32, bytes.as_ptr(), bytes.len()) }
}
