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
  await tapAndAwait(page, x, y, w.handle);
}

/** Click the first widget with the given role + accessible name
 *  (typically the visible label). Use this for buttons that don't
 *  carry an id, like the calculator's `+`/`=` (their label is the
 *  identifier). */
export async function tapRole(page: Page, role: string, name: string) {
  const w = await findByRole(page, role, name);
  if (!w) throw new Error(`tapRole: no ${role} named "${name}" — current snapshot:\n${await snapshotDump(page)}`);
  const { x, y } = await widgetCenter(page, w);
  await tapAndAwait(page, x, y, w.handle);
}

/** Click at a viewport coord by dispatching real PointerEvents on
 *  the canvas via `page.evaluate`. We don't use `page.mouse.click`
 *  because Playwright's CDP-synthesised mouse events generate
 *  pointer events that race with the worker dispatch under load —
 *  see `docs/testing.md §10.1`. Direct `surf.dispatchEvent` runs in
 *  the page context, so each pointerdown/pointerup pair lands as a
 *  paired DOM event the canvas listener picks up synchronously
 *  (still async to the worker, but the *order* across taps is
 *  preserved by the postMessage channel). The activity-frame poll
 *  afterwards waits for the activity to publish at least one new
 *  semantic tree — confirming the dispatch round-trip completed. */
