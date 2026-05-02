# Risk Register & Deferred Concerns

> **What this doc is for.** A small, single place for architectural concerns that aren't actionable right now but **must not be forgotten**. Each entry names the concern, why deferral is acceptable today, and the trigger that would promote it to a stone. If a concern becomes a stone, its entry moves to `ROADMAP.md` and is deleted here.
>
> **What it is not.** A bug list (use GitHub issues), a feature wishlist (use `vision.md`), or a design doc (use `docs/*-design.md`).

_Created: 2026-04-22, after Stone 9.4 landing._

---

## 1. Process-level isolation + crash recovery → **promoted to Phase 11 of v0.3**

> **Promoted 2026-04-22.** See [`ROADMAP.md` — שלב 11](../ROADMAP.md). The short version of the trigger criterion ("first genuinely third-party app") was tightened after an external review: we don't need a third-party app to know this is the gating systems problem. Phase 10 (Permissions) closing means the capability model is in place — the next systems-core bet is this one. The full concern text below stays here for historical context until Phase 11 starts; at that point the whole section moves to the "retired / promoted" footer.

**Original concern (kept for context).** Every app runs in the **same Web Worker** as the kernel. Each package gets its own `WebAssembly.Memory` (memory-space isolation is fine), but all apps share: the worker event loop, the JS heap, the OPFS `SyncAccessHandle`, the rAF frame budget.

Consequences: infinite loop in any app freezes the whole OS (no preemption); wasm trap is caught + logged but the `ActivityRecord` stays Resumed (zombie); no Low-Memory-Killer (instances live until worker terminates).

**Code surfaces that will change** (annotated for Phase 11 planning):

- [`runtime/web/app_registry.js`](../runtime/web/app_registry.js) — `_instantiateProcess` moves to a worker-per-app model; error-catch paths become message-handler paths.
- [`runtime/web/worker.js`](../runtime/web/worker.js) — frame loop + event dispatch go through `postMessage` to app workers.
- [`framework/src/activity_manager.rs`](../framework/src/activity_manager.rs) — new `Trapped` lifecycle state (Phase 11.5).
- [`framework/src/wasm_activity.rs`](../framework/src/wasm_activity.rs) — `WasmHost` trait becomes async-capable (Phase 11.4a). Stone 11.3 made the **upward** direction (`akiva_host_*`) async via the unified-ABI futures; the downward direction (kernel→app `lifecycle`/`render`/etc.) is still inline-sync and falls to 11.4a.
- [`sdk-rust/src/runtime.rs`](../sdk-rust/src/runtime.rs) — host-import externs collapsed to three multiplexers in 11.3b/c (`akiva_host_fire`, `akiva_host_io_start`, `akiva_host_io_poll`); per-op extern wrappers gone. The Worker seam in 11.4c becomes JS-side only — Rust extern signatures don't change.

---

## 2. ABI v0 → v1 (Component Model) transition

**Concern.** Every new `extern "C"` pair (`akiva_host_*` + `hal_app_*`) adds to the eventual migration cost. Stone 9.4 added two (`akiva_host_start_activity`, `hal_app_drain_intent`) plus one export (`akiva_app_new_intent`). Stone 10 will add a capability-guarded import set.

The `research-component-model.md` memo deferred the migration to v0.4+. Meanwhile `wit/` sits as scaffolding with no concrete mapping to the current ABI.

**Why deferred.** Component Model tooling (`wit-bindgen`, host-side `wasmtime-component` equivalents for browser wasm) is still maturing. Migrating now would lock us into whichever toolchain is "working this quarter", risking a second migration.

**Trigger to promote.** Either (a) first breaking-ABI event that forces a version bump anyway — bundle the WIT cutover with it; or (b) the `wit-bindgen` JS target reaches feature parity with `extern "C"` wire format. Whichever comes first.

**Mitigations we already do.**

- Every new primitive is documented in [`docs/abi.md`](abi.md) with its wire shape — migration mapping is mechanical, not archeological.
- `ABI_VERSION` exists on both sides so a mismatched pair fails loudly at instantiation.
- Wire codecs (paint, saved-state, intent) live in named modules, each a single decode/encode pair — swappable in isolation.

**Size estimate when we pull the trigger.** 11 primitives × ~30 lines each = ~330 LoC of mechanical migration + toolchain churn + test-fixture rebuild. Budget 1 week.

---

## 3. 🔴 JS-side ABI governance / versioning policy — promoted to Stone 11.6.8

**Concern.** The project has `ABI_VERSION = "0.2.0"` in the SDK, `Intent::MAX_WIRE_BYTES` pinned in abi.md §2.5.1, and a §1.2 "narrow ABI review" clause — but no written rule for:

