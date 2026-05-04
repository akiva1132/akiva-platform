# Risk Register & Deferred Concerns

> **What this doc is for.** A small, single place for architectural
> concerns that aren't actionable right now but **must not be
> forgotten**. Each entry names the concern, why deferral is
> acceptable today, and the trigger that would promote it to a
> stone. If a concern becomes a stone, its entry moves to
> [`ROADMAP.md`](../ROADMAP.md) and is deleted here.
>
> **What it is not.** A bug list (use GitHub issues), a feature
> wishlist (use [`vision.md`](../vision.md)), or a design doc
> (use `docs/*-design.md`).

_Created 2026-04-22, after Stone 9.4 landed._

---

## 1. Process-level isolation + crash recovery — **promoted to Phase 11 of v0.3**

> **Promoted 2026-04-22.** See
> [`ROADMAP.md` — Phase 11](../ROADMAP.md). The original trigger
> ("first genuinely third-party app") was tightened after an
> external review: we don't need a third-party app to know this is
> the gating systems problem. Phase 10 (Permissions) closing meant
> the capability model was in place — the next systems-core bet
> was this one. Phase 11 is now closed; the long-form text below
> stays for historical context until the next risk-register
> rewrite.

**Original concern (kept for context).** Every app ran in the
**same Web Worker** as the kernel. Each package got its own
`WebAssembly.Memory` (memory-space isolation was fine), but all
apps shared: the worker event loop, the JS heap, the OPFS
`SyncAccessHandle`, the rAF frame budget.

Consequences: an infinite loop in any app froze the whole OS (no
preemption); a wasm trap was caught + logged but the
`ActivityRecord` stayed `Resumed` (zombie); no Low-Memory Killer
(instances lived until the worker terminated).

**Code surfaces that did change** (annotated for Phase 11
planning):

- [`runtime/web/app_registry.js`](../runtime/web/app_registry.js)
  — `_instantiateProcess` moved to a worker-per-app model;
  error-catch paths became message-handler paths.
- [`runtime/web/worker.js`](../runtime/web/worker.js) — frame
  loop + event dispatch now go through `postMessage` to app
  workers.
- [`framework/src/activity_manager.rs`](../framework/src/activity_manager.rs)
  — added a `Trapped` lifecycle state (Phase 11.5).
- [`framework/src/wasm_activity.rs`](../framework/src/wasm_activity.rs)
  — `WasmHost` trait became async-capable (Phase 11.4a). Stone
  11.3 made the **upward** direction (`akiva_host_*`) async via
  the unified-ABI futures; the downward direction (kernel → app
  `lifecycle` / `render` / etc.) became async in 11.4a.
- [`sdk-rust/src/runtime.rs`](../sdk-rust/src/runtime.rs) —
  host-import externs collapsed to three multiplexers in 11.3 b/c
  (`akiva_host_fire`, `akiva_host_io_start`,
  `akiva_host_io_poll`); per-op extern wrappers gone. The Worker
  seam in 11.4c stayed JS-side only — Rust extern signatures did
  not change.

---

## 2. ABI v0 → v1 (Component Model) transition

**Concern.** Every new `extern "C"` pair (`akiva_host_*` +
`hal_app_*`) adds to the eventual migration cost. Stone 9.4 added
two (`akiva_host_start_activity`, `hal_app_drain_intent`) plus one
export (`akiva_app_new_intent`). Phase 10 added a
capability-guarded import set; Phase 14 added five capability
imports.

The [`research-component-model.md`](research-component-model.md)
memo deferred the migration to v0.4+. Meanwhile `wit/` sits as
scaffolding with no concrete mapping to the current ABI.

**Why deferred.** Component Model tooling (`wit-bindgen`,
host-side `wasmtime-component` equivalents for browser wasm) is
still maturing. Migrating now would lock us in to whichever
toolchain is "working this quarter", risking a second migration.

**Trigger to promote.** Either (a) the first breaking-ABI event
that forces a version bump anyway — bundle the WIT cutover with
it; or (b) the `wit-bindgen` JS target reaches feature parity with
`extern "C"` wire format. Whichever comes first.

**Mitigations already in place.**

- Every new primitive is documented in
  [`docs/abi.md`](abi.md) with its wire shape — migration mapping
  is mechanical, not archeological.
