// Shared Playwright helpers for Akiva e2e tests.
//
// Akiva renders to a single canvas; the browser's a11y tree is empty
// and Playwright's `getByRole`/`getByTestId` would find nothing. The
// runtime instead exposes `window.__akiva` (mounted only when
// `?inspector=1` is set) — an in-memory query API over the live
// semantic tree the SDK publishes each frame. Every helper here
// translates a logical query ("find the button labelled '+'") into
// `page.evaluate(() => window.__akiva.find...)` and a synthesised
// pointer event on the canvas at the widget's bounds.
//
// No DOM mirror means no fake elements to flake on, no 60 Hz rebuilds,
// and the test path stays identical to what a future native shell
// would do (read the same wire format, dispatch the same input).

import type { Page } from "@playwright/test";

/** A widget the inspector found. Mirrors `InspectorMatch` in
 *  runtime/web/inspector.js — repeated here so the helper file
 *  type-checks standalone without importing the runtime module
 *  (which is loaded by the browser, not by Node-side tests). */
export type Widget = {
  packageId: string;
  handle: number;
  role: string;
  id: string | null;
  label: string | null;
  value: string | null;
  rect: { x: number; y: number; w: number; h: number };
};

/** Centre of a widget in CSS-pixel page coordinates, ready for
 *  `page.mouse.click`. Computed by adding the canvas's bounding-rect
 *  origin to the surface-local rect from the inspector. With
 *  deviceScaleFactor=1 (pinned in playwright.config.ts) canvas
 *  physical pixels equal CSS pixels, so no DPR division is needed. */
async function widgetCenter(page: Page, w: Widget): Promise<{ x: number; y: number }> {
  return page.evaluate((w) => {
    const surf = document.getElementById("akiva-surface")!;
    const r = surf.getBoundingClientRect();
    return {
      x: r.left + w.rect.x + w.rect.w / 2,
      y: r.top  + w.rect.y + w.rect.h / 2,
    };
  }, w);
}

async function findById(page: Page, id: string): Promise<Widget | null> {
  return page.evaluate((id) => (window as any).__akiva?.findById(id) ?? null, id);
}

async function findByRole(page: Page, role: string, name?: string): Promise<Widget | null> {
  return page.evaluate(({ role, name }) =>
    (window as any).__akiva?.findByRole(role, name === undefined ? {} : { name }) ?? null,
    { role, name },
  );
}

/** Click the widget with the given stable id (set via AKML `id="..."`
 *  or `Ui::button(id, ...)`). Throws when no widget by that id has
 *  published in the current frame.
 *
 *  The click is dispatched as a real pointer event on the canvas at
 *  the widget's centre. The canvas listener in tab.js forwards it to
 *  the worker, which routes through the SDK's input pipeline — the
 *  same path a human user takes. */
export async function tapId(page: Page, id: string) {
  const w = await findById(page, id);
  if (!w) throw new Error(`tapId: no widget with id="${id}" — current snapshot:\n${await snapshotDump(page)}`);
  const { x, y } = await widgetCenter(page, w);
  await page.mouse.click(x, y);
  // Akiva's input pipeline crosses an async boundary (tab → worker
  // postMessage → kernel dispatch → Activity::on_pointer). Back-to-back
  // Playwright clicks fired with no gap can outpace the dispatcher:
  // the second click's pointerdown lands while the first is still
  // working its way through, the calc's "operator pending" flag never
  // flips, and digits concatenate instead of starting a new entry.
  // Yielding one frame after each tap is enough to let the worker
  // ack the previous event before we send the next.
  await page.waitForTimeout(50);
}

/** Click the first widget with the given role + accessible name
 *  (typically the visible label). Use this for buttons that don't
 *  carry an id, like the calculator's `+`/`=` (their label is the
 *  identifier). */
export async function tapRole(page: Page, role: string, name: string) {
  const w = await findByRole(page, role, name);
  if (!w) throw new Error(`tapRole: no ${role} named "${name}" — current snapshot:\n${await snapshotDump(page)}`);
  const { x, y } = await widgetCenter(page, w);
  await page.mouse.click(x, y);
  // See the comment in `tapId` — same async-pipeline gap.
  await page.waitForTimeout(50);
}

/** Wait until the inspector reports a widget with the given id, or
 *  the timeout elapses. Used as a "first paint" signal for
 *  freshly-launched activities. */
export async function waitForId(page: Page, id: string, opts: { timeoutMs?: number } = {}) {
  const timeoutMs = opts.timeoutMs ?? 10_000;
  await page.waitForFunction(
    (id) => !!(window as any).__akiva?.findById(id),
    id,
    { timeout: timeoutMs },
  );
}

/** Wait until any text-role widget shows the given content. The
 *  calculator's display is a Text node with no id, so this is the
 *  natural "did the result appear" assertion. */
export async function waitForText(page: Page, text: string, opts: { timeoutMs?: number; exact?: boolean } = {}) {
  const timeoutMs = opts.timeoutMs ?? 5_000;
  const exact = opts.exact ?? true;
  await page.waitForFunction(
    ({ text, exact }) => !!(window as any).__akiva?.findByText(text, { exact }),
    { text, exact },
    { timeout: timeoutMs },
  );
}

