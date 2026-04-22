# Risk Register & Deferred Concerns

> **What this doc is for.** A small, single place for architectural concerns that aren't actionable right now but **must not be forgotten**. Each entry names the concern, why deferral is acceptable today, and the trigger that would promote it to a stone. If a concern becomes a stone, its entry moves to `ROADMAP.md` and is deleted here.
>
> **What it is not.** A bug list (use GitHub issues), a feature wishlist (use `vision.md`), or a design doc (use `docs/*-design.md`).

_Created: 2026-04-22, after Stone 9.4 landing._

---

## 1. Process-level isolation + crash recovery

**Concern.** Every app runs in the **same Web Worker** as the kernel. Each package gets its own `WebAssembly.Memory` (memory-space isolation is fine), but all apps share:

- the worker event loop
- the JS heap
- the OPFS `SyncAccessHandle`
- the rAF frame budget

Consequences:

- An infinite loop in one app freezes the whole OS. There is no preemption.
- A wasm trap in one app is caught in `AppRegistry.*` try-blocks, logged, and ignored — but the `ActivityRecord` stays at `Resumed`. The next frame/pointer/key dispatch calls the same trapping path again. Zombie record, no cleanup.
- No Low-Memory-Killer: once a package's `WebAssembly.Instance` is created in `_instantiateProcess`, it lives until the worker terminates. Idle apps hold memory forever.

**Why deferred.** Solving this means either (a) one worker per package (cross-tab messaging overhead, OPFS handle sharing headaches) or (b) a cooperative scheduler with time-slicing + trap-recovery state machine. Both are multi-week efforts. MVP pain is low because the app set is tiny and developer-controlled.

**Trigger to promote.** First genuinely third-party app + first production report of "one app froze the OS". Both are v0.3+ territory.

**Where the code would change.**

- [`runtime/web/app_registry.js`](../runtime/web/app_registry.js) — `_instantiateProcess` + error-catch paths.
- [`runtime/web/worker.js`](../runtime/web/worker.js) — frame loop + event dispatch.
- [`framework/src/activity_manager.rs`](../framework/src/activity_manager.rs) — new `Trapped` lifecycle state.

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

**Concern noted, rejected for now.** A design review flagged that the host now implements 11 independent protocols (paint, save_state, restore, finish drain, intent drain, new_intent, resources × 4, log, text_width) and suggested consolidation into "a few strong primitives" — e.g. a single typed message bus.

**Why rejected.** Each protocol maps 1:1 to a user-visible capability. Consolidating now would invent an abstraction layer (message-bus dispatch, typed routing, error mapping) before we've seen the third new primitive. The "rule of three" isn't met. Stone 9.4's work **did** consolidate the two _byte-transfer patterns_ (alloc-copy-call vs 2-phase read) into shared helpers in `AppRegistry._copyAppToKernel` / `_callAppWithBytes`, which is the real duplication. Protocol count is a symptom of feature breadth, not a complexity we should disguise.

**Trigger to revisit.** If we hit ~20 protocols or if a third byte-transfer pattern appears. Neither is near.

---

## Retired / promoted entries

_(none yet)_
