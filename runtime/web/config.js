// Runtime tuning knobs — the JS-side equivalent of the Rust `const`
// block at the top of `kernel/src/lib.rs`. Magic numbers used to live
// inline at the call site, which made it hard to tell at a glance
// what was load-bearing (cross-worker timing) vs. what was just an
// arbitrary buffer size.
//
// Conventions:
//   * Times are in ms, sizes in bytes (`_BYTES`) or `_KIB` for buffers.
//   * Each const carries a `Why:` comment explaining the choice — the
//     value matters less than the reasoning, since dev mode may want
//     to override.
//   * Override pattern: a host page can set
//     `globalThis.__akivaConfig.watchdog = { TIMEOUT_MS: 200 }` before
//     `tab.js` boots; `effectiveWatchdog(packageId, perPackageMs?)`
//     merges the override with the defaults. Stone 11.6.7 #5 added
//     this hook so vitest can sharpen the watchdog and per-package
//     `manifest.runtime.watchdog_timeout_ms` can override per app.

/**
 * Default watchdog tunables. Read via `effectiveWatchdog()` so
 * runtime overrides (`globalThis.__akivaConfig.watchdog`) and per-
 * package manifest values can flow through one helper.
 *
 * `WARN_MS` is logged-only — the worker stays alive. `TIMEOUT_MS`
 * is terminate. Android's `ANR` model is the inspiration: warn at a
 * threshold that nudges devs to look but doesn't kill, terminate at
 * a threshold that protects the system.
 */
export const WATCHDOG = Object.freeze({
  /// Time without a `pong` reply before we declare the worker hung
  /// and `worker.terminate()` it.
  ///
  /// Why: a wasm dispatch executes on the worker's single thread, so
  /// a `loop {}` blocks the message pump until termination. 1.5s is
  /// long enough that an honest cold-start compile (~50-200ms) +
  /// first dispatch can't trip a false positive, short enough that
  /// the user sees the UI come back well within "did the OS just
  /// hang?" perception (~2s).
  TIMEOUT_MS: 1500,

  /// Time without a `pong` reply before we log a warning. Stone
  /// 11.6.7 #5 — Android calls this the "ANR warning" threshold:
  /// the worker isn't dead yet, but the response time has crossed
  /// into "user can probably tell something is off."
  ///
  /// Why 1000ms: a real frame dispatch typically settles in
  /// <50ms. Crossing 1000ms means either honest cold-start work
  /// happening (which we want a breadcrumb for) or genuine wedge
  /// approaching the 1500ms terminate. Either way, surface it.
  WARN_MS: 1000,

  /// Period between `ping` messages to each app worker.
  ///
  /// Why: 500ms gives three chances to detect a wedge inside the
  /// 1500ms timeout window — single missed pong is statistical
  /// noise, three in a row is a hung thread. Halving this would
  /// only pay extra postMessage cost without sharpening detection
  /// since termination is gated on the timeout, not the tick.
  HEARTBEAT_MS: 500,
});

/**
 * Resolve the effective watchdog config for a given heartbeat tick,
 * applying overrides in priority order:
 *
 *   1. per-package override (manifest.runtime.watchdog_timeout_ms,
 *      threaded via `AppRegistry.setWatchdogTimeoutMs(pkg, ms)`).
 *      Wins when present and finite.
 *   2. global runtime override (`globalThis.__akivaConfig.watchdog`).
 *      For dev / test runners that want a sharper or looser timer.
 *   3. defaults from `WATCHDOG` above.
 *
 * `WARN_MS` always falls back to `TIMEOUT_MS - 500` if the override
 * lifts the timeout but doesn't supply a warn — keeps the warn
 * threshold proportional rather than firing immediately. The 500ms
 * floor matches `HEARTBEAT_MS` so we don't warn before any pong
 * could have arrived.
 *
 * Returns a plain frozen object with `{TIMEOUT_MS, WARN_MS,
 * HEARTBEAT_MS}` so callers can compare against `lastPongAt` deltas
 * without re-reading globalThis on every comparison.
 */
export function effectiveWatchdog(perPackageTimeoutMs) {
  const global = (typeof globalThis !== "undefined" && globalThis.__akivaConfig?.watchdog) || {};
  const timeout = (typeof perPackageTimeoutMs === "number" && Number.isFinite(perPackageTimeoutMs) && perPackageTimeoutMs > 0)
    ? perPackageTimeoutMs
    : (typeof global.TIMEOUT_MS === "number" && Number.isFinite(global.TIMEOUT_MS) && global.TIMEOUT_MS > 0)
      ? global.TIMEOUT_MS
      : WATCHDOG.TIMEOUT_MS;
  const warnRaw = (typeof global.WARN_MS === "number" && Number.isFinite(global.WARN_MS) && global.WARN_MS > 0)
    ? global.WARN_MS
    : Math.max(timeout - 500, WATCHDOG.HEARTBEAT_MS);
  // Warn must precede timeout — clamp so a misconfigured override
  // doesn't make warn fire after terminate (which would be silent).
  const warn = Math.min(warnRaw, timeout - 1);
  const heartbeat = (typeof global.HEARTBEAT_MS === "number" && Number.isFinite(global.HEARTBEAT_MS) && global.HEARTBEAT_MS > 0)
    ? global.HEARTBEAT_MS
    : WATCHDOG.HEARTBEAT_MS;
  return Object.freeze({ TIMEOUT_MS: timeout, WARN_MS: warn, HEARTBEAT_MS: heartbeat });
}

/// Frame loop fallback when `requestAnimationFrame` isn't available
/// in the worker scope (older browsers, some test runners).
///
/// Why: 16ms approximates 60Hz. Used only by `worker.js` as a
/// `setTimeout` fallback; production hits `requestAnimationFrame`.
export const FRAME_FALLBACK_MS = 16;
