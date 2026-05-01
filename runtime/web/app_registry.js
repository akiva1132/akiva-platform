// App runtime registry.
//
// Each installed package runs as its own `WebAssembly.Instance` inside
// this worker, alongside the kernel. The kernel never touches app memory
// directly — it talks to us through the `hal_app_*` imports, and we
// route each call to the right app by a `u32` handle we assign.
//
// One wasm *process* per package (shared across all that package's
// activity instances); one *handle* per framework-side activity. That
// matches Android's model: the process is the Linux PID; each Activity
// is its own Java object living inside it.
//
// Loading is synchronous end-to-end (OPFS SyncAccessHandle + synchronous
// WebAssembly compile) so `hal_app_register` can return a ready handle
// from within kernel code — no promise juggling needed.

import { fontString } from "./renderer/canvas2d.js";
import { FIRE, IO, STATUS, packPoll, isFireKind, isIoKind } from "./op_kinds.mjs";

export class AppRegistry {
  /**
   * @param {object} deps
   * @param {{ get(key: string): Uint8Array | null }} deps.storage  Worker Storage (OPFS-backed).
   * @param {() => WebAssembly.Memory} deps.kernelMemory  Returns the live kernel memory reference; re-read every call because wasm memory can grow and the ArrayBuffer detaches.
   * @param {(level: string, text: string) => void} deps.log  Logger (level name, not numeric code).
   */
  constructor({ storage, kernelMemory, log }) {
    this._storage = storage;
    this._kernelMemory = kernelMemory;
    this._log = log;

    /** @type {Map<number, {packageId: string, class: string}>} */
    this._slots = new Map();
    /** @type {Map<string, {module: WebAssembly.Module, instance: WebAssembly.Instance, memory: WebAssembly.Memory}>} */
    this._processes = new Map();
    /**
     * Per-package `@string/` and `@color/` resource tables. Populated
     * when a package's wasm is instantiated and served from the
     * `akiva_host_resource_*` imports without a round-trip through the
     * kernel. The framework parses the same TOML on its side
     * independently, so both caches stay in lock-step with OPFS.
     * @type {Map<string, {strings: Map<string, string>, colors: Map<string, number>}>}
     */
    this._resources = new Map();
    this._nextHandle = 1;
    /**
     * Handles with a pending app-raised `finish` signal. Populated when
     * the app calls `akiva_host_finish(handle)`; drained (atomically
     * cleared + returned) by `drainFinish(handle)` which the kernel's
     * `hal_app_drain_finish` import reads after each lifecycle/input
     * callback. A Set rather than a Map because the signal is boolean;
     * multiple finish calls within one dispatch collapse to one.
     * @type {Set<number>}
     */
    this._pendingFinish = new Set();
    /**
     * Stone 9.4 — handles with a pending encoded `Intent` blob the app
     * staged via `akiva_host_start_activity`. The map stores the most
     * recent request; multiple start_activity calls within one dispatch
     * collapse to the last one, matching the single-slot framework
     * `ctx.launch_requested` semantic. Drained by `drainIntent(handle)`
     * once per dispatch via `hal_app_drain_intent`.
     * @type {Map<number, Uint8Array>}
     */
    this._pendingIntent = new Map();
    /**
     * Stone 9.5 — handles with a pending
     * `start_activity_for_result(intent, request_code)` request. Same
     * single-slot semantics as `_pendingIntent`; the tuple keeps the
     * caller-supplied `request_code` paired with the encoded Intent
     * bytes so the HAL drain can hand back both in one call.
     * @type {Map<number, { bytes: Uint8Array, requestCode: number }>}
     */
    this._pendingStartForResult = new Map();
    /**
     * Stone 9.5 — bytes an activity stashed via `akiva_host_set_result`.
     * Drained on the destroy-path of the activity, not per callback —
     * set_result is allowed to be called multiple times in one lifetime,
     * and only the last one is delivered. Last call wins.
     * @type {Map<number, Uint8Array>}
     */
    this._pendingSetResult = new Map();
    /**
     * Enabled debug categories. Empty Set = quiet (every `_debug` call
     * short-circuits). Populated via `setDebugCategories` from the main
     * thread's URL/query parser (e.g. `?debug=lifecycle,intent`). The
     * wildcard `"*"` enables everything. Kept small by design — each
     * category is a string, and adding a new one is cheap; avoids the
     * "log everything at info level" anti-pattern that makes a running
     * OS's console unreadable.
     * @type {Set<string>}
     */
    this._debug = new Set();
    /**
     * Stone 10.3b — permission grants per package, pushed from
     * `framework::boot` via `hal_host_set_grants`. Read at
     * `_instantiateProcess` time to decide which gated
     * `akiva_host_*` imports are real functions vs deny-stubs. A
     * missing entry is treated identically to an empty Set: the app
     * gets the auto import set only. Populated once per boot; on
     * upgrade `PackageManager::install` re-pushes the full new set
     * (last-write-wins — replace the stored Set outright).
     * @type {Map<string, Set<string>>}
     */
    this._grants = new Map();
    /**
     * Stone 11.0a — opaque snapshot of the installed-packages list
     * pushed from `framework::boot` via `hal_host_set_packages`. The
     * format is the framework's `pkgs_wire::encode` output (length-
     * prefixed, see framework/src/pkgs_wire.rs); we never decode it on
     * this side. When an app holding `akiva.permission.QUERY_PACKAGES`
     * calls `akiva_host_query_packages` we serve these same bytes back
     * via the standard two-phase HAL pattern.
     *
     * Empty Uint8Array on first construction so `query_packages` returns
     * a clean empty blob if it's ever called before boot finishes.
     * @type {Uint8Array}
     */
    this._packagesBlob = new Uint8Array(0);
    this._decoder = new TextDecoder("utf-8");
    this._encoder = new TextEncoder();

    /**
     * Stone 11.3c — registry of in-flight async ops started via
     * `akiva_host_io_start`. Single-shot: an entry is removed on the
     * first `io_poll` that successfully consumes it (READY/ERROR with
     * a payload that fits in the caller's buffer). If `payload.length
     * > out_cap` the entry is *retained* so the caller can realloc and
     * retry, mirroring the two-phase shape of `akiva_host_query_packages`.
     *
     * Cross-package ID guessing is prevented by storing `packageId` per
     * entry and rejecting polls from a different package: even though
     * future_ids are an opaque monotonic counter, we don't rely on
     * obscurity for isolation. Stone 11.4 (worker-per-app) drops the
     * shared map entirely; until then the ownership check is the
     * boundary.
     *
     * In Stone 11.3 single-worker, every io op resolves synchronously
     * inside `_dispatchIoStart`, so polls never observe `PENDING`. The
     * shape is async-ready for 11.4+ where the worker can return
     * before a real cross-thread handler completes.
     *
     * @type {Map<bigint, {packageId: string, status: number, payload: Uint8Array}>}
     */
    this._futures = new Map();
    /**
     * Monotonic future_id allocator. Starts at 1n — `0n` is reserved as
     * the "couldn't start" sentinel (`io_start` returns it on unknown
     * op_kind, denied capability, or malformed args). u64 wraps in 2^64
     * polls which is not a real concern; reuse-after-wrap would still
     * be guarded by the per-entry `packageId` check anyway.
     *
     * @type {bigint}
     */
    this._nextFutureId = 1n;

    // Dedicated 1×1 context used only for `measureText`. Sharing the
    // renderer's main context would work but ties measurement to display
    // readiness — an app can call `text_width` before the display has
    // attached. Keeping it separate also lets us `font=` it freely
    // without thrashing the renderer's state.
    //
    // Constructed lazily on the first `measureTextWidth` call: the
    // typical registry lifetime spans boot (where no app has painted
    // yet) through shutdown, and deferring keeps the constructor
    // free of `OffscreenCanvas` — a browser/worker-only global that
    // Node's vitest env doesn't provide, letting these tests run
    // without a DOM shim.
    this._measureCanvas = null;
    this._measureCtx = null;
  }

