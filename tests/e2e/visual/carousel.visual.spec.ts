// Carousel — visual regression for the page-snap horizontal pager.
//
// Pairs with `apps/carousel/` and the semantic/snap/wheel suite in
// `../carousel.spec.ts`. Gates one pixel-perfect golden:
//
//   carousel-idle.png  — first paint, scroll = 0. Title strip on
//                        top, first card (royal-blue "Page one")
//                        centred between the side gutters, "1 / 12"
//                        indicator at the bottom.
//
// Subsequent scrolled-page goldens are intentionally deferred until
// after we have a deterministic `Ui::scroll_to(key, y)` bridge from
// JS — wheel-driven offsets land at non-deterministic positions
// across browsers.

import { test, expect } from "@playwright/test";
import { launchCarousel, waitForStableLayout } from "../helpers/akiva";

test("carousel: idle frame matches golden", async ({ page }) => {
  await launchCarousel(page);
  await waitForStableLayout(page);
  await expect(page).toHaveScreenshot("carousel-idle.png");
});