- **When does `0.X` → `1.0` happen?** Specifically: what minimum feature set + stability bar?
- **What constitutes a breaking change?** Adding an import is additive; renaming one is breaking; changing a wire format… depends.
- **Who gates changes?** Implicit "the design doc" today.

**Status.** Promoted from "premature to formalise" to Stone 11.6.8 פריט 5. The May 2026 audit determined that policy MUST be written before Stone 11.7 (akiva new + public docs) opens — otherwise the public docs will set an implicit policy by example, and rewinding becomes social, not technical.

**Where policy will live.** New section `## 1.X ABI versioning policy` in [`docs/abi.md`](abi.md), with bump rules (PATCH/MINOR/MAJOR), 1.0 promotion criteria, review process. New `CHANGELOG.md` at repo root. See [ROADMAP.md Stone 11.6.8 פריט 5](../ROADMAP.md) for DoD.

---

## 4. Web-platform interop

**Concern.** Akiva apps run in a browser tab but can't:

- open a URL in the host browser
- read or write the host clipboard
- accept a drag-and-dropped file
- embed a browser view (iframe equivalent)
- use platform APIs like geolocation, camera, notifications, Web Share

This is simultaneously a **missing feature** (obvious omissions from "an OS") and an **architectural inflection** — the clean sandbox ends the moment the first interop bridge lands. Stone 10's capability-via-imports model is the right foundation, but the actual imports (`akiva_host_clipboard_*`, `akiva_host_open_url`, etc.) haven't been designed.

**Why deferred.** The current app set (calculator, hello, tic-tac-toe) doesn't need any of it. Text/input (the other inflection point, §5 below) is the bigger lever.

**Trigger to promote.** The first app that genuinely needs one of these — most likely URL opening (notifications tap, "learn more" links) or clipboard (paste into calculator).

**Design anchors when we get there.**

- Every interop import gets a permission in the manifest.
- Host-visible state (clipboard, URL, camera) is **never** returned synchronously — permission prompts are async and the current ABI is sync-only. Will require a task/promise seam.

---

## 5. Text/input as the other inflection point

**Concern.** Current input model (`on_pointer(ctx, ev)` + `on_key(ctx, ev)`) is adequate for buttons and display text. It does **not** cover:

- Editable text fields (focus, caret, selection)
- IME / composition events (CJK, accent marks, dead keys)
- Clipboard integration (cut/copy/paste at field level)
- Accessibility — screen reader for text content
- Bidirectional text (LTR/RTL mix) in editable contexts — note: **MVP explicitly skips Hebrew/RTL/BiDi** per `memory/feedback_mvp_focus.md`, but English-only IMEs still need composition events

When the first app needs a real text field, the `on_key` path has to grow focus routing, composition state, selection API, clipboard bridges, and probably a new `TextInputEvent` alongside `KeyEvent`. The AKML widget set grows an `edit_text` primitive + its handler wiring.

**Why deferred.** None of the three current apps have editable text. It's a large design — deserves its own research doc when it lands.

**Trigger to promote.** First app proposal with a text field (search, form, chat). Or: explicit deliberate start-here — this is the single biggest platform-completeness lever after lifecycle.

**Size estimate.** Comparable to Phase 9 (activity model) — ~4-6 weeks and a research doc of its own.

---

## 6. Premature-abstraction temptation: consolidating "many small protocols"

**Originally raised pre-11.3, partially superseded.** A design review flagged that the host implemented 11 independent `akiva_host_*` protocols (paint, save_state, restore, finish drain, intent drain, new_intent, resources × 4, log, text_width) and suggested consolidation into "a few strong primitives" — e.g. a single typed message bus.

**Status after Stone 11.3.** Six of those protocols (log, text_width, finish, start_activity{,_for_result}, set_result, query_packages) collapsed into three multiplexers (`akiva_host_fire` / `akiva_host_io_start` / `akiva_host_io_poll`) routed by `op_kind`. The "rule of three" was met during the async-ABI redesign, and the consolidation paid for itself in 11.4 prep — one async seam to plumb cross-worker, not eleven.

**What stayed separate.** Resource lookups (4 functions: color, string, has_color, has_string) — readonly per-package queries with no async, no handle context, no side effect. Not worth the encode/decode overhead. `hal_app_*` (kernel→app side) is unchanged — different concern, different shape.

**Trigger to revisit anything further.** A third byte-transfer pattern beyond `_copyAppToKernel` / `_callAppWithBytes`, or a streaming surface that doesn't fit the future_id model. Neither is near.

---

## 7. Foundation gaps surfaced by Phase 11.4c/11.5 live testing (May 2026)

A self-audit after Stone 11.5 closed (process isolation + fault containment work) surfaced these gaps. Each is documented in detail elsewhere; this section is the single place that lists them so they don't get lost. See [`docs/isolation-design.md` §10.5 Phase 11 Foundation Gaps](isolation-design.md) for the full work plan.