  /**
   * Replace the active debug-category Set. `["*"]` enables everything;
   * `[]` disables all debug output. Overrides rather than merges so a
   * second call is a reset, not an accumulation. Main thread reads
   * `?debug=<csv>` and forwards via the `set_debug` worker message.
   */
  setDebugCategories(categories) {
    this._debug = new Set(categories);
  }

  /**
   * Stone 10.3b — record the full list of permissions granted to a
   * package. Kernel pushes one call per installed package at the end
   * of `framework::boot` (via `hal_host_set_grants`) and again from
   * `PackageManager::install` on upgrade. The filter in
   * `_buildAppImports` consults this map to decide which gated
   * `akiva_host_*` imports resolve to real functions vs deny-stubs.
   *
   * Last-write-wins: a second call for the same `packageId` replaces
   * the earlier list, matching the Rust-side controller behaviour
   * where `apply_grants` writes the full new set on upgrade.
   *
   * @param {string} packageId
   * @param {string[]} permissionNames  e.g. `["akiva.permission.NETWORK"]`
   */
  setGrants(packageId, permissionNames) {
    this._grants.set(packageId, new Set(permissionNames));
    this._dbg("permissions", `${packageId} grants: [${permissionNames.join(", ")}]`);
  }

  /**
   * Stone 11.0a — replace the cached package-list snapshot. Called once
   * at boot from `hal_host_set_packages`; future install/uninstall flows
   * will re-push the full set. The bytes are opaque (the framework's
   * length-prefixed encoding) and served back verbatim to apps via
   * `akiva_host_query_packages`.
   *
   * @param {Uint8Array} bytes  framework's encoded package list, or empty
   */
  setPackages(bytes) {
    this._packagesBlob = bytes instanceof Uint8Array ? bytes : new Uint8Array(0);
    this._dbg("permissions", `packages snapshot: ${this._packagesBlob.length} bytes`);
  }

  /**
   * Emit a `debug`-level log tagged with the category if the category
   * is enabled (or `"*"` is). Category strings are free-form — adding
   * a new one is as cheap as a string literal at the call site, and
   * tests/grep find every emitter under one tag.
   */
  _dbg(category, text) {
    if (this._debug.has(category) || this._debug.has("*")) {
      this._log("debug", `[${category}] ${text}`);
    }
  }

  /** Width in pixels of `text` at `size_px` in the shared UI font. */
  measureTextWidth(text, size_px) {
    if (!this._measureCtx) {
      this._measureCanvas = new OffscreenCanvas(1, 1);
      this._measureCtx = this._measureCanvas.getContext("2d");
    }
    this._measureCtx.font = fontString(size_px);
    return this._measureCtx.measureText(text).width;
  }

  // ── Public API (called by worker.js `hal_app_*` bridges) ────────────────

  /** Returns a new u32 handle, or 0 on failure. */
  register(packageId, className) {
    if (!packageId || !className) return 0;

    // Lazily instantiate the wasm process on first use of a package.
    let proc = this._processes.get(packageId);
    if (!proc) {
      proc = this._instantiateProcess(packageId);
      if (!proc) return 0;
    }

    const handle = this._nextHandle++;
    this._slots.set(handle, { packageId, class: className });
    this._dbg("lifecycle", `register ${packageId}/${className} → handle ${handle}`);
    return handle;
  }

  unregister(handle) {
    const slot = this._slots.get(handle);
    if (!slot) return;
    this._slots.delete(handle);
    // Cheap tidy-up; the handle isn't reusable (`_nextHandle` is
    // monotonic) but a stale flag sitting in the set after destroy
    // would be harmless — still, belt-and-braces.
    this._pendingFinish.delete(handle);
    this._pendingIntent.delete(handle);
    this._pendingStartForResult.delete(handle);
    this._pendingSetResult.delete(handle);
    // The process is kept alive even when no slots reference it. A future
    // GC pass could retire idle processes; for MVP the cost is one
    // `WebAssembly.Instance` per installed package, which is cheap.
  }

  /**
   * Read + clear the pending-finish flag for `handle`. Returns `true` if
   * the app requested finish via `akiva_host_finish` since the last
   * drain, `false` otherwise. Called by the kernel's
   * `hal_app_drain_finish` HAL bridge after every ctx-bearing callback,
   * so a wasm-app-initiated finish shows up in the framework's
   * `ActivityContext.finish_requested` identically to an in-kernel
   * activity writing that flag directly. See docs/abi.md §2.3.
   */
  drainFinish(handle) {
    const had = this._pendingFinish.delete(handle);
    if (had) this._dbg("intent", `drain_finish[handle=${handle}] → true`);
    return had;
  }

  /**
   * Read + clear the pending intent blob for `handle`. Returns the bytes
   * (Uint8Array) the app staged via `akiva_host_start_activity`, or
   * `null` if no start_activity was raised this dispatch. The kernel's
   * `hal_app_drain_intent` bridge consumes this with the two-phase
   * buffer contract: if the requested `outCap` is too small, the buffer
   * stays armed and the next drain retries. See docs/abi.md §2.3.
   */
  drainIntent(handle) {
    const bytes = this._pendingIntent.get(handle);
    if (bytes) this._dbg("intent", `drain_intent[handle=${handle}] → ${bytes.length} bytes`);
    return bytes ?? null;
  }

