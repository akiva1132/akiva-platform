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
import { PNG } from "pngjs";
import { launchCalculator, tapId, tapRole, waitForText, waitForStableLayout } from "../helpers/akiva";

test("calculator: idle state matches golden", async ({ page }) => {
  await launchCalculator(page);
  // Gate on a settled frame: `launchCalculator` returns as soon as
  // `d2` is queryable, which is the calculator's first PUBLISH_SEMANTICS
  // — drawables (the @drawable/icon PNG → ImageBitmap path) decode
  // asynchronously off-thread and may land a few frames later. Without
  // this wait the screenshot occasionally captures the magenta
  // missing-drawable placeholder instead of the app icon.
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

// The renderer's drawable cache is shared by every package. When each
// package numbered its drawables from 1, every app's `@drawable/icon`
// was id 1 and the calculator painted whichever app's icon decoded
// last (Loop Test's red tile). The golden alone does not catch that:
// a 24x24 tile is under `maxDiffPixels`. So sample the tile's colour
// directly: the calculator's own icon is the amber one.
test("calculator: header shows the calculator's own icon", async ({ page }) => {
  await launchCalculator(page);
  await waitForStableLayout(page);
  const png = PNG.sync.read(await page.screenshot({ clip: { x: 1170, y: 50, width: 4, height: 4 } }));
  const [r, g, b] = [png.data[0], png.data[1], png.data[2]];
  // apps/calculator/res/drawable/icon.png fill: rgb(167, 126, 68).
  expect(Math.abs(r - 167)).toBeLessThan(12);
  expect(Math.abs(g - 126)).toBeLessThan(12);
  expect(Math.abs(b - 68)).toBeLessThan(12);
});
