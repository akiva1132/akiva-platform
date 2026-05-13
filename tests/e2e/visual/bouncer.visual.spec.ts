// Bouncer — visual regression for the spring demo's idle frame.
//
// Pairs with `apps/bouncer/` and `../bouncer.spec.ts`. Gates one
// pixel-perfect golden:
//
//   bouncer-idle.png  — first paint at rest. Title + subtitle on
//                       top, light-grey track with a royal-blue
//                       rounded square at its centre, three
//                       preset chips below ("Bouncy", "Stiff",
//                       "Wobbly"), and a "at rest · preset:
//                       bouncy" status line at the bottom.
//
// Drag-and-release goldens are deferred until we have a way to
// step the spring deterministically from JS — Playwright mouse
// timing across browsers shifts the spring's mid-flight pixels.

import { test, expect } from "@playwright/test";
import { launchBouncer, waitForStableLayout } from "../helpers/akiva";

test("bouncer: idle frame matches golden", async ({ page }) => {
  await launchBouncer(page);
  await waitForStableLayout(page);
  await expect(page).toHaveScreenshot("bouncer-idle.png");
});