- `ABI_VERSION` exists on both sides so a mismatched pair fails
  loudly at instantiation.
- Wire codecs (paint, saved-state, intent) live in named modules,
  each a single decode/encode pair — swappable in isolation.

**Size estimate when we pull the trigger.** ~12+ primitives × ~30
LoC each = ~360 LoC of mechanical migration + toolchain churn +
test-fixture rebuild. Budget one week.

---

## 3. ✅ JS-side ABI governance / versioning policy — fixed (Stone 11.6.8 #5)

**Was.** `ABI_VERSION = "0.2.0"` in the SDK,
`Intent::MAX_WIRE_BYTES` pinned in
[`docs/abi.md §2.5.1`](abi.md), and a §1.2 "narrow ABI review"
clause — but no written rule for when `0.X` → `1.0` happens, what
constitutes a breaking change, or who gates the bump.

**Fix.** [`docs/abi.md §1.2`](abi.md) defines an in-scope /
out-of-scope split (extern signatures, op_kind constants, status
codes, wire layouts, capabilities, public `MAX_*_BYTES` are in;
SDK Rust ergonomics, internal kernel logic, runtime/web JS
internals, and docs are out), PATCH / MINOR / MAJOR rules, 1.0
promotion criteria, and a review process. The new
[`CHANGELOG.md`](../CHANGELOG.md) at the repo root tracks every
bump from `0.1.0` forward.

**ABI bumped to `0.3.0`** as part of Stone 11.6.8 (additive:
+ `akiva_host_random_bytes`, + `akiva_host_clock_ms`,
+ `STATUS_BUSY = 3`). Subsequent additive bumps continued through
`0.12.0` (Phase 14 closeout, May 2026).

---

## 4. Web-platform interop

**Concern (history).** Akiva apps run in a browser tab but could
not:

- open a URL in the host browser;
- read or write the host clipboard;
- accept a drag-and-dropped file;
- embed a browser view (iframe equivalent);
- use platform APIs — geolocation, camera, notifications, Web
  Share.

Simultaneously a missing feature ("an OS doesn't open URLs?") and
an architectural inflection — the clean sandbox ends the moment
the first interop bridge lands.

**Status (May 2026).** Phase 14 closed five of the listed gaps:
**Notifications** (Stone 14.3), **Clipboard read/write**
(14.4), **Vibration** (14.5), **Geolocation** (14.6), and the
prior Phase 14.1 / 14.2 **WebSocket / WebTransport**. Each rides
the cap_req/cap_resp main-thread bridge introduced in 14.3, with
a permission gate in
[`framework/src/permissions.rs`](../framework/src/permissions.rs)
and a corresponding manifest declaration. The `akiva_host_open_url`,
drag-and-drop, and iframe-style embed surfaces are still open;
they will follow the same pattern when the first app needs them.

**Mitigations.** Every new interop import gets a permission in the
manifest. Host-visible state is never returned synchronously — a
permission prompt is async. The async ABI from Phase 11 supports
this naturally.

---

## 5. Text/input — partially closed (May 2026)

**Concern (history).** The original input model
(`on_pointer(ctx, ev)` + `on_key(ctx, ev)`) was adequate for
buttons and display text. It did **not** cover:

- editable text fields (focus, caret, selection);
- IME / composition events (CJK, accent marks, dead keys);
- clipboard integration (cut / copy / paste at field level);
- accessibility — screen reader for text content;
- bidirectional text (LTR / RTL mix) in editable contexts —
  **MVP explicitly skips Hebrew/RTL/BiDi** per
  `memory/feedback_mvp_focus.md`, but English-only IMEs still
  need composition events.

**Status (May 2026).** The text-input pipeline shipped — see
[`docs/runtime-web.md §4.1`](runtime-web.md). `Event::TextInput
{ text }` (`EVENT_TEXT = 5`) carries typed characters and the
result of `compositionend` for IME flows. `cmd::TEXT_INPUT = 0x13`
delivers the event into the activity through
`Activity::on_text_input`. Editable-text **widgets** (focus, caret,
selection) and accessibility integration are **still open**.

**Trigger to promote what remains.** First app proposal with an
editable text field (search, form, chat). The widget-side work is
comparable in scope to Phase 9 (activity model) — ~4–6 weeks and
a research doc of its own. The clipboard half closed under §4.

---

## 6. Premature-abstraction temptation: consolidating "many small protocols"