  /** Clear the intent slot after a successful drain. */
  _consumeIntent(handle) {
    this._pendingIntent.delete(handle);
  }

  /**
   * Deliver a freshly-resolved intent blob to an already-existing
   * activity handle — fires `akiva_app_new_intent`. Same rc convention
   * as `restoreState`: 0 ok, 1 handle miss, 2 alloc failed, 3 trap.
   */
  newIntent(handle, bytesPtr, bytesLen) {
    if (bytesLen === 0) return this._lookup(handle) ? 0 : 1;
    const bytes = this._copyKernelSlice(bytesPtr, bytesLen);
    return this._callAppWithBytes(handle, "new_intent", bytes,
      (proc, appPtr, len) => proc.instance.exports.akiva_app_new_intent(handle, appPtr, len));
  }

  /**
   * Stone 9.5 — read the pending `start_activity_for_result` entry for
   * `handle`. Returns `{ bytes, requestCode }` or `null`. Does NOT
   * clear; clearing happens in `_consumeStartForResult` once the HAL
   * has successfully copied the bytes into kernel memory (same
   * two-phase contract as `drainIntent`).
   */
  drainStartForResult(handle) {
    const entry = this._pendingStartForResult.get(handle);
    if (entry) {
      this._dbg("intent", `drain_start_for_result[handle=${handle}] → ${entry.bytes.length} bytes, rc=${entry.requestCode}`);
    }
    return entry ?? null;
  }
  _consumeStartForResult(handle) {
    this._pendingStartForResult.delete(handle);
  }

  /**
   * Stone 9.5 — read the pending set_result bytes for `handle`. Returns
   * a `Uint8Array` or `null`. Does NOT clear; see `_consumeSetResult`.
   */
  drainSetResult(handle) {
    const bytes = this._pendingSetResult.get(handle);
    if (bytes) {
      this._dbg("intent", `drain_set_result[handle=${handle}] → ${bytes.length} bytes`);
    }
    return bytes ?? null;
  }
  _consumeSetResult(handle) {
    this._pendingSetResult.delete(handle);
  }

  /**
   * Stone 9.5 — deliver an `ActivityResult` blob to the caller activity.
   * Fires `akiva_app_activity_result(handle, request_code, ptr, len)`.
   * Same rc convention as `newIntent` / `restoreState`.
   */
  activityResult(handle, requestCode, bytesPtr, bytesLen) {
    if (bytesLen === 0) return this._lookup(handle) ? 0 : 1;
    const bytes = this._copyKernelSlice(bytesPtr, bytesLen);
    return this._callAppWithBytes(handle, "activity_result", bytes,
      (proc, appPtr, len) => proc.instance.exports.akiva_app_activity_result(
        handle, requestCode, appPtr, len,
      ));
  }

  lifecycle(handle, transition) {
    const ctx = this._lookup(handle);
    if (!ctx) return;
    const { slot, proc } = ctx;
    const name = LIFECYCLE_NAMES[transition] ?? transition;
    this._dbg("lifecycle", `${slot.packageId}[handle=${handle}] → ${name}`);
    try {
      if (transition === 0 /* Create */) {
        // Create is the only lifecycle transition that needs extra
        // payload (the class name). Every other transition is just
        // (handle) because the app remembers which class a handle
        // corresponds to from its own on_create call.
        this._dispatchCreate(slot, proc, handle);
      } else {
        const fn = LIFECYCLE_EXPORTS[transition];
        if (!fn) {
          this._log("warn", `lifecycle: unknown transition ${transition} for ${slot.packageId}`);
          return;
        }
        proc.instance.exports[fn](handle);
      }
    } catch (err) {
      this._log("error", `app ${slot.packageId}.${name}: ${err?.message ?? err}`);
    }
  }

  pointer(handle, x, y, button, phase, tsMs) {
    const ctx = this._lookup(handle);
    if (!ctx) return;
    try {
      ctx.proc.instance.exports.akiva_app_pointer(handle, x, y, button, phase, tsMs);
    } catch (err) {
      this._log("error", `app ${ctx.slot.packageId}.pointer: ${err?.message ?? err}`);
    }
  }

  key(handle, code, modifiers, phase, tsMs) {
    const ctx = this._lookup(handle);
    if (!ctx) return;
    try {
      ctx.proc.instance.exports.akiva_app_key(handle, code, modifiers, phase, tsMs);
    } catch (err) {
      this._log("error", `app ${ctx.slot.packageId}.key: ${err?.message ?? err}`);
    }
  }

  resize(handle, widthPx, heightPx, density) {
    const ctx = this._lookup(handle);
    if (!ctx) return;
    try {
      ctx.proc.instance.exports.akiva_app_resize(handle, widthPx, heightPx, density);
    } catch (err) {
      this._log("error", `app ${ctx.slot.packageId}.resize: ${err?.message ?? err}`);
    }
  }

  /**
   * Ask the app to paint. Two-phase buffer: app produces a paint-op
   * stream in its own memory; we copy up to `outCap` bytes into kernel
   * memory. Return value is the full length the app produced — if it
   * exceeds `outCap` the kernel retries with a larger buffer and we
   * copied nothing on this call.
   */
  render(handle, tsMs, outPtr, outCap) {
    const ret = this._copyAppToKernel(handle, "render", outPtr, outCap,
      (proc, lenPtr) => proc.instance.exports.akiva_app_render(handle, tsMs, lenPtr));
    // Only emit the "paint" category when a frame actually produced
    // bytes — `ret === 0` is the idle-app path and every frame would
    // spam the log with zeros. `ret > outCap` is the retry path (no
    // copy happened yet); count it too since it's informative.
    if (ret > 0) this._dbg("paint", `render[handle=${handle}] ${ret} bytes${ret > outCap ? " (retry)" : ""}`);
    return ret;
  }

  /**
   * Ask the app to serialise its UI state. Same two-phase buffer contract
   * as `render`: returns the number of bytes the app produced.
   */
  saveState(handle, outPtr, outCap) {
    return this._copyAppToKernel(handle, "save_state", outPtr, outCap,
      (proc, lenPtr) => proc.instance.exports.akiva_app_save_state(handle, lenPtr));
  }

  /**
   * Hand previously-persisted bytes back to the app via
   * `akiva_app_restore_state`. Returns the rc convention shared by every
   * kernel→app bytes path: 0 on success, 1 = handle miss, 2 = alloc
   * failed, 3 = app export threw.
   */
  restoreState(handle, bytesPtr, bytesLen) {
    if (bytesLen === 0) {
      // Fast-path: still fail the handle-miss case so the kernel can
      // log, but skip every alloc/copy/dealloc step for a no-op restore.
      return this._lookup(handle) ? 0 : 1;
    }
    const bytes = this._copyKernelSlice(bytesPtr, bytesLen);
    return this._callAppWithBytes(handle, "restore_state", bytes,
      (proc, appPtr, len) => proc.instance.exports.akiva_app_restore_state(handle, appPtr, len));
  }

