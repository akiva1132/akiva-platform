# Testing Strategy

## 1. מטרה

Akiva היא פרויקט עם surface גדול: Rust kernel, framework, SDK, apps, JS HAL, ו-ABI ביניהם. החלטות testing נכונות ב-Phase 1 חוסכות כאב לאורך כל ה-roadmap.

## 2. שכבות בדיקה

```
┌────────────────────────────────────────────────────┐
│ E2E (Playwright)                                    │  ← full boot, real browser
├────────────────────────────────────────────────────┤
│ Integration (wasm32 in headless browser)            │  ← kernel + framework + wit bindings
├────────────────────────────────────────────────────┤
│ Component tests (cargo test, native target)         │  ← kernel / framework modules in isolation
├────────────────────────────────────────────────────┤
│ Unit (cargo test, native target)                    │  ← pure functions, algorithms
└────────────────────────────────────────────────────┘
```

## 3. Unit tests (Rust native)

**נכתבים לצד הקוד** (`#[cfg(test)]` modules). רצים ב-`x86_64-unknown-linux-gnu` — מהיר, `cargo test`.

דוגמאות:
- AKML parser: `parse_attribute("16dp")` → `SizeValue::Dp(16.0)`
- Resource qualifier matcher: `best_match(config, candidates)`
- Intent filter matching
- Task stack operations

**חוק:** כל קוד שלא נוגע ישירות ב-wasm imports/exports ולא דורש browser — נבדק ב-unit. זה רוב ה-kernel ו-framework.

## 4. Component tests (Rust native)

**אותו פרוצס,** עם fake ל-HAL. למשל ResourceManager עם `FakeStorage` במקום OPFS.

דוגמה:
```rust
#[test]
fn resource_manager_finds_hebrew_string() {
    let storage = FakeStorage::new()
        .with_file("apps/calc/res/values/strings.toml", "[strings]\nhello=\"Hi\"")
        .with_file("apps/calc/res/values-he/strings.toml", "[strings]\nhello=\"שלום\"");
    let rm = ResourceManager::new(&storage, DeviceConfig::he_il());
    assert_eq!(rm.get_string("@string/hello"), Some("שלום"));
}
```

## 5. Integration tests (wasm32)

**מטרה:** לוודא שה-kernel עובד בתוך wasm, ושה-ABI (WIT) עובד כפי שצריך.

**הכלי:** `wasm-bindgen-test` ב-MVP, נבחן מעבר ל-`wasmtime` native test runner ברגע ש-Component Model יציב ב-wit-bindgen (נכון ל-2026 — יציב).

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

רץ דרך: `wasm-pack test --headless --chrome kernel/`.

## 6. E2E tests (Playwright)

**מטרה:** לאמת boot מלא, interaction של user, multi-tab.

**מבנה:**
```
tests/e2e/
├── playwright.config.ts
├── boot.spec.ts
├── multi_tab.spec.ts
└── calculator.spec.ts
```

דוגמה:
```typescript
test("calculator adds 2 + 3 = 5", async ({ page }) => {
    await page.goto("http://localhost:4173/");
    await page.locator('[data-testid="app-icon-calculator"]').click();
    await page.locator('[data-akiva-widget-tag="2"]').click();
    await page.locator('[data-akiva-widget-tag="plus"]').click();
    await page.locator('[data-akiva-widget-tag="3"]').click();
    await page.locator('[data-akiva-widget-tag="equals"]').click();
    await expect(page.locator('[data-akiva-widget-id="display"]')).toHaveText("5");
});
```

**עיקרון:** ה-Canvas 2D render מציג ציור ולא DOM — איך ה-tests עושים hit-test? **Akiva מוסיפה `data-*` attributes על elements של overlay DOM בלתי-נראים המיוצרים ע"י ה-Tab HAL עבור כל widget עם id/tag**. Overlay זה disabled ב-production (בחירה ב-build flag), בפיתוח/CI מופעל לצורכי testing + a11y עתידי.

## 7. CI

**GitHub Actions pipeline:**
1. `cargo fmt --check && cargo clippy -D warnings`
2. `cargo test --workspace` (native)
3. `wasm-pack test --headless --chrome kernel/`
4. `cargo build --target wasm32-unknown-unknown --release` (kernel + all apps)
5. `wasm-opt -O3` on every built wasm
6. `playwright test` (serves dev server + runs E2E)

## 8. Test fixtures

- `tests/fixtures/apps/` — app packages מיניאטוריים לבדיקה (hello, two-activity-hello, needs-network).
- `tests/fixtures/akml/` — קבצי AKML לבדיקת parser.
- `tests/fixtures/manifests/` — manifests תקינים ולא תקינים.

## 9. מה *לא* מכוסה ב-MVP

- Performance benchmarks (אין מדידה אוטומטית של frame budgets).
- Fuzz testing של AKML parser / TOML manifest.
- Visual regression (screenshot diffing) — יכול להיווסף בקלות ב-Playwright, נעשה כש-Shell יציב.
- Chaos / crash recovery automation.

כל אלו יוספו ב-v0.2+. ב-MVP: unit + integration + basic E2E של boot + hello + calculator.

## 10. כיסוי דרוש לפני MVP ship

- [ ] כל parser (AKML, manifest, resource TOML) עם unit tests.
- [ ] ResourceManager qualifier matcher עם >= 30 test cases (כולל combinations).
- [ ] ActivityManager task stack — כל launch mode + back navigation.
- [ ] WasmLoader טוען אפליקציה דמה בתוך wasm.
- [ ] E2E: boot → Shell → open calculator → 2+3=5 → back → Shell.
- [ ] E2E: 2 tabs → both show Shell → open calc in tab 1 → tab 2 still shows Shell → close tab 1 → tab 2 becomes leader → calc state preserved.