**Originally raised pre-11.3, partially superseded.** A design
review flagged that the host implemented 11 independent
`akiva_host_*` protocols (paint, save_state, restore, finish
drain, intent drain, new_intent, resources × 4, log, text_width)
and suggested consolidation into "a few strong primitives" —
e.g. a single typed message bus.

**Status after Stone 11.3.** Six of those protocols (log,
text_width, finish, start_activity{,_for_result}, set_result,
query_packages) collapsed into three multiplexers
(`akiva_host_fire` / `akiva_host_io_start` /
`akiva_host_io_poll`) routed by `op_kind`. The "rule of three"
was met during the async-ABI redesign, and the consolidation paid
for itself in 11.4 prep — one async seam to plumb cross-worker,
not eleven.

**What stayed separate.** Resource lookups (4 functions: color,
string, has_color, has_string) — readonly per-package queries
with no async, no handle context, no side effect. Not worth the
encode/decode overhead. `hal_app_*` (kernel → app side) is
unchanged — different concern, different shape.

**Trigger to revisit anything further.** A third byte-transfer
pattern beyond `_copyAppToKernel` / `_callAppWithBytes`, or a
streaming surface that doesn't fit the future_id model. Neither
is near.

---

## 7. Foundation gaps surfaced by Phase 11.4c / 11.5 live testing (May 2026)

A self-audit after Stone 11.5 closed (process isolation + fault
containment work) surfaced the following gaps. Each is documented
in detail elsewhere; this section is the single place that lists
them so they don't get lost. See
[`docs/isolation-design.md` §10.5 Phase 11 Foundation Gaps](isolation-design.md)
for the full work plan.

### 7.1 ✅ SDK async executor — fixed (P0 #4, May 2026)

**Was.** `Activity` callbacks were `async fn`, but
`block_on_ready` polled exactly once and panicked on `Pending`.
Cross-worker host calls (storage::read) took ≥ 1 frame to
resolve, so `.await` on them would panic. The `ReadHandle::poll()`
workaround leaked the multi-frame nature into every app.

**Fix (~700 lines, breaking ABI bump).** A single-task-per-handle
async-resumable runtime, with the dispatch protocol changed to:

```rust
akiva_app_dispatch(dispatch_id: u64, cmd: u32, ptr, len) -> u32   // 0 accepts, !=0 refuses
akiva_app_poll_dispatch(dispatch_id: u64) -> u64                    // packed (status, len)
akiva_app_get_result(dispatch_id: u64, out_len_ptr) -> *const u8   // single-shot
```

The wasm side stores the in-flight `Pin<Box<dyn Future +
'static>>` in the slot via raw-pointer borrows into
`slot.activity` and `slot.ctx`. Soundness rests on
`HashMap<u32, Box<Slot>>` heap-pinning every slot and on the
framework's `PendingTransition` discipline guaranteeing no
concurrent dispatches per handle. The `executor` module documents
the SAFETY invariants.

The worker shell tracks `pendingDispatches: Map<bigint, …>`. When
`host_op_done` arrives the shell re-polls every still-pending
dispatch — the wasm side caches terminal results, so re-polls of
already-resolved dispatches are no-ops. Storage `read` is now a
real `Future`:

```rust
async fn on_create(&mut self, _ctx: &mut Context<'_>) {
    if let Some(fut) = storage::read("notes") {
        if let Ok(Some(bytes)) = fut.await {
            self.text = String::from_utf8_lossy(&bytes).into_owned();
        }
    }
}
```

Render and `save_state` keep the synchronous fast-path: paint code
should not block on host I/O, and `save_state` is `&self` per the
trait. `executor::run_to_ready` (the sync drain) still panics on
Pending — accidental awaits in render surface immediately.

### 7.2 ✅ `on_save_state` degraded under cross-worker — fixed (P0 #1, May 2026)

**Was.** `WasmActivity::save_state_bytes` called
`host.save_state()` + `poll_future()` inline. Post-11.4c the
first poll always returned `Pending`; release builds silently
fell through to `Vec::new()`. Phase 6 saved-state was broken in
production.