  // ── Internals ───────────────────────────────────────────────────────────

  _instantiateProcess(packageId) {
    const key = `packages/${packageId}/app.wasm`;
    const bytes = this._storage.get(key);
    if (!bytes) {
      this._log("warn", `app register: no wasm at ${key}`);
      return null;
    }

    // Reserve the slot BEFORE instantiation so host imports closing over
    // `packageId` can look up the (still partial) record if the wasm
    // start function happens to call into the host. Memory is only bound
    // after construction succeeds; calls from inside start that need
    // memory will see a null and silently drop — apps that need the
    // log during start should fire it from on_create instead.
    const proc = { module: null, instance: null, memory: null };
    this._processes.set(packageId, proc);

    let module, instance;
    try {
      module = new WebAssembly.Module(bytes);
      const imports = this._buildAppImports(packageId);
      instance = new WebAssembly.Instance(module, imports);
    } catch (err) {
      this._processes.delete(packageId);
      this._log("error", `app ${packageId}: instantiate failed: ${err?.message ?? err}`);
      return null;
    }

    const exports = instance.exports;
    for (const req of REQUIRED_EXPORTS) {
      if (typeof exports[req] !== "function" && req !== "memory") {
        this._processes.delete(packageId);
        this._log("error", `app ${packageId}: missing export '${req}'`);
        return null;
      }
    }
    if (!(exports.memory instanceof WebAssembly.Memory)) {
      this._processes.delete(packageId);
      this._log("error", `app ${packageId}: missing 'memory' export`);
      return null;
    }

    proc.module = module;
    proc.instance = instance;
    proc.memory = exports.memory;

    // Load this package's value tables once, before any host import
    // can fire. If either file is absent the corresponding map is
    // empty; parse failures are logged but don't block instantiation
    // — a broken colors.toml just means `@color/` refs return magenta.
    this._loadResources(packageId);

    return proc;
  }

  _loadResources(packageId) {
    const strings = new Map();
    const colors = new Map();
    const tryParse = (leaf, parseFn, target) => {
      const key = `packages/${packageId}/res/values/${leaf}.toml`;
      const bytes = this._storage.get(key);
      if (!bytes) return;
      let text;
      try {
        text = this._decoder.decode(bytes);
      } catch (err) {
        this._log("warn", `[${packageId}] ${leaf}.toml: not valid UTF-8`);
        return;
      }
      try {
        parseFn(text, target, (key, reason) =>
          this._log("warn", `[${packageId}] ${leaf}.toml: '${key}' ${reason}`));
      } catch (err) {
        this._log("warn", `[${packageId}] ${leaf}.toml: ${err?.message ?? err}`);
      }
    };
    tryParse("strings", parseStringsToml, strings);
    tryParse("colors", parseColorsToml, colors);
    this._resources.set(packageId, { strings, colors });
  }

  _buildAppImports(packageId) {
    // Imports close over `packageId` so log lines carry the app tag
    // automatically. Memory is resolved lazily because it isn't bound
    // until after `new WebAssembly.Instance` returns.
    const self = this;
    const readName = (ptr, len) => {
      const proc = self._processes.get(packageId);
      if (len === 0 || !proc?.memory) return null;
      return self._decoder.decode(new Uint8Array(proc.memory.buffer, ptr, len));
    };
    const full = {
      env: {
        akiva_host_resource_color: (ptr, len) => {
          const name = readName(ptr, len);
          if (name === null) return MISSING_COLOR;
          const table = self._resources.get(packageId);
          const value = table?.colors.get(name);
          if (value === undefined) {
            self._log("warn", `[${packageId}] @color/${name}: undefined (using magenta placeholder)`);
            return MISSING_COLOR;
          }
          return value >>> 0;  // force unsigned so WASI u32 round-trip is clean
        },
        akiva_host_resource_string: (ptr, len, outPtr, outCap) => {
          const proc = self._processes.get(packageId);
          if (!proc?.memory) return 0;
          const name = readName(ptr, len);
          if (name === null) return 0;
          const table = self._resources.get(packageId);
          let value = table?.strings.get(name);
          if (value === undefined) {
            self._log("warn", `[${packageId}] @string/${name}: undefined (using placeholder)`);
            value = `⟨missing:${name}⟩`;
          }
          const bytes = self._encoder.encode(value);
          // Contract: if the caller's buffer is too small, write
          // nothing and return the required length so they can
          // realloc and try again. The SDK wrapper handles this.
          if (bytes.length > outCap) return bytes.length;
          new Uint8Array(proc.memory.buffer, outPtr, bytes.length).set(bytes);
          return bytes.length;
        },
        akiva_host_resource_has_color: (ptr, len) => {
          const name = readName(ptr, len);
          if (name === null) return 0;
          return self._resources.get(packageId)?.colors.has(name) ? 1 : 0;
        },
        akiva_host_resource_has_string: (ptr, len) => {
          const name = readName(ptr, len);
          if (name === null) return 0;
          return self._resources.get(packageId)?.strings.has(name) ? 1 : 0;
        },

        // Stone 11.3c — three multiplexers that route every host-mediated
        // op through a single dispatch table. The op_kind constants live
        // in `op_kinds.mjs` (mirror of `framework/src/wire/op_kinds.rs`);
        // every op_kind has a fixed args layout decoded inside the
        // dispatcher (no central registry of decoders — keeps the call
        // graph readable, and a missing case in the switch is a hard
        // tripwire rather than a silent pass).
        //
        // Capability gating moves into the dispatch helpers (one import
        // serves many op_kinds — some always-available, some gated). The
        // `_applyPermissionFilter` mechanism above remains for the legacy
        // per-op imports; both gates stay in sync because the dispatcher
        // consults the same `_grants` map.
        //
        // Args copying: the dispatcher reads `args_ptr..args_ptr+args_len`
        // out of app memory into a Uint8Array. We copy (`slice()`) rather
        // than view because the args may be re-read during the op (e.g.
        // an intent decode) and any subsequent wasm call could grow
        // memory and detach the underlying ArrayBuffer.
        akiva_host_fire: (opKind, argsPtr, argsLen) => {
          const proc = self._processes.get(packageId);
          if (!proc?.memory) return 1;
          const args = self._readArgs(proc, argsPtr, argsLen);
          if (args === null) return 1;
          return self._dispatchFire(packageId, opKind, args);
        },
        akiva_host_io_start: (opKind, argsPtr, argsLen) => {
          const proc = self._processes.get(packageId);
          if (!proc?.memory) return 0n;
          const args = self._readArgs(proc, argsPtr, argsLen);
          if (args === null) return 0n;
          return self._dispatchIoStart(packageId, opKind, args);
        },
        akiva_host_io_poll: (futureId, outPtr, outCap) => {
          return self._pollFuture(packageId, futureId, outPtr, outCap);
        },
      },
    };
    return this._applyPermissionFilter(packageId, full);
  }

