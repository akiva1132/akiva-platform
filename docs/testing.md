# Testing Strategy

> **Implementation status (May 2026):** **unit** + **component** + **E2E**
> layers are live. The **unit** layer covers ~600 Rust tests across
> `framework`, `kernel`, `wire`, `sdk-rust` and 235 vitest cases
> across `runtime/web/__tests__/`. The **component** layer overlaps
> with unit tests today — fakes / in-memory storage are passed
> directly into struct constructors rather than dispatched via a
> separate harness. The **E2E** layer is a 6-test Playwright suite
> driven through a JS-only inspector (`window.__akiva`); see §7.
> The **integration** (wasm-bindgen) layer is the only one still
> deferred, tracked in §5.

## 1. Purpose

Akiva covers a wide surface: the kernel, the framework, the SDK,
sample apps, and the JS HAL — plus the ABI contract between them.
Testing decisions made early save weeks of pain across the roadmap.

## 2. Layers (target shape)

```
┌────────────────────────────────────────────────────┐
│ E2E (Playwright)                                    │  ← full boot, real browser  (LIVE — see §7)
├────────────────────────────────────────────────────┤
│ Integration (wasm32 in headless browser)            │  ← kernel + framework + WIT (deferred)
├────────────────────────────────────────────────────┤
│ Component tests (cargo test, native target)         │  ← module-level wiring with fakes (LIVE — overlaps with unit)
├────────────────────────────────────────────────────┤
│ Unit (cargo test + vitest)                          │  ← pure functions, algorithms (LIVE)
└────────────────────────────────────────────────────┘
```

## 3. Unit tests (Rust, native)

Written next to the code in `#[cfg(test)]` modules. Run on
`x86_64-unknown-linux-gnu` — fast; `cargo test` is the only command
needed.

Examples already in tree:

- AKML parser: `parse_attribute("16dp")` → `SizeValue::Dp(16.0)`.
- Resource qualifier matcher: `best_match(config, candidates)`.
- Intent filter matching.
- Task stack operations (push / pop / launch modes).
- PaintOp encode/decode (`framework/src/wire/paint.rs`).
- LMK victim selection (`framework/src/lmk.rs`).
- AnimationController progress + ease-out (`framework/src/animation.rs`).
- SavedState TLV codec (`sdk-rust/src/state.rs`).

**Rule.** Anything that does not directly touch wasm imports/exports
and does not require a browser is unit-testable on the native
target. That covers the bulk of the kernel and framework today.

## 4. Component tests (Rust, native)

In-process tests with fakes substituted for HAL boundaries. In
practice these overlap with unit tests today — there is no separate
harness or separate Cargo test binary; struct constructors take a
`&dyn Storage` (or similar) directly and a `FakeStorage` is wired
in-line.

Example (paraphrased from the existing tests):

```rust
#[test]
fn resource_manager_finds_hebrew_string() {
    let storage = FakeStorage::new()
        .with_file("apps/calc/res/values/strings.toml",    "[strings]\nhello=\"Hi\"")
        .with_file("apps/calc/res/values-he/strings.toml", "[strings]\nhello=\"שלום\"");
    let rm = ResourceManager::new(&storage, DeviceConfig::he_il());
    assert_eq!(rm.get_string("@string/hello"), Some("שלום"));
}
```

When component coverage outgrows the unit-test overlap, we'll split
it out — but until then the boundary is intentionally thin.

## 5. Integration tests (wasm32) — deferred

**Goal.** Verify that the kernel boots inside wasm and that the ABI
behaves the way the native-target tests claim it does.

**Tooling decision.** `wasm-bindgen-test` is the MVP path; the
longer-term option is the `wasmtime` native test runner, available
once the Component Model + `wit-bindgen` story stabilises.

**Status.** Not in tree yet. The `kernel.wasm` build is exercised
end-to-end manually (open browser, run apps), and the breadth of
unit tests catches most regressions before they reach the wasm
pipeline. The integration layer lands when we hit the first bug
that *only* reproduces in wasm.

Sketch of the eventual harness:

```toml
# kernel/Cargo.toml
[dev-dependencies]
wasm-bindgen-test = "0.3"

[[test]]
name = "kernel_integration"
path = "tests/kernel_integration.rs"
```

```rust
// tests/kernel_integration.rs
use wasm_bindgen_test::*;
wasm_bindgen_test_configure!(run_in_browser);

#[wasm_bindgen_test]
fn wasm_loader_instantiates_hello_app() {
    let hello_bytes = include_bytes!("fixtures/hello.wasm");
    let kernel = Kernel::new();
    let inst = kernel.wasm_loader().load(hello_bytes).unwrap();
    assert!(inst.call_abi_version().starts_with("0.1."));
}
```