**Fix.** Added `Activity::dispatch_save_state_async(&self) -> u64`
to the trait. `WasmActivity` overrides it to dispatch
`save_state` and return the `future_id`; the legacy sync
`save_state_bytes` returns empty for wasm activities.
`Framework::destroy_with_persist` records a `PendingSave {
future_id, package_id, class }` before walking pause → stop →
destroy (worker FIFO handles all four in order). New
`Framework::drive_pending_saves`, called once per frame from
`akiva_frame`, polls each entry and persists the bytes when the
worker replies. Empty bytes delete the entry; `Err` drops the
entry without persisting.

### 7.3 ✅ Graceful shutdown signal — fixed (P0 #2, May 2026)

**Was.** Closing a tab terminated the workers without notifying
apps. No chance for `save_state` to run; apps lost unflushed
in-memory state on every tab close.

**Fix.** Checkpoint hook layered on top of the P0 #1 pipeline:

- `Framework::checkpoint(ts) -> usize` dispatches `save_state`
  on the foreground activity (no lifecycle change), records a
  `PendingSave`, and de-dups against in-flight saves so rapid
  checkpoints don't pile up dispatches.
- `akiva_checkpoint(ts: f64) -> u32` kernel export.
- `worker.js` handles `{type: "checkpoint"}` → calls the export.
- `tab.js` fires the checkpoint message on `visibilitychange` to
  `hidden` (the recommended hook — fires reliably on tab switch,
  swipe-close, OS-level backgrounding) and on `pagehide` with
  `persisted=false` (last-resort for tab close while
  foregrounded).

**Caveat that remains.** Save bytes travel async over the worker
boundary. On `visibilitychange` the page stays alive (browsers
keep background pages around for tens of seconds) so the
round-trip completes. On a hard close via `pagehide` the tab may
die before the bytes land — best effort. The "last save" is the
most-recent `visibilitychange`-triggered checkpoint; data
produced after that and before close is lost.

### 7.4 ✅ Trapped activity recycle — fixed (P0 #3, May 2026)

**Was.** When the watchdog terminated a worker (or a wasm trap
was reported), `ActivityRecord` flipped to `Trapped` but never
advanced to `Destroyed`. Bounded by session length, but the leak
grew with every crash.

**Fix.**

- New field `ActivityRecord::trapped_at_ms: Option<f64>` records
  the moment of trap. `mark_trapped(id, now_ms)` stamps it;
  `mark_package_trapped(pkg, now_ms)` threads the kernel's frame
  timestamp through.
- New `ActivityManager` helpers
  `reap_trapped_ids(now_ms, max_age_ms)` and `reap(id)` — the
  latter drops the record + instance without firing `on_destroy`
  (the worker is dead, dispatch would either time out or land in
  a recycled handle slot).
- New `Framework::reap_trapped(now_ms, max_age_ms) -> usize`
  wraps both.
- The kernel calls `fw.reap_trapped(timestamp_ms,
  TRAPPED_GRACE_MS)` once per frame from `akiva_frame`.
  `TRAPPED_GRACE_MS = 30_000.0` (30 s) — long enough for a
  debugger pause / future tap-to-restart confirm UI to claim the
  record explicitly before recycle.

### 7.5 ✅ `drain_post` separated from render — fixed (P1 #3, May 2026)

**Was.** Stone 11.4c-5 had bolted `self.drain_post(ctx)` to the
top of `WasmActivity::render` to fix the "first click ignored"
bug. Render is conceptually "produce paint commands", not "process
pending app signals" — mixing the two muddied the contract.

**Fix.** New `Activity::drive_pending_signals(&mut self, ctx)`
trait method (default no-op), overridden on `WasmActivity` to
call `drain_post`. `Framework::drive_pending_signals(ts)` walks
the foreground activity through that hook + `apply_post`. The
kernel calls it once per frame in `akiva_frame`, **before**
`render_frame`. Render is now purely paint, drain has its own
phase. The first-click property survives — drive runs every frame,
render or not.

### 7.6 ✅ `pkgs_wire` decoder dedup — fixed (Stone 11.6.7 #2, May 2026)

**Was.** Three independent decoders for the same length-prefixed
blob (`framework::wire::pkgs`, `sdk::pkgs`,
`runtime/web/app_registry.js::_warmupWorkers`) — every
`PackageEntry` field addition meant three coordinated edits with
drift risk between them.

