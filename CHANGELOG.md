# Akiva ABI Changelog

This file tracks every change to the `ABI_VERSION` constant in
[`sdk-rust/src/lib.rs`](sdk-rust/src/lib.rs) and
[`framework/src/lib.rs`](framework/src/lib.rs). Bumps follow the
semantic-versioning policy in [docs/abi.md §1.2](docs/abi.md). PATCH
bumps are intentionally unused in the `0.X` line — every change either
adds (MINOR) or breaks (MAJOR).

The "what" column lists the public surface that changed; details and
rationale live in the linked stone or risk entry.

## [0.4.0] — 2026-05 (Stone 11.6.9)

**MINOR** — additive: one new HAL paint op + one new device-config
export. The qualifier system that backs `values-night/` ships entirely
inside the framework + SDK + JS runtime; no new ABI surface for that
piece.

- **Added.** `hal_draw_round_rect(x, y, w, h, radius, rgba)` HAL
  import (kernel-side) — backs the new `Canvas::fill_round_rect` SDK
  method. New paint opcode `OP_FILL_ROUND_RECT = 7` joins the v0
  paint stream; encode/decode is byte-for-byte symmetric (1 opcode
  byte + 5 × f32 + u32 = 25 bytes total). Maps to `roundRect()` on
  the Canvas2D backend (Baseline 2023). `radius=0.0` falls back to
  `fillRect`. Promoted from Stone 11.6.9 #1 — every native OS uses
  rounded corners as its default UI shape; sharp-cornered Akiva apps
  looked like late-90s web pages by comparison.
- **Added.** `akiva_config_change(night: i32)` kernel export — host
  calls it on boot and on every `prefers-color-scheme` change
  observed via `matchMedia`. Flips
  `ResourceManager::set_night_mode(bool)`, which selects the
  `values-night/` overlay for subsequent `@string/` / `@color/`
  lookups. Idempotent on the current state. Promoted from Stone
  11.6.9 #3.
- **Added (framework — no ABI surface).** Qualifier-aware
  `ResourceTable` with default + `Option<night>` overlays;
  `ResourceManager` owns a `DeviceConfig` and resolves through it on
  every lookup. `PackageManager::read_resource_tables` loads both
  `res/values/*.toml` and `res/values-night/*.toml` at boot.
  Uninstall sweeps both qualifier directories. CLI `akiva build` /
  `akiva package` / dev-apps stage all carry `values-night/`
  alongside `values/`.
- **Added (JS runtime — no ABI surface).** `AppRegistry.setNightMode`
  re-resolves resources for every running app and posts
  `set_resources` to each app worker; the worker hot-swaps its
  in-memory strings/colors maps without restarting.
  `tab.js` installs a `matchMedia('(prefers-color-scheme: dark)')`
  listener and forwards `config-change` to the kernel-worker.
- **Changed (JS runtime — no ABI surface).** `tab.js` resize handler
  switched from `window.addEventListener("resize")` to
  `ResizeObserver` reading `devicePixelContentBoxSize` (Baseline
  2022). Catches zoom-level DPR changes (Ctrl+/-) that the plain
  resize event misses; reports physical-pixel boxes directly so the
  CSS-px×DPR rounding mismatch on fractional zoom levels is gone.
  Promoted from Stone 11.6.9 #2.
- **Changed (apps — no ABI surface).** Calculator and Shell palettes
  split: `res/values/colors.toml` is now the light-theme default and
  the previous-default dark palette moved under
  `res/values-night/colors.toml`. Shell src/lib.rs converted from
  hardcoded `const u32` constants to per-frame
  `akiva_sdk::res::color()` lookups so a runtime night-mode flip is
  visible on the next paint without a respawn.

## [0.3.0] — 2026-05 (Stone 11.6.8)

**MINOR** — additive: two new host imports, a new status code, and
an SDK-only sync surface.

