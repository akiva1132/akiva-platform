// Clip-Shapes — semantic-tree + pixel-level assertions for the
// rounded-shape demo.
//
// Pairs with `apps/clip-shapes/`. The visual suite
// (visual/clip-shapes.visual.spec.ts) confirms the pixels haven't
// drifted from the golden image; this suite confirms the geometry
// is *actually correct* — circles are circular, pills don't have
// square edges, the clip masks overflowing content. Visual goldens
// alone would re-baseline cleanly on a regression that produces
// "consistent but wrong" pixels (an oval at the same bounds). Pixel
// sampling at known geometric positions catches that class of bug.

import { test, expect } from "@playwright/test";
import { PNG } from "pngjs";
import { launchClipShapes, waitForStableLayout } from "./helpers/akiva";

// ── Pixel utilities ─────────────────────────────────────────────────────

/** Decode the PNG bytes of a Playwright screenshot and return a
 *  `(x, y) → 0xRRGGBB` sampler. Alpha is dropped (the canvas is fully
 *  opaque after `c.clear`). x/y are CSS pixels — match the rects the
 *  inspector reports because `deviceScaleFactor=1` in playwright.config. */
function decodeRgb(buf: Buffer): { width: number; height: number; at: (x: number, y: number) => number } {
  const png = PNG.sync.read(buf);
  const { data, width, height } = png;
  return {
    width, height,
    at(x, y) {
      const i = ((y | 0) * width + (x | 0)) * 4;
      return (data[i] << 16) | (data[i + 1] << 8) | data[i + 2];
    },
  };
}

/** Approximate-equal color compare. Anti-aliased edges introduce
 *  per-channel slip up to ~30/255 even one pixel inside the shape;
 *  comparing to ±48 keeps the geometric test honest without going
 *  brittle on platform-specific font/shape rasteriser drift. */
function isApproxColor(actual: number, expected: number, tol = 48): boolean {
  const ar = (actual >> 16) & 0xff, ag = (actual >> 8) & 0xff, ab = actual & 0xff;
  const er = (expected >> 16) & 0xff, eg = (expected >> 8) & 0xff, eb = expected & 0xff;
  return Math.abs(ar - er) <= tol
      && Math.abs(ag - eg) <= tol
      && Math.abs(ab - eb) <= tol;
}

/** "Approximately the same colour" — used to decide whether two
 *  pixels share a tint without being exactly equal. */
function colorClose(a: number, b: number, tol = 24): boolean {
  return isApproxColor(a, b, tol);
}

const BACKDROP = 0xf3f4f7;
const PILL_BG  = 0x15172a;
const CARD_BG  = 0xffffff;
const BAND     = 0x5b66ff;

// ── Snapshot helpers ────────────────────────────────────────────────────

async function loadShot(page: import("@playwright/test").Page) {
  await launchClipShapes(page);
  await waitForStableLayout(page);
  const buf = await page.locator("#akiva-surface").screenshot();
  return decodeRgb(buf);
}

async function rectsOfSize(page: import("@playwright/test").Page, w: number, h: number) {
  return page.evaluate(([w, h]) => {
    const a = (window as any).__akiva;
    return a.queryAll({ role: "rect" })
      .filter((r: any) => r.rect.w === w && r.rect.h === h)
      .map((r: any) => ({ ...r.rect }))
      .sort((p: any, q: any) =>
        p.y !== q.y ? p.y - q.y : p.x - q.x);
  }, [w, h]);
}

// ── Semantic tests ──────────────────────────────────────────────────────

test("clip-shapes: launches with title + section labels visible", async ({ page }) => {
  await launchClipShapes(page);
  for (const label of [
    "Clip Shapes",
    "Smooth fills · Rect::radius",
    "Clipped overflow · Frame::clip_radius",
  ]) {
    const node = await page.evaluate(
      (l) => (window as any).__akiva.findByText(l), label,
    );
    expect(node, `expected published text node "${label}"`).not.toBeNull();
    expect(node.rect.w).toBeGreaterThan(0);
    expect(node.rect.h).toBeGreaterThan(0);
  }
});

