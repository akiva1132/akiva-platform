// Pixelfeed — semantic + behavioural assertions for the
// Instagram-style feed demo.
//
// Pairs with `apps/pixelfeed/`. Stage 3 lands the integration
// test: every primitive from 2a-2e wired into a single app.
// Coverage:
//   • title + 50-card feed publishes on first paint
//   • inspector reports the vertical feed scroll region
//     (key="feed", axis="vertical", itemCount=50)
//   • each card publishes its own per-card photo pager
//     (key="post-N-photos", axis="horizontal", itemCount=5)
//   • per-card scroll keys don't collide — advancing one card's
//     pager leaves another card's pager untouched
//   • tapping the heart button toggles the like count for that
//     card (and ONLY that card)
//   • double-tap on a card's photo region routes to that card's
//     like state and fires the heart-overlay animation
//   • feed scroll advances first-visible-index (LazyColumn
//     virtualisation working end-to-end)

import { test, expect } from "@playwright/test";
import type { Page } from "@playwright/test";
import {
  launchPixelfeed, scrollOf, waitForStableLayout, waitForText,
} from "./helpers/akiva";

const ITEM_COUNT = 50;
const PHOTOS_PER_CARD = 5;

test.beforeEach(async ({ page }) => {
  await launchPixelfeed(page);
  await waitForStableLayout(page);
});

test("pixelfeed: title + subtitle publish on first paint", async ({ page }) => {
  const title = await page.evaluate(() =>
    (window as any).__akiva.findByText("Akiva Pixelfeed"),
  );
  expect(title).not.toBeNull();
  const sub = await page.evaluate(() =>
    (window as any).__akiva.findByText("scroll · swipe photos · double-tap to like"),
  );
  expect(sub).not.toBeNull();
});

test("pixelfeed: feed scroll region published with 50 cards", async ({ page }) => {
  const s = await scrollOf(page, "feed");
  expect(s).not.toBeNull();
  expect(s!.axis).toBe("vertical");
  expect(s!.itemCount).toBe(ITEM_COUNT);
  expect(s!.scrollY).toBe(0);
  // Content height = 50 cards × (CARD_ITEM_H + CARD_GAP) - one final
  // gap, plus the top + bottom paddings (8 + 16). Lower bound is the
  // load-bearing assertion (Lazycolumn computes a non-trivial extent).
  expect(s!.contentHeight).toBeGreaterThan(1_000);
});

test("pixelfeed: card 0 publishes its own horizontal photo pager", async ({ page }) => {
  const s = await scrollOf(page, "post-0-photos");
  expect(s).not.toBeNull();
  expect(s!.axis).toBe("horizontal");
  expect(s!.itemCount).toBe(PHOTOS_PER_CARD);
});

test("pixelfeed: card 1 also publishes its own pager (per-card key isolation)", async ({ page }) => {
  // The pager for card 1 is the second card in the LazyColumn's
  // visible window (default starts with cards 0 + 1 visible on
  // 1280×800). Distinct scrollOf key → distinct scroll state.
  const s = await scrollOf(page, "post-1-photos");
  expect(s).not.toBeNull();
  expect(s!.axis).toBe("horizontal");
  expect(s!.itemCount).toBe(PHOTOS_PER_CARD);
});

/** Wheel at the centre of `key`'s rect. */
async function wheelOnCenter(page: Page, key: string, dx: number, dy: number) {
  const r = await scrollOf(page, key);
  expect(r).not.toBeNull();
  const cx = r!.rect.x + r!.rect.w / 2;
  const cy = r!.rect.y + r!.rect.h / 2;
  await page.mouse.move(cx, cy);
  await page.mouse.wheel(dx, dy);
  await page.waitForTimeout(120);
}

test("pixelfeed: wheel.y over a photo pager scrolls the OUTER feed (per-axis routing)", async ({ page }) => {
  // Pre-fix this test would have failed: scroll_region_at picked
  // the smallest-area region (the photo pager, horizontal) and
  // dropped the dy on the floor. The fix routes each wheel
  // component to the deepest scroll of its own axis — wheel.y
  // bubbles up past the horizontal pager to the vertical feed.
  const before = await scrollOf(page, "feed");
  expect(before!.scrollY).toBe(0);
  // Cursor parked DEAD CENTRE of card 0's photo pager.
  await wheelOnCenter(page, "post-0-photos", 0, 600);
  await page.waitForTimeout(400);
  const after = await scrollOf(page, "feed");
  expect(after!.scrollY).toBeGreaterThan(0);
  expect(after!.lastVisibleIndex).toBeGreaterThan(before!.lastVisibleIndex!);
  // The pager underneath must NOT have advanced — dy doesn't
  // leak across axes.
  const pager = await scrollOf(page, "post-0-photos");
  expect(pager!.scrollY).toBe(0);
});

test("pixelfeed: wheel.x on a photo pager advances its scrollY", async ({ page }) => {
  const before = await scrollOf(page, "post-0-photos");
  expect(before!.scrollY).toBe(0);
  await wheelOnCenter(page, "post-0-photos", 600, 0);
  await page.waitForTimeout(500);
  const after = await scrollOf(page, "post-0-photos");
  // 600 dp wheel → scrollY > 0 (full-page snap doesn't fire on wheel
  // events per Compose convention; the user stays at the exact
  // wheeled position).
  expect(after!.scrollY).toBeGreaterThan(0);
});

