// Smoke tests — Akiva boots, the inspector mounts (or doesn't), and
// the dev server is alive. Anything beyond green here is a system-level
// regression. Keep small — this is the "are we even running?" signal
// CI fires first.
//
// Note: the current Shell (`sys.shell`) renders via raw canvas
// primitives (`c.fill_rect`, `c.draw_text`) rather than the SDK's
// `Ui` builder, so it does NOT fire `PUBLISH_SEMANTICS`. The first
// activity to populate the inspector is whichever app the user
// launches (e.g. calculator). When the Shell migrates to `Ui`
// buttons, this file gets a stronger assertion: `window.__akiva.list()`
// contains an entry for `sys.shell` immediately after boot.

import { test, expect } from "@playwright/test";

test("dev server boots into a real page", async ({ page }) => {
  await page.goto("/?inspector=1");
  await expect(page.locator("#akiva-surface")).toHaveCount(1, { timeout: 5_000 });
  await expect(page.locator("#akiva-boot")).toHaveCount(1);
});

test("inspector exposes window.__akiva when ?inspector=1", async ({ page }) => {
  await page.goto("/?inspector=1");
  // `window.__akiva` is installed synchronously when tab.js sees the
  // flag — it doesn't wait for wasm boot. Polling here mostly guards
  // against load-order surprises (script-defer changes etc).
  await expect.poll(
    async () => page.evaluate(() => typeof (window as any).__akiva),
    { timeout: 5_000 },
  ).toBe("object");
  // The API surface tests downstream depend on:
  const shape = await page.evaluate(() => {
    const a = (window as any).__akiva;
    return {
      hasFindById:   typeof a.findById   === "function",
      hasFindByRole: typeof a.findByRole === "function",
      hasFindByText: typeof a.findByText === "function",
      hasSnapshot:   typeof a.snapshot   === "function",
      hasList:       typeof a.list       === "function",
    };
  });
  expect(shape).toEqual({
    hasFindById: true, hasFindByRole: true, hasFindByText: true,
    hasSnapshot: true, hasList: true,
  });
});

test("inspector is absent without ?inspector=1", async ({ page }) => {
  // Production tabs must not expose the inspector. This is the gate
  // that makes "always emit" cheap in production: AppRegistry ships
  // the bytes, the tab silently drops them.
  await page.goto("/");
  await page.waitForTimeout(2_000);
  expect(await page.evaluate(() => typeof (window as any).__akiva)).toBe("undefined");
});
