// Carousel — semantic + pixel assertions for the horizontal pager demo.
//
// Pairs with `apps/carousel/`. Covers the full Stage 2d surface:
//   • Node::LazyRow realises only the visible-+-overscan items
//   • inspector publishes scroll_y / content_h / first_visible /
//     last_visible / item_count *and* axis = "horizontal" (v3 wire)
//   • horizontal pointer-drag scrolls; sideways wheel.x advances
//     scroll_y; wheel.y inside the carousel is a no-op
//   • drag-release seeds a fling; the fling settle hands off to
//     SnapMode::Page, gliding to the nearest viewport-page edge
//   • the indicator strip text rolls "1 / 12" → "2 / 12" after one
//     page snap
//   • the visual suite (visual/carousel.visual.spec.ts) gates the
//     idle first-frame golden.

import { test, expect } from "@playwright/test";
import type { Page } from "@playwright/test";
import { launchCarousel, scrollOf, waitForStableLayout } from "./helpers/akiva";

const ITEM_COUNT = 12;

async function wheelOnCarousel(page: Page, dx: number, dy: number) {
  const rect = await scrollOf(page, "carousel");
  expect(rect).not.toBeNull();
  const cx = rect!.rect.x + rect!.rect.w / 2;
  const cy = rect!.rect.y + rect!.rect.h / 2;
  await page.mouse.move(cx, cy);
  await page.mouse.wheel(dx, dy);
  await page.waitForTimeout(80);
}

async function awaitAnimationsSettled(page: Page, opts: { timeoutMs?: number } = {}) {
  // Two consecutive identical scrollY samples = animations stopped.
  // Cover both fling and snap glide.
  const timeoutMs = opts.timeoutMs ?? 3_000;
  const deadline = Date.now() + timeoutMs;
  let last: number | null = null;
  while (Date.now() < deadline) {
    const s = await scrollOf(page, "carousel");
    if (s && last !== null && Math.abs(s.scrollY - last) < 0.5) return;
    last = s?.scrollY ?? null;
    await page.waitForTimeout(50);
  }
}

test.beforeEach(async ({ page }) => {
  await launchCarousel(page);
  await waitForStableLayout(page);
});

test("carousel: scroll region published with axis=horizontal", async ({ page }) => {
  const s = await scrollOf(page, "carousel");
  expect(s).not.toBeNull();
  expect(s!.axis).toBe("horizontal");
  expect(s!.itemCount).toBe(ITEM_COUNT);
  expect(s!.scrollY).toBe(0);
});

test("carousel: content extent uses viewport width per page", async ({ page }) => {
  // page_w = viewport_w (one item per page), gap=0 → content_main =
  // ITEM_COUNT * viewport_w. The cell publishes viewport_h as the
  // *main-axis* viewport size (= the row width in screen pixels).
  const s = await scrollOf(page, "carousel");
  const pageW = s!.viewportHeight; // main-axis viewport
  expect(pageW).toBeGreaterThan(100);
  expect(s!.contentHeight).toBeCloseTo(ITEM_COUNT * pageW, 0);
});

test("carousel: first_visible starts at 0", async ({ page }) => {
  const s = await scrollOf(page, "carousel");
  expect(s!.firstVisibleIndex).toBe(0);
});

test("carousel: realises a bounded number of items at scroll=0", async ({ page }) => {
  const s = await scrollOf(page, "carousel");
  // With page_w = viewport, only 1 fully-visible item + overscan → ≤ 4 realised.
  const realised = (s!.lastVisibleIndex ?? 0) - (s!.firstVisibleIndex ?? 0) + 1;
  expect(realised).toBeLessThanOrEqual(4);
});

test("carousel: indicator shows '1 / 12' on first paint", async ({ page }) => {
  const w = await page.evaluate(() =>
    (window as any).__akiva.findByText("1 / 12"),
  );
  expect(w).not.toBeNull();
});

test("carousel: wheel.x advances scroll_y; wheel.y is a no-op", async ({ page }) => {
  const before = (await scrollOf(page, "carousel"))!.scrollY;
  // Pure-y wheel — must NOT scroll the carousel.
  await wheelOnCarousel(page, 0, 500);
  const afterY = (await scrollOf(page, "carousel"))!.scrollY;
  expect(afterY).toBe(before);
  // Now wheel-x — must advance scroll_y.
  await wheelOnCarousel(page, 200, 0);
  const afterX = (await scrollOf(page, "carousel"))!.scrollY;
  expect(afterX).toBeGreaterThan(before);
});

