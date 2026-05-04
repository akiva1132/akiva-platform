# Development Plan — Akiva Platform

> **Deferred concerns:** architectural worries that are not
> stones in flight but are essential to track —
> sandboxing/crash-recovery, ABI v0→v1, web interop,
> text/input — live in [`docs/risks.md`](docs/risks.md). If
> one of them gets a trigger, it moves to ROADMAP and out of
> the risks file.

> **Status (May 2026):** v0.1 (Phases 0–7), v0.2 (Phases 8–10),
> and v0.3 (Phases 11–14) are all closed. ABI is at `0.12.0`.
> The platform is past the "POC → real OS skeleton" milestone
> and into capability expansion. Phase 14 closeout (Stones
> 14.3–14.6) brought browser Notifications, Clipboard,
> Vibration, and Geolocation behind permission gates. The next
> waves are v0.4 (additional capabilities + audio + multi-tab
> rendering) and v0.5 (Component Model migration).
>
> Per-stone detail lives in [`CHANGELOG.md`](CHANGELOG.md) and
> the per-feature docs ([`docs/abi.md`](docs/abi.md),
> [`docs/activity-model.md`](docs/activity-model.md),
> [`docs/storage.md`](docs/storage.md),
> [`docs/window-manager.md`](docs/window-manager.md), …).
> This file is the high-level phase map.

## Philosophy

**A thin skeleton, but a real skeleton.** Every OS subsystem
must exist. None of them may be a mock or a simulation. They
may be minimal — they have to be real.

**The guiding rule:** if we ever need to add a basic subsystem
in retrospect, it requires a rewrite. So every subsystem is
built from the start, even if minimally.

## Component map — what must exist by MVP

| component       | minimal MVP                                                                        | covered in phase |
|-----------------|------------------------------------------------------------------------------------|------------------|
| Akiva Kernel     | wasm hello-world in browser, frame loop                                             | 1a, 2            |
| Akiva Framework  | Activity + Package + Resource + Intent + Permission Managers                        | 2                |
| AKML language    | parser + 5 widgets + 4 attributes + value qualifiers (`-he`, `-night`)              | 3                |
| ABI / SDK        | WIT spec + Rust SDK + first app exports lifecycle                                   | 4                |
| Shell            | home screen with installed apps                                                     | 5                |
| Reference app    | Calculator                                                                          | 6                |
| Dev tools        | `akiva` CLI + dev server                                                            | 7                |

---

## Phase 0 — Planning ✅

Goal: pin design decisions before any code.

- [x] Define the OS architecture (`ARCHITECTURE.md`).
- [x] Define the activity model (`docs/activity-model.md`).
- [x] Define the resource system (`docs/resources.md`).
- [x] Define AKML (`docs/akml.md`).
- [x] Define intents (`docs/intents.md`).
- [x] Define the WindowManager + Shell (`docs/window-manager.md`).
- [x] Define the manifest (`docs/manifest.md`).
- [x] Define the ABI (`docs/abi.md`).
- [x] Define the SDK (`docs/sdk-rust.md`).

DoD: anyone reading `docs/` understands the platform's
boundaries before code starts.

---

## Phase 1a — Wasm Hello World in the browser ✅

Tooling, the web shell, and the first frame.

- [x] `cargo` with the `wasm32-unknown-unknown` target.
- [x] `wasm-tools` + `wasm-bindgen` toolchain.
- [x] `runtime/web/index.html` + `tab.js`.
- [x] OffscreenCanvas transferred to the worker.
- [x] Kernel boots, paints a black canvas with `[kernel] boot
      banner` text.

---

## Phase 1b — HAL Structure + Leader Election + Multi-tab ✅

Architecture + leader election + cross-tab transport.

- [x] **Leader election + BroadcastChannel coordination**
      ([leader.js](runtime/web/leader.js)).
- [x] **Display registration** — every tab registers as a
      display in the worker.
