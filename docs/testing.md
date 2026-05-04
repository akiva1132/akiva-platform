# Testing Strategy

> **Implementation status (May 2026):** the **unit** layer is fully
> live (~600 Rust unit tests across `framework`, `kernel`, `wire`,
> `sdk-rust`; 181 vitest cases across `runtime/web/__tests__/`). The
> **component** layer overlaps with unit tests today — fakes /
> in-memory storage are passed directly into struct constructors
> rather than dispatched via a separate harness. The **integration**
> (wasm-bindgen) and **E2E** (Playwright) layers from the original
> design **do not exist yet** — they are deferred to v0.3+ and
> tracked in §9.

## 1. Purpose

Akiva covers a wide surface: the kernel, the framework, the SDK,
sample apps, and the JS HAL — plus the ABI contract between them.
Testing decisions made early save weeks of pain across the roadmap.

## 2. Layers (target shape)

```
┌────────────────────────────────────────────────────┐
│ E2E (Playwright)                                    │  ← full boot, real browser  (deferred)
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

## 7. E2E tests (Playwright) — deferred

**Goal.** Validate full boot, real interaction, and multi-tab flows
through a real browser.

**Status.** Not in tree. The hand-test today is "open the dev
server, click around the bundled apps." That is fine for the
present cadence; E2E will land once shipping cadence requires
regression coverage that matches the manual test surface.

Planned shape:

```
tests/e2e/
├── playwright.config.ts
├── boot.spec.ts
├── multi_tab.spec.ts
└── calculator.spec.ts
```

Example sketch:

```typescript
test("calculator adds 2 + 3 = 5", async ({ page }) => {
    await page.goto("http://localhost:4173/");
    await page.locator('[data-testid="app-icon-calculator"]').click();
    await page.locator('[data-akiva-widget-tag="2"]').click();
    await page.locator('[data-akiva-widget-tag="plus"]').click();
    await page.locator('[data-akiva-widget-tag="3"]').click();
    await page.locator('[data-akiva-widget-tag="equals"]').click();
    await expect(page.locator('[data-akiva-widget-id="display"]'))
        .toHaveText("5");
});
```

**Hit-testing.** Since Akiva renders to canvas, not the DOM, the Tab
HAL will need to maintain an invisible overlay of `data-*` elements
keyed by widget id/tag for tests to target. The overlay is gated
behind a build flag — disabled in production, enabled in dev / CI.
That overlay is also the foundation for accessibility tooling
(screen-reader bridge), so the work has long-term value beyond E2E.

## 8. CI

Today, the only enforced job is local: `cargo test --workspace` +
`vitest run` + a manual `cargo build --target
wasm32-unknown-unknown --release`. The full pipeline below is the
target, not the current state:

1. `cargo fmt --check && cargo clippy -D warnings`
2. `cargo test --workspace` (native)
3. `wasm-pack test --headless --chrome kernel/` *(deferred)*
4. `cargo build --target wasm32-unknown-unknown --release` (kernel +
   every app crate)
5. `wasm-opt -O3` on every built wasm
6. `npx vitest run`
7. `playwright test` *(deferred — needs the E2E surface)*

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

All of these are v0.3+ work. The MVP coverage today is unit + JS
unit + manual E2E of `boot + hello + calculator + tic-tac-toe +
network`.

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
- [ ] E2E: `boot → Shell → open calculator → 2+3=5 → back → Shell`.
      **Pending — see §7.**
- [ ] E2E: 2 tabs, both show Shell, calc opens in tab 1, tab 2
      stays on Shell, close tab 1, tab 2 promotes to leader, calc
      state preserved. **Pending — same.**