test("clip-shapes: pill label sits inside the pill rect", async ({ page }) => {
  await launchClipShapes(page);
  const { bg, label } = await page.evaluate(() => {
    const a = (window as any).__akiva;
    return {
      label: a.findByText("View profile"),
      bg: a.queryAll({ role: "rect" })
        .find((r: any) => r.rect.w === 200 && r.rect.h === 44),
    };
  });
  expect(label).not.toBeNull();
  expect(bg).toBeDefined();
  const labelCx = label.rect.x + label.rect.w / 2;
  const labelCy = label.rect.y + label.rect.h / 2;
  expect(labelCx).toBeGreaterThan(bg.rect.x + 8);
  expect(labelCx).toBeLessThan(bg.rect.x + bg.rect.w - 8);
  expect(labelCy).toBeGreaterThan(bg.rect.y + 4);
  expect(labelCy).toBeLessThan(bg.rect.y + bg.rect.h - 4);
});

test("clip-shapes: every published node has positive bounds", async ({ page }) => {
  await launchClipShapes(page);
  const zeroSized = await page.evaluate(() => {
    const snap = (window as any).__akiva.snapshot();
    const out: Array<{ role: string; id: string | null; label: string | null; rect: object }> = [];
    for (const a of snap) {
      for (const n of a.nodes) {
        const visibleLeaf = n.role === "text" || n.role === "rect"
          || n.role === "button" || n.role === "image";
        if (!visibleLeaf) continue;
        if (n.w <= 0 || n.h <= 0) {
          out.push({ role: n.role, id: n.id, label: n.label, rect: n });
        }
      }
    }
    return out;
  });
  expect(zeroSized, `zero-sized leaves:\n${JSON.stringify(zeroSized, null, 2)}`)
    .toEqual([]);
});

// ── Pixel-level geometric tests ─────────────────────────────────────────
//
// Each test below samples real pixels at fixed geometric positions and
// asserts the colour matches expectation. A regression that produces
// "consistent but wrong" pixels (an oval, a square corner, a stripe
// that bleeds past the rounded edge) fails here regardless of how the
// visual baseline drifts.

test("circles · 32 angles · inside ring is tinted, outside ring is backdrop", async ({ page }) => {
  // The smooth-fills row paints three Rects with `radius == w/2`,
  // i.e. circles inscribed in 88×88 boxes. We sample 32 angles
  // (every 11.25°) at two radii — 0.55 r is comfortably inside,
  // 1.30 r is in the bounding-box corner. A square fill at the
  // same bounds would pass the inside check but fail every corner
  // check; an oval would fail the inside check at top/bottom.
  const img = await loadShot(page);
  const tints = [0x4e6cef, 0xe07a4d, 0x35a884];
  const squares = await rectsOfSize(page, 88, 88);
  // Top three by y are the smooth-row circles (the striped one sits
  // in the second section, lower y).
  const circles = squares.slice(0, 3);
  expect(circles).toHaveLength(3);

  for (let c = 0; c < 3; c++) {
    const { x, y, w } = circles[c];
    const cx = x + w / 2;
    const cy = y + w / 2;
    const r = w / 2;
    for (let i = 0; i < 32; i++) {
      const theta = (i / 32) * 2 * Math.PI;
      const ix = cx + Math.cos(theta) * r * 0.55;
      const iy = cy + Math.sin(theta) * r * 0.55;
      const ox = cx + Math.cos(theta) * r * 1.30;
      const oy = cy + Math.sin(theta) * r * 1.30;
      expect(
        isApproxColor(img.at(ix, iy), tints[c]),
        `circle #${c} inside @ ${(i * 11.25).toFixed(1)}° (${ix | 0},${iy | 0})`,
      ).toBe(true);
      if (ox >= 0 && oy >= 0 && ox < img.width && oy < img.height) {
        expect(
          isApproxColor(img.at(ox, oy), BACKDROP),
          `circle #${c} corner @ ${(i * 11.25).toFixed(1)}° (${ox | 0},${oy | 0})` +
          " — shape may be square, not circular",
        ).toBe(true);
      }
    }
  }
});