- [x] **Kernel skeleton** (Rust → wasm) with the major
      modules (events, scheduler, display manager).
- [x] **Leader failover** — closing the leader tab promotes
      another, kernel reboots from OPFS state.

Open from this phase (deferred):

- Follower tab rendering — a non-leader tab does not paint.
  Move to a frame-delta or ScreenSharing-API stream in v0.4.

---

## Phase 2 — Framework Core ✅

Activity + Package + Resource + Intent + Permission +
WindowManager.

- [x] **PackageManager** — install/uninstall/rehydrate from
      OPFS.
- [x] **ResourceManager** — strings + colors per package
      (with `night` qualifier from Stone 11.6.9 #3).
- [x] **ActivityManager** — full state machine, all 6
      lifecycle transitions.
- [x] **IntentRouter** — implicit + explicit resolve.
- [x] **PermissionController** — capability catalog +
      install-time grants.
- [x] **WindowManager** — surfaces, z-order layers (Wallpaper
      / App / Overlay / System), focus tracking.

---

## Phase 3 — Rendering Pipeline ✅

AKML + Layout + Compositor + frame loop.

- [x] **AKML parser** — XML tokenizer + AST.
- [x] **LayoutInflater** — AKML → widget tree (in the SDK; see
      [`ARCHITECTURE.md §2.1`](ARCHITECTURE.md)).
- [x] **Widget types** — `VStack` / `HStack` / `Frame` /
      `Grid` / `Text` / `Button` / `Rect` / `Spacer` /
      `Image`.
- [x] **Layout engine** — measure pass + place pass.
- [x] **Compositor** — `PaintOp` wire format + `DrawHal`
      trait.
- [x] **Frame loop** — rAF cadence, 60 Hz target.

---

## Phase 4 — ABI + SDK + First Real App ✅

- [x] **WIT definitions** stayed as future-target spec.
- [x] **Component loader** — replaced by raw v0 (deferred per
      `docs/research-component-model.md`).
- [x] **Rust SDK** — `Activity` trait async, `Context`,
      `Canvas`, `Ui` builder.
- [x] **First app: "Hello Akiva"** — exports lifecycle,
      paints text.

---

## Phase 5 — Shell App ✅

Shell as a real wasm app (post Stone 11.0).

- [x] **Shell codebase** lives at `apps/shell/`.
- [x] **Framework completions** — boot intent flow, `HOME` /
      `LAUNCHER` filters, cross-package drawable cache for
      launcher icons.

---

## Phase 6 — Calculator (the flagship app) ✅

- [x] **Calculator codebase** at `apps/calculator/`.
- [x] All four operations + display + memory.
- [x] `res/values/colors.toml` palette.
- [x] `on_save_state` / restore (5 stones).
- [x] Night-mode `res/values-night/colors.toml` (Stone 11.6.9 #3).

Deferred:

- `res/values/strings.toml` — calculator labels are single
  glyphs; not currently needed.
- `res/layout-land/main.akml` — depends on qualifier matching
  (post-MVP).
- `values-he` — out of MVP scope (per user preference).

---

## Phase 7 — Dev Tooling: CLI ✅

- [x] **`akiva new <name>`** — scaffold a new app
      (`Cargo.toml` + `manifest.toml` + `src/lib.rs` +
      `.gitignore`).
- [x] **`akiva build [names...]`** — the wasm build pipeline.
- [x] **`akiva run`** — kernel + every app + devserver in one
      command.
- [x] **`akiva devserver`** — stdlib-only HTTP server with
      COOP / COEP / CORP, thread-per-request, mime table.
- [x] **`akiva docs`** — open the SDK docs.

Deferred to Phase 8:
- `akiva package` / `akiva install` / `akiva uninstall`.

---

## Phase 8 — Real Package Manager ✅ (v0.2 pillar 1)

- [x] **`.akv` archive format** — AKPK v1, fully documented in
      [`docs/archive-format.md`](docs/archive-format.md).
- [x] **`akiva package <app>`** ([`cli/src/package.rs`](cli/src/package.rs)).
- [x] **`akiva install <archive>`**
      ([`cli/src/install.rs`](cli/src/install.rs)).
- [x] **Shell discovery** from `/dev-apps/index.json` (no
      hard-coded `DEV_APPS`).
- [x] **Upgrade + uninstall** with `version_code`-driven
      upgrade rules and major-version saved-state wipes.

---

## Phase 9 — Full Activity Model ✅ (v0.2 pillar 2)

- [x] **Full lifecycle** — every transition reaches the app
      (Stone 9.1).
- [x] **`SingleTask` + `SingleInstance` launch modes** —
      including dedicated tasks (Stone 9.6).
- [x] **Back stack + `Context::finish()`** — Stone 9.3.
- [x] **`start_activity` from app + `on_new_intent`** —
      Stone 9.4 (SingleTop / SingleTask / SingleInstance reuse
      delivers fresh intents).
- [x] **`start_activity_for_result` + `on_activity_result`** —
      Stone 9.5.
- [x] **Multiple tasks / foreground switching** — Stone 9.6
      (`Framework::switch_to_task`, `task_history` reverts).

---

## Phase 10 — Permissions (Capability-via-Imports) ✅ (v0.2 pillar 3)

- [x] **System permission registry**
      ([`framework/src/permissions.rs`](framework/src/permissions.rs))
      — declared at boot via `permissions::declare_mvp_permissions`.
- [x] **Install-time grant + persistence** —
      `package_manager::install` rejects unknown required
      permissions; grants stored at
      `sys/permissions/{package_id}` (Stone 10.2).
- [x] **Import map per capability** — wasm loader builds the
      import set from grants.
- [x] **ABI documentation per capability** —
      [`docs/abi.md §2.3.3`](docs/abi.md).
- [x] **Uninstall cleanup** — grants removed alongside the
      package.
- [x] **Calculator + tic-tac-toe + Shell declare permissions**.

---

## v0.2 summary

v0.2 closed three architectural pillars: real Package Manager,
full Activity model, real Permission model. The platform moved
from "POC running" to "OS skeleton with a contract".

See [`docs/v0.2-design.md`](docs/v0.2-design.md) for the
historical design spec and the gap-map showing how each pillar
resolved.

---

## Phase 11 — Process / Fault Isolation + Modern Async SDK ✅ (v0.3 pillar 1)

This phase is the largest in the platform's history. It moved
every app into its own Web Worker, introduced an async-native
ABI, and rebuilt the SDK around lifecycle scopes and an
embassy-shaped (custom-built) executor. See
[`docs/isolation-design.md`](docs/isolation-design.md) for the
full design spec.

Stones (every one closed; see CHANGELOG for the per-stone
detail):

- **11.0** — Shell extracted to `apps/shell/` as a wasm app.
- **11.1** — Design doc.
- **11.2** — async SDK + `Activity` trait async.
- **11.3** — Async ABI + future registry (unified
  multiplexers).
- **11.4a** — async `WasmHost` trait + framework state
  machine.
- **11.4b** — single-export ABI (`akiva_app_dispatch` +
  `_get_result`).
- **11.4c** — `app_worker_shell.js` + WorkerPool isolation.
- **11.5** — fault containment + watchdog.
- **11.6 a/b/c** — Storage SDK (kernel-routed).
- **11.6.5** — SDK API completeness pass — real TaskScope,
  concurrent host calls per activity.
- **11.6.7** — Foundation polish backlog — `WriteHandle`,
  `pkgs_wire` dedup, direct readonly storage, lazy-spawn
  workers, watchdog tunability.
- **11.6.8** — Foundation primitives gap — CSPRNG,
  `MAX_PENDING_PER_SLOT`, sync primitives, wall-clock,
  ABI versioning policy.
- **11.6.9** — Rendering + platform polish — `OP_FILL_ROUND_RECT`,
  `ResizeObserver` with `devicePixelContentBoxSize`,
  `matchMedia('prefers-color-scheme')` auto dark mode.
- **11.7** — `akiva new` + dev tooling polish.
- **11.8** — performance pass + benchmarks
  ([`docs/perf-baseline.md`](docs/perf-baseline.md)).

---

## Phase 12 — Resource Governance ✅ (12.1, 12.2 + 12.3)

- [x] **12.1** Per-app memory accounting (May 2026).
- [x] **12.2** Per-app CPU attribution (May 2026).
- [x] **12.3** Low-Memory Killer policy
      ([`framework/src/lmk.rs`](framework/src/lmk.rs)).
- [ ] **12.4** App suspension (soft eviction) — open.
- [ ] **12.5** Framework enforcement of `manifest.runtime.max_memory_mb`
      — open.

---

## Phase 13 — WindowManager Completion ✅

- [x] **Stone 13.0prep** — Foundation primitives: timer
      (Stone 13.0prep A — `IO::TIMER_SLEEP`), stroke + line
      paint ops (B), `@drawable/` + image rendering (C).
- [x] **Stone 13.0** — Window surface model
      ([`framework/src/window_manager.rs`](framework/src/window_manager.rs)).
- [x] **Stone 13.1** — Compositor z-ordering + per-surface
      paint pass.
- [x] **Stone 13.2** — System dialog overlays
      ([`framework/src/dialog.rs`](framework/src/dialog.rs)).
- [x] **Stone 13.3** — Transitions (slide + fade with
      ease-out cubic) — final stone closing the cross-package
      drawable cache + `EVENT_TEXT` (kind 5) text-input
      pipeline.
- [ ] **Stone 13.4** — Cross-display / multi-tab paint —
      partial; follower tabs see the multi-tab banner today.

Open follow-ups:
- Toasts in `WindowLayer::Overlay` — the layer exists, no
  widget renders into it. See
  [`docs/notifications.md §5`](docs/notifications.md#5-not-yet-implemented).

---

## Phase 14 — Browser Capabilities ✅

Every Phase 14 stone is closed (May 2026). ABI bumped to
`0.12.0`.

- [x] **Stone 14.1** — Net capability foundation + HTTP +
      WebSocket transports.
- [x] **Stone 14.2** — WebTransport.
- [x] **Stone 14.3** — Notifications (browser
      `Notification` + `Notification.requestPermission()` via
      cap_req/cap_resp main-thread bridge).
- [x] **Stone 14.4** — Clipboard
      (`navigator.clipboard.{readText, writeText}`).
- [x] **Stone 14.5** — Vibration (`navigator.vibrate`).
- [x] **Stone 14.6** — Geolocation
      (`navigator.geolocation.getCurrentPosition`).

Each ships under a manifest permission gate
(`akiva.permission.NETWORK / NOTIFICATIONS / CLIPBOARD /
VIBRATE / LOCATION`). Wire shapes pinned in
[`docs/abi.md §2.3.2`](docs/abi.md).

Demo coverage: `apps/network` exercises clipboard read/write,
vibration, and geolocation; manifest declares the full Phase
14 grant set.

Planned Phase 14.x follow-ups (not blocking):

- HTTP / WebSocket / WebTransport ergonomics — `fetch`
  helpers, retry, backoff.
- Network capability scoping — per-app allow-list of hosts /
  ports (today the grant is "any").

---

## v0.3 summary

v0.3 closed the three "hard systems tests" of a real OS:

- **Isolation:** an app cannot bring the system down.
- **Governance:** the system measures and enforces quotas on
  apps.
- **Window control:** the system manages multiple surfaces,
  not just a single fullscreen activity.

External-review roadmap calling these "the three tests before
real OS" is satisfied (the codex-notes external-review file
was retired in May 2026).

---

## v0.4 — Connected OS (next)

After v0.3, Akiva moves from an isolated OS to a **connected
OS**. The capabilities phase already started (Phase 14 + its
follow-ups). What remains:

- **Audio** — Web Audio API integration + sample streaming
  primitive (depends on the streams ABI from
  [`docs/risks.md §7.11`](docs/risks.md)).
- **Multi-tab paint** — follower tabs render their own surface
  rather than showing the multi-tab banner.
- **Native-grade animations** — shared-element transitions
  beyond slide+fade, easing curves per app.
- **More platform integration** — `open-url`, drag-and-drop
  acceptance, embedded browser view.
- **Demonstrate** — Akiva apps do what apps on every other OS
  do.

Estimated time: ~5–7 weeks after v0.3.

---

## v0.5+ — Component Model migration

Plan to migrate the ABI from raw v0 to the WebAssembly
Component Model:

- WASI 0.3 stable lands (late 2026 / early 2027).
- `wit-bindgen` Rust async settles.
- Browser-side jco transpile pipeline integrated into
  `akiva package`.

See [`docs/research-component-model.md`](docs/research-component-model.md)
for the deferred decision and re-evaluation triggers.

---

## Time summary

### MVP (v0.1) — Phases 0–7

| phase | description                                          | time          |
|-------|------------------------------------------------------|---------------|
| 0     | Planning (docs)                                      | done          |
| 1a    | Wasm hello world in browser                          | 1 week        |
| 1b    | HAL + Leader Election + Multi-tab                    | 2 weeks       |
| 2     | Framework managers                                   | 4–6 weeks     |
| 3     | AKML + Layout + Compositor                            | 6–8 weeks     |
| 4     | ABI (WIT) + SDK + hello app                          | 3–4 weeks     |
| 5     | Shell                                                 | 2 weeks       |
| 6     | Calculator                                           | 2–3 weeks     |
| 7     | CLI                                                   | 2–3 weeks     |

**Total MVP: ~5–7 months** of focused single-developer work.

### post-MVP — Phases 8–14

| phase | description                                                                                                 | time       |
|-------|-------------------------------------------------------------------------------------------------------------|------------|
| 8     | Package distribution (.akv + install)                                                                       | 2–3 weeks  |
| 9     | Full Activity lifecycle (back stack, for-result)                                                            | 3–4 weeks  |
| 10    | Permission model (capability-via-imports)                                                                   | 2–3 weeks  |
| 11    | Process isolation (dedicated workers + ABI)                                                                 | 13–19 weeks |
| 12    | Resource governance (memory + CPU quota)                                                                    | 3–5 weeks  |
| 13    | WindowManager completion (overlays, transitions) + foundation prep (timers/stroke/drawable)                 | 6–8 weeks  |
| 14    | Capabilities (WebSocket, Audio, Notify, Clipboard…)                                                         | 5–7 weeks  |

**Total post-MVP v0.2–v0.4: ~8–13 additional months.**

---

## Open MVP items — maintenance backlog

Items from largely-closed phases that aren't done yet.
Aggregated so they don't get lost.

**From Phase 1b:**

- Follower rendering — a follower tab paints nothing today.
  Was deferred to Phase 2 in the original ROADMAP and never
  revisited.

**From Phase 2 (Framework):**

- ✅ **IntentRouter wired** (Apr 2026) — every manifest
  intent_filter registered in `framework::boot`, including a
  synthetic Shell filter (MAIN+HOME+LAUNCHER).
- ✅ **ResourceManager wired** (Apr 2026) — strings + colors,
  default `values/`, `night` qualifier (Stone 11.6.9 #3).
- ✅ **PermissionController** — fully implemented in Phase 10.
- 🟡 **WindowManager** — Phase 13 closed it for a single
  display.

**From Phase 3 (AKML):**

- ✅ **Grid widget** (Apr 2026).
- 🟡 **Image widget** — done. **Input widget** — open.
- Attributes: `margin`, `background`, `visible`, `textAlign`,
  `fontWeight`, `tag` — partial coverage.
- Units `dp` / `sp` / `%`; `match_parent` / `wrap_content` —
  partial.
- ✅ **Resource references** — `@color/` + `@string/` work;
  `@dimen/` / `@drawable/` / `@style/` post-MVP (drawable
  shipped post-Stone 13.0prep #C).

**From Phase 4 (ABI):**

- WIT / Component Model migration — see
  [`docs/research-component-model.md`](docs/research-component-model.md).

**From Phase 5 (Shell):**

- ✅ Shell extracted to `apps/shell/` as a real wasm package
  in Stone 11.0.

**From Phase 6 (Calculator):**

- ✅ `res/values/colors.toml` — full palette migrated.
- 🟡 `res/values/strings.toml` — deferred; calculator labels
  are single glyphs.
- 🟡 `res/layout-land/main.akml` — depends on qualifier
  matching (post-MVP).
- ✅ `on_save_state` / restore (Apr 2026) — TLV codec, full
  framework + SDK pipeline.
- 🟡 `values-he` — out of MVP scope (user preference).

**From Phase 7 (CLI):**

- ✅ `akiva devserver`, `akiva build`, `akiva run`,
  `akiva new`.
- ✅ `akiva package` / `akiva install` (Phase 8).

**Foundation gaps deferred (May 2026 audit, post-Stone 11.6.9):**

Not blocking Phase 12 / 13. Add when a real app demands them:

- **Text input — character data:** today key events carry
  `keyCode` only. Phase 14 follow-up wired `EVENT_TEXT`
  (kind 5) for typed characters and `compositionend`. **Done.**
  Editable-text *widgets* (focus, caret, selection) and
  Hebrew/IME/RTL still open. Blocks notes/email/chat apps.
- ✅ **`storage::list_keys()` / scan** — shipped post-11.6.7.
- 🟡 **App icon + display name in manifest:**
  `app.icon = "@drawable/launcher"` +
  `app.label = "@string/app_name"` — depends on `@drawable/`
  density buckets (post-MVP).
- 🟡 **Crash log persistence:** trapped apps lose their logs
  on `worker.terminate()`. A ring buffer that collects log
  events in flight and persists to `sys/crashes/{id}/{ts}.log`
  on trap is helpful for postmortems.
- 🟡 **`select!` macro:** thin wrapper over the existing
  `Channel` / `Mutex` / `Signal` (Stone 11.6.8 #3). Apps can
  use match-loop or `futures::future::select` for now.
- 🟡 **Multi-key storage atomicity:** OPFS does not expose
  transactions. Recommendation: never; apps either pack into
  a single `on_save_state` blob or use an index-file pattern.
- 🟡 **CI / GitHub Actions:** 600+ Rust + 181 vitest tests run
  locally. No pre-merge gate. Add `.github/workflows/test.yml`
  when the codebase opens to external contributors.

---

## What is **not** in MVP (deliberate)

Thin **in depth**, not in completeness:

- ❌ Animations beyond push/pop slide+fade (rich animations
  arrive with v0.4).
- ❌ Multi-window / split-screen (one activity per display).
- ❌ Intent chooser UI (if multiple matches → pick first).
- ❌ App store / distribution — `.akv` archives + CLI install
  exist, but no central store.
- ✅ Network APIs — Phase 14 shipped HTTP / WebSocket /
  WebTransport.
- ❌ Accessibility (architecture is ready; implementation
  deferred to v0.4+).
- ❌ Themes (colors only, no styles).
- ❌ Sophisticated layouts (only VStack / HStack / Grid /
  Frame).
- ❌ SVG / JPG drawables (PNG only — see
  [`docs/resources.md §8`](docs/resources.md)).
- ❌ WebGPU (Canvas 2D through abstract `RendererBackend` —
  swap-in available post-MVP).
- ❌ Hebrew / RTL / BiDi text rendering (Latin-only `fillText`
  in MVP; HarfBuzz + FriBidi in v0.3+).
- ❌ IME / virtual keyboard for Hebrew.
- ❌ Multi-threading inside an app (wasm single-threaded).
- ❌ Inter-app storage sharing.
- ❌ Hot reload (proper).
- ❌ Debugger / inspector.
- ❌ PendingIntent + Broadcast receivers (see
  [`docs/intents.md §13–14`](docs/intents.md)).
- 🟡 Notifications: ✅ browser API (Stone 14.3); Shell-side
  tray + toasts open.
- ❌ SDKs for non-Rust (Kotlin/Dart/Go post-MVP — see "SDK
  roadmap" below).

**Each of these can be added in v0.X+ without rewriting
foundations.**

### What's included in v0.2 (and not v0.1/MVP)

v0.2 focused on the **system contract with apps**, not UI. See
[`docs/v0.2-design.md`](docs/v0.2-design.md) for the inventory.

- ✅ `.akv` distribution + `akiva package`/`install`/`uninstall`
  (Phase 8).
- ✅ Full Activity lifecycle + SingleTask/SingleInstance + Back
  navigation (Phase 9).
- ✅ `start_activity` / `start_activity_for_result` /
  `on_new_intent` / `on_activity_result` (Phase 9).
- ✅ Permission model (Capability-via-Imports, install-time
  grants, persistence) (Phase 10).

**Outside v0.2, deferred to v0.3+:**

- ✅ Real Window Manager (multi-surface, z-order, focus) —
  Phase 13.
- ✅ System services (notifications, clipboard, location) —
  Phase 14.
- 🟡 Runtime permission prompts (dangerous perms — camera /
  microphone) — open.
- ❌ PendingIntent + Broadcast receivers.
- 🟡 Image (✅) / Input (❌) / ScrollView (❌) widgets.
- 🟡 Animations (✅ slide+fade with ease-out cubic).
- 🟡 Themes / sophisticated layouts.
- ✅ Network API (Phase 14.1+); ❌ keyboard input / IME.
- ❌ Code signing, hot reload, debugger / inspector.
- ❌ Non-Rust SDKs, multi-threading, inter-app storage
  sharing.
- ❌ Hebrew / RTL / BiDi.

---

## SDK roadmap

The ABI is grounded in **WIT / Component Model**, which opens
the path to additional-language SDKs without runtime changes.

| SDK                      | when             | note                                                                                                |
|--------------------------|------------------|-----------------------------------------------------------------------------------------------------|
| Rust (MVP)               | Phase 4          | `wit-bindgen` produces bindings automatically.                                                      |
| C / C++ (v0.4+)          | post-MVP         | `wit-bindgen c` — tooling matures.                                                                  |
| Go (v0.4+)               | post-MVP         | TinyGo + `wit-bindgen` (or Go upstream's component support).                                        |
| Kotlin / Dart (v0.5+)    | post-MVP         | Depends on WasmGC maturity — stable in browsers as of 2026; binary sizes meaningfully smaller than Go. |
| Python (v0.5+)            | post-MVP         | Componentize-py; suits data-heavy apps.                                                             |
| JavaScript / TypeScript  | not planned      | Deliberately out of scope.                                                                          |

---

## Environmental dependencies

Before starting:

- `rustc` + `cargo` (rustup) — nightly through MVP (for
  `target-wasm` and `wit-bindgen` latest).
- `wasm32-unknown-unknown` target.
- `wasm-tools` (`cargo install wasm-tools`) — to inspect
  components and WIT.
- `wit-bindgen-cli` (`cargo install wit-bindgen-cli`) — for
  Rust bindings from WIT.
- `wasm-opt` (from binaryen) for production builds.
- Node.js 20+ (Playwright in CI; an axum-based dev server is
  optional in dev).
- Playwright (`npm i -D @playwright/test`) for E2E (deferred —
  see [`docs/testing.md §7`](docs/testing.md)).
- Browser: Chrome ≥ 119, Firefox ≥ 120, Safari ≥ 18.2 (because
  of WasmGC + Web Locks + OPFS SyncAccessHandle).
