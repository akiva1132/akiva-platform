# Test-coverage audit

> **Baseline:** 274 passing tests across the workspace. Framework 71, CLI
> 68, SDK 135, kernel/runtime 0. Zero JS tests anywhere (`worker.js`,
> `app_registry.js`, `storage.js`). Three crates with 0 passing tests —
> `kernel` (by design, wasm target without host), `akiva-host` (binary
> entry), and a third workspace member. The Stone 9.1 destroy() bug
> reminded us that untested driver code is where silent bugs live; this
> doc ranks what to backfill first.

## 1. Per-module tally

### Framework crate (71 tests)

| module                           | lines | tests | comment                                                                |
| -------------------------------- | ----- | ----- | ---------------------------------------------------------------------- |
| `activity_manager.rs`            | 696   | 22    | strongest coverage — Stone 9.1 + Task 1 audit both landed here         |
| `package_manager.rs`             | 360+  | 17    | install/uninstall/upgrade all exercised                                |
| `wasm_activity.rs`               | —     | 10    | paint-op encode/decode, lifecycle proxy, state-blob passthrough        |
| `manifest.rs`                    | —     | 9     | TOML parsing + validation                                              |
| `resources.rs`                   | —     | 7     | strings + color tables                                                 |
| `intent.rs`                      | 255   | 6     | resolve + filter matching + clear_package                              |
| **`lib.rs`**                     | **548** | **0** | **biggest gap** — `Framework` dispatch + bring-up + tear-down          |
| `shell.rs`                       | 224   | 0     | ShellActivity — visually tested end-to-end only                        |
| `compositor.rs`                  | 77    | 0     | thin accumulator — exercised indirectly via `render_frame`             |
| `window_manager.rs`              | 77    | 0     | registry stub; not wired into dispatch until Stone 9.6                 |
| `permissions.rs`                 | 69    | 0     | types only; no enforcement until Phase 10                              |
| `storage.rs`                     | —     | 0     | trait + `MemoryStorage` helper — trivial surface                       |
| `activity.rs`                    | 123   | 0     | trait + event structs; no logic                                        |

### CLI crate (68 tests)

| module                           | tests | comment                                         |
| -------------------------------- | ----- | ----------------------------------------------- |
| `archive.rs`                     | 23    | TLV + decode + roundtrip                        |
| `install.rs`                     | 11    | path picking + id-extract + overrides           |
| `devserver.rs`                   | 11    | MIME map, dynamic endpoint, list_dev_apps_json  |
| `new.rs`                         | 7     | scaffolder                                      |
| `uninstall.rs`                   | 6     | by-name / by-id / ambiguous / missing           |
| `package.rs`                     | 6     | build + collect + archive-write                 |
| `build.rs`                       | 4     | cargo invocation shape                          |

### SDK crate (135 tests)

Evenly spread across `activity`, `canvas`, `akml`, `ui`, `res`. Not the
weak area.

### Runtime JS (0 tests)

- `runtime/web/worker.js` — kernel loader, leader election, dev-apps
  discovery, lifecycle dispatch fan-out.
- `runtime/web/app_registry.js` — the biggest one: wasm app loading,
  paint-op decode, lifecycle fan-out, save-state marshalling, resource
  TOML mini-parser, host import surface.
- `runtime/web/storage.js` — OPFS `SyncAccessHandle` wrapper + binary
  file format.
- All four exercised only via browser smoke-testing.

## 2. Top untested driver surfaces (by risk)

The ranking below weights "amount of logic" × "how silently a bug would
regress." Stone 9.1's destroy() break is the reference point for
"silent bug."