**Fix.** New workspace crate `akiva-wire` (at `wire/src/pkgs.rs`)
holds the canonical encode / decode + types. Both Rust crates
depend on it via `workspace.dependencies`;
`framework/src/wire/pkgs.rs` and `sdk-rust/src/pkgs.rs` are now
thin re-exports. The JS side stays separate (it can't import a
Rust crate) but `app_registry.js::_warmupWorkers` now carries a
`// SCHEMA:` comment pointing back at `wire/src/pkgs.rs`, so
drift surfaces at code review when the layout changes.

### 7.7 ✅ Watchdog tunability + warn threshold — fixed (Stone 11.6.7 #5, May 2026)

**Original concern.** The watchdog had a single 1500 ms terminate
threshold with no warning, no per-package override, and no
test-time override. Android has WARN + TIMEOUT (ANR warning
before kill); we had only kill.

**Fix.**

- `WATCHDOG.WARN_MS = 1000` added in `runtime/web/config.js`.
  Crossing it logs a warn-level breadcrumb (`"no pong in N ms
  (warn threshold; will terminate at M ms)"`) but doesn't kill.
  The warn latches per process so a worker stuck around the warn
  band doesn't spam the log every heartbeat.
- `effectiveWatchdog(perPackageMs?)` helper resolves per tick:
  per-package override → `globalThis.__akivaConfig.watchdog` →
  defaults. Comparisons use `>=` so the boundaries are
  deterministic on tick.
- `manifest.runtime.watchdog_timeout_ms: Option<u32>` parsed by
  `framework::manifest`. The kernel's `akiva_boot` walks
  `fw.packages.list()` and pushes each override through new HAL
  `hal_app_set_watchdog_timeout_ms(pid_ptr, len, ms)` to
  `AppRegistry.setWatchdogTimeoutMs(pkg, ms)`. Absent =
  default; `0` clears.

### 7.8 🟢 Worker shell has no unit tests

**Concern.** `runtime/web/app_worker_shell.js` is exercised only
via integration through AppRegistry. Direct dispatcher logic
(fire / io_start / io_poll), capability gating (the SDK-side
mirror of the kernel's check), and the futures registry have no
isolated coverage.

**Why deferred.** Vitest doesn't expose real Web Workers; testing
requires a fake-Worker harness like the one AppRegistry already
has, but with the shell's globals (`postMessage`,
`addEventListener`) mocked.

**Trigger to promote.** A perf + benchmarks pass (the natural
pairing for Playwright + a real-browser e2e test bed). Build the
worker-shell harness as a step on the way there.

### 7.9 ✅ TaskScope cancellation — fixed (Stone 11.6.5 #11, May 2026)

**Was.** `Context::activity_scope()` / `resumed_scope()` returned
`&'static TaskScope` whose `spawn(fut)` was literally
`block_on_ready(fut)` — no cross-callback retention, no
cancellation, no lifecycle awareness. The doc-comment said the
quiet part out loud: *"renamed `block_on_ready`."* Locking that
into Stone 11.7's docs would have been bedrock-level damage.

**Fix.** `TaskScope` carries `(handle, kind)` by value;
`spawn<F: Future + 'static>` queues into `SPAWN_QUEUE`
(re-borrow-safe), drained into `Slot.spawned: Vec<SpawnedTask>`
after each dispatch poll. New SDK export
`akiva_app_drive_spawned()` polls every spawned task across every
slot; the worker shell calls it after each `host_op_done` and
dispatch resolution. Cancellation: `PendingDispatch.cmd_kind`
lets `drive_pending` drop `Resumed`-scoped spawns when a `PAUSE`
resolves; `DESTROY` evicts the slot which drops `spawned` by
Drop.

### 7.10 ✅ Concurrent host calls per activity — fixed (Stone 11.6.5 #12, May 2026)

**Was.** `Slot.pending: Option<PendingDispatch>` made
one-callback-in-flight a structural invariant of the SDK runtime,
not just a property of the framework's `PendingTransition`
discipline. The `executor.rs` comment cemented this as "only one
in-flight dispatch per handle" — a claim the data structure
enforced even for user-spawned tasks that don't go through
`PendingTransition`.

**Fix.** `Slot.pending` is now `HashMap<u64, PendingDispatch>`
keyed by `dispatch_id`. Every `install_*` / `run_*_sync` helper
takes a `dispatch_id` parameter and inserts at that key;
`drive_pending(id)` and `get_result(id)` operate on the specific
entry; the `debug_assert` of "second dispatch arrived" is gone.
Soundness for callback dispatches still holds — the framework
still serialises them. User-spawned tasks live in `Slot.spawned`
(7.9), are `'static` futures with no `&mut Activity` borrow, and
run concurrently with whatever callback is in flight.