  /**
   * Stone 10.3b — apply the capability→imports filter to the fully
   * built `env` object. Every import is classified:
   *
   *   1. **Auto** (`AUTO_IMPORTS`): passes through untouched. Every app
   *      gets these — log, resource lookups, text measurement, finish,
   *      set_result. Gating them would mean the app can't even draw.
   *   2. **Gated + granted**: passes through untouched. The controller
   *      stored this grant via `setGrants` (Stone 10.3b push from
   *      Rust), so the app earned access at install time.
   *   3. **Gated + NOT granted**: replaced with a deny-stub that logs
   *      a warning and returns a zero-shaped value. An app shouldn't
   *      import these in the first place (Stone 10.3b-true / LinkError
   *      enforcement waits for SDK build-tooling that conditions
   *      extern emission on manifest). Until then, soft deny is the
   *      shippable enforcement: deterministic no-op, not silent pass.
   *   4. **Unknown** (present in `env` but not in AUTO + any capability
   *      list): treated as auto. This is the tripwire slot for a new
   *      host import that was added in JS but never mapped in Rust;
   *      worth noting in logs but not blocking. The Rust-side Vitest
   *      mirror test catches a persistent drift.
   */
  _applyPermissionFilter(packageId, env) {
    const grants = this._grants.get(packageId) ?? new Set();
    const allowedGated = new Set();
    for (const capability of grants) {
      const mapped = CAPABILITY_IMPORTS[capability];
      if (mapped) for (const name of mapped) allowedGated.add(name);
    }
    // Build the reverse map: import name → capability that gates it.
    // Used only to shape the deny-stub's log message; fast enough to
    // rebuild per instantiation since it's ~15 strings.
    const gatedBy = new Map();
    for (const [cap, names] of Object.entries(CAPABILITY_IMPORTS)) {
      for (const name of names) gatedBy.set(name, cap);
    }
    const filtered = {};
    const envImports = env.env;
    const autoSet = new Set(AUTO_IMPORTS);
    for (const [name, fn] of Object.entries(envImports)) {
      if (autoSet.has(name)) {
        filtered[name] = fn;
        continue;
      }
      const cap = gatedBy.get(name);
      if (cap === undefined) {
        // Unknown import — not in AUTO and not in any capability list.
        // Pass through, but flag once in logs so the mapping drift is
        // observable.
        this._log("warn",
          `[${packageId}] host import '${name}' is not in AUTO_IMPORTS or any capability mapping — passing through. Update runtime/web/app_registry.js and framework/src/permissions.rs in lockstep.`);
        filtered[name] = fn;
        continue;
      }
      if (allowedGated.has(name)) {
        filtered[name] = fn;
      } else {
        filtered[name] = this._makeDenyStub(packageId, name, cap, fn);
      }
    }
    return { env: filtered };
  }

  /**
   * Build a no-op stand-in for a gated import the package doesn't have
   * the capability for. Logs once per call so a silent call site is
   * observable; returns a zero-shaped value matching the wasm function
   * signature (we can't know the signature statically, but returning
   * `0` is safe for every import in the current set — all of them are
   * `() -> ()` or `() -> i32`-or-smaller). `originalFn` is kept in the
   * closure for diagnostics only; we never call it.
   */
  _makeDenyStub(packageId, importName, capability, originalFn) {
    void originalFn;  // retained so a future audit can show what was blocked
    const self = this;
    return function permissionDeniedStub(..._args) {
      self._log("warn",
        `[${packageId}] permission denied: '${importName}' requires '${capability}' which is not granted. Call returned no-op (0).`);
      return 0;
    };
  }

  // ── Stone 11.3c — unified ABI dispatcher ─────────────────────────────
  //
  // `akiva_host_fire` / `akiva_host_io_start` / `akiva_host_io_poll`
  // funnel here. Each op_kind has a fixed args layout decoded inside
  // its branch; results for io ops are stored in the FutureRegistry
  // (`this._futures`) so `io_poll` can serve them through the same
  // two-phase buffer protocol as the legacy per-op imports.
  //
  // Why a single switch instead of a registry of decoders: the op set
  // is small (~10 today, capped at ~250 by the disjoint-range layout
  // in op_kinds.rs) and a missing case is a hard tripwire. A registry
  // would let an op silently default to "unknown" if the registration
  // line were ever lost in a merge.

  /**
   * Copy `len` bytes from `proc`'s memory at `ptr` into a fresh
   * Uint8Array. Returns `null` on bounds violation (length zero is
   * legitimate — returns an empty array, not null). Always copies:
   * the args may outlive subsequent wasm calls that grow memory and
   * detach the live buffer.
   */
  _readArgs(proc, ptr, len) {
    if (len === 0) return new Uint8Array(0);
    const bufLen = proc.memory.buffer.byteLength;
    if (ptr < 0 || ptr + len > bufLen) return null;
    const copy = new Uint8Array(len);
    copy.set(new Uint8Array(proc.memory.buffer, ptr, len));
    return copy;
  }

  /** True iff `packageId` has been granted `capability`. */
  _hasGrant(packageId, capability) {
    return this._grants.get(packageId)?.has(capability) ?? false;
  }