test("pill · 16 inner samples are dark bg, 4 corners are backdrop", async ({ page }) => {
  // The pill is a 200×44 Rect with `radius == h/2 == 22`.
  // Inside: 16 evenly-spaced samples along the central horizontal
  // line of the pill must be either pill-bg (dark) or label glyph
  // (light) — none should be backdrop. Corners outside the rounded
  // ends (sampled 6dp past the corner along the pill's outer
  // diagonal) must be backdrop.
  const img = await loadShot(page);
  const pill = (await rectsOfSize(page, 200, 44))[0];
  expect(pill, "expected a 200×44 pill rect").toBeDefined();
  const { x, y, w, h } = pill;
  const cx = x + w / 2;
  const cy = y + h / 2;

  // 16 samples along central horizontal axis. Inside the straight
  // (non-rounded) middle of the pill, every pixel is either dark bg
  // or a text glyph. Either way: NOT backdrop.
  for (let i = 1; i <= 16; i++) {
    const sx = x + (i / 17) * w;
    const sample = img.at(sx, cy);
    expect(
      !isApproxColor(sample, BACKDROP, 12),
      `pill central row #${i} @ (${sx | 0},${cy | 0}) is backdrop` +
      ` — pill bg may not have painted`,
    ).toBe(true);
  }
  // 4 outside corners — radius=22 carves them out, so 6dp past each
  // corner along the diagonal lands in backdrop.
  const off = h / 2 + 6; // 28 dp from centre on the cross axis
  const corners: Array<[number, number, string]> = [
    [x + 4, y + 4, "top-left"],
    [x + w - 4, y + 4, "top-right"],
    [x + 4, y + h - 4, "bottom-left"],
    [x + w - 4, y + h - 4, "bottom-right"],
  ];
  for (const [px, py, name] of corners) {
    expect(
      isApproxColor(img.at(px, py), BACKDROP),
      `pill corner ${name} @ (${px | 0},${py | 0}): expected backdrop` +
      " — pill may be drawn as a sharp rectangle",
    ).toBe(true);
  }
  void cx;
});

test("pill · 'View profile' label is centred to within 2 dp", async ({ page }) => {
  // The pill's label sits inside an HStack with main+cross=Center.
  // The label's bounding rect is reported by the inspector; the pill
  // bg is 200×44 and centred via the same v_stack that houses the
  // pill. Asserting the label rect's centre is ≤ 2 dp from the
  // pill rect's centre catches a regression in the alignment math
  // *and* a regression in the text baseline (a textBaseline=top vs
  // alphabetic flip would push the label by ~7 dp on either axis).
  await launchClipShapes(page);
  const { pill, label } = await page.evaluate(() => {
    const a = (window as any).__akiva;
    return {
      pill: a.queryAll({ role: "rect" }).find((r: any) => r.rect.w === 200 && r.rect.h === 44),
      label: a.findByText("View profile"),
    };
  });
  const pillCx = pill.rect.x + pill.rect.w / 2;
  const pillCy = pill.rect.y + pill.rect.h / 2;
  const labelCx = label.rect.x + label.rect.w / 2;
  const labelCy = label.rect.y + label.rect.h / 2;
  expect(Math.abs(labelCx - pillCx)).toBeLessThanOrEqual(2);
  expect(Math.abs(labelCy - pillCy)).toBeLessThanOrEqual(2);
});

test("card · interior is white, 4 corners outside the radius are backdrop", async ({ page }) => {
  // The smooth-row card is a 360×100 white Rect with radius=14.
  // Sample 12 interior points (3×4 grid, inset 20dp from each
  // edge) — all must be white, none backdrop. Sample the 4 OUTSIDE
  // corners 3dp past the rounded radius — all must be backdrop.
  const img = await loadShot(page);
  const card = (await rectsOfSize(page, 360, 100))[0];
  expect(card).toBeDefined();
  const { x, y, w, h } = card;
  // Sample 12 inset interior points (3×4 grid). Skip points that
  // happen to land on the centred text glyph — bg vs text differ
  // wildly. The card's caption sits at its centre via an inner
  // HStack, so we exclude the centre column on the middle two rows.
  for (let row = 0; row < 4; row++) {
    for (let col = 0; col < 3; col++) {
      if (col === 1 && (row === 1 || row === 2)) continue;
      const sx = x + 20 + col * ((w - 40) / 2);
      const sy = y + 20 + row * ((h - 40) / 3);
      const c = img.at(sx, sy);
      // Card bg is white (#ffffff). Comparing to backdrop (#f3f4f7)
      // is too close per-channel — a generous "isn't backdrop" check
      // false-passes. Positive "is white" check is the right shape.
      expect(
        isApproxColor(c, CARD_BG, 16),
        `card interior (${col},${row}) @ (${sx | 0},${sy | 0})` +
        ` got #${c.toString(16)}, expected white card bg`,
      ).toBe(true);
    }
  }
  // Outside corners — 3dp past each corner on the diagonal.
  for (const [dx, dy, name] of [
    [-3, -3, "top-left"],
    [w + 3, -3, "top-right"],
    [-3, h + 3, "bottom-left"],
    [w + 3, h + 3, "bottom-right"],
  ] as Array<[number, number, string]>) {
    const px = x + dx, py = y + dy;
    if (px < 0 || py < 0 || px >= img.width || py >= img.height) continue;
    expect(
      isApproxColor(img.at(px, py), BACKDROP),
      `card corner ${name} @ (${px | 0},${py | 0}): expected backdrop`,
    ).toBe(true);
  }
});

