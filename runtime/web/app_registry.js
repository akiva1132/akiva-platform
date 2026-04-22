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
    this._decoder = new TextDecoder("utf-8");
    this._encoder = new TextEncoder();

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
    return {
      env: {
        akiva_host_log: (level, ptr, len) => {
          const proc = self._processes.get(packageId);
          if (!proc?.memory) return;
          const text = self._decoder.decode(new Uint8Array(proc.memory.buffer, ptr, len));
          self._log(LEVEL_NAMES[level] ?? "info", `[${packageId}] ${text}`);
        },
        akiva_host_text_width: (ptr, len, size) => {
          const proc = self._processes.get(packageId);
          // A length of 0 is a legitimate call (empty label) and should
          // return 0 without touching memory.
          if (len === 0) return 0;
          if (!proc?.memory) return 0;
          const text = self._decoder.decode(new Uint8Array(proc.memory.buffer, ptr, len));
          return self.measureTextWidth(text, size);
        },
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

        // Stone 9.3 — app-initiated finish. App calls this from inside a
        // lifecycle or input callback; kernel drains the flag immediately
        // afterward via `hal_app_drain_finish`. The handle identifies
        // which activity of this package is asking — two handles for the
        // same package wouldn't conflate.
        akiva_host_finish: (handle) => {
          self._pendingFinish.add(handle);
        },

        // Stone 9.4 — app-initiated start_activity. `ptr..ptr+len` is
        // the encoded Intent blob in app memory. Copy it out
        // immediately: the app's memory buffer can detach on any
        // subsequent wasm call (including the final drain), and we'll
        // be serving it back through `drainIntent` which runs on the
        // kernel's schedule. Overwrites any previously-staged intent
        // for this handle — a second start_activity in the same
        // dispatch wins, matching the single-slot `launch_requested`
        // field on the framework side.
        akiva_host_start_activity: (handle, ptr, len) => {
          const proc = self._processes.get(packageId);
          if (!proc?.memory || len === 0) return;
          const copy = new Uint8Array(len);
          copy.set(new Uint8Array(proc.memory.buffer, ptr, len));
          self._pendingIntent.set(handle, copy);
        },
      },
    };
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