### 7.1 ✅ SDK async executor — fixed (P0 #4, May 2026)

**Was.** `Activity` callbacks were `async fn`, but `block_on_ready` polled exactly once and panicked on `Pending`. Cross-worker host calls (storage::read) took ≥1 frame to resolve, so `.await` on them would panic. The `ReadHandle::poll()` workaround leaked the multi-frame nature into every app.

**Fix (~700 lines, breaking ABI bump).** A single-task-per-handle async-resumable runtime, with the dispatch protocol changed to:

```rust
akiva_app_dispatch(dispatch_id: u64, cmd: u32, ptr, len) -> u32   // 0 accepts, !=0 refuses
akiva_app_poll_dispatch(dispatch_id: u64) -> u64                   // packed (status, len)
akiva_app_get_result(dispatch_id: u64, out_len_ptr) -> *const u8   // single-shot
```

The wasm side stores the in-flight `Pin<Box<dyn Future + 'static>>` in the slot via raw-pointer borrows into `slot.activity` and `slot.ctx`. Soundness rests on `HashMap<u32, Box<Slot>>` heap-pinning every slot and on the framework's `PendingTransition` discipline guaranteeing no concurrent dispatches per handle. The `executor` module documents the SAFETY invariants.

The worker shell tracks `pendingDispatches: Map<bigint, ...>`. When `host_op_done` arrives the shell re-polls every still-pending dispatch — the wasm side caches terminal results, so re-polls of already-resolved dispatches are no-ops. Storage `read` is now a real `Future`:

```rust
async fn on_create(&mut self, _ctx: &mut Context<'_>) {
    if let Some(fut) = storage::read("notes") {
        if let Ok(Some(bytes)) = fut.await {
            self.text = String::from_utf8_lossy(&bytes).into_owned();
        }
    }
}
```

Render and `save_state` keep the synchronous fast-path: paint code shouldn't block on host I/O, and `save_state` is `&self` per the trait. `executor::run_to_ready` (the sync drain) still panics on Pending — accidental awaits in render surface immediately.

**Code.** `sdk-rust/src/{executor.rs (new), runtime.rs, storage.rs, lib.rs, activity.rs}`, `runtime/web/app_worker_shell.js`. Tests: `executor::tests::*` (4), `runtime::tests::*` (4), `storage::tests::read_future_resolves_to_none_on_non_wasm_stub` (1). 407 Rust tests + 54 vitest tests green.

**Carry-forward.** `WriteHandle` (P1 §7.x) for write-confirmation futures is a natural follow-up — the same async runtime now supports it.

### 7.2 ✅ `on_save_state` degraded under cross-worker — fixed (P0 #1, May 2026)

**Was.** `WasmActivity::save_state_bytes` called `host.save_state()` + `poll_future()` inline. Post-11.4c the first poll always returned `Pending`; release builds silently fell through to `Vec::new()`. Phase 6 saved-state was broken in production.

**Fix.** Added `Activity::dispatch_save_state_async(&self) -> u64` to the trait. `WasmActivity` overrides it to dispatch `save_state` and return the `future_id`; the legacy sync `save_state_bytes` returns empty for wasm activities. `Framework::destroy_with_persist` records a `PendingSave { future_id, package_id, class }` before walking pause→stop→destroy (worker FIFO handles all four in order). New `Framework::drive_pending_saves`, called once per frame from `akiva_frame`, polls each entry and persists the bytes when the worker replies. Empty bytes delete the entry; `Err` drops the entry without persisting.

In-process activities are unchanged — they still go through the sync `save_state_bytes` path, which `destroy_with_persist` consults only when `dispatch_save_state_async` returned `0`.

**Code.** `framework/src/{activity.rs, wasm_activity.rs, activity_manager.rs, lib.rs}`, `kernel/src/lib.rs::akiva_frame`. Two new tests: `drive_pending_saves_persists_when_future_resolves`, `drive_pending_saves_drops_entry_on_host_error`. 391 Rust tests + 54 vitest tests green.

### 7.3 ✅ Graceful shutdown signal — fixed (P0 #2, May 2026)

**Was.** Closing a tab terminated the workers without notifying apps. No chance for `save_state` to run; apps lost unflushed in-memory state on every tab close.

**Fix.** Checkpoint hook layered on top of the P0 #1 pipeline:
- `Framework::checkpoint(ts) -> usize` dispatches `save_state` at the foreground activity (no lifecycle change), records a `PendingSave`, and de-dups against in-flight saves so rapid checkpoints don't pile up dispatches.
- `akiva_checkpoint(ts: f64) -> u32` kernel export.
- `worker.js` handles `{type: "checkpoint"}` → calls the export.
- `tab.js` fires the checkpoint message on `visibilitychange` to `hidden` (the recommended hook — fires reliably on tab switch, swipe-close, OS-level backgrounding) and on `pagehide` with `persisted=false` (last-resort for tab close while foregrounded).