/** Stringified snapshot, used in error messages so a failing tap()
 *  shows what *was* on screen rather than just "not found". */
async function snapshotDump(page: Page): Promise<string> {
  try {
    const snap = await page.evaluate(() => (window as any).__akiva?.snapshot() ?? []);
    return JSON.stringify(snap, null, 2).slice(0, 2_000);
  } catch {
    return "(no inspector — was ?inspector=1 set?)";
  }
}

/** Navigate to Akiva with `?inspector=1` and wait for the kernel to
 *  finish booting. "Booted" here means the Worker has answered
 *  `boot_ok` to the leader tab — at which point input events flow
 *  end-to-end, the framework has resolved the HOME activity, and the
 *  first paint is on its way. tab.js flips `body.akiva-booted` at
 *  exactly that moment, so the helper waits on that class.
 *
 *  Pre-this we waited on `#akiva-boot.hidden`, but that class is
 *  added during role assignment — well before the wasm has compiled.
 *  Tests that clicked immediately after lost their events to a
 *  pre-boot worker that drops them silently. */
export async function bootAkiva(page: Page, opts: { timeoutMs?: number } = {}) {
  const timeoutMs = opts.timeoutMs ?? 20_000;
  await page.goto("/?inspector=1");
  await page.waitForSelector("body.akiva-booted", {
    state: "attached", timeout: timeoutMs,
  });
  // Confirm the inspector mounted — every other helper assumes
  // `window.__akiva` exists.
  await page.waitForFunction(() => !!(window as any).__akiva, { timeout: timeoutMs });
}

/** Boot Akiva, then click the calculator's launcher row in the Shell.
 *  Returns once the calculator's first `PUBLISH_SEMANTICS` lands —
 *  i.e. when its semantic tree shows up in the inspector and the
 *  push-transition has settled.
 *
 *  Caveat: the Shell paints its launcher rows with raw canvas
 *  primitives, not `Ui` buttons, so we can't query them through the
 *  inspector. Click coordinates are derived from the constants in
 *  `apps/shell/src/lib.rs` (`LIST_TOP=120`, `ROW_HEIGHT=56`,
 *  `ROW_GAP=8`, `ROW_PAD_X=24`). Apps are sorted by id; calculator
 *  (`com.akiva.calculator`) is row 0. When the Shell migrates to
 *  the SDK widget toolkit, this helper switches to a real
 *  `tapRole(page, "button", "Calculator")` and the coordinate magic
 *  disappears. */
export async function launchCalculator(page: Page, opts: { timeoutMs?: number } = {}) {
  const timeoutMs = opts.timeoutMs ?? 20_000;
  await bootAkiva(page, { timeoutMs });
  // Click the first launcher row's centre. With deviceScaleFactor=1
  // (pinned in playwright.config.ts) CSS pixels match canvas physical
  // pixels, so coords from the Shell's Rust-side constants apply
  // directly. y = LIST_TOP + ROW_HEIGHT/2 = 120 + 28 = 148.
  // `force: true` to bypass Playwright's "intercepted by other
  // element" check — there's no DOM overlay above the canvas any
  // more, but the actionability heuristic still occasionally trips
  // on the boot splash if the calculator launches before splash hide
  // settles.
  await page.locator("#akiva-surface").click({
    position: { x: 320, y: 148 }, force: true,
  });
  // First-paint signal — d2 is a digit button the calculator publishes
  // on its very first frame.
  await waitForId(page, "d2", { timeoutMs });
  // Push transition is 250 ms — during the slide-in the calculator's
  // surface composites with a non-zero `translate_x`, so the canvas
  // paint of d7 lives ~180 px right of where the SDK's layout (and
  // therefore the inspector's bounds) reports it. Clicks via mouse
  // would land on the canvas at the inspector's coordinates and miss
  // the visually-correct button. Waiting past the transition is a
  // temporary fix; the durable fix is for the inspector to consume
  // the surface's `SurfaceDrawOpts` (translate / scale / opacity) and
  // apply them to the rect it returns, so the cursor always lands on
  // the painted content. Tracked in the testing roadmap.
  await page.waitForTimeout(400);
}

/** Clear OPFS + localStorage on the current origin. Use in
 *  `beforeEach` for tests that depend on a fresh boot (no
 *  saved-state, no installed packages). The OPFS clear is
 *  best-effort — Chromium recently added `removeEntry({recursive:
 *  true})` but older builds lack it; the helper falls back to a
 *  manual recursive walk in that case. */
export async function clearAkivaStorage(page: Page) {
  await page.evaluate(async () => {
    try {
      const root = await navigator.storage.getDirectory();
      const tryRecursive = async () => {
        for await (const [name] of (root as any).entries()) {
          await root.removeEntry(name, { recursive: true });
        }
      };
      try {
        await tryRecursive();
      } catch {
        for await (const [name, handle] of (root as any).entries()) {
          try {
            await root.removeEntry(name, { recursive: false });
          } catch {
            void handle;
          }
        }
      }
    } catch {
      // No OPFS in this environment — nothing to clean.
    }
    try { localStorage.clear(); } catch {}
  });
}