  /**
   * Dispatch a fire-and-forget op. Returns 0 on success, non-zero on
   * any failure — unknown op_kind, malformed args, denied capability.
   * Side effects (pendingFinish, pendingIntent, etc.) happen inline
   * and the kernel drains them at the end of the dispatch via the
   * existing `hal_app_drain_*` HAL bridges, identical to the legacy
   * per-op imports.
   */
  _dispatchFire(packageId, opKind, args) {
    if (!isFireKind(opKind)) {
      this._log("warn",
        `[${packageId}] fire(0x${opKind.toString(16)}): not a fire op_kind (range 0x0001..0x00FF)`);
      return 1;
    }
    switch (opKind) {
      case FIRE.LOG: {
        // Args: [level: u32 LE | msg_bytes...]
        if (args.length < 4) return 1;
        const view = new DataView(args.buffer, args.byteOffset, args.byteLength);
        const level = view.getUint32(0, true);
        const text = this._decoder.decode(args.subarray(4));
        this._log(LEVEL_NAMES[level] ?? "info", `[${packageId}] ${text}`);
        return 0;
      }
      case FIRE.FINISH: {
        // Args: [handle: u32 LE]
        if (args.length < 4) return 1;
        const view = new DataView(args.buffer, args.byteOffset, args.byteLength);
        const handle = view.getUint32(0, true);
        this._pendingFinish.add(handle);
        return 0;
      }
      case FIRE.START_ACTIVITY: {
        // Args: [handle: u32 LE | intent_bytes...]
        if (!this._hasGrant(packageId, "akiva.permission.LAUNCH_ANY_APP")) {
          this._log("warn",
            `[${packageId}] fire(START_ACTIVITY): denied — no LAUNCH_ANY_APP grant`);
          return 2;
        }
        if (args.length < 4) return 1;
        const view = new DataView(args.buffer, args.byteOffset, args.byteLength);
        const handle = view.getUint32(0, true);
        const intentBytes = args.slice(4);
        if (intentBytes.length === 0) return 1;
        this._pendingIntent.set(handle, intentBytes);
        return 0;
      }
      case FIRE.START_ACTIVITY_FOR_RESULT: {
        // Args: [handle: u32 LE | request_code: u32 LE | intent_bytes...]
        if (!this._hasGrant(packageId, "akiva.permission.LAUNCH_ANY_APP")) {
          this._log("warn",
            `[${packageId}] fire(START_ACTIVITY_FOR_RESULT): denied — no LAUNCH_ANY_APP grant`);
          return 2;
        }
        if (args.length < 8) return 1;
        const view = new DataView(args.buffer, args.byteOffset, args.byteLength);
        const handle = view.getUint32(0, true);
        const requestCode = view.getUint32(4, true);
        const intentBytes = args.slice(8);
        if (intentBytes.length === 0) return 1;
        this._pendingStartForResult.set(handle, { bytes: intentBytes, requestCode });
        return 0;
      }
      case FIRE.SET_RESULT: {
        // Args: [handle: u32 LE | result_bytes...]
        if (args.length < 4) return 1;
        const view = new DataView(args.buffer, args.byteOffset, args.byteLength);
        const handle = view.getUint32(0, true);
        const resultBytes = args.slice(4);
        if (resultBytes.length === 0) return 1;
        this._pendingSetResult.set(handle, resultBytes);
        return 0;
      }
      default:
        this._log("warn",
          `[${packageId}] fire(0x${opKind.toString(16)}): unknown op_kind`);
        return 1;
    }
  }

  /**
   * Dispatch an async op. In Stone 11.3 single-worker every op resolves
   * synchronously inside this method and the result is staged in
   * `_futures` for the imminent `io_poll`. Stone 11.4 (worker-per-app)
   * lets some branches return a real `PENDING` future the kernel
   * resolves on a later turn of the event loop.
   *
   * Returns the future_id on success, `0n` on failure (unknown op_kind,
   * denied capability, malformed args). `0n` is the only sentinel —
   * real future_ids are non-zero.
   */
  _dispatchIoStart(packageId, opKind, args) {
    if (!isIoKind(opKind)) {
      this._log("warn",
        `[${packageId}] io_start(0x${opKind.toString(16)}): not an io op_kind (range 0x0101..0x01FF)`);
      return 0n;
    }
    let payload;
    let status = STATUS.READY;
    switch (opKind) {
      case IO.TEXT_WIDTH: {
        // Args: [size: f32 LE | text_bytes...] -> [width: f32 LE]
        if (args.length < 4) return 0n;
        const view = new DataView(args.buffer, args.byteOffset, args.byteLength);
        const size = view.getFloat32(0, true);
        const text = this._decoder.decode(args.subarray(4));
        const width = this.measureTextWidth(text, size);
        const out = new Uint8Array(4);
        new DataView(out.buffer).setFloat32(0, width, true);
        payload = out;
        break;
      }
      case IO.QUERY_PACKAGES: {
        // Args: [] -> [pkgs_wire blob...]
        if (!this._hasGrant(packageId, "akiva.permission.QUERY_PACKAGES")) {
          this._log("warn",
            `[${packageId}] io_start(QUERY_PACKAGES): denied — no QUERY_PACKAGES grant`);
          return 0n;
        }
        // Slice so a later setPackages reassignment can't mutate the
        // staged payload out from under a pending poll.
        payload = this._packagesBlob.slice();
        break;
      }
      case IO.STORAGE_GET:
      case IO.STORAGE_SET:
        // Stone 11.6 wires storage onto OPFS handles. Until then, ERROR.
        // Keeping the op_kind reachable (rather than rejecting) means
        // the SDK wrapper can be written today and naturally start
        // working when the handler lands.
        if (!this._hasGrant(packageId, "akiva.permission.STORAGE_SANDBOX")) {
          this._log("warn",
            `[${packageId}] io_start(0x${opKind.toString(16)}): denied — no STORAGE_SANDBOX grant`);
          return 0n;
        }
        status = STATUS.ERROR;
        payload = new Uint8Array(0);
        break;
      default:
        this._log("warn",
          `[${packageId}] io_start(0x${opKind.toString(16)}): unknown op_kind`);
        return 0n;
    }
    const futureId = this._nextFutureId++;
    this._futures.set(futureId, { packageId, status, payload });
    return futureId;
  }

  /**
   * Resolve a future started via `io_start`. Returns the packed
   * `(status, length)` u64 the SDK side decodes. Behaviour matrix:
   *
   *   - unknown future_id, or owned by a different package → ERROR, len 0.
   *   - payload fits in `outCap` → write it, status=READY/ERROR, len=actual,
   *     evict the entry (single-shot).
   *   - payload too large → don't write, status=READY, len=actual,
   *     KEEP the entry so the caller can realloc and retry. Same shape
   *     as `akiva_host_query_packages`'s legacy two-phase contract.
   *   - PENDING (not used in 11.3 single-worker; reserved for 11.4+)
   *     → status=PENDING, len 0, KEEP the entry.
   */
  _pollFuture(packageId, futureId, outPtr, outCap) {
    const entry = this._futures.get(futureId);
    if (!entry) return packPoll(STATUS.ERROR, 0);
    if (entry.packageId !== packageId) {
      // Cross-package poll attempt. Treat as unknown — don't even
      // confirm the future exists. The originator can still poll;
      // log once so the attempt is observable.
      this._log("warn",
        `[${packageId}] io_poll(${futureId}): not owned by this package — ignored`);
      return packPoll(STATUS.ERROR, 0);
    }
    if (entry.status === STATUS.PENDING) {
      return packPoll(STATUS.PENDING, 0);
    }
    const len = entry.payload.length;
    if (len > outCap) {
      // Two-phase retry: keep the entry, return the required length.
      return packPoll(entry.status, len);
    }
    if (len > 0) {
      const proc = this._processes.get(packageId);
      if (!proc?.memory) {
        // Process gone — drop the future. Don't try to write to a
        // detached buffer.
        this._futures.delete(futureId);
        return packPoll(STATUS.ERROR, 0);
      }
      new Uint8Array(proc.memory.buffer, outPtr, len).set(entry.payload);
    }
    // Single-shot consume.
    this._futures.delete(futureId);
    return packPoll(entry.status, len);
  }