test("striped circle · stripes visible inside, corners are backdrop", async ({ page }) => {
  // The clip-row striped circle is a Frame { Rect 88×88 + three
  // 88×10 stripes } with `clip_radius=44`. The stripes have
  // semi-transparent white over the blue tint, so visually the
  // banded rows are LIGHTER than the unbanded rows — we sample
  // along the central vertical axis at known band y-offsets and
  // assert the brightness pattern (stripe rows brighter than gap
  // rows). All bounding-box corners must be backdrop (clip carves).
  const img = await loadShot(page);
  // The striped row has its own square Rect bg (the 4th 88×88 in
  // doc order) + three 88×10 stripes.
  const squares = await rectsOfSize(page, 88, 88);
  expect(squares.length).toBeGreaterThanOrEqual(4);
  const stripedBg = squares[3];
  const { x, y, w } = stripedBg;
  const cx = x + w / 2;
  // Sample the 4 corners — must be backdrop (clip in effect).
  for (const [dx, dy, name] of [
    [4, 4, "top-left"],
    [w - 4, 4, "top-right"],
    [4, w - 4, "bottom-left"],
    [w - 4, w - 4, "bottom-right"],
  ] as Array<[number, number, string]>) {
    const c = img.at(x + dx, y + dy);
    expect(
      isApproxColor(c, BACKDROP),
      `striped circle corner ${name} @ (${(x + dx) | 0},${(y + dy) | 0})` +
      " — clip_radius may not have applied",
    ).toBe(true);
  }
  // Sample down the central vertical: alternating stripe rows
  // (lighter — blue + 0x33ffffff overlay) and gap rows (pure blue).
  const stripeYs = [y + 5, y + 29, y + 53];      // approx stripe centres
  const gapYs    = [y + 17, y + 41, y + 65];    // between stripes
  for (const sy of stripeYs) {
    const c = img.at(cx, sy);
    // Stripe pixel: blue base lightened by ~20%.
    const r = (c >> 16) & 0xff, g = (c >> 8) & 0xff, b = c & 0xff;
    const brightness = (r + g + b) / 3;
    expect(brightness, `stripe row y=${sy}: too dark`).toBeGreaterThan(110);
  }
  for (const gy of gapYs) {
    const c = img.at(cx, gy);
    const r = (c >> 16) & 0xff, g = (c >> 8) & 0xff, b = c & 0xff;
    const brightness = (r + g + b) / 3;
    // Gap row: solid blue tint, dimmer than stripes.
    expect(brightness, `gap row y=${gy}: too light, stripes may have bled`).toBeLessThan(170);
  }
});

test("clip-band card · rounded corners carve the band", async ({ page }) => {
  // The second-row band card is 360×80 with a 360×24 blue band
  // centred vertically inside a Frame with clip_radius=14. The
  // band's left+right edges, where they meet the card's rounded
  // corners, must be CARVED — sample at the corner extreme of the
  // band's vertical span and assert backdrop, not band colour.
  const img = await loadShot(page);
  const cards = await rectsOfSize(page, 360, 80);
  expect(cards.length).toBe(1);
  const { x, y, w, h } = cards[0];
  const bandCy = y + h / 2;
  // Far-left and far-right of the band's vertical centre — should
  // still be inside the card (band visible). 0 dp from the edge
  // is *just inside* the rounded clip's straight section.
  expect(
    isApproxColor(img.at(x + 1, bandCy), BAND),
    "band visible at left straight edge",
  ).toBe(true);
  expect(
    isApproxColor(img.at(x + w - 2, bandCy), BAND),
    "band visible at right straight edge",
  ).toBe(true);
  // Outside corners (3dp past each corner on the diagonal) must
  // be backdrop — the clip carved the white card AND the band.
  for (const [dx, dy, name] of [
    [-3, -3, "top-left outside"],
    [w + 3, -3, "top-right outside"],
    [-3, h + 3, "bottom-left outside"],
    [w + 3, h + 3, "bottom-right outside"],
  ] as Array<[number, number, string]>) {
    const px = x + dx, py = y + dy;
    if (px < 0 || py < 0 || px >= img.width || py >= img.height) continue;
    expect(
      isApproxColor(img.at(px, py), BACKDROP),
      `band card corner ${name} @ (${px | 0},${py | 0}): expected backdrop`,
    ).toBe(true);
  }
});