**Caveat that remains.** The save bytes travel async over the worker boundary. On `visibilitychange` the page stays alive (browsers keep background pages around for tens of seconds) so the round-trip completes. On hard close via `pagehide` the tab may die before bytes land — best effort only. The "last save" is the most-recent `visibilitychange`-triggered checkpoint; data produced after that and before close is lost.

**Code.** `framework/src/lib.rs::checkpoint`, `kernel/src/lib.rs::akiva_checkpoint`, `runtime/web/{worker.js, tab.js}`. Tests: `checkpoint_dispatches_save_state_for_foreground_activity` (de-dup + persist), `checkpoint_skips_when_no_foreground` (boot-time edge).

### 7.4 ✅ Trapped activity recycle — fixed (P0 #3, May 2026)

**Was.** When the watchdog terminated a worker (or a wasm trap was reported), `ActivityRecord` flipped to `Trapped` but never advanced to `Destroyed`. Bounded by session length, but the leak grew with every crash.

**Fix.**
- New field `ActivityRecord::trapped_at_ms: Option<f64>` records the moment of trap. `mark_trapped(id, now_ms)` stamps it; `mark_package_trapped(pkg, now_ms)` threads the kernel's frame timestamp through.
- New `ActivityManager` helpers `reap_trapped_ids(now_ms, max_age_ms)` and `reap(id)` — the latter drops the record + instance without firing `on_destroy` (the worker is dead, dispatch would either time out or land in a recycled handle slot).
- New `Framework::reap_trapped(now_ms, max_age_ms) -> usize` wraps both.
- Kernel calls `fw.reap_trapped(timestamp_ms, TRAPPED_GRACE_MS)` once per frame from `akiva_frame`. `TRAPPED_GRACE_MS = 30_000.0` (30s) — long enough for a debugger pause / future tap-to-restart confirm UI to claim the record explicitly before recycle.

**Code.** `framework/src/{activity_manager.rs, lib.rs}`, `kernel/src/lib.rs::{akiva_frame, drain_trapped_packages_into}`. Tests: `reap_trapped_ids_returns_only_aged_records`, `reap_drops_record_and_instance_for_trapped`, `reap_refuses_non_trapped_records`, `reap_trapped_ids_skips_records_without_timestamp`, `reap_trapped_recycles_records_older_than_grace_window`. The 30s constant lands in the runtime config block planned for P1 (`runtime/web/config.js`).

### 7.5 ✅ `drain_post` separated from render — fixed (P1 #3, May 2026)

**What was wrong.** Stone 11.4c-5 had bolted `self.drain_post(ctx)` to the top of `WasmActivity::render` to fix the "first click ignored" bug. Render is conceptually "produce paint commands", not "process pending app signals" — mixing the two muddied the contract.

**Fix in place.** New `Activity::drive_pending_signals(&mut self, ctx)` trait method (default no-op), overridden on `WasmActivity` to call `drain_post`. `Framework::drive_pending_signals(ts)` walks the foreground activity through that hook + `apply_post`. The kernel calls it once per frame in `akiva_frame`, BEFORE `render_frame`. Render is now purely paint, drain has its own phase. The first-click property survives — drive runs every frame, render or not.

**Code.** `framework/src/{activity.rs, wasm_activity.rs, lib.rs}`, `kernel/src/lib.rs::akiva_frame`. Tests: `drive_pending_signals_invokes_foreground_with_writable_ctx`, `drive_pending_signals_is_noop_on_empty_stack` (174 framework tests green).

### 7.6 ✅ `pkgs_wire` decoder dedup — fixed (Stone 11.6.7 #2, May 2026)

**What was wrong.** Three independent decoders for the same length-prefixed blob (`framework::wire::pkgs`, `sdk::pkgs`, `runtime/web/app_registry.js::_warmupWorkers`) — every `PackageEntry` field addition meant three coordinated edits with drift risk between them.

