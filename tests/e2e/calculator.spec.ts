// Calculator — semantic-first end-to-end test.
//
// This is the headline test for the entire test infrastructure: it
// proves that a canvas-rendered Rust app, paint-only across the ABI,
// can be driven and asserted on with a pure-JS query API. Every
// interaction here goes through `window.__akiva` (the inspector
// installed by tab.js when `?inspector=1`); no DOM mirror, no
// coordinate math inside the calculator. The only coordinate click
// is in `launchCalculator`, and it disappears once the Shell
// migrates to SDK widgets.
//
// The arithmetic is the trivial part. The story is the testing shape:
// when the calculator renames a button id, AKML is the only place
// that changes, and this test points at the new id.

import { test, expect } from "@playwright/test";
import { launchCalculator, tapId, tapRole, waitForText } from "./helpers/akiva";

test("calculator: 2 + 3 = 5 via semantic queries", async ({ page }) => {
  await launchCalculator(page);

  await tapId(page, "d2");
  await tapRole(page, "button", "+");
  await tapId(page, "d3");
  await tapRole(page, "button", "=");

  // The display Text widget has no id — `findByText` resolves it by
  // its visible content, the way Flutter Web's a11y projection makes
  // canvas text findable.
  await waitForText(page, "5");
});

test("calculator: 7 × 8 = 56", async ({ page }) => {
  await launchCalculator(page);

  await tapId(page, "d7");
  await tapRole(page, "button", "×");
  await tapId(page, "d8");
  await tapRole(page, "button", "=");

  await waitForText(page, "56");
});

test("calculator: every digit is published with role=button + label", async ({ page }) => {
  await launchCalculator(page);

  // Lock the shape, not the count: every digit's semantic node
  // carries `role="button"` and a label matching its visible text.
  // A regression in `Ui::button` that drops either fails here.
  for (let d = 0; d <= 9; d++) {
    const node = await page.evaluate(
      (id) => (window as any).__akiva.findById(id),
      `d${d}`,
    );
    expect(node, `digit d${d} should be published`).not.toBeNull();
    expect(node.role).toBe("button");
    expect(node.label).toBe(String(d));
  }
});
