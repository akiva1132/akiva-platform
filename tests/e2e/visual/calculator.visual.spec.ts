// Calculator — visual regression baselines.
//
// The semantic suite (../calculator.spec.ts) verifies "the SDK
// publishes a button with id=d7 labelled '7'"; this suite verifies
// "the pixels Akiva paints for d7 match the golden image". The two
// catch different bugs:
//
//   - A regression that drops a drop-shadow uniform from the WebGPU
//     compositor (Phase 15.4 territory) leaves the semantic tree
//     unchanged but visibly flattens the keypad. Semantic tests pass;
//     visual tests fail.
//   - A regression that renames the calculator's `d7` button to `d_7`
//     keeps the pixels identical (label is still "7") but breaks the
//     semantic tap path. Semantic tests fail; visual tests pass.
//
// Goldens live in `<spec>.visual.spec.ts-snapshots/` next to the
// spec, scoped per-platform per-browser (`chromium-linux` etc.).
// To regenerate after an intentional design change:
//
//     npm run test:e2e:update-snapshots
//
// then commit the new PNGs alongside the source change.

import { test, expect } from "@playwright/test";
import { launchCalculator, tapId, tapRole, waitForText, waitForStableLayout } from "../helpers/akiva";

test("calculator: idle state matches golden", async ({ page }) => {
  await launchCalculator(page);
  // Gate on a settled frame: `launchCalculator` returns as soon as
  // `d2` is queryable, which is the calculator's first PUBLISH_SEMANTICS
  // — drawables (the @drawable/icon PNG → ImageBitmap path) decode
  // asynchronously off-thread and may land a few frames later. Without
  // this wait the screenshot occasionally captures the magenta
  // missing-drawable placeholder instead of the orange icon.
  await waitForStableLayout(page);
  await expect(page).toHaveScreenshot("calc-idle.png");
});

test("calculator: 7 × 8 = 56 result frame matches golden", async ({ page }) => {
  await launchCalculator(page);
  await tapId(page, "d7");
  await tapRole(page, "button", "×");
  await tapId(page, "d8");
  await tapRole(page, "button", "=");
  await waitForText(page, "56");
  await waitForStableLayout(page);
  await expect(page).toHaveScreenshot("calc-7x8-equals-56.png");
});