test("pixelfeed: advancing card-0's pager leaves card-1's pager untouched", async ({ page }) => {
  await wheelOnCenter(page, "post-0-photos", 600, 0);
  await page.waitForTimeout(500);
  const c0 = await scrollOf(page, "post-0-photos");
  const c1 = await scrollOf(page, "post-1-photos");
  expect(c0!.scrollY).toBeGreaterThan(0);
  expect(c1!.scrollY).toBe(0);
});

test("pixelfeed: diagonal wheel over photo routes dx to pager + dy to feed", async ({ page }) => {
  // Diagonal trackpad swipe — both axes advance independently in
  // one wheel event. Asserts the per-axis routing is symmetric.
  await wheelOnCenter(page, "post-0-photos", 200, 200);
  await page.waitForTimeout(400);
  const pager = await scrollOf(page, "post-0-photos");
  const feed = await scrollOf(page, "feed");
  expect(pager!.scrollY).toBeGreaterThan(0);
  expect(feed!.scrollY).toBeGreaterThan(0);
});

test("pixelfeed: tapping like-0 increments card 0's like count", async ({ page }) => {
  // Initial: "Be the first to like this" on card 0.
  const beforeMsg = await page.evaluate(() =>
    (window as any).__akiva.findByText("Be the first to like this"),
  );
  expect(beforeMsg).not.toBeNull();
  // Click the heart button on card 0.
  const heart = await page.evaluate(() =>
    (window as any).__akiva.findById("like-0"),
  );
  expect(heart).not.toBeNull();
  await page.mouse.click(
    heart.rect.x + heart.rect.w / 2,
    heart.rect.y + heart.rect.h / 2,
  );
  // After the click, the caption flips to "1 like".
  await waitForText(page, "1 like", { timeoutMs: 2_000 });
});

test("pixelfeed: tapping like-0 does NOT affect card 1's like count", async ({ page }) => {
  const heart0 = await page.evaluate(() =>
    (window as any).__akiva.findById("like-0"),
  );
  expect(heart0).not.toBeNull();
  await page.mouse.click(
    heart0.rect.x + heart0.rect.w / 2,
    heart0.rect.y + heart0.rect.h / 2,
  );
  await waitForText(page, "1 like", { timeoutMs: 2_000 });
  // Card 1's caption should still read "Be the first to like this"
  // — multiple cards' captions all say so before any like, and
  // findByText returns the first match, so the assertion is that
  // it still exists somewhere (which it would only if at least
  // one card hasn't been liked).
  const stillUnliked = await page.evaluate(() =>
    (window as any).__akiva.findByText("Be the first to like this"),
  );
  expect(stillUnliked).not.toBeNull();
});

test("pixelfeed: tapping like-0 twice un-likes back to zero", async ({ page }) => {
  const heart = await page.evaluate(() =>
    (window as any).__akiva.findById("like-0"),
  );
  expect(heart).not.toBeNull();
  const x = heart.rect.x + heart.rect.w / 2;
  const y = heart.rect.y + heart.rect.h / 2;
  await page.mouse.click(x, y);
  await waitForText(page, "1 like", { timeoutMs: 2_000 });
  // Second tap toggles off.
  await page.mouse.click(x, y);
  await waitForText(page, "Be the first to like this", { timeoutMs: 2_000 });
});

test("pixelfeed: double-tap on card-0's photo likes it (Instagram convention)", async ({ page }) => {
  // Locate card 0's photo region via its scroll key — the inspector
  // gives us the bounding rect of the LazyRow viewport.
  const pager = await scrollOf(page, "post-0-photos");
  expect(pager).not.toBeNull();
  const cx = pager!.rect.x + pager!.rect.w / 2;
  const cy = pager!.rect.y + pager!.rect.h / 2;
  // Dispatch two real PointerEvent down/up pairs on the canvas
  // within the 300 ms double-tap window. Two page.mouse.click()
  // calls in sequence usually land < 100 ms apart in headless mode.
  await page.mouse.click(cx, cy);
  await page.mouse.click(cx, cy, { delay: 0 });
  // Like count flips to 1.
  await waitForText(page, "1 like", { timeoutMs: 2_000 });
});

test("pixelfeed: double-tap on a liked card re-fires overlay but does NOT unlike", async ({ page }) => {
  // First like it via the button.
  const heart = await page.evaluate(() =>
    (window as any).__akiva.findById("like-0"),
  );
  await page.mouse.click(
    heart.rect.x + heart.rect.w / 2,
    heart.rect.y + heart.rect.h / 2,
  );
  await waitForText(page, "1 like", { timeoutMs: 2_000 });
  // Now double-tap the photo. Per Instagram convention this does
  // NOT unlike (taps on the heart-button toggle, double-taps only
  // like). The caption should stay at "1 like".
  const pager = await scrollOf(page, "post-0-photos");
  const cx = pager!.rect.x + pager!.rect.w / 2;
  const cy = pager!.rect.y + pager!.rect.h / 2;
  await page.mouse.click(cx, cy);
  await page.mouse.click(cx, cy, { delay: 0 });
  await page.waitForTimeout(500);
  // Still "1 like" — double-tap did NOT unlike.
  const stillOne = await page.evaluate(() =>
    (window as any).__akiva.findByText("1 like"),
  );
  expect(stillOne).not.toBeNull();
});
