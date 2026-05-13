// Pixelfeed — visual regression for the integration-test feed.
//
// Pairs with `apps/pixelfeed/` and `../pixelfeed.spec.ts`. Gates two
// pixel-perfect goldens:
//
//   pixelfeed-idle.png       — first paint at rest. Title + subtitle
//                              on top, two centred cards stacked
//                              vertically, each with header + 400×400
//                              photo + action row + caption.
//
//   pixelfeed-post-like.png  — after one heart-button tap on card 0.
//                              Card 0's heart glyph flips ♡ → ♥; the
//                              caption flips from "Be the first to
//                              like this" to "1 like". Heart-overlay
//                              animation has settled before snapshot.
//
// Mid-flight animations (heart-overlay fade, image fade-in) are not
// snapshotted — Playwright mouse timing across browsers shifts the
// AnimatedValue position on capture. waitForStableLayout settles
// every active animation before the screenshot fires.

import { test, expect } from "@playwright/test";
import { launchPixelfeed, waitForStableLayout, waitForText } from "../helpers/akiva";

test("pixelfeed: idle frame matches golden", async ({ page }) => {
  await launchPixelfeed(page);
  await waitForStableLayout(page);
  await expect(page).toHaveScreenshot("pixelfeed-idle.png");
});

test("pixelfeed: post-like frame matches golden", async ({ page }) => {
  await launchPixelfeed(page);
  await waitForStableLayout(page);
  // Tap the heart on card 0 via inspector-resolved coords.
  const heart = await page.evaluate(() =>
    (window as any).__akiva.findById("like-0"),
  );
  expect(heart).not.toBeNull();
  await page.mouse.click(
    heart.rect.x + heart.rect.w / 2,
    heart.rect.y + heart.rect.h / 2,
  );
  await waitForText(page, "1 like", { timeoutMs: 2_000 });
  // Settle the heart-overlay spring before snapshot — otherwise the
  // golden captures whatever opacity the spring was at when the
  // text-find returned, which varies frame-to-frame.
  await waitForStableLayout(page);
  await expect(page).toHaveScreenshot("pixelfeed-post-like.png");
});
