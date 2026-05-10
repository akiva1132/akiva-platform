// Clip-Shapes — visual regression for the rounded-clip primitive.
//
// Pairs with `apps/clip-shapes/`. The semantic suite next to this
// (../clip-shapes.spec.ts) verifies the activity launches and
// publishes the right text nodes; this suite verifies the rounded
// clip path actually clips — circles look round, pills don't have
// square corners, the card's stripe accent stops at the border
// radius.
//
// A regression that wires `push_round_clip` to a sharp-rect clip
// (radius dropped on the floor) leaves the semantic tree intact and
// the visual frame visibly square. Goldens catch it.

import { test, expect } from "@playwright/test";
import { launchClipShapes, waitForStableLayout } from "../helpers/akiva";

test("clip-shapes: idle frame matches golden", async ({ page }) => {
  await launchClipShapes(page);
  await waitForStableLayout(page);
  await expect(page).toHaveScreenshot("clip-shapes-idle.png");
});
