// Open Akiva in a real Chromium window with the inspector enabled and
// hold until the user closes the browser. Companion to the headless
// test suite: when you want to *see* the platform, run
//
//     npm run dev:open
//
// Assumes `cargo run -p akiva -- run` is already serving on
// 127.0.0.1:4173 (start it in another terminal). The script doesn't
// auto-launch the dev server — we want one obvious place where the
// server lives so a stale process can be tracked down quickly.
//
// Why this and not `playwright open <url>`? Two reasons:
//   1. We want predictable defaults — viewport, scale, inspector flag —
//      identical to what the headless tests use, so what you see is
//      what they see.
//   2. We inject a debug overlay canvas that visualises the inspector's
//      live semantic tree (boxes around buttons, labels). This makes
//      the "why did the test click here?" question answerable at a
//      glance.

import { chromium } from "@playwright/test";

const PORT = Number(process.env.AKIVA_E2E_PORT ?? 4173);
const URL = process.env.AKIVA_OPEN_URL ?? `http://127.0.0.1:${PORT}/?inspector=1`;

// Single absolutely-positioned <canvas> that paints rectangles + ids
// for every node in `window.__akiva.snapshot()`. One DOM element
// total — not one per widget — so this stays a debug-time view, not
// the kind of DOM mirror the OS deliberately avoids.
const inspectorViz = `
(function () {
  function start() {
    if (!window.__akiva) { setTimeout(start, 100); return; }
    const surf = document.getElementById("akiva-surface");
    if (!surf) { setTimeout(start, 100); return; }

    const c = document.createElement("canvas");
    Object.assign(c.style, {
      position: "fixed", inset: "0",
      width: "100vw", height: "100vh",
      pointerEvents: "none", zIndex: "2147483646",
    });
    document.body.appendChild(c);

    function draw() {
      const r = surf.getBoundingClientRect();
      c.width = Math.floor(r.width);
      c.height = Math.floor(r.height);
      const ctx = c.getContext("2d");
      ctx.clearRect(0, 0, c.width, c.height);
      const snap = window.__akiva.snapshot();
      for (const a of snap) {
        for (const n of a.nodes) {
          // colour by role
          let stroke = "rgba(120,220,120,0.5)", fill = null;
          if (n.role === "button") { stroke = "rgba(80,200,255,0.9)"; fill = "rgba(80,200,255,0.08)"; }
          else if (n.role === "text") { stroke = "rgba(255,200,80,0.7)"; }
          else if (n.role === "image") { stroke = "rgba(220,120,255,0.7)"; }
          else if (n.role === "rect" || n.role === "spacer") { continue; }
          ctx.lineWidth = 1;
          ctx.strokeStyle = stroke;
          if (fill) { ctx.fillStyle = fill; ctx.fillRect(n.x, n.y, n.w, n.h); }
          ctx.strokeRect(n.x + 0.5, n.y + 0.5, n.w - 1, n.h - 1);
          // tag with id or label
          const tag = n.id ?? n.label ?? "";
          if (tag) {
            ctx.font = "10px ui-monospace, monospace";
            ctx.fillStyle = "rgba(0,0,0,0.85)";
            ctx.fillRect(n.x + 2, n.y + 2, ctx.measureText(tag).width + 6, 14);
            ctx.fillStyle = "rgba(255,255,255,0.95)";
            ctx.fillText(tag, n.x + 5, n.y + 12);
          }
        }
      }
      requestAnimationFrame(draw);
    }
    requestAnimationFrame(draw);

    // Floating legend.
    const legend = document.createElement("div");
    legend.textContent =
      "Akiva inspector — blue: buttons · yellow: text · green: groups · pink: images" +
      "  ·  open DevTools and call window.__akiva.snapshot()";
    Object.assign(legend.style, {
      position: "fixed", right: "12px", bottom: "12px",
      background: "rgba(0,0,0,0.75)", color: "white",
      font: "12px/1.4 ui-monospace, monospace",
      padding: "6px 10px", borderRadius: "6px",
      zIndex: "2147483647", pointerEvents: "none",
    });
    document.body.appendChild(legend);
  }
  if (document.body) start();
  else document.addEventListener("DOMContentLoaded", start);
})();
`;

const browser = await chromium.launch({
  headless: false,
  args: ["--auto-open-devtools-for-tabs"],   // open DevTools so the user can poke
});
const context = await browser.newContext({
  viewport: { width: 1280, height: 800 },
  deviceScaleFactor: 1,
});

const page = await context.newPage();
// `addInitScript` runs before the page's own scripts on each
// navigation, so the visualiser starts polling the moment
// `window.__akiva` shows up.
await page.addInitScript({ content: inspectorViz });

// Useful console banner — prints once after window.__akiva mounts.
await page.addInitScript({ content: `
  (function () {
    function announce() {
      if (!window.__akiva) { setTimeout(announce, 100); return; }
      console.log("%c[akiva] inspector ready", "color:#5cf;font-weight:bold");
      console.log("Try in DevTools:");
      console.log("  __akiva.snapshot()                 // every active activity + nodes");
      console.log("  __akiva.list()                     // packageId/handle directory");
      console.log("  __akiva.findById('d7')             // calculator's '7' button");
      console.log("  __akiva.findByRole('button', { name: 'x' })");
      console.log("  __akiva.findByText('56')           // result display");
    }
    announce();
  })();
`});

await page.goto(URL);

console.log(`[akiva] opened ${URL}`);
console.log("[akiva] DevTools auto-opened — call window.__akiva.snapshot() to inspect");
console.log("[akiva] visual overlay shows rect bounds the inspector reports");
console.log("[akiva] close the browser window to exit");

await new Promise((resolve) => {
  let done = false;
  const finish = () => { if (!done) { done = true; resolve(undefined); } };
  page.on("close", finish);
  browser.on("disconnected", finish);
});
await browser.close().catch(() => {});
process.exit(0);
