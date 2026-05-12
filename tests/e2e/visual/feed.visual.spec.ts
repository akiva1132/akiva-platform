// Feed — visual regression for the virtualised list.
//
// Pairs with `apps/feed/` and the semantic/pointer/wheel suite in
// `../feed.spec.ts`. This file gates two pixel-perfect goldens:
//
//   feed-idle.png      — first paint, scroll_y = 0. Title strip on
//                        top + the first ~5 cards rendered.
//
// Subsequent scrolled/flung goldens are intentionally deferred until
// after we have a deterministic `Ui::scroll_to(key, y)` bridge from
// JS — wheel-driven offsets land at non-deterministic positions
// across browsers (Chromium vs Firefox normalise deltaY differently).

import { test, expect } from "@playwright/test";
import { launchFeed, waitForStableLayout } from "../helpers/akiva";

test("feed: idle frame matches golden", async ({ page }) => {
  await launchFeed(page);
  await waitForStableLayout(page);
  await expect(page).toHaveScreenshot("feed-idle.png");
});