async function tapAndAwait(page: Page, x: number, y: number, _handle: number) {
  // DispatchEvent (not page.mouse.click) so the pointerdown/up pair
  // reach the canvas listener in the same task — Playwright's CDP
  // mouse machinery raced the worker postMessage queue under load.
  // The post-tap wait covers the asynchronous tail:
  //   tab → worker postMessage → kernel → Activity::on_pointer.
  // 150 ms holds even with all dev apps registered (boot cost grew
  // when clip-shapes joined the launcher). Tracked as "use a real
  // ack signal" in docs/testing.md §10.1 — the kernel-tick frame
  // counter doesn't distinguish input frames from idle ones.
  await page.evaluate(({ x, y }) => {
    const surf = document.getElementById("akiva-surface")!;
    const opts: PointerEventInit = {
      bubbles: true, cancelable: true, clientX: x, clientY: y,
      pointerType: "mouse", isPrimary: true, button: 0, buttons: 1,
    };
    surf.dispatchEvent(new PointerEvent("pointerdown", opts));
    surf.dispatchEvent(new PointerEvent("pointerup",
      { ...opts, buttons: 0 }));
  }, { x, y });
  await page.waitForTimeout(150);
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

/** Wait until the inspector reports the same rect for a given query
 *  across two consecutive samples — the activity has stopped moving
 *  (push transitions / sheet drags / spring overshoot all settle).
 *
 *  Why this helper exists. The inspector applies the framework's
 *  composite transform when it returns rects, but the transform
 *  travels via the kernel's PaintOp side-channel and the semantic
 *  tree travels via the SDK's wire op — two channels, two latencies.
 *  Mid-animation, a `findByText` lookup can return rects that match
 *  *one* channel's snapshot but not the canvas paint frame Playwright
 *  is about to screenshot. Polling for two-sample stability gates the
 *  visual + geometric tests on a frame where both channels agree. */
export async function waitForStableLayout(
  page: Page, opts: { timeoutMs?: number; stableFrames?: number } = {},
) {
  const timeoutMs = opts.timeoutMs ?? 3_000;
  const need = opts.stableFrames ?? 8;
  // Stability = every active activity's composite transform is
  // identity for `stableFrames` consecutive published frames.
  //
  // A single-frame check ("transform reads identity right now") races
  // the *initial* frame of a push transition: the framework's first
  // `SetInspectorTransform` for a fresh activity reports identity
  // before the animation kicks in, then jumps to `tx=screen_width`
  // for the slide-in. Polling in JS lands somewhere in that window
  // 1-in-3 times. Requiring 8 consecutive identity frames (≈130 ms
  // at 60 Hz) is wider than the longest transition (250 ms slide)
  // overlaps with so a settled animation passes immediately while a
  // mid-transition reading flips back to non-identity within a frame
  // and resets the streak.
  await page.evaluate(() => { (window as any).__akivaStreak = 0; });
  await page.waitForFunction(
    (need) => {
      const a = (window as any).__akiva;
      if (!a) return false;
      const list = a.list();
      if (list.length === 0) return false;
      for (const e of list) {
        const t = a.transformOf(e.handle);
        if (!t) {
          (window as any).__akivaStreak = 0;
          return false;
        }
        if (t.tx !== 0 || t.ty !== 0 || t.scale !== 1 || t.opacity !== 1) {
          (window as any).__akivaStreak = 0;
          return false;
        }
      }
      // Tied to the activity's frame counter so we count *published*
      // identity frames, not just polling cycles. A frozen activity
      // that stops painting still wins this check (its frame stays
      // pinned and the streak inches up across polls), which is what
      // we want for tests that intentionally pause the activity.
      const total = list.reduce((acc: number, e: any) => acc + e.frame, 0);
      const last = (window as any).__akivaStreakFrame ?? -1;
      if (total !== last) {
        (window as any).__akivaStreak = ((window as any).__akivaStreak || 0) + 1;
        (window as any).__akivaStreakFrame = total;
      }
      return ((window as any).__akivaStreak || 0) >= need;
    },
    need,
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

/** Constants from `apps/shell/src/lib.rs`. The Shell paints its
 *  launcher rows with raw canvas primitives (not Ui widgets), so we
 *  can't query them through the inspector — clicks here use
 *  coordinates derived from these values. The list is sorted
 *  alphabetically by package id; row indices are assigned in that
 *  order. When the Shell migrates to the SDK widget toolkit this
 *  function disappears in favour of `tapRole(page, "button", appName)`.
 *  Apps under `apps/` whose package ids in alphabetical order
 *  determine the row index — `com.akiva.calculator` (0),
 *  `com.akiva.clipshapes` (1), `com.akiva.hello` (2), etc. */
const SHELL_ROW_X = 320;
const SHELL_LIST_TOP = 120;
const SHELL_ROW_HEIGHT = 56;
const SHELL_ROW_GAP = 8;

function shellRowY(rowIndex: number): number {
  return SHELL_LIST_TOP + (SHELL_ROW_HEIGHT + SHELL_ROW_GAP) * rowIndex + SHELL_ROW_HEIGHT / 2;
}

/** Click the Shell's launcher row at `rowIndex` (alphabetical). The
 *  caller must have already booted Akiva — `bootAkiva` is the
 *  natural prerequisite. Returns when the click has been dispatched;
 *  follow with a `waitForId`/`waitForText` to gate on the launched
 *  activity's first paint. */
async function clickShellLauncherRow(page: Page, rowIndex: number) {
  await page.locator("#akiva-surface").click({
    position: { x: SHELL_ROW_X, y: shellRowY(rowIndex) }, force: true,
  });
}

/** Boot Akiva, then click the clip-shapes launcher row. Returns once
 *  the demo's first frame has published — gated on the title text
 *  showing up in the inspector. Row index 1 because clip-shapes sits
 *  alphabetically after calculator. */
export async function launchClipShapes(page: Page, opts: { timeoutMs?: number } = {}) {
  const timeoutMs = opts.timeoutMs ?? 20_000;
  await bootAkiva(page, { timeoutMs });
  await clickShellLauncherRow(page, 1);
  await waitForText(page, "Clip Shapes", { timeoutMs });
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
  await clickShellLauncherRow(page, 0);
  // First-paint signal — d2 is a digit button the calculator publishes
  // on its very first frame. The inspector applies the framework's
  // composite transform (translate / scale / opacity) to every rect
  // it returns, so taps land on the visually-correct pixel even
  // mid-animation — no need to wait for the push transition to settle.
  await waitForId(page, "d2", { timeoutMs });
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
