# Akiva ABI Changelog

This file tracks every change to the `ABI_VERSION` constant in
[`sdk-rust/src/lib.rs`](sdk-rust/src/lib.rs) and
[`framework/src/lib.rs`](framework/src/lib.rs). Bumps follow the
semantic-versioning policy in [docs/abi.md §1.2](docs/abi.md). PATCH
bumps are intentionally unused in the `0.X` line — every change either
adds (MINOR) or breaks (MAJOR).

The "what" column lists the public surface that changed; details and
rationale live in the linked stone or risk entry.

## [0.3.0] — 2026-05 (Stone 11.6.8)

**MINOR** — additive: new host imports, a new status code, and an
SDK-only sync surface.

- **Added.** `akiva_host_random_bytes(out_ptr, len)` import — CSPRNG
  primitive, ungated. Maps to `crypto.getRandomValues` in the worker
  shell. Promoted from `risks.md §7.15`.
- **Added.** `akiva_host_wall_clock_ms() -> u64` import — wall-clock
  primitive, ungated. Maps to `Date.now()`. Distinct from the
  monotonic `ctx.timestamp_ms`. Promoted from `risks.md §7.18`.
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

[0.3.0]: docs/abi.md
[0.2.0]: docs/abi.md
[0.1.0]: docs/abi.md