Run via `wasm-pack test --headless --chrome kernel/`.

## 6. JS-side unit tests (vitest)

The JS HAL has its own test suite — `runtime/web/__tests__/` —
exercised by `vitest`. As of May 2026, four files / 181 cases:

- `canvas2d_paintops.test.mjs` — full PaintOp coverage, foreign
  drawables, `globalAlpha`.
- `app_registry.test.mjs` — package install / uninstall / spawn /
  watchdog / capability bridge / STORAGE_LIST.
- `storage.test.mjs` — OPFS + sandbox-strip + per-key paths.
- `storage_path.test.mjs` — path encoding / decoding rules.

Run with:

```bash
npx vitest run --reporter=basic
```

These tests stand in for both unit and component layers on the JS
side. They mock OPFS / `postMessage` / `OffscreenCanvas` rather than
spinning up a real browser.

## 7. E2E tests (Playwright) — LIVE

**Goal.** Validate full boot, real interaction, and (eventually)
multi-tab flows through a real browser.

**Status.** Live as of May 2026. Six tests in `tests/e2e/` cover the
boot smoke path and the calculator end-to-end. Run with
`npm run test:e2e` (headless), `npm run test:e2e:headed` (visible
browser), or `npm run test:e2e:ui` (interactive Playwright UI with
time-travel). The dev server is auto-started by Playwright's
`webServer` config.

```
tests/e2e/
├── playwright.config.ts
├── helpers/
│   └── akiva.ts             — bootAkiva, launchCalculator, tapId, tapRole, waitForText
├── boot.spec.ts             — dev server alive + inspector mount/unmount
├── calculator.spec.ts       — 2+3=5, 7×8=56, every-digit-published
└── open.mjs                 — `npm run dev:open` headed Chromium with debug overlay
```

### 7.1 Inspector — `window.__akiva`

Akiva renders to canvas, so `document.querySelector` finds nothing
useful and Playwright's `getByRole`/`getByText`/`toMatchAriaSnapshot`
can't see widgets. Each frame the SDK publishes a flat semantic tree
(see [`sdk-rust/src/ui/semantics.rs`](../sdk-rust/src/ui/semantics.rs))
of what's on screen — role, surface-local bounds, label, optional
stable id. The host's `runtime/web/inspector.js` decodes that stream
and exposes the latest tree on `window.__akiva`:

```typescript
window.__akiva.findById("d7")              // → { rect, role, label, ... } | null
window.__akiva.findByRole("button", { name: "+" })
window.__akiva.findByText("56")
window.__akiva.queryAll({ role: "button" })
window.__akiva.snapshot()                  // every active activity + nodes
window.__akiva.list()                      // packageId/handle directory
```

The inspector mounts only when `?inspector=1` is set (or
`localStorage.akivaInspector === "1"` for sticky dev sessions);
production tabs receive the bytes and silently drop them. **No DOM
mirror** — an earlier draft projected the tree into hidden
ARIA-decorated divs so `getByRole` would work, but that coupled the
OS to a browser-only fallback path Akiva will run without on native
shells, and ran a 60 Hz DOM rebuild on every paint. The pure-JS form
is faster and decouples the platform from the browser DOM entirely.

### 7.2 Composite-aware rects

The SDK publishes bounds in surface-local coordinates. The
framework's compositor applies translate / scale / opacity per frame
during push transitions, sheet drags, and modal scale-ins. The
inspector applies the current transform to every rect it returns
via a per-frame `PaintOp::SetInspectorTransform` op routed through
the `DrawHal` trait — see
[`framework/src/compositor.rs`](../framework/src/compositor.rs)
and the `applyTransform` method in
[`runtime/web/inspector.js`](../runtime/web/inspector.js). Tests can
therefore tap mid-animation and land on the visually-correct pixel.

### 7.3 Helpers (`tests/e2e/helpers/akiva.ts`)

```typescript
test("calculator: 2 + 3 = 5", async ({ page }) => {
    await launchCalculator(page);             // boot + click Shell launcher + wait for first paint
    await tapId(page, "d2");                  // by stable id
    await tapRole(page, "button", "+");       // by role + accessible name
    await tapId(page, "d3");
    await tapRole(page, "button", "=");
    await waitForText(page, "5");             // matches a text-role node's visible content
});
```

Each tap dispatches a real `PointerEvent` on the canvas at the
widget's reported centre — same input pipeline a human user takes.
A 50 ms gap follows each tap to let the worker ack the previous
event before the next one arrives; without it, back-to-back clicks
outpace the kernel's dispatcher and the second one's pointerdown
lands while the first is still working its way through, dropping
operators between digits. See §10 for the durable fix.

