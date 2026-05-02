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
//   * Override pattern (future): a host page can stash a partial
//     object on `globalThis.__akivaConfig` before `tab.js` boots and
//     `loadConfig()` will merge it over the defaults. Today this file
//     is the single source — the override hook is documented but not
//     yet exercised, so the dev-mode story stays simple.
//
// Imported by `app_registry.js` (and any other JS module that needs
// a tunable). Keeping a hard import — not a dynamic lookup — means
// the config is statically reachable for tree-shaking and tests can
// shadow the values via vitest's `vi.mock` if a faster watchdog is
// needed.

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

  /// Period between `ping` messages to each app worker.
  ///
  /// Why: 500ms gives three chances to detect a wedge inside the
  /// 1500ms timeout window — single missed pong is statistical
  /// noise, three in a row is a hung thread. Halving this would
  /// only pay extra postMessage cost without sharpening detection
  /// since termination is gated on the timeout, not the tick.
  HEARTBEAT_MS: 500,
});

/// Frame loop fallback when `requestAnimationFrame` isn't available
/// in the worker scope (older browsers, some test runners).
///
/// Why: 16ms approximates 60Hz. Used only by `worker.js` as a
/// `setTimeout` fallback; production hits `requestAnimationFrame`.
export const FRAME_FALLBACK_MS = 16;
