// Articles — visual regression for multi-line text (single / wrap / ellipsis).
//
// Pairs with `apps/articles/`. The semantic suite
// (../articles.spec.ts) verifies the activity launches and publishes
// the right text nodes; this suite verifies the pixels paint the
// three behaviours the demo claims:
//
//   row 1 — single line, no wrap, content overflows past the card's
//           right edge (calculator-button label baseline).
//   row 2 — `wrap()`: text reflows inside the card.
//   row 3 — `max_lines(3)`: three lines max, trailing `…`.
//
// A regression that drops `max_lines` (always wraps unbounded) or
// drops the ellipsis truncation leaves the semantic tree intact and
// the layout visibly different. The golden catches it.
import { test, expect } from "@playwright/test";
import { launchArticles, waitForStableLayout } from "../helpers/akiva";

test("articles: idle frame matches golden", async ({ page }) => {
  await launchArticles(page);
  await waitForStableLayout(page);
  await expect(page).toHaveScreenshot("articles-idle.png");
});
