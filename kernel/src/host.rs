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
    pub fn hal_draw_round_rect(x: f32, y: f32, w: f32, h: f32, radius: f32, rgba: u32);
    pub fn hal_draw_text(x: f32, y: f32, size_px: f32, rgba: u32, ptr: *const u8, len: usize);
    // Stone 13.0prep — outline + line ops. width is the stroke in dp.
    pub fn hal_draw_stroke_rect(x: f32, y: f32, w: f32, h: f32, width: f32, rgba: u32);
    pub fn hal_draw_stroke_round_rect(
        x: f32, y: f32, w: f32, h: f32, radius: f32, width: f32, rgba: u32,
    );
    pub fn hal_draw_line(x0: f32, y0: f32, x1: f32, y1: f32, width: f32, rgba: u32);
    /// Stone 13.0prep C — blit a drawable. `drawable_id == 0` is the
    /// missing-drawable sentinel; the host paints a magenta placeholder.
    pub fn hal_draw_image(drawable_id: u32, x: f32, y: f32, w: f32, h: f32);
    /// Foundation gap — blit a drawable owned by another package. The
    /// host's foreign-drawable cache is keyed by `(package_id, name)`
    /// strings (string keys are needed because each package's own
    /// drawable_ids start at 1 and would collide on a flat cache).
    /// Missing entries paint the same magenta placeholder as
    /// `hal_draw_image`. Used by the Shell to render foreign-package
    /// launcher icons after reading `pkgs::PackageEntry.icon`.
    pub fn hal_draw_foreign_image(
        pkg_ptr: *const u8, pkg_len: usize,
        name_ptr: *const u8, name_len: usize,
        x: f32, y: f32, w: f32, h: f32,
    );
    /// Stone 13.3 — transform stack. `push_transform` pairs with
    /// `pop_transform`. Used by activity push/pop slide animations
    /// (`apply_props` wraps a surface's ops with these). Backed by
    /// the Canvas 2D state stack (`save` + `translate` + `scale` /
    /// `restore`) so matrix composition follows the standard.
    pub fn hal_draw_push_transform(dx: f32, dy: f32, scale: f32);
    pub fn hal_draw_pop_transform();
    /// Push a rectangular clip. `pop_clip` restores the previous
    /// mask. The SDK's `Canvas::push_clip` / `pop_clip` enforces
    /// matched pairs.
    pub fn hal_draw_push_clip(x: f32, y: f32, w: f32, h: f32);
    pub fn hal_draw_pop_clip();
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
    // must not persist it. All per-callback traffic (lifecycle, input,
    // render, saved-state, intents, results) flows through
    // `hal_app_dispatch` / `hal_app_get_result` below.

    pub fn hal_app_register(
        package_id_ptr: *const u8, package_id_len: usize,
        class_ptr: *const u8, class_len: usize,
    ) -> u32;
    pub fn hal_app_unregister(handle: u32);

    // ── Unified host→app dispatch (Stone 11.4b) ───────────────────────────
    //
    // `hal_app_dispatch` is the single multiplexer keyed by `cmd_kind`
    // that replaced the per-callback `hal_app_lifecycle`/`hal_app_pointer`/
    // `hal_app_key`/`hal_app_resize`/`hal_app_render`/`hal_app_save_state`/
    // `hal_app_restore_state`/`hal_app_new_intent`/`hal_app_activity_result`
    // imports in Stone 11.4b-5. The JS host reads the `[handle: u32 LE, ...]`
    // prefix from `payload`, looks up the wasm process that owns the handle,
    // posts the payload to that package's app-worker (Stone 11.4c), and
    // returns a `future_id` immediately:
    //   - `0`        — synchronously rejected (handle miss, alloc fail);
    //                  no follow-up needed.
    //   - non-zero   — request in-flight; kernel must `hal_app_poll_future`
    //                  to drive it forward. Once the worker replies the
    //                  status flips to READY (or ERROR) and the kernel
    //                  fetches result bytes — when any are produced — via
    //                  `hal_app_get_result`.
    //
    // CREATE special case: the kernel passes only `[handle]` in the
    // payload; the JS host augments with the slot's class string before
    // forwarding to the worker's `akiva_app_dispatch`.
    pub fn hal_app_dispatch(
        cmd_kind: u32,
        payload_ptr: *const u8,
        payload_len: u32,
    ) -> u64;

    /// Stone 11.4c — poll the per-future status the worker reports back
    /// via `dispatch_done`. Status codes match `op_kinds::STATUS`:
    ///   0 = PENDING (try again next frame),
    ///   1 = READY   (kernel may now call `hal_app_get_result`),
    ///   2 = ERROR   (worker trapped or rejected the dispatch).
    /// Polling an unknown future_id returns ERROR. A successful READY
    /// or ERROR poll **does not** evict the entry — `hal_app_get_result`
    /// is the single-shot consume.
    pub fn hal_app_poll_future(future_id: u64) -> u32;

    /// Companion to `hal_app_dispatch`: fetch the result bytes for a
    /// READY future (RENDER paint stream, SAVE_STATE encoded state, or
    /// any other byte-producing cmd_kind). Two-phase buffer convention:
    /// kernel passes `out_ptr` + `out_cap`, host copies up to `out_cap`
    /// bytes, returns the *needed* length. If the return exceeds
    /// `out_cap` the kernel reallocates and retries; the entry is
    /// evicted only when the copy fits.
    pub fn hal_app_get_result(
        future_id: u64,
        out_ptr: *mut u8,
        out_cap: u32,
    ) -> u32;

    // ── Stone 11.5: trapped-package drain ─────────────────────────────────
    //
    // The kernel calls `hal_drain_trapped_packages` once per frame. The
    // host returns the total bytes needed to encode every package_id
    // that has trapped since the last drain (LF-separated UTF-8). If
    // the return value exceeds `out_cap` nothing is copied and the
    // entries stay queued; the kernel retries with a larger buffer.
    // A successful copy evicts the staged set — single-shot consume.
    //
    // Each drained package_id maps to a `Framework::mark_package_trapped`
    // call on the kernel side, which transitions every Activity record
    // in that package to `LifecycleState::Trapped` and clears any
    // pending future slots.
    pub fn hal_drain_trapped_packages(out_ptr: *mut u8, out_cap: u32) -> u32;

    // ── Phase 12.1: per-package memory accounting ─────────────────────────
    //
    // `hal_app_memory_current(package_id)` returns the most recent
    // `WebAssembly.Memory.buffer.byteLength` reported by the named
    // app's worker on its last pong. The kernel calls this once per
    // frame for every installed package and forwards the value to
    // `Framework::record_app_memory`, which keeps a `(current, peak)`
    // entry per package for Phase 12.3 / 12.5 to consult.
    //
    // Returns 0 when the package has no spawned worker yet (e.g. a
    // freshly installed package between manifest registration and
    // first activity launch) or the worker hasn't replied to a pong
    // yet (cold boot before the first heartbeat lands).
    pub fn hal_app_memory_current(pid_ptr: *const u8, pid_len: usize) -> u64;

    // ── Phase 12.2: per-package CPU attribution ───────────────────────────
    //
    // `hal_app_cpu_ms(package_id)` returns the cumulative CPU time
    // (ms, f64) the named app's worker has spent inside wasm exports
    // — `akiva_app_dispatch`, `akiva_app_poll_dispatch`,
    // `akiva_app_get_result`, `akiva_app_drive_spawned`. Each app
    // worker brackets these with `performance.now()` and reports the
    // running total on every heartbeat pong; the kernel pulls the
    // value once per frame and forwards through
    // `Framework::record_app_cpu`, which derives the recent-window
    // rate.
    //
    // Returns `0.0` for packages with no spawned worker yet or no
    // pong reply landed. A worker restart resets the cumulative to
    // `0.0`; `CpuBudget` clamps the resulting negative delta at zero
    // so the rate doesn't get poisoned.
    pub fn hal_app_cpu_ms(pid_ptr: *const u8, pid_len: usize) -> f64;

    // ── Stone 11.6.7 #5: per-package watchdog timeout override ────────────
    //
    // `hal_app_set_watchdog_timeout_ms(package_id, ms)` registers a
    // per-package override sourced from
    // `manifest.runtime.watchdog_timeout_ms`. The host's heartbeat
    // tick consults the per-package map first, then the global
    // `WATCHDOG.TIMEOUT_MS` (or its `globalThis.__akivaConfig`
    // override). Pass `0` to clear an override.
    //
    // Called during `framework::boot` once per package that
    // declared the field; absent packages stay on the default.
    pub fn hal_app_set_watchdog_timeout_ms(pid_ptr: *const u8, pid_len: usize, ms: u32);

    // ── Phase 12.3: LMK eviction ──────────────────────────────────────────
    //
    // `hal_evict_package(package_id)` terminates the named app's worker
    // and discards its in-flight dispatch entries. The kernel calls this
    // for every victim returned by `Framework::drive_lmk` per frame —
    // host-side it's `worker.terminate()` plus a `_dispatches` cleanup,
    // mirroring the watchdog kill path but driven by memory pressure
    // rather than liveness. Framework-side cleanup (mark every activity
    // for the package as `Trapped`, forget memory + cpu accounting) is
    // a separate `Framework::evict_package` call the kernel makes
    // immediately after; the two halves don't depend on each other and
    // are documented separately so a future host (Stone 13's
    // multi-display) can swap in its own implementation.
    //
    // Returns `1` on a successful terminate, `0` if no worker was
    // spawned for the package (no-op). Idempotent — calling twice is
    // not an error; the second call returns `0`.
    pub fn hal_evict_package(pid_ptr: *const u8, pid_len: usize) -> u32;

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

    // ── Activity result drain (Stone 9.5) ─────────────────────────────────
    //
    // Two drain primitives mirror the Stone 9.4 shape:
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
    // The host→app delivery (firing the activity_result callback) goes
    // through `hal_app_dispatch` with cmd_kind ACTIVITY_RESULT, not a
    // dedicated import.
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

    // ── Stone 13.2: dialog resolution ─────────────────────────────────────
    //
    // After `akiva_open_dialog` has been called, the framework owns the
    // dialog's lifetime. Each frame the kernel drains
    // `Framework::drain_dialog_resolutions` and forwards each resolution
    // to the host with `hal_dialog_complete`. The host looks up the
    // (`handle`, `future_id`) → `(packageId, workerFutureId)` pair it
    // recorded at SHOW_DIALOG time and posts `host_op_done` back to that
    // app worker, which unblocks the SDK's `DialogFuture`.
    //
    // `kind` matches `framework::dialog::KIND_BUTTON` (0) or
    // `KIND_DISMISSED` (1); `button_idx` is meaningful only for
    // `KIND_BUTTON`.
    pub fn hal_dialog_complete(handle: u32, future_id: u64, kind: u32, button_idx: u32);
}

/// Safe wrapper for hal_log that never panics.
#[inline]
pub fn log(level: LogLevel, msg: &str) {
    let bytes = msg.as_bytes();
    // SAFETY: slice pointer is valid for `bytes.len()`, HAL reads only.
    unsafe { hal_log(level as u32, bytes.as_ptr(), bytes.len()) }
}
