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

## 3. JS-side ABI governance / versioning policy

**Concern.** The project has `ABI_VERSION = "0.1.0"` in the SDK, `Intent::MAX_WIRE_BYTES` pinned in abi.md §2.5.1, and a §1.2 "narrow ABI review" clause — but no written rule for:

- **When does `0.X` → `1.0` happen?** Specifically: what minimum feature set + stability bar?
- **What constitutes a breaking change?** Adding an import is additive; renaming one is breaking; changing a wire format… depends.
- **Who gates changes?** Implicit "the design doc" today.

**Why deferred.** Premature to formalise with one author and three apps. A policy written too early calcifies the wrong thing.

**Trigger to promote.** First external contributor wanting to add a host import. That's when "informal review" breaks.

**Where policy would live.** A new section in [`docs/abi.md`](abi.md) §1. Probably ~30 lines.

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

### 7.1 🔴 SDK async executor missing (cardinal)

**Concern.** `Activity` callbacks are `async fn`, but the SDK's executor (`block_on_ready`) calls `.poll()` exactly once and panics on `Pending`. Cross-worker host calls (storage::read, future API like network) take a frame to resolve, so `.await` on them would panic. The deferred-handle workaround (`storage::ReadHandle::poll`) works but is a leaky abstraction — apps must manage state across frames manually.

**Why deferred.** Embassy executor integration is a 1-2 day refactor that would touch the dispatch protocol on both sides of the worker boundary. We shipped MVP-quality storage SDK first, planning to retrofit once the platform is stable.

**Trigger to promote.** First "real" app that needs more than 1-2 awaits (notes, settings, anything reading user data). Or: when the deferred-handle pattern shows up across 3+ SDK modules and the cost of NOT having async surpasses the cost of the embassy integration.

**Code surfaces.** `sdk-rust/src/activity.rs` (replace `block_on_ready`), `sdk-rust/src/runtime.rs` (dispatch glue), `runtime/web/app_worker_shell.js` (resumption protocol), `framework/src/wasm_activity.rs` (lifecycle pipelining). Detailed plan in [`docs/sdk-rust.md` §19.1](sdk-rust.md).

### 7.2 🟡 `on_save_state` degraded under cross-worker

**Concern.** `WasmActivity::save_state_bytes` calls `host.save_state()` + `poll_future()` inline. Post-11.4c the first poll always returns `Pending`; release builds silently fall through to `Vec::new()`. **Phase 6 saved-state is broken in production.**

**Why deferred.** The save_state fix needs the same pipelining trick we used for render but applied to the pause→stop transition (multi-frame). Coupled with the framework state machine. Roughly 30-45 min of focused work but waited on the audit pass.

**Trigger to promote.** ALWAYS — as soon as the first foundation-hardening pass starts. This is P0.

**Code surfaces.** `framework/src/wasm_activity.rs` (`save_state_bytes` signature change), `framework/src/lib.rs` (`drive_to_state(Stopped)` becomes async-aware). Detailed plan in [`docs/sdk-rust.md` §19.2](sdk-rust.md).

### 7.3 🟡 No graceful shutdown signal

**Concern.** Closing a tab terminates the worker(s) without notifying apps. No chance for `on_pause` → `save_state` → `on_stop` to run. Apps lose unflushed state every time the user closes the tab.

**Why deferred.** Out of scope for the isolation work (Stone 11.4-11.5 focused on per-app crash handling, not whole-system shutdown). MVP apps don't have critical unsaved state today.

**Trigger to promote.** First app that produces user-perceptible data loss on close (notes, draft email, etc.). P0 in the Phase 11 closeout work plan.

**Code surfaces.** `runtime/web/tab.js` (intercept beforeunload), `runtime/web/worker.js` (drain message), `framework/src/lib.rs` (`shutdown_all` walking the activity stack with state-saving pause).

### 7.4 🟡 Trapped activities leak ActivityRecord

**Concern.** When the watchdog terminates a worker (or a wasm trap is reported), `ActivityRecord` flips to `Trapped` but never advances to `Destroyed`. Bounded by session length (each crash adds one zombie); unrecycled.

**Why deferred.** Bounded leak — survives a session, doesn't persist. UX recovery (Shell tap-to-restart) was deferred to post-foundation work, so there was no user trigger to recycle the record.

**Trigger to promote.** Foundation closeout work. Add a `Framework::reap_trapped(now_ms, max_age_ms)` walker the kernel calls each frame.

**Code surfaces.** `framework/src/activity_manager.rs` (new method), `framework/src/lib.rs` (call site), `kernel/src/lib.rs` (akiva_frame integration). ~30 min.

### 7.5 🟡 `drain_post` on every render is concept-muddy

**Concern.** Stone 11.4c-5 added `self.drain_post(ctx)` at the start of `WasmActivity::render` to fix the "first click ignored" bug. But render is conceptually "produce a frame", not "process pending app signals." Mixing the two makes the contract harder to reason about.

**Why deferred.** Works correctly today (drain is idempotent). The cleanup is architectural, not functional.

**Trigger to promote.** Either (a) when adding another deferred-signal type that needs the same drain (e.g. notification post-callback), or (b) the foundation polish pass.

**Code surfaces.** `framework/src/lib.rs` (new `drive_pending_signals(ts)` phase in the kernel's frame loop). Move `drain_post` out of `render`.

### 7.6 🟡 `pkgs_wire` decoder duplicated 3x

**Concern.** `framework::wire::pkgs::decode`, `sdk-rust::pkgs::decode`, and `runtime/web/app_registry.js::_warmupWorkers` each parse the same length-prefixed binary blob. Adding a field to `PackageEntry` means three coordinated changes.

**Why deferred.** Drift hasn't bitten yet — the format is small (3 strings) and the test suite cross-validates parity.

**Trigger to promote.** First time a field gets added or renamed (e.g., adding `icon_ref` for launcher tiles). Or: introducing a fourth decoder (e.g., a debugger panel that reads the blob).

**Code surfaces.** Either switch to JSON for non-perf paths (cheap, debuggable) or introduce a schema-driven generator (real fix but more setup).

### 7.7 🟢 Magic numbers spread across the codebase

**Concern.** Watchdog timeout (1500ms), heartbeat interval (500ms), render initial buf (16KiB), save_state initial buf (4KiB), trap drain buf (256B). All hardcoded at point of use, no central tuning surface.

**Why deferred.** Defaults are reasonable; no reason to tune them in production yet.

**Trigger to promote.** First time we want to reduce watchdog timeout for tests, or run on a slower device with bigger margins. Probably along with Phase 11.8 (perf baseline).

**Code surfaces.** New `runtime/web/config.js` with documented constants.

### 7.8 🟢 Worker shell has no unit tests

**Concern.** `runtime/web/app_worker_shell.js` is exercised only via integration through AppRegistry. Direct dispatcher logic (fire / io_start / io_poll), capability gating (the SDK-side mirror of the kernel's check), and the futures registry have no isolated coverage.

**Why deferred.** Vitest doesn't expose real Web Workers; testing requires a fake-Worker harness like the AppRegistry side already has, but with the shell's globals (postMessage, addEventListener) mocked.

**Trigger to promote.** Phase 11.8 (perf + benchmarks) is a natural pairing — both want a real-browser e2e test bed (Playwright). Build the worker-shell harness as a step on the way there.

---

## Retired / promoted entries

_(none yet)_