  _lookup(handle) {
    const slot = this._slots.get(handle);
    if (!slot) return null;
    const proc = this._processes.get(slot.packageId);
    if (!proc || !proc.instance) return null;
    return { slot, proc };
  }

  _dispatchCreate(_slot, _proc, handle) {
    // Class-name passing reuses the generic app-bytes path: the only
    // quirk is the export takes `(handle, ptr, len)` instead of
    // `(handle, bytes_ptr, bytes_len)` but that's identical semantics.
    // rc is ignored — the only recoverable outcome (handle miss) can't
    // happen here because the slot was just inserted by `register`.
    this._callAppWithBytes(handle, "create", this._encoder.encode(_slot.class),
      (proc, appPtr, len) => proc.instance.exports.akiva_app_create(handle, appPtr, len));
  }

  // ── Shared host ↔ app byte-transfer helpers ────────────────────────────
  //
  // Three patterns used to live inline at five call sites each with its
  // own try/catch/rc convention. They've been hoisted so every
  // app-memory exchange goes through the same code path — less surface
  // area for subtle bugs (wrong rc on alloc failure, forgotten
  // dealloc, mismatched length field handling) and less noise when a
  // future primitive (e.g. Stone 9.5's `activity_result`) lands.

  /**
   * Two-phase buffer read: the app-side export produces a byte stream
   * in its own memory; we copy up to `outCap` bytes into kernel memory
   * at `outPtr`. `invoke(proc, lenPtr)` calls the export and returns
   * the source pointer in app memory. Returns the total length the app
   * produced; `len > outCap` means nothing was copied and the caller
   * should retry with a larger buffer.
   */
  _copyAppToKernel(handle, opLabel, outPtr, outCap, invoke) {
    const ctx = this._lookup(handle);
    if (!ctx) return 0;
    const { slot, proc } = ctx;

    let len, srcPtr;
    try {
      const lenPtr = proc.instance.exports.akiva_app_alloc(4);
      if (lenPtr === 0) {
        this._log("warn", `app ${slot.packageId}: alloc(4) for ${opLabel} len returned null`);
        return 0;
      }
      try {
        srcPtr = invoke(proc, lenPtr);
        len = new DataView(proc.memory.buffer).getUint32(lenPtr, true);
      } finally {
        proc.instance.exports.akiva_app_dealloc(lenPtr, 4);
      }
    } catch (err) {
      this._log("error", `app ${slot.packageId}.${opLabel}: ${err?.message ?? err}`);
      return 0;
    }

    if (len === 0 || srcPtr === 0) return 0;
    if (len > outCap) return len;  // caller retries with a larger buffer

    const src = new Uint8Array(proc.memory.buffer, srcPtr, len);
    const kernelBuf = this._kernelMemory().buffer;
    new Uint8Array(kernelBuf, outPtr, len).set(src);
    return len;
  }

  /**
   * Two-phase buffer write: we have kernel-owned bytes, allocate a
   * matching region in the app's memory, copy them in, invoke the
   * export that consumes them, then free. `invoke(proc, appPtr, len)`
   * fires the export. Returns rc: 0 ok, 1 handle miss, 2 alloc failed,
   * 3 app export threw. A zero-length `bytes` short-circuits to 0 / 1
   * without touching the allocator.
   */
  _callAppWithBytes(handle, opLabel, bytes, invoke) {
    const ctx = this._lookup(handle);
    if (!ctx) return 1;
    const { slot, proc } = ctx;
    if (bytes.length === 0) return 0;

    let appPtr;
    try {
      appPtr = proc.instance.exports.akiva_app_alloc(bytes.length);
      if (appPtr === 0) {
        this._log("warn", `app ${slot.packageId}: alloc(${bytes.length}) for ${opLabel} returned null`);
        return 2;
      }
      new Uint8Array(proc.memory.buffer, appPtr, bytes.length).set(bytes);
      invoke(proc, appPtr, bytes.length);
    } catch (err) {
      this._log("error", `app ${slot.packageId}.${opLabel}: ${err?.message ?? err}`);
      return 3;
    } finally {
      if (appPtr) {
        try { proc.instance.exports.akiva_app_dealloc(appPtr, bytes.length); }
        catch (_) { /* best effort */ }
      }
    }
    return 0;
  }

  /**
   * Copy `len` bytes out of kernel memory at `ptr` into an owned
   * `Uint8Array`. Necessary before any call that may detach the kernel
   * buffer — `WebAssembly.Memory.grow` on either side invalidates
   * existing views.
   */
  _copyKernelSlice(ptr, len) {
    const kernelBuf = this._kernelMemory().buffer;
    const copy = new Uint8Array(len);
    copy.set(new Uint8Array(kernelBuf, ptr, len));
    return copy;
  }
}

// ── Constants ────────────────────────────────────────────────────────────

// Stone 10.3b — mirror of Rust's `framework::permissions::AUTO_IMPORTS` +
// `imports_for_capability`. Must stay byte-for-byte in sync with
// `framework/src/permissions.rs`; a Vitest test asserts the same shape
// by round-tripping an app through `setGrants` + `_buildAppImports`,
// and a Rust test in the same stone pins the canonical list. If you
// add a new host import, update BOTH sides or the filter will either
// stub a real function or ship an ungated one.
// Stone 11.3-final: every fire/io op runs through the unified-ABI
// multiplexers (`fire`, `io_start`, `io_poll`). The legacy per-op
// imports (akiva_host_log, akiva_host_finish, akiva_host_text_width,
// akiva_host_query_packages, akiva_host_set_result, and the
// `start_activity` pair) are gone from both sides of the boundary.
//
// AUTO_IMPORTS is now resource-lookups (4) plus the trio (3). Resource
// lookups stay legacy intentionally — they're per-package readonly
// queries that don't fit the fire/io shape (no async, no side effect,
// no handle context), and consolidating them under op_kinds would
// just add encode/decode overhead for no benefit.
export const AUTO_IMPORTS = Object.freeze([
  "akiva_host_resource_color",
  "akiva_host_resource_string",
  "akiva_host_resource_has_color",
  "akiva_host_resource_has_string",
  // Stone 11.3 unified-ABI multiplexers. Capability gating happens
  // inside the dispatcher (one import serves many op_kinds), not at
  // the import surface. Mirror in framework/src/permissions.rs.
  "akiva_host_fire",
  "akiva_host_io_start",
  "akiva_host_io_poll",
]);

