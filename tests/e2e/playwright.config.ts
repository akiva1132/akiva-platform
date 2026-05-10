// Playwright configuration for the Akiva e2e suite.
//
// Strategy: every test runs against the real dev server with the
// inspector forced on (`?inspector=1`). The runtime's tab.js mounts a
// JS-only query surface on `window.__akiva` over the SDK's semantic
// stream — no DOM mirror, no ARIA injection. Tests drive the canvas
// through `helpers/akiva.ts`, which translates `findById` / `findByRole`
// / `findByText` queries into synthesised pointer events at each
// widget's bounds. See `runtime/web/inspector.js` for the protocol.
//
// Determinism: pinned to one Chromium browser, one viewport, one
// `force-device-scale-factor=1`. Visual regression goldens (added in
// a follow-up PR) only stay green inside this fixed environment;
// running locally on macOS produces different anti-aliased pixels
// than the Linux CI image. Today the suite is pure semantic, so the
// pinning is a discipline-up-front move rather than a load-bearing
// requirement.

import { defineConfig, devices } from "@playwright/test";

const PORT = Number(process.env.AKIVA_E2E_PORT ?? 4173);
const BASE_URL = `http://127.0.0.1:${PORT}`;

export default defineConfig({
  testDir: ".",
  // Run tests serially against a single dev server instance — the
  // server multiplexes Akiva displays per tab, but every test
  // expects a clean OPFS / fresh boot. Worker isolation alone
  // doesn't give that (OPFS is origin-scoped, persists between
  // page loads). Each test that needs a clean slate clears OPFS
  // explicitly via `evaluate(() => navigator.storage.getDirectory()...)`
  // — see helpers/akiva.ts.
  fullyParallel: false,
  workers: 1,
  forbidOnly: !!process.env.CI,
  retries: process.env.CI ? 2 : 0,
  reporter: process.env.CI ? "github" : [["list"], ["html", { open: "never" }]],
  use: {
    baseURL: BASE_URL,
    // Force the inspector overlay on for every test. Tests that want
    // to verify the "off" path can override at the test level.
    extraHTTPHeaders: {},
    actionTimeout: 5_000,
    navigationTimeout: 15_000,
    // Trace the first retry of every failure so flaky tests are
    // post-mortem-able.
    trace: "on-first-retry",
    screenshot: "only-on-failure",
    video: "retain-on-failure",
    // Pin device scale so future visual goldens stay deterministic.
    viewport: { width: 1280, height: 800 },
    deviceScaleFactor: 1,
  },
  projects: [
    {
      name: "chromium",
      use: {
        ...devices["Desktop Chrome"],
        // Override the device default to match `use.viewport` /
        // `deviceScaleFactor` above; otherwise devices['Desktop Chrome']
        // applies its own DPR and the test viewport silently differs.
        viewport: { width: 1280, height: 800 },
        deviceScaleFactor: 1,
      },
    },
  ],
  webServer: {
    // `akiva run --no-build --port N` reuses already-staged wasm so we
    // don't rebuild on every `playwright test` invocation. CI calls
    // `cargo run -p akiva -- build` separately before the test step.
    // Locally, run `cargo run -p akiva -- build` once after edits;
    // re-running tests on stale wasm reuses the same artifact.
    command: `cargo run -p akiva -- run --no-build --port ${PORT}`,
    cwd: "../..",
    url: BASE_URL,
    reuseExistingServer: !process.env.CI,
    timeout: 60_000,
  },
});