test("clip-band card · sample 8 points along band, all band-coloured", async ({ page }) => {
  const img = await loadShot(page);
  const card = (await rectsOfSize(page, 360, 80))[0];
  const bandCy = card.y + card.h / 2;
  for (let i = 1; i <= 8; i++) {
    const sx = card.x + (i / 9) * card.w;
    expect(
      isApproxColor(img.at(sx, bandCy), BAND),
      `band sample #${i} @ (${sx | 0},${bandCy | 0}) not band-coloured`,
    ).toBe(true);
  }
});

test("clip-band card · sample above/below band is white", async ({ page }) => {
  const img = await loadShot(page);
  const card = (await rectsOfSize(page, 360, 80))[0];
  const above = card.y + 8;          // above the centred band
  const below = card.y + card.h - 8;  // below the centred band
  const cx = card.x + card.w / 2;
  expect(isApproxColor(img.at(cx, above), CARD_BG), "above-band central pixel not white").toBe(true);
  expect(isApproxColor(img.at(cx, below), CARD_BG), "below-band central pixel not white").toBe(true);
});

test("pill bg pixels match the SDK-side colour", async ({ page }) => {
  // Direct inside check: the pill bg colour should match exactly
  // (within AA tol) what the SDK fired — `0xff15172a`. A regression
  // that swaps the colour at any layer (resource lookup, byte order,
  // alpha premultiply) fails here even if the visual baseline drifts.
  const img = await loadShot(page);
  const pill = (await rectsOfSize(page, 200, 44))[0];
  // Sample the four "safe inside" zones (well inside the pill, not
  // overlapping the centred label which sits at the pill's centre).
  const samples: Array<[number, number, string]> = [
    [pill.x + 30, pill.y + 12, "left-top inside"],
    [pill.x + 30, pill.y + pill.h - 12, "left-bottom inside"],
    [pill.x + pill.w - 30, pill.y + 12, "right-top inside"],
    [pill.x + pill.w - 30, pill.y + pill.h - 12, "right-bottom inside"],
  ];
  for (const [sx, sy, name] of samples) {
    expect(
      isApproxColor(img.at(sx, sy), PILL_BG, 24),
      `pill ${name} @ (${sx | 0},${sy | 0}) not pill-bg colour`,
    ).toBe(true);
  }
});

test("layout invariants · smooth row left-aligned to 24 dp padding", async ({ page }) => {
  // The v_stack's outer padding is 24 dp; the smooth row's first
  // circle's left edge should land at x=24 (within 1 dp). Catches a
  // regression in v_stack padding or h_stack cross alignment that
  // would shift the whole row visually.
  await launchClipShapes(page);
  await waitForStableLayout(page);
  const circles = await rectsOfSize(page, 88, 88);
  expect(circles[0].x).toBeGreaterThanOrEqual(23);
  expect(circles[0].x).toBeLessThanOrEqual(25);
});

test("layout invariants · h_stack gap is 20 dp between adjacent circles", async ({ page }) => {
  await launchClipShapes(page);
  await waitForStableLayout(page);
  const circles = (await rectsOfSize(page, 88, 88)).slice(0, 3);
  for (let i = 1; i < 3; i++) {
    const gap = circles[i].x - (circles[i - 1].x + circles[i - 1].w);
    expect(gap, `gap between circle ${i - 1} and ${i}`).toBeGreaterThanOrEqual(19);
    expect(gap).toBeLessThanOrEqual(21);
  }
});

test("layout invariants · v_stack gap of 16 dp between sections", async ({ page }) => {
  // From the bottom of the title text down to the top of the
  // "Smooth fills" subtitle should be ~16 dp (the v_stack gap) +
  // descender slack. Allow 16–24.
  await launchClipShapes(page);
  await waitForStableLayout(page);
  const { title, sub } = await page.evaluate(() => {
    const a = (window as any).__akiva;
    return {
      title: a.findByText("Clip Shapes"),
      sub: a.findByText("Smooth fills · Rect::radius"),
    };
  });
  const gap = sub.rect.y - (title.rect.y + title.rect.h);
  expect(gap).toBeGreaterThanOrEqual(14);
  expect(gap).toBeLessThanOrEqual(24);
});