### 7.11 🟡 No streams primitive in the ABI

**Concern.** The unified ABI (`io_start` → `io_poll` → result,
single-shot) has no `stream<T>` equivalent. Sensor readings,
audio samples, file watch, animation tickers, real-time data
feeds — every "many values over time" capability has to be faked
with polling-as-future, which loses backpressure and ordering
guarantees.

**Why deferred.** No current capability needs a stream. The gap
stays cosmetic until a real-time capability (audio playback,
gestures) lands.

**Trigger to promote.** First capability where the
polling-as-future fake costs us correctness: audio sample
streaming, drag/scroll gestures with sub-frame ordering, or
WindowManager event fan-out. Also: the WASI 0.3 migration is built
around `stream<T>` — every day of delay raises the migration cost
when we eventually adopt it.

**Code surfaces.** New op_kind `STREAM_OPEN` returning
`stream_id`; `stream_poll(stream_id)` returns `Vec<Item>` of
values batched since the prior poll; `stream_close(stream_id)`.
Wire shape should mirror Component Model `stream<T>` to keep the
migration mechanical. Detailed plan in
[`docs/isolation-design.md §10.5 #13`](isolation-design.md).
**Recommended for the next prep cycle, not 11.6.5.**

### 7.12 ✅ `storage::write` returns `WriteHandle` — fixed (Stone 11.6.7 #1, May 2026)

**Was.** `pub fn write(key: &str, value: &[u8])` returned `()`.
The `STORAGE_SET` host_op fired and the host posted
`host_op_done` with a status, but the SDK abandoned the
`future_id` so no app could observe success or failure.

**Fix.** `pub fn write(key, value) -> WriteHandle`, where
`WriteHandle: Future<Output = Result<(), StorageError>>`.
Awaiting yields the host's verdict; dropping the handle without
awaiting preserves the pre-Stone-11.6.7 fire-and-forget shape
because the `STORAGE_SET` host_op is dispatched inside `write`
itself — only the result is what the future delivers.
`#[must_use]` ensures `storage::write(k, v);` (no `let _`) emits
a compiler warning.

### 7.13 ✅ Storage direct readonly handles — fixed (Stone 11.6.7 #3, May 2026)

**Was.** `STORAGE_GET` from an app paid a full app-worker →
kernel-worker → OPFS → kernel-worker → app-worker round-trip
(~2 ms) for what should be a sub-millisecond direct read. The
original 11.6 design (v2 §3.2) called for a hybrid model —
readonly handles opened directly in the app worker, writes routed
through the kernel — but only the kernel-routed path shipped.

**Fix.** Full restructure of the storage HAL plus a thin
direct-read path in the app worker shell. See
[`docs/storage.md §3 + §8.4`](storage.md) for the protocol; tests
in `storage_path.test.mjs`, `storage.test.mjs`,
`app_registry.test.mjs`.

### 7.14 ✅ Lazy-spawn workers — fixed (Stone 11.6.7 #4, May 2026)

**Was.** `_warmupWorkers` (Stone 11.4c-6) spawned a Web Worker
for every installed package at boot. 4 apps was fine; 100 apps
would burn ~4 s of boot delay plus 95 idle workers' worth of
wasm memory before the user clicked anything.

**Fix.** `_warmupWorkers` now scans the snapshot, finds
`sys.shell`, and spawns only that one worker. Every other
package waits for `register(packageId, …)` to demand a worker —
that path was already lazy and just wasn't being exercised
because the warmup pre-empted it.

### 7.15 ✅ CSPRNG / entropy primitive — fixed (Stone 11.6.8 #1, May 2026)

**Was.** `grep -nE "rand|getRandomValues" sdk-rust/` returned
nothing. The SDK exposed no host import for entropy. Wasm itself
has no source of randomness. Apps that needed crypto, UUIDs,
session tokens, password salts, or anti-CSRF nonces had nowhere
to go.