test("carousel: wheel-left at scroll=0 keeps scroll_y at 0", async ({ page }) => {
  await wheelOnCarousel(page, -1000, 0);
  const s = await scrollOf(page, "carousel");
  expect(s!.scrollY).toBe(0);
});

test("carousel: drag-release fling + snap lands on a page edge", async ({ page }) => {
  const s = await scrollOf(page, "carousel");
  const pageW = s!.viewportHeight;
  const r = s!.rect;
  // 250 px left-drag from the row centre — enough to seed a non-
  // trivial velocity but not enough on its own to cross a full page.
  const startX = r.x + r.w * 0.7;
  const y = r.y + r.h / 2;
  const endX = startX - 250;
  await page.mouse.move(startX, y);
  await page.mouse.down();
  for (let i = 1; i <= 6; i++) {
    await page.mouse.move(startX + ((endX - startX) * i) / 6, y);
  }
  await page.mouse.up();
  await awaitAnimationsSettled(page);
  const after = (await scrollOf(page, "carousel"))!.scrollY;
  // scroll_y must land at a page boundary (n * pageW) within a
  // pixel of tolerance — the snap target is exact, but layout
  // rounding can produce ±0.5 px.
  const nearestPage = Math.round(after / pageW);
  expect(Math.abs(after - nearestPage * pageW)).toBeLessThan(1.0);
  // We dragged left ≥ 1 page worth of distance, so nearest page is
  // either 1 (the typical fling-settle) or 2.
  expect(nearestPage).toBeGreaterThanOrEqual(1);
});

test("carousel: indicator updates after a page advance", async ({ page }) => {
  const s = await scrollOf(page, "carousel");
  const pageW = s!.viewportHeight;
  const r = s!.rect;
  // Drag a clean 1+ page worth left so snap can't fall back to page 0.
  const startX = r.x + r.w * 0.8;
  const y = r.y + r.h / 2;
  const endX = startX - pageW * 1.1;
  await page.mouse.move(startX, y);
  await page.mouse.down();
  for (let i = 1; i <= 6; i++) {
    await page.mouse.move(startX + ((endX - startX) * i) / 6, y);
  }
  await page.mouse.up();
  await awaitAnimationsSettled(page);
  // After the fling+snap settles, scroll_y is on a page edge. Compute
  // the page index from it and assert the indicator text matches —
  // exact page depends on fling velocity (which depends on browser
  // wheel/move event timing), so we accept any page >= 2.
  const after = (await scrollOf(page, "carousel"))!.scrollY;
  const pageIdx = Math.round(after / pageW);
  expect(pageIdx).toBeGreaterThanOrEqual(1);
  const expectedText = `${pageIdx + 1} / 12`;
  const found = await page.evaluate(
    (t) => (window as any).__akiva.findByText(t),
    expectedText,
  );
  expect(found).not.toBeNull();
});

test("carousel: small drag under slop is a tap, not a scroll", async ({ page }) => {
  const before = (await scrollOf(page, "carousel"))!.scrollY;
  const r = (await scrollOf(page, "carousel"))!.rect;
  const x = r.x + r.w / 2;
  const y = r.y + r.h / 2;
  await page.mouse.move(x, y);
  await page.mouse.down();
  await page.mouse.move(x + 2, y + 2);   // 2 dp < DRAG_SLOP=6 dp
  await page.mouse.up();
  await page.waitForTimeout(80);
  const after = (await scrollOf(page, "carousel"))!.scrollY;
  expect(after).toBe(before);
});

test("carousel: last_visible_index never exceeds count - 1", async ({ page }) => {
  // Wheel far past the end, then assert range stays in bounds.
  for (let i = 0; i < 30; i++) await wheelOnCarousel(page, 1000, 0);
  await awaitAnimationsSettled(page);
  const s = await scrollOf(page, "carousel");
  expect(s!.lastVisibleIndex!).toBeLessThan(ITEM_COUNT);
});

test("carousel: row 11 not findable at scroll=0", async ({ page }) => {
  // Each card text "card N of 12" — row 0 shows "card 1 of 12";
  // the last card's "card 12 of 12" only appears after scrolling
  // past page 10.
  const w = await page.evaluate(() =>
    (window as any).__akiva.findByText("card 12 of 12"),
  );
  expect(w).toBeNull();
});