### 7.4 Visual debugging — `npm run dev:open`

Launches a headed Chromium with DevTools open and a debug-overlay
canvas that paints the inspector's reported bounds in real time
(blue: buttons, yellow: text, green: groups). Useful when a test is
flaking and you want to confirm the SDK and the inspector agree on
"where d7 is" before diving into helper coordinate math.

## 8. CI

Today, the only enforced job is local: `cargo test --workspace` +
`vitest run` + a manual `cargo build --target
wasm32-unknown-unknown --release`. The full pipeline below is the
target, not the current state:

1. `cargo fmt --check && cargo clippy -D warnings`
2. `cargo test --workspace` (native)
3. `wasm-pack test --headless --chrome kernel/` *(deferred)*
4. `cargo build --target wasm32-unknown-unknown --release` (kernel +
   every app crate) — or, equivalently, `cargo run -p akiva -- build`
5. `wasm-opt -O3` on every built wasm
6. `npx vitest run`
7. `npx playwright test --config tests/e2e/playwright.config.ts`

GitHub Actions wiring lands when we have external contributors;
until then the manual run is sufficient.

## 9. Test fixtures (planned)

When `tests/fixtures/` lands:

- `tests/fixtures/apps/` — miniature app packages used by the
  integration / E2E layers (`hello`, `two-activity-hello`,
  `needs-network`).
- `tests/fixtures/akml/` — AKML files for the parser tests.
- `tests/fixtures/manifests/` — `manifest.toml`s, valid and invalid.

## 10. Not yet covered

- Performance benchmarks — the per-bench tables in
  [`docs/perf-baseline.md`](perf-baseline.md) are filled for SDK /
  framework microbenches; browser-side timings are deferred.
- Fuzz testing of the AKML parser and the TOML manifest parser.
- Visual regression (screenshot diffing) — easy to bolt onto
  Playwright once Shell is stable.
- Chaos / crash-recovery automation.
- Multi-tab E2E (leader-election, follower paint, promotion
  on-close). The single-tab smoke + calculator suite is the only
  E2E coverage today; multi-tab needs a fixture that drives two
  pages against the same dev server.

All of these are v0.3+ work.

### 10.1 Known E2E debt

Two workarounds in [`tests/e2e/helpers/akiva.ts`](../tests/e2e/helpers/akiva.ts)
that are tracked rather than fixed:

1. **Coordinate click on the Shell launcher** — `launchCalculator`
   clicks `(320, 148)` to open the calculator, derived from the
   Shell's Rust-side `LIST_TOP=120, ROW_HEIGHT=56` constants. The
   Shell paints with raw canvas primitives (`c.fill_rect`,
   `c.draw_text`), not `Ui::button`, so it doesn't show up in the
   inspector. **Fix:** migrate the Shell's launcher rows to the SDK
   widget toolkit; the helper then becomes
   `tapRole(page, "button", "Calculator")` and the constants
   disappear. Tracked alongside the Phase 16 Shell rewrite.

2. **50 ms gap between consecutive taps** — `tapId`/`tapRole` end
   with `await page.waitForTimeout(50)` to let the kernel-worker ack
   each event before the next pointerdown arrives. Back-to-back
   clicks faster than that outpace the
   `tab → worker postMessage → kernel dispatch → Activity::on_pointer`
   pipeline, dropping the operator between digits in a `2 + 3` press
   sequence. **Fix:** expose a frame-counter or per-handle
   "pending events" field on `__akiva.snapshot()` and have the
   helper poll for it instead of timing out. Cosmetic until the
   pipeline gets faster anyway.

## 11. Coverage required before any "v0.2 ship" milestone

- [ ] Every parser (AKML, manifest, resource TOML) carries
      unit tests. **Done.**
- [ ] `ResourceManager` qualifier matcher has ≥ 30 test cases
      (combinations included). **Done — see
      `framework/src/resources.rs` test module.**
- [ ] `ActivityManager` task stack — every launch mode + Back
      navigation. **Done — Stones 9.1–9.6 land matching tests.**
- [ ] Integration test: `WasmLoader` instantiates a stub app inside
      wasm. **Pending — see §5.**
- [x] E2E: `boot → Shell → open calculator → 2+3=5 → back → Shell`.
      **Done — `tests/e2e/calculator.spec.ts` covers `2+3=5` and
      `7×8=56` via the `window.__akiva` inspector. The `back → Shell`
      step is not yet asserted (calculator stays foreground); add
      when the back-button helper lands.**
- [ ] E2E: 2 tabs, both show Shell, calc opens in tab 1, tab 2
      stays on Shell, close tab 1, tab 2 promotes to leader, calc
      state preserved. **Pending — needs a multi-page fixture.**