**Fix in place.** New workspace crate `akiva-wire` (at `wire/src/pkgs.rs`) holds the canonical encode/decode + types. Both Rust crates depend on it via `workspace.dependencies`; `framework/src/wire/pkgs.rs` and `sdk-rust/src/pkgs.rs` are now thin re-exports. The JS side stays separate (it can't import a Rust crate) but `app_registry.js::_warmupWorkers` now carries a `// SCHEMA:` comment pointing back at `wire/src/pkgs.rs` so drift surfaces at code review when the layout changes.

**Code.** New `wire/Cargo.toml` + `wire/src/{lib.rs, pkgs.rs}`; thin wrappers in `framework/src/wire/pkgs.rs` and `sdk-rust/src/pkgs.rs`. SDK-side `PackageEntry.entry_class` was renamed to `class` for consistency with the wire type (apps only used `id` and `name`, no migration needed). 8 wire-crate tests cover roundtrip, unicode, truncation, oversize-truncation, bad UTF-8.

### 7.7 ✅ Watchdog tunability + warn threshold — fixed (Stone 11.6.7 #5, May 2026)

**Original concern.** Watchdog had a single 1500ms terminate threshold with no warning, no per-package override, and no test-time override. Android has WARN+TIMEOUT (ANR warning before kill); we had only kill.

**Fix in place.**
- `WATCHDOG.WARN_MS = 1000` added in `runtime/web/config.js`. Crossing it logs a warn-level breadcrumb (`"no pong in Nms (warn threshold; will terminate at Mms)"`) but doesn't kill. The warn latches per process so a worker stuck around the warn band doesn't spam the log every heartbeat.
- `effectiveWatchdog(perPackageMs?)` helper resolves per-tick: per-package override → `globalThis.__akivaConfig.watchdog` → defaults. Comparisons use `>=` so the boundaries are deterministic on tick.
- `manifest.runtime.watchdog_timeout_ms: Option<u32>` parsed by `framework::manifest`. The kernel's `akiva_boot` walks `fw.packages.list()` and pushes each override through new HAL `hal_app_set_watchdog_timeout_ms(pid_ptr, len, ms)` to `AppRegistry.setWatchdogTimeoutMs(pkg, ms)`. Absent = default; `0` clears.

**Code.** `runtime/web/{config.js, app_registry.js, worker.js}`, `framework/src/manifest.rs`, `kernel/src/{host.rs, lib.rs::akiva_boot}`. Tests: 6 new vitest cases (warn at threshold without terminate, healthy pong clears latch, global override, per-package override survives default + trips at its own threshold, clear-via-zero, malformed-input rejection). 68 vitest green.

**Original "magic numbers" concern is fully closed.** P1 #2 centralised the constants; this stone added the warn split + override hooks.

### 7.8 🟢 Worker shell has no unit tests

**Concern.** `runtime/web/app_worker_shell.js` is exercised only via integration through AppRegistry. Direct dispatcher logic (fire / io_start / io_poll), capability gating (the SDK-side mirror of the kernel's check), and the futures registry have no isolated coverage.

**Why deferred.** Vitest doesn't expose real Web Workers; testing requires a fake-Worker harness like the AppRegistry side already has, but with the shell's globals (postMessage, addEventListener) mocked.

**Trigger to promote.** Phase 11.8 (perf + benchmarks) is a natural pairing — both want a real-browser e2e test bed (Playwright). Build the worker-shell harness as a step on the way there.

### 7.9 ✅ TaskScope cancellation — fixed (Stone 11.6.5 #11, May 2026)

**What was wrong.** `Context::activity_scope()` / `resumed_scope()` returned `&'static TaskScope` whose `spawn(fut)` was literally `block_on_ready(fut)` — no cross-callback retention, no cancellation, no lifecycle awareness. The doc-comment said the quiet part out loud: *"renamed block_on_ready."* Locking that into Stone 11.7's docs would have been bedrock-level damage.

**Fix in place.** `TaskScope` carries `(handle, kind)` by value; `spawn<F: Future + 'static>` queues into `SPAWN_QUEUE` (re-borrow-safe), drained into `Slot.spawned: Vec<SpawnedTask>` after each dispatch poll. New SDK export `akiva_app_drive_spawned()` polls every spawned task across every slot; the worker shell calls it after each `host_op_done` and dispatch resolution. Cancellation: `PendingDispatch.cmd_kind` lets `drive_pending` drop `Resumed`-scoped spawns when a `PAUSE` resolves; `DESTROY` evicts the slot which drops `spawned` by Drop.

**Code.** `sdk-rust/src/{activity.rs, runtime.rs, lib.rs}`, `runtime/web/app_worker_shell.js`. Tests: 5 new in `runtime::tests` covering each cancellation edge. ABI still 0.2.0 — purely additive.

### 7.10 ✅ Concurrent host calls per activity — fixed (Stone 11.6.5 #12, May 2026)

**What was wrong.** `Slot.pending: Option<PendingDispatch>` made one-callback-in-flight a structural invariant of the SDK runtime, not just a property of the framework's `PendingTransition` discipline. The executor.rs comment cemented this as "only one in-flight dispatch per handle" — a claim the data structure enforced even for user-spawned tasks that don't go through PendingTransition.

**Fix in place.** `Slot.pending` is now `HashMap<u64, PendingDispatch>` keyed by `dispatch_id`. Every `install_*` / `run_*_sync` helper takes a `dispatch_id` parameter and inserts at that key; `drive_pending(id)` and `get_result(id)` operate on the specific entry; the `debug_assert` of "second dispatch arrived" is gone. Soundness for callback dispatches still holds — the framework still serialises them, so the raw-ptr `&mut Activity` borrow inside any callback future never aliases. User-spawned tasks live in `Slot.spawned` (Stone 11.6.5 #11), are `'static` futures with no `&mut Activity` borrow, and run concurrently with whatever callback is in flight.

**Code.** `sdk-rust/src/{runtime.rs, executor.rs}`. Tests: `slot_pending_holds_concurrent_entries_keyed_by_dispatch_id`. The executor.rs invariant docstring was updated to articulate the new truth — callback dispatches serialised by framework, user-spawned tasks concurrent.

### 7.11 🟡 No streams primitive in the ABI

**Concern.** The unified ABI (`io_start` → `io_poll` → result, single-shot) has no `stream<T>` equivalent. Sensor readings, audio samples, file watch, animation tickers, real-time data feeds — every "many values over time" capability has to be faked with polling-as-future, which loses backpressure and ordering guarantees.

**Why deferred.** No current capability needs a stream. Phase 12 doesn't introduce any; the gap stays cosmetic until WindowManager event routing (Phase 13) or the first real-time capability (audio playback, gestures) lands.

**Trigger to promote.** First capability where the polling-as-future fake costs us correctness: audio sample streaming, drag/scroll gestures with sub-frame ordering, or WindowManager event fan-out. Also: the WASI 0.3 migration (deferred to v0.5+) is built around `stream<T>` — every day of delay raises the migration cost when we eventually adopt it.

**Code surfaces.** New op_kind `STREAM_OPEN` returning `stream_id`; `stream_poll(stream_id)` returns `Vec<Item>` of values batched since the prior poll; `stream_close(stream_id)`. Wire shape should mirror Component Model `stream<T>` to keep the v0.5 migration mechanical. Detailed plan in [`docs/isolation-design.md` §10.5 #13](isolation-design.md). **Recommended for Phase 13 prep, not 11.6.5.**

### 7.12 ✅ `storage::write` returns `WriteHandle` — fixed (Stone 11.6.7 #1, May 2026)

**What was wrong.** `pub fn write(key: &str, value: &[u8])` returned `()`. The `STORAGE_SET` host_op fired and the host posted `host_op_done` with a status, but the SDK abandoned the `future_id` so no app could observe success or failure.

**Fix in place.** `pub fn write(key, value) -> WriteHandle`, where `WriteHandle: Future<Output = Result<(), StorageError>>`. Awaiting yields the host's verdict; dropping the handle without awaiting preserves the pre-Stone-11.6.7 fire-and-forget shape because the `STORAGE_SET` host_op is dispatched inside `write` itself — only the result is what the future delivers. `#[must_use]` ensures `storage::write(k, v);` (no `let _`) emits a compiler warning.

**Code.** [`sdk-rust/src/storage.rs::{WriteHandle, write, poll_write_inner}`](../sdk-rust/src/storage.rs). The host handler ([`runtime/web/app_registry.js::_dispatchStorage`](../runtime/web/app_registry.js)) was already returning `STATUS.READY` / `STATUS.ERROR`; no host change required. Tests: 3 new in `storage.rs` (empty-key BadKey, non-wasm stub Denied, must_use shape). 174 sdk tests green.

### 7.13 ✅ Storage direct readonly handles — fixed (Stone 11.6.7 #3, May 2026)

**Was.** `STORAGE_GET` from an app paid a full app-worker → kernel-worker → OPFS → kernel-worker → app-worker round-trip (~2ms) for what should be a sub-millisecond direct read. The original 11.6 design (v2 §3.2) called for a hybrid model — readonly handles opened directly in the app worker, writes routed through the kernel — but only the kernel-routed path shipped.

**Fix in place.** Full restructure of the storage HAL plus a thin direct-read path in the app worker shell:

- **Per-key OPFS files** (storage.js). Replaced the single `akiva-store.v1.bin` blob with a directory tree: `pkgs/<pkg>/<userKey>`, `packages/<pkg>/<...>`, `sys/<...>`. Each path segment is percent-encoded so any UTF-8 string is a safe filename; the encoding (`storage_path.mjs`) is shared between the kernel and the app worker so they agree byte-for-byte. `Storage` keeps an in-memory mirror as the source of truth for sync reads/writes, plus an async write queue that flushes to per-key OPFS files in the background. Migration from the legacy single-file blob runs once at first boot.
- **Direct readonly path** (app_worker_shell.js). The app worker lazily opens a `pkgs/<packageId>/` directory handle and caches per-key `SyncAccessHandle`s in `read-only` mode. `STORAGE_GET` cache hits fulfill the future synchronously (~0.1ms); cache misses fall through to the existing host_op path AND kick off an async open in the background to warm the cache for next time.
- **Invalidation protocol** (app_registry.js + worker.js). `Storage.onInvalidate(pkg, userKey)` fires after every kernel-side write/delete to a `pkgs/<pkg>/<userKey>` path; AppRegistry hooks this into a `storage_invalidate` postMessage to the package's worker, which closes the matching readonly handle. `userKey === null` (uninstall) drops everything for the package.
- **WriteHandle durability** (app_registry.js). `_handleHostOp` awaits `storage.drain()` before posting `host_op_done` for SET / DELETE, so `WriteHandle::await` keeps its "when this resolves the write is durable" semantics even though `Storage.set` is now async-flushed.

**Code.** New [`runtime/web/storage_path.mjs`](../runtime/web/storage_path.mjs) (shared encoding); [`runtime/web/storage.js`](../runtime/web/storage.js) (full rewrite); [`runtime/web/app_worker_shell.js`](../runtime/web/app_worker_shell.js) (direct path + invalidate handler); [`runtime/web/app_registry.js`](../runtime/web/app_registry.js) (`broadcastStorageInvalidate`, drain-await for SET/DELETE); [`runtime/web/worker.js`](../runtime/web/worker.js) (wires `storage.onInvalidate` to AppRegistry). Tests: 25 new in `storage_path.test.mjs` (encoding round-trip), 22 new in `storage.test.mjs` (in-memory mode, invalidation callback, legacy parser), 5 new in `app_registry.test.mjs` (broadcast-only-to-targeted-worker). 125 vitest green; 437 Rust workspace tests green (no Rust changes).

**Caveat that remains.** The direct-read path's first-read-per-key still pays a host_op (cold cache); only subsequent reads of the same key hit the sub-millisecond path. The async-open-on-cold-miss warms the cache in the background, so steady-state apps reading their own config / saved-state see direct reads after the first frame. A pre-warm-on-init pass (open every key in the package directory at worker spawn) is a natural follow-up but not required to meet the DoD.

### 7.14 ✅ Lazy-spawn workers — fixed (Stone 11.6.7 #4, May 2026)

**What was wrong.** `_warmupWorkers` (Stone 11.4c-6) spawned a Web Worker for every installed package at boot. 4 apps was fine; 100 apps would burn ~4s of boot delay plus 95 idle workers' worth of wasm memory before the user clicked anything.

**Fix in place.** `_warmupWorkers` now scans the snapshot, finds `sys.shell`, and spawns only that one worker. Every other package waits for `register(packageId, ...)` to demand a worker — that path was already lazy and just wasn't being exercised because the warmup pre-empted it. `HOME_PACKAGE_ID` is a const at module top so the home identity is one-place to change.

**Code.** [`runtime/web/app_registry.js`](../runtime/web/app_registry.js) — `_warmupWorkers` body + new `HOME_PACKAGE_ID` const. Tests: 5 new vitest cases (warmup pre-spawns shell only, empty snapshot is no-op, snapshot without shell is no-op, register lazily spawns non-shell, repeated register doesn't respawn). 73 vitest green.

### 7.15 🔴 No CSPRNG / entropy primitive — promoted to Stone 11.6.8

**Concern.** `grep -nE "rand|getRandomValues" sdk-rust/` returns nothing. The SDK exposes no host import for entropy. wasm itself has no source of randomness. Apps that need crypto, UUIDs, session tokens, password salts, or anti-CSRF nonces have nowhere to go — they would have to ship their own seeded PRNG, which is unsafe for any security purpose.

**Why deferred (until now).** None of the four MVP apps (calc, hello, tic-tac-toe, infinite-loop) need entropy. The gap is invisible until the first app that does cryptography or session management is written. May 2026 audit caught it before Stone 11.7 docs would have either omitted the primitive entirely or papered over its absence.

**Trigger.** Stone 11.6.8 פריט 1 owns this. Implementation: new dedicated extern `akiva_host_random_bytes(out_ptr, len)` that the JS shell maps to `crypto.getRandomValues`, plus a thin `akiva_sdk::random` module. Ungated (RNG is a primitive, not a resource — same as on iOS/Android).

**Code surfaces.** [`sdk-rust/src/runtime.rs`](../sdk-rust/src/runtime.rs) (extern), `sdk-rust/src/random.rs` (new module), [`sdk-rust/src/lib.rs`](../sdk-rust/src/lib.rs) (re-export), [`runtime/web/app_worker_shell.js`](../runtime/web/app_worker_shell.js) (host body).

### 7.16 🔴 No bound on concurrent in-flight dispatches per slot — promoted to Stone 11.6.8

**Concern.** Stone 11.6.5 #12 changed `Slot.pending` from `Option<PendingDispatch>` to `HashMap<u64, PendingDispatch>` to enable `futures::join!(read("a"), read("b"))`. The HashMap is unbounded. A hostile or buggy app calling `for _ in 0..1_000_000 { ctx.activity_scope().spawn(slow_io()); }` allocates a million `future_id`s and a million `Pin<Box<dyn Future>>`s in a burst, before the watchdog catches the worker at 1.5s. Memory consumption is unbounded; the slot's HashMap could pin tens of MB before termination.

**Why deferred (until now).** 11.6.5 #12 prioritised landing the concurrency primitive over hardening it. The bound was deliberately left open with the comment "concurrent entries in the map are now expected to coexist in principle" — true, but "in principle" doesn't include "without limit."

**Trigger.** Stone 11.6.8 פריט 2 owns this. Implementation: const `MAX_PENDING_PER_SLOT = 256` in `runtime.rs`; a new `STATUS_BUSY = 3` op_kind status returned when the slot is full; futures resolve to `Err(_::Busy)` so apps can backoff. 256 is wide enough for legitimate `join_all` of 100+ items, narrow enough to catch spam loops in <50ms.

**Code surfaces.** [`sdk-rust/src/runtime.rs`](../sdk-rust/src/runtime.rs), op_kinds in `sdk-rust` + `framework` + JS, [`sdk-rust/src/storage.rs`](../sdk-rust/src/storage.rs) (StorageError::Busy variant).

### 7.17 🔴 No SDK concurrency primitives (Channel, Mutex, Signal) — promoted to Stone 11.6.8

**Concern.** Stone 11.6.5 #11 shipped `TaskScope::spawn`, but two spawned tasks have no way to coordinate. Kotlin Coroutines ship `Channel<T>`, `Mutex`, `Flow<T>`, `StateFlow<T>` out of the box. SwiftUI ships `AsyncStream`, `actor`, `@MainActor`. Akiva ships nothing. An app that wants a producer-consumer pattern between two spawned tasks has to build it from `RefCell<VecDeque<T>>` + manual waker plumbing.

This is a real foundation gap, not DX polish: the vision in [`docs/isolation-design.md` §1](isolation-design.md) explicitly names "DX like Compose/SwiftUI." Without sync primitives, the vision is unmet.

**Why deferred (until now).** 11.6.5 #11 was scoped to "make scope-based cancellation real," not "ship the full Coroutines surface." The audit promoted the gap once it was clear that 11.7's public docs would either ship apps with manual waker plumbing or document a feature absence.

**Trigger.** Stone 11.6.8 פריט 3 owns this. Implementation: new `sdk-rust/src/sync/{channel.rs, mutex.rs, signal.rs}` modules, ~200-300 lines total. Custom (not embassy-sync) for the same reason as the executor: tightly fitted to single-threaded wasm, no cross-thread atomics needed, no runtime dependency.

**Code surfaces.** New: `sdk-rust/src/sync/`. Updated: [`sdk-rust/src/lib.rs`](../sdk-rust/src/lib.rs) (re-export), [`sdk-rust/src/executor.rs`](../sdk-rust/src/executor.rs) (waker registration helpers if needed).

### 7.18 🟡 No wall-clock time primitive — promoted to Stone 11.6.8

**Concern.** `ctx.timestamp_ms` is monotonic (frame-based, derived from the host's `performance.now()`). There is no wall-clock import. An app writing a note and wanting to stamp it "created at 2026-05-02 14:32" has nowhere to fetch the wall time. Real OSes expose CLOCK_MONOTONIC and CLOCK_REALTIME separately because they serve different purposes and are not interchangeable.

**Why deferred (until now).** No MVP app needs the wall clock. Calc, hello, tic-tac-toe, infinite-loop all use only frame timing. A notes-style app would expose the gap immediately.

**Trigger.** Stone 11.6.8 פריט 4 owns this. Implementation: new extern `akiva_host_wall_clock_ms() -> u64` mapped to `Date.now()`, plus `akiva_sdk::clock::wall_clock_ms()`. Ungated (time is a primitive). Doc-comment must explicitly call out the difference from `ctx.timestamp_ms` — confusing the two is a common source of bugs.

**Code surfaces.** [`sdk-rust/src/runtime.rs`](../sdk-rust/src/runtime.rs) (extern), `sdk-rust/src/clock.rs` (new), [`sdk-rust/src/lib.rs`](../sdk-rust/src/lib.rs) (re-export), [`runtime/web/app_worker_shell.js`](../runtime/web/app_worker_shell.js) (host body).

---

## Retired / promoted entries

_(none yet)_