// Capability → gated imports. After 11.3 every capability gate lives
// inside the unified-ABI dispatcher (the multiplexers are auto, but
// op_kinds like QUERY_PACKAGES and START_ACTIVITY are checked against
// `_grants` per call). The map is preserved for two reasons:
//   * If a future capability lands a per-op import that doesn't fit
//     the fire/io shape (e.g. a streaming surface), it slots in here
//     without the dispatcher having to know about it.
//   * Vitest mirror tests assert that no capability has accidentally
//     re-acquired imports — drift would silently bypass the
//     in-dispatcher gating.
export const CAPABILITY_IMPORTS = Object.freeze({
  "akiva.permission.LAUNCH_ANY_APP":  Object.freeze([]),
  "akiva.permission.QUERY_PACKAGES":  Object.freeze([]),
  "akiva.permission.STORAGE_SANDBOX": Object.freeze([]),
  "akiva.permission.NOTIFICATIONS":   Object.freeze([]),
  "akiva.permission.NETWORK":         Object.freeze([]),
  "akiva.permission.CAMERA":          Object.freeze([]),
});

// Indexed by `LifecycleTransition` wire value from wasm_activity.rs.
const LIFECYCLE_EXPORTS = [
  "akiva_app_create",   // 0 — handled specially (see _dispatchCreate)
  "akiva_app_start",    // 1
  "akiva_app_resume",   // 2
  "akiva_app_pause",    // 3
  "akiva_app_stop",     // 4
  "akiva_app_destroy",  // 5
];
const LIFECYCLE_NAMES = ["create", "start", "resume", "pause", "stop", "destroy"];

const LEVEL_NAMES = ["trace", "debug", "info", "warn", "error"];

// Every app MUST expose these — see docs/abi.md §2.2. Missing any of them
// means the wasm wasn't built against sdk-rust (or was built for a
// different ABI) and we refuse to load it rather than discovering the
// gap at first lifecycle call.
const REQUIRED_EXPORTS = [
  "memory",
  "akiva_app_alloc",
  "akiva_app_dealloc",
  "akiva_app_abi_version",
  "akiva_app_create",
  "akiva_app_start",
  "akiva_app_resume",
  "akiva_app_pause",
  "akiva_app_stop",
  "akiva_app_destroy",
  "akiva_app_pointer",
  "akiva_app_key",
  "akiva_app_resize",
  "akiva_app_render",
  "akiva_app_save_state",
  "akiva_app_restore_state",
  "akiva_app_new_intent",
  "akiva_app_activity_result",
];

// Loud-and-obvious placeholder when an `@color/` ref is undefined.
// Mirrors `akiva_sdk::res::MISSING_COLOR`. Packed 0xAARRGGBB.
const MISSING_COLOR = 0xFFFF00FF;

// ── Tiny TOML parsers for values files ──────────────────────────────────
//
// The values files are deliberately trivial — flat `key = "value"` pairs,
// `#` comments, nothing else. A real TOML parser on the worker side would
// pull in ~10 KB of JS for a grammar we never exercise. The framework
// (Rust side) uses the full `toml` crate, which catches anything our
// hand rolled parser misses and reports via `BootReport.resource_errors`.

/** Iterate non-comment, non-blank lines. Strips leading/trailing
 * whitespace and cuts trailing `#` comments — but only when the hash
 * sits outside a double-quoted string. The naive `indexOf("#")` would
 * have eaten `"#ff15172a"` as a comment; colour literals start with
 * `#` and live inside quotes, so the scanner has to track quote
 * state. TOML doesn't permit `"` inside a `"..."` literal without an
 * escape, so a simple `\"` check is sufficient. */
function* valuesLines(src) {
  for (let raw of src.split(/\r?\n/)) {
    raw = stripTrailingComment(raw).trim();
    if (raw.length > 0) yield raw;
  }
}

function stripTrailingComment(line) {
  let inQuote = false;
  for (let i = 0; i < line.length; i++) {
    const c = line[i];
    if (c === '\\' && inQuote) { i++; continue; }  // skip escaped char inside quote
    if (c === '"') { inQuote = !inQuote; continue; }
    if (c === '#' && !inQuote) return line.slice(0, i);
  }
  return line;
}

/** Parse `key = "value"` lines into `target`. `onBad(key, reason)` runs
 * for malformed rows instead of throwing — one bad line shouldn't mask
 * the rest of the file. */
function parseStringsToml(src, target, onBad) {
  for (const line of valuesLines(src)) {
    const m = line.match(/^([A-Za-z_][A-Za-z0-9_]*)\s*=\s*"((?:[^"\\]|\\.)*)"\s*$/);
    if (!m) { onBad(line, `doesn't match key = "value"`); continue; }
    // Strings values only need to unescape the pair we emit ourselves;
    // the framework-side full parser validates anything more exotic.
    const value = m[2].replace(/\\"/g, '"').replace(/\\\\/g, "\\");
    target.set(m[1], value);
  }
}

function parseColorsToml(src, target, onBad) {
  for (const line of valuesLines(src)) {
    const m = line.match(/^([A-Za-z_][A-Za-z0-9_]*)\s*=\s*"([^"]+)"\s*$/);
    if (!m) { onBad(line, `doesn't match key = "value"`); continue; }
    const argb = parseColorLiteral(m[2]);
    if (argb === null) {
      onBad(m[1], `'${m[2]}' isn't a valid color`);
      continue;
    }
    target.set(m[1], argb);
  }
}

/** Parse `#RGB`, `#RRGGBB`, `#AARRGGBB`, or `0xAARRGGBB` to a 32-bit
 * ARGB integer. Mirrors `framework::resources::parse_color_literal`. */
function parseColorLiteral(s) {
  const trimmed = s.trim();
  let hex;
  if (trimmed.startsWith("0x")) {
    hex = trimmed.slice(2);
  } else if (trimmed.startsWith("#")) {
    hex = trimmed.slice(1);
  } else {
    return null;
  }
  if (!/^[0-9a-fA-F]+$/.test(hex)) return null;
  if (hex.length === 3) {
    const r = parseInt(hex[0], 16), g = parseInt(hex[1], 16), b = parseInt(hex[2], 16);
    return (0xff << 24) | ((r * 0x11) << 16) | ((g * 0x11) << 8) | (b * 0x11);
  }
  if (hex.length === 6) return (0xff000000 | parseInt(hex, 16)) >>> 0;
  if (hex.length === 8) return parseInt(hex, 16) >>> 0;
  return null;
}