- **Added.** `akiva_host_random_bytes(out_ptr, len) -> u32` import —
  CSPRNG primitive, ungated. Maps to `crypto.getRandomValues` in the
  worker shell with internal chunking around the 64 KiB cap. Wired
  via `getrandom`'s `register_custom_getrandom!` so the entire Rust
  crypto ecosystem (`rand`, `uuid`, `ring`, `rustls`, …) works in
  Akiva apps without app-side glue. Promoted from `risks.md §7.15`.
- **Added.** `akiva_host_clock_ms(clock_id) -> u64` import — single
  extern that mirrors POSIX `clock_gettime(clockid_t)`. `clock_id`
  values: `0 = Monotonic` (`performance.now()`), `1 = RealTime`
  (`Date.now()`). Single extern with id keeps the ABI extensible —
  future clock types can land without a new import or another bump.
  SDK side: `clock_gettime_ms(ClockId)` in `akiva_sdk::clock`.
  Promoted from `risks.md §7.18`.
- **Added.** `STATUS_BUSY = 3` — io_poll status returned when a slot
  hits `MAX_PENDING_PER_SLOT = 256` concurrent in-flight dispatches.
  Apps backoff or treat as `Err(_::Busy)`. Promoted from
  `risks.md §7.16`.
- **Added (SDK only — no ABI surface).** `akiva_sdk::sync` module —
  `Channel<T, N>`, `Mutex<T>`, `Signal<T>`. Single-threaded futurized
  primitives for spawned-task coordination. Backed by the executor's
  wake-flag mechanism (also new in this release). Promoted from
  `risks.md §7.17`.
- **Added (docs only).** Versioning policy in [`docs/abi.md
  §1.2`](docs/abi.md) + this CHANGELOG. Promoted from
  `risks.md §3`.
- **Internal.** `executor::poll_once` switched from a no-op waker to
  one that sets a thread-local flag; `runtime::drive_spawned` loops
  until quiescence. Affects no public API but enables the sync
  primitives to coordinate via real `Waker::wake()` calls.

## [0.2.0] — 2026-05 (P0 #4 + Stone 11.6.5)

**MAJOR** — pre-1.0 breaking change to the dispatch protocol.

- **Changed.** `akiva_app_dispatch` is now async-resumable: takes a
  `dispatch_id: u64`, returns acceptance status (0 = accepted), and
  the result is fetched via the new `akiva_app_poll_dispatch` and
  `akiva_app_get_result` exports. The old "single-shot synchronous
  dispatch returning packed (status, len)" shape is gone. See
  [`docs/risks.md §7.1`](docs/risks.md).
- **Added.** `akiva_app_drive_spawned()` export — pumps tasks queued
  by `TaskScope::spawn`. Stone 11.6.5 #11.
- **Changed.** `Slot.pending` from `Option<PendingDispatch>` to
  `HashMap<u64, PendingDispatch>` so multiple concurrent host calls
  per activity work. Stone 11.6.5 #12. SDK-internal but visible
  through `futures::join!(read("a"), read("b"))` patterns now
  succeeding where they used to assertion-fail.

## [0.1.0] — 2026-05 (initial)

First tagged ABI. Includes:
- `akiva_app_register / unregister`, `akiva_app_dispatch` (single-shot),
  `akiva_app_alloc / dealloc`, `akiva_app_abi_version`.
- `akiva_host_fire / io_start / io_poll` multiplexers — fire kinds
  `LOG / FINISH / START_ACTIVITY / START_ACTIVITY_FOR_RESULT /
  SET_RESULT`; io kinds `TEXT_WIDTH / QUERY_PACKAGES / STORAGE_GET /
  STORAGE_SET / STORAGE_DELETE`.
- Resource imports: `akiva_host_resource_color / string / has_color /
  has_string`.
- `hal_app_*` HAL imports for the host-side dispatcher.
- Wire codecs: paint, saved-state, intent, pkgs blob, result blob.
- Capabilities: `akiva.permission.{LAUNCH_ANY_APP, QUERY_PACKAGES,
  STORAGE_SANDBOX}`.

[0.4.0]: docs/abi.md
[0.3.0]: docs/abi.md
[0.2.0]: docs/abi.md
[0.1.0]: docs/abi.md