**Fix.** New
`akiva_host_random_bytes(out_ptr: *mut u8, len: usize) -> u32`
extern, mapped to `crypto.getRandomValues` in
[`runtime/web/app_worker_shell.js`](../runtime/web/app_worker_shell.js).
The key design decision: instead of a standalone
`akiva_sdk::random` module that only apps aware of it would use,
the extern is wired as a `getrandom` v0.2 custom backend via
`register_custom_getrandom!`. This means the entire Rust
ecosystem — `rand`, `uuid`, `ring`, `rustls`,
`chacha20poly1305` — works in Akiva apps without any app-side
change. `akiva_sdk::random::{fill, bytes}` are thin convenience
wrappers over `getrandom::getrandom`. Ungated.

### 7.16 ✅ Backpressure cap on concurrent in-flight dispatches — fixed (Stone 11.6.8 #2, May 2026)

**Was.** Stone 11.6.5 #12 changed `Slot.pending` from
`Option<PendingDispatch>` to `HashMap<u64, PendingDispatch>` to
enable `futures::join!(read("a"), read("b"))`. The HashMap was
unbounded. A hostile or buggy app calling
`for _ in 0..1_000_000 { ctx.activity_scope().spawn(slow_io());
}` could allocate a million `future_id`s and a million
`Pin<Box<dyn Future>>`s before the watchdog caught the worker at
1.5 s.

**Fix.** Const `MAX_PENDING_PER_SLOT = 256` in
[`sdk-rust/src/runtime.rs`](../sdk-rust/src/runtime.rs) caps
`Slot.pending`; matching JS-side const
`MAX_PENDING_FUTURES = 256` in
[`runtime/web/app_worker_shell.js`](../runtime/web/app_worker_shell.js)
caps the per-app worker `futures` map. On overflow,
`dispatchIoStart` allocates a `future_id` pre-resolved with
`STATUS::BUSY = 3` instead of forwarding to the kernel. New
`StorageError::Busy` variant; `poll_read_inner` /
`poll_write_inner` map `status::BUSY` to `Err(StorageError::Busy)`.

### 7.17 ✅ SDK concurrency primitives — fixed (Stone 11.6.8 #3, May 2026)

**Was.** Stone 11.6.5 #11 shipped `TaskScope::spawn`, but two
spawned tasks had no way to coordinate. Apps wanting a producer-
consumer pattern had to build it from `RefCell<VecDeque<T>>` +
manual waker plumbing.

**Fix.** New module tree
[`sdk-rust/src/sync/`](../sdk-rust/src/sync/) with three
primitives:

- `Channel<T, N>` — bounded MPSC ring buffer with async
  backpressure on `send` and async waiting on `recv`. Cheaply
  cloneable across spawned tasks via `Rc<Channel>`.
- `Mutex<T>` — futurised exclusive lock with RAII `MutexGuard`.
  `UnsafeCell<T>` interior + `Cell<bool>` lock flag; safe because
  wasm is single-threaded.
- `Signal<T: Clone>` — single-slot, multi-waiter notification
  (mirror of Compose's `mutableStateOf`). `wait()` resolves only
  on the next `set()`, not the current value.

Custom (not embassy-sync) — single-threaded wasm doesn't need
cross-thread atomics; the whole module is ~250 lines.

### 7.18 ✅ Wall-clock time primitive — fixed (Stone 11.6.8 #4, May 2026)

**Was.** `ctx.timestamp_ms` is monotonic (frame-based, derived
from the host's `performance.now()`). There was no wall-clock
import. An app writing a note and wanting to stamp it "created
at 2026-05-02 14:32" had nowhere to fetch the wall time.

**Fix.** Single new extern
`akiva_host_clock_ms(clock_id: u32) -> u64` mapped to
`performance.now()` (`id=0`) or `Date.now()` (`id=1`) in
[`runtime/web/app_worker_shell.js`](../runtime/web/app_worker_shell.js).
New [`sdk-rust/src/clock.rs`](../sdk-rust/src/clock.rs) with a
`ClockId` enum (`Monotonic = 0`, `RealTime = 1`) and
`clock_gettime_ms(id: ClockId) -> u64` — a direct mirror of
POSIX `clock_gettime(clockid_t, …)`. The single-extern-with-id
design keeps the ABI extensible (future clock types such as
`ProcessCputimeId` can be added without a new extern). Ungated.
Non-wasm builds delegate to `std::time` so unit tests run on the
developer machine.

---

## Retired / promoted entries

_(none yet)_
