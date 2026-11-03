// Renders every app's launcher icon (`apps/<app>/res/drawable/icon.png`)
// and its staged copy under `runtime/web/dev-apps/`. One style for all:
// a rounded tile in a muted per-app hue, with a white Material Symbols
// Rounded glyph (Apache 2.0). Needs network access for the font.
//
//     node tools/app-icons/render.mjs
//
// To add an app or change an icon, edit ICONS and re-run.
import { chromium } from "@playwright/test";
import { copyFileSync, mkdirSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

// [app dir, Material Symbols glyph, hue in degrees]
const ICONS = [
  ["articles", "article", 210],
  ["bouncer", "sports_basketball", 20],
  ["calculator", "calculate", 35],
  ["carousel", "view_carousel", 265],
  ["clip-shapes", "interests", 320],
  ["feed", "dynamic_feed", 190],
  ["hello", "waving_hand", 45],
  ["infinite-loop-app", "all_inclusive", 0],
  ["network", "language", 170],
  ["pixelfeed", "photo_library", 290],
  ["tic-tac-toe", "grid_3x3", 140],
];
const SIZE = 96;

const root = join(dirname(fileURLToPath(import.meta.url)), "..", "..");
const html = (glyph, hue) => `<!doctype html><html><head>
<link rel="stylesheet" href="https://fonts.googleapis.com/css2?family=Material+Symbols+Rounded:opsz,wght,FILL,GRAD@48,500,1,0&display=block">
<style>html,body{margin:0;background:transparent}
.t{width:${SIZE}px;height:${SIZE}px;border-radius:${SIZE * 0.24}px;background:hsl(${hue} 42% 46%);display:flex;align-items:center;justify-content:center}
.g{font-family:'Material Symbols Rounded';font-size:${SIZE * 0.58}px;color:#fff;font-variation-settings:'FILL' 1,'wght' 500,'opsz' 48;line-height:1}
</style></head><body><div class="t"><span class="g">${glyph}</span></div></body></html>`;

const browser = await chromium.launch();
const page = await browser.newPage({ viewport: { width: SIZE, height: SIZE } });
for (const [app, glyph, hue] of ICONS) {
  await page.setContent(html(glyph, hue), { waitUntil: "networkidle" });
  await page.evaluate(() => document.fonts.ready);
  const out = join(root, "apps", app, "res", "drawable", "icon.png");
  mkdirSync(dirname(out), { recursive: true });
  await page.screenshot({ path: out, omitBackground: true });
  const staged = join(root, "runtime", "web", "dev-apps", app, "res", "drawable", "icon.png");
  mkdirSync(dirname(staged), { recursive: true });
  copyFileSync(out, staged);
  console.log(`${app}: ${glyph}, hue ${hue}`);
}
await browser.close();