1. **`Framework::launch_app`** ([lib.rs:437](../framework/src/lib.rs#L437)) — pushes new top, pauses previous, drives Create → restore → Start → Resume. Regression here breaks every inter-activity launch.
2. **`Framework::finish_top`** ([lib.rs:462](../framework/src/lib.rs#L462)) — destroys top, runs save-state, persists blob, resumes new top. This is the method that surfaced the destroy() break; worth having a direct mock-driven test even though `ActivityManager::destroy` is now covered.
3. **`Framework::apply_post`** ([lib.rs:349](../framework/src/lib.rs#L349)) — drains `finish_requested` / `launch_requested` after every callback. Determines launch-wins-over-finish precedence. One-line regression could silently swallow a user's back-tap.
4. **`Framework::dispatch_key` Back short-circuit** ([lib.rs:265](../framework/src/lib.rs#L265)) — global Esc → `finish_top` when stack > 1. Easy to break by changing the condition.
5. **`Framework::try_restore_state`** ([lib.rs:529](../framework/src/lib.rs#L529)) — mid-bring-up restore hook. Wrong condition means saved state gets lost silently.
6. **`boot()`** ([lib.rs:82](../framework/src/lib.rs#L82)) — package rehydration, resource loading, intent filter registration, Shell launch. A smoke-test exists indirectly via kernel boot but nothing isolates the framework bring-up contract.

All six are reachable through `Framework` alone with a `MockWasmHost`
(already written for `wasm_activity.rs` tests) and `MemoryStorage`
(already in `storage.rs`). No new test scaffolding required.

## 3. Priority list

| priority | item                                                    | reasoning                                                          |
| -------- | ------------------------------------------------------- | ------------------------------------------------------------------ |
| **P0**   | `Framework::finish_top` — persist-then-stop-then-destroy | same shape as the Task 1 canary; one test locks the whole chain in |
| **P0**   | `Framework::launch_app` — previous-paused, new-top-pushed, restore-between-create-and-start | covers launch + stack + restore in one sweep                       |
| **P1**   | `Framework::apply_post` — launch-vs-finish precedence   | surfaces a rule that's currently only documented in the method     |
| **P1**   | `Framework::dispatch_key` Back short-circuit            | "one-line regression = everyone loses Back" class                  |
| **P2**   | `boot()` — package load + intent register + Shell launch | heavy fixture but gives a baseline smoke for the whole framework   |
| **P3**   | `compositor.rs`, `window_manager.rs`, `shell.rs`        | low surface, tested indirectly. Defer until they grow.             |

P0 + P1 together = ~4 new tests, est. half-day, cleanly fit next to the
existing `RecordingActivity` harness.

## 4. Integration test plan

Currently none exist. Options:

- **Rust-side end-to-end (cheap):** build the framework with
  `MockWasmHost` + a hand-coded `RecordingActivity` for every role,
  script "boot → launch calculator → dispatch pointer sequence →
  assert SavedState bytes." This would sit in `framework/tests/` as an
  integration test (not a unit test). **~half day.**
- **Browser smoke (expensive):** real wasm + real OPFS + real DOM. A
  Playwright harness that boots the runtime, installs an
  `.akv`, clicks a button, asserts the pixel output. Meaningful
  coverage but ~1-2 days of scaffolding — defer until a CI lane exists.

**Recommendation:** do the Rust-side one in Phase 9.3 (same frame as
`Context::finish()` plumbing — natural end-to-end sanity check for
user-triggered finish). Defer the Playwright harness until v0.3.

## 5. JS coverage — deferred

`worker.js` + `app_registry.js` + `storage.js` deserve tests but Rust's
`cargo test` doesn't reach them. Options (not taken in v0.2):

1. Node-side unit tests with a harness that stubs `WebAssembly`, OPFS,
   and `postMessage`. Viable but heavy.
2. Playwright end-to-end (see §4).
3. Port the non-browser-specific bits (TOML mini-parser,
   paint-op decode) to a shared JS module that Node can exercise.

Filed as a ROADMAP item for Phase 11+. For v0.2 we accept that JS is
covered by hand-testing in the browser.

## 6. Actions from this audit

1. Four unit tests queued for a near-future commit (P0 + P1 items in §3),
   slotted for whenever the next framework/lib.rs change lands so the
   test scaffolding gets built up incrementally rather than as a big-bang.
2. Integration-test sketch earmarked for Stone 9.3 (per §4).
3. No ROADMAP changes beyond an explicit note that JS tests are deferred
   to Phase 11+.
