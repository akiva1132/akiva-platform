// App runtime registry — Stone 11.4c WorkerPool.
//
// Each installed package runs as its own dedicated `Worker` (one
// `app_worker_shell.js` per package), spawned by the kernel-worker
// on first `register`. The kernel never touches app memory directly —
// it talks to us through the `hal_app_*` imports, and we route each
// call to the right app worker by a `u32` handle we assign.
//
// Pre-11.4c the same class held `WebAssembly.Instance`s in this
// worker; everything ran synchronously inside a single thread. Post-
// 11.4c the seam shifts: `dispatch` posts to a peer worker, returns
// a kernel-side `future_id` immediately, and the kernel polls via
// `hal_app_poll_future` until the worker replies with the bytes
// (RENDER, SAVE_STATE) or the empty ack (every other cmd_kind).
//
// One `Worker` per package, shared across all that package's activity
// instances; one `handle` per framework-side activity. The Worker
// lives until the `unregister` of the last handle (today: no GC; the
// Worker stays alive — Stone 11.5 adds the cleanup).

import { fontString } from "./renderer/canvas2d.js";
import { STATUS, FIRE, IO } from "./op_kinds.mjs";

export class AppRegistry {
  /**
   * @param {object} deps
   * @param {{ get(key: string): Uint8Array | null }} deps.storage  Worker Storage (OPFS-backed).
   * @param {() => WebAssembly.Memory} deps.kernelMemory  Returns the live kernel memory reference; re-read every call because wasm memory can grow and the ArrayBuffer detaches.
   * @param {(level: string, text: string) => void} deps.log  Logger (level name, not numeric code).
   * @param {(packageId: string) => Worker} [deps.spawnWorker]  Override for tests; defaults to `new Worker("./app_worker_shell.js", {type: "module"})`.
   */
  constructor({ storage, kernelMemory, log, spawnWorker }) {
    this._storage = storage;
    this._kernelMemory = kernelMemory;
    this._log = log;
    this._spawnWorker = spawnWorker ?? defaultSpawnWorker;

    /** @type {Map<number, {packageId: string, class: string}>} */
    this._slots = new Map();
    /**
     * One worker per installed package. Spawned lazily on first
     * `register`; never reaped until 11.5's lifecycle GC. The Worker
     * may still be in `init` (wasm compile in flight) when its first
     * `dispatch` is posted — postMessage's FIFO ordering inside a
     * single Worker guarantees `init` is processed first, so callers
     * never observe the gap.
     * @type {Map<string, {worker: Worker, packageId: string}>}
     */
    this._processes = new Map();
    this._nextHandle = 1;
    /**
     * Pending app-raised `finish` signals. Populated when the app
     * calls `akiva_host_fire(FIRE::FINISH, …)` (now in the app
     * worker; the worker forwards as `{type:"side_effect"}` and we
     * stage here). Drained by `drainFinish(handle)` from the
     * kernel's `hal_app_drain_finish` — same contract as 11.3.
     * @type {Set<number>}
     */
    this._pendingFinish = new Set();
    /** Stone 9.4 — handles with a pending encoded `Intent` blob. @type {Map<number, Uint8Array>} */
    this._pendingIntent = new Map();
    /** Stone 9.5 — pending `start_activity_for_result` requests. @type {Map<number, { bytes: Uint8Array, requestCode: number }>} */
    this._pendingStartForResult = new Map();
    /** Stone 9.5 — bytes the activity stashed via `set_result`. @type {Map<number, Uint8Array>} */
    this._pendingSetResult = new Map();
    /** @type {Set<string>} */
    this._debug = new Set();
    /**
     * Stone 10.3b — permission grants per package, pushed via
     * `setGrants`. Stored on this side so we can include them in the
     * Worker's `init` message; the dispatcher inside the worker is
     * the actual gate.
     * @type {Map<string, Set<string>>}
     */
    this._grants = new Map();
    /**
     * Stone 11.0a — opaque snapshot of the installed-packages list.
     * Same as 11.3: stored as a flat byte blob, served back to apps
     * via the QUERY_PACKAGES io op (round-tripped through the
     * worker as a `host_op` in 11.4c-3).
     * @type {Uint8Array}
     */
    this._packagesBlob = new Uint8Array(0);
    /**
     * Stone 11.5 — packages whose worker reported a trap (uncaught
     * exception in `akiva_app_dispatch`, `worker.onerror`, or the
     * watchdog timeout in 11.5c). The kernel pulls this once per
     * frame via `hal_drain_trapped_packages`; we copy the entries
     * out and clear on a fitting drain (single-shot). Stored as a
     * Set so repeated traps from the same package collapse to one
     * notification — `mark_package_trapped` is idempotent on the
     * framework side anyway.
     * @type {Set<string>}
     */
    this._pendingTraps = new Set();
    this._decoder = new TextDecoder("utf-8");
    this._encoder = new TextEncoder();

    /**
     * Stone 11.4c — kernel-side cache for in-flight dispatches.
     * Populated when the kernel calls `dispatch` (status=PENDING,
     * bytes=null) and updated on each `dispatch_done` from the
     * worker (status=READY/ERROR, bytes=Uint8Array|null). Drained
     * by `getResult` (single-shot consume — the framework polls
     * every future_id at most once via `WasmHost::poll_future`).
     *
     * @type {Map<bigint, { status: number, bytes: Uint8Array | null }>}
     */
    this._dispatches = new Map();
    /** Monotonic kernel-side dispatch_id allocator. Starts at 1; 0 reserved as the "rejected" sentinel `dispatch` returns when the handle is unknown. */
    this._nextDispatchId = 1n;

    // Dedicated 1×1 context used only for `measureText`. Sharing the
    // renderer's main context would work but ties measurement to
    // display readiness — an app can call `text_width` before the
    // display has attached. Keeping it separate also lets us `font=`
    // it freely without thrashing the renderer's state.
    //
    // Constructed lazily on the first `measureTextWidth` call so the
    // constructor itself has no `OffscreenCanvas` reference — tests
    // running under vitest jsdom can build a registry without a DOM
    // shim, as long as they don't exercise the measure path.
    this._measureCanvas = null;
    this._measureCtx = null;
  }

  /**
   * Replace the active debug-category Set. `["*"]` enables everything;
   * `[]` disables all debug output. Overrides rather than merges so a
   * second call is a reset, not an accumulation.
   */
  setDebugCategories(categories) {
    this._debug = new Set(categories);
  }

  /**
   * Stone 10.3b — record the full list of permissions granted to a
   * package. Called once per installed package at boot, and again on
   * upgrade. Last-write-wins. The grants are passed to the worker via
   * the `init` message; in-flight workers receive a `set_grants`
   * update so a re-grant doesn't require a respawn.
   *
   * @param {string} packageId
   * @param {string[]} permissionNames
   */
  setGrants(packageId, permissionNames) {
    const set = new Set(permissionNames);
    this._grants.set(packageId, set);
    const proc = this._processes.get(packageId);
    if (proc) {
      proc.worker.postMessage({ type: "set_grants", grants: [...set] });
    }
    this._dbg("permissions", `${packageId} grants: [${permissionNames.join(", ")}]`);
  }

  /**
   * Stone 11.0a — replace the cached package-list snapshot. Stored
   * here for cross-worker host_op routing in 11.4c-3 (any worker
   * doing QUERY_PACKAGES sends a `host_op` to this side; we serve
   * back the bytes verbatim).
   *
   * @param {Uint8Array} bytes
   */
  setPackages(bytes) {
    this._packagesBlob = bytes instanceof Uint8Array ? bytes : new Uint8Array(0);
    // Push the new snapshot to every running worker so its local
    // QUERY_PACKAGES handler returns up-to-date data without a kernel
    // round-trip. setPackages is rare (boot + install/uninstall), so
    // a fan-out broadcast here is cheaper than the per-call host_op
    // we'd need otherwise.
    for (const proc of this._processes.values()) {
      proc.worker.postMessage({
        type: "set_packages",
        packagesBlob: this._packagesBlob.slice(),
      });
    }
    this._dbg("permissions", `packages snapshot: ${this._packagesBlob.length} bytes`);

    // Stone 11.4c — defer-warmup. Pre-spawn a Worker for every
    // installed package so the first user-initiated launch pays
    // only the dispatch round-trip, not the cold worker + wasm
    // compile cost (saved ~2s per app on the live demo).
    //
    // setTimeout(0) puts the loop after the current task — the
    // boot screen / first paint go through unimpeded; warmup
    // bills against idle time afterward. Each worker compiles
    // its wasm in parallel on its own thread, so total wall-clock
    // is dominated by the slowest single compile (~150KB).
    setTimeout(() => this._warmupWorkers(), 0);
  }

  /**
   * Stone 11.4c — proactively spawn workers for every installed
   * package so the first launch is hot. Driven from the package
   * snapshot the framework hands us via `setPackages`; missing
   * wasm in storage is a benign skip (`_spawnProcess` logs and
   * returns null). Idempotent: re-running is a no-op for already-
   * spawned packages.
   */
  _warmupWorkers() {
    const decoder = this._decoder;
    const blob = this._packagesBlob;
    if (blob.length < 4) return;
    const view = new DataView(blob.buffer, blob.byteOffset, blob.byteLength);
    const count = view.getUint32(0, true);
    let off = 4;
    for (let i = 0; i < count; i++) {
      // Wire format mirrors framework/src/pkgs_wire.rs::encode:
      //   [u16 LE id_len][id][u16 LE class_len][class]
      //   [u16 LE name_len][name]
      // We only need the id; skip the rest.
      if (off + 2 > blob.length) return;
      const idLen = view.getUint16(off, true); off += 2;
      if (off + idLen > blob.length) return;
      const id = decoder.decode(blob.subarray(off, off + idLen));
      off += idLen;
      // Skip class + name to advance to the next record.
      if (off + 2 > blob.length) return;
      const classLen = view.getUint16(off, true); off += 2 + classLen;
      if (off + 2 > blob.length) return;
      const nameLen = view.getUint16(off, true); off += 2 + nameLen;

      if (!this._processes.has(id)) {
        // Fire-and-forget; failures log inside _spawnProcess.
        this._spawnProcess(id);
      }
    }
  }

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

    let proc = this._processes.get(packageId);
    if (!proc) {
      proc = this._spawnProcess(packageId);
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
    // monotonic) so a stale flag would be inert anyway.
    this._pendingFinish.delete(handle);
    this._pendingIntent.delete(handle);
    this._pendingStartForResult.delete(handle);
    this._pendingSetResult.delete(handle);
    // The worker is left running even when no slots reference it —
    // mirrors the pre-11.4c policy where one in-memory `WebAssembly.
    // Instance` survived per package. Stone 11.5 adds the GC pass.
  }

  drainFinish(handle) {
    const had = this._pendingFinish.delete(handle);
    if (had) this._dbg("intent", `drain_finish[handle=${handle}] → true`);
    return had;
  }

  drainIntent(handle) {
    const bytes = this._pendingIntent.get(handle);
    if (bytes) this._dbg("intent", `drain_intent[handle=${handle}] → ${bytes.length} bytes`);
    return bytes ?? null;
  }
  _consumeIntent(handle) { this._pendingIntent.delete(handle); }

  drainStartForResult(handle) {
    const entry = this._pendingStartForResult.get(handle);
    if (entry) {
      this._dbg("intent", `drain_start_for_result[handle=${handle}] → ${entry.bytes.length} bytes, rc=${entry.requestCode}`);
    }
    return entry ?? null;
  }
  _consumeStartForResult(handle) { this._pendingStartForResult.delete(handle); }

  drainSetResult(handle) {
    const bytes = this._pendingSetResult.get(handle);
    if (bytes) this._dbg("intent", `drain_set_result[handle=${handle}] → ${bytes.length} bytes`);
    return bytes ?? null;
  }
  _consumeSetResult(handle) { this._pendingSetResult.delete(handle); }

  /**
   * Stone 11.5 — copy the LF-separated UTF-8 list of package_ids
   * that have trapped since the last drain into kernel memory at
   * `outPtr`. Two-phase buffer: returns the *needed* length (so the
   * kernel can realloc and retry) and only evicts entries on a
   * fitting copy.
   *
   * Called once per frame from `akiva_frame` via the
   * `hal_drain_trapped_packages` HAL import.
   */
  drainTrappedPackages(outPtr, outCap) {
    if (this._pendingTraps.size === 0) return 0;
    const entries = [...this._pendingTraps];
    const blob = this._encoder.encode(entries.join("\n"));
    if (blob.length > outCap) {
      // Two-phase retry: keep the entries, report required size.
      return blob.length;
    }
    const kernelBuf = this._kernelMemory().buffer;
    new Uint8Array(kernelBuf, outPtr, blob.length).set(blob);
    this._pendingTraps.clear();
    this._dbg("lifecycle", `drain_trapped_packages → [${entries.join(", ")}]`);
    return blob.length;
  }

  // ── Stone 11.4c: dispatch / pollFuture / getResult ─────────────────────

  /**
   * Post a host→app command to the package's worker and return a
   * BigInt `future_id` the kernel polls until READY/ERROR.
   *
   * The kernel-side payload always starts with `[handle: u32 LE, ...]`,
   * so we read the handle to find the package, then forward the
   * payload as-is. CREATE is special-cased here: the kernel only
   * emits `[handle]` (it doesn't know the class string), so we
   * splice the encoded class onto the payload before forwarding,
   * matching the legacy `_dispatchCreate` path.
   *
   * Returns `0n` if the handle is unknown (caller never gets a
   * future to poll). Otherwise returns the kernel-side dispatch_id.
   */
  dispatch(cmdKind, payloadPtr, payloadLen) {
    if (payloadLen < 4) return 0n;
    const kernelBuf = this._kernelMemory().buffer;
    const handle = new DataView(kernelBuf).getUint32(payloadPtr, true);
    const slot = this._slots.get(handle);
    if (!slot) return 0n;
    const proc = this._processes.get(slot.packageId);
    if (!proc) return 0n;

    // CREATE augmentation: append the class string after the handle.
    let payload;
    if (cmdKind === 0x01 /* CMD.CREATE */) {
      const cls = this._encoder.encode(slot.class);
      payload = new Uint8Array(4 + cls.length);
      payload.set(new Uint8Array(kernelBuf, payloadPtr, 4), 0);
      payload.set(cls, 4);
    } else {
      payload = this._copyKernelSlice(payloadPtr, payloadLen);
    }

    const dispatchId = this._nextDispatchId++;
    this._dispatches.set(dispatchId, { status: STATUS.PENDING, bytes: null });
    proc.worker.postMessage(
      { type: "dispatch", dispatch_id: dispatchId, cmd_kind: cmdKind, payload },
      [payload.buffer],
    );
    this._dbg("dispatch",
      `${slot.packageId}[handle=${handle}] cmd=0x${cmdKind.toString(16)} → dispatch_id=${dispatchId}`);
    return dispatchId;
  }

  /**
   * Poll the kernel-side cache for a future started via `dispatch`.
   * The `hal_app_poll_future` import returns this value verbatim;
   * the kernel's framework state machine only calls `get_result`
   * after seeing READY here.
   */
  pollFuture(futureId) {
    const fid = typeof futureId === "bigint" ? futureId : BigInt(futureId);
    const entry = this._dispatches.get(fid);
    if (!entry) return STATUS.ERROR;
    return entry.status;
  }

  /**
   * Copy the result bytes for a READY dispatch into kernel memory.
   * Two-phase buffer contract: returns the *needed* length; if
   * `len > outCap` the caller reallocates and retries — the entry
   * stays alive across that retry. A successful copy (or a copy of
   * 0 bytes for ack-only cmds) evicts the entry — single-shot.
   */
  getResult(futureId, outPtr, outCap) {
    const fid = typeof futureId === "bigint" ? futureId : BigInt(futureId);
    const entry = this._dispatches.get(fid);
    if (!entry) return 0;
    if (entry.status === STATUS.PENDING) return 0;  // shouldn't happen — caller checks pollFuture first
    const bytes = entry.bytes;
    const len = bytes ? bytes.length : 0;
    if (len > outCap) return len;
    if (len > 0) {
      const kernelBuf = this._kernelMemory().buffer;
      new Uint8Array(kernelBuf, outPtr, len).set(bytes);
    }
    this._dispatches.delete(fid);
    return len;
  }

  // ── Internals ───────────────────────────────────────────────────────────

  /**
   * Spawn the per-package Worker, post the `init` message, and wire
   * its inbound channel. Synchronous from the kernel's POV — the
   * Worker may still be compiling the wasm when `register` returns,
   * but postMessage FIFO guarantees the first `dispatch` arrives
   * after `init`.
   */
  _spawnProcess(packageId) {
    const wasmBytes = this._storage.get(`packages/${packageId}/app.wasm`);
    if (!wasmBytes) {
      this._log("warn", `app register: no wasm at packages/${packageId}/app.wasm`);
      return null;
    }

    const resources = this._loadResources(packageId);
    const grants = [...(this._grants.get(packageId) ?? new Set())];

    let worker;
    try {
      worker = this._spawnWorker(packageId);
    } catch (err) {
      this._log("error", `app ${packageId}: spawn worker failed: ${err?.message ?? err}`);
      return null;
    }
    worker.onerror = (e) => {
      // Stone 11.5 — uncaught error in the worker (typically a
      // wasm trap that escaped our try/catch in the shell, or a JS
      // syntax error). Stage the package for the kernel's frame
      // drain; the framework will mark every activity Trapped on
      // the next `akiva_frame`.
      this._log("error", `app ${packageId}: worker.onerror: ${e?.message ?? e}`);
      this._pendingTraps.add(packageId);
    };
    worker.addEventListener("message", (e) => this._onWorkerMessage(packageId, e.data));

    // wasmBytes ownership: `Storage.get` returns the SAME Uint8Array
    // it stored, NOT a fresh copy — transferring its buffer would
    // detach the live storage entry, and the next register() for
    // the same package would crash with "ArrayBuffer is already
    // detached." Slice into a fresh buffer so the transfer only
    // affects our copy. Cost is one extra wasm-bytes-sized memcpy
    // per spawn, paid once per package since `_processes` caches.
    //
    // packagesBlob is also sliced — every worker gets its own
    // snapshot copy and the kernel keeps the canonical one for
    // future broadcasts.
    const wasmCopy = wasmBytes.slice();
    worker.postMessage(
      {
        type: "init",
        packageId,
        wasmBytes: wasmCopy,
        grants,
        resources,
        packagesBlob: this._packagesBlob.slice(),
      },
      [wasmCopy.buffer],
    );

    const proc = { worker, packageId, lastPongAt: 0, ready: false, pingInterval: null };
    this._processes.set(packageId, proc);
    // Heartbeat doesn't start until `init_ok` lands — kicking the
    // watchdog while the worker is mid-wasm-compile would mean every
    // cold start trips a false-positive terminate at the first
    // 1.5s tick.
    return proc;
  }

  /**
   * Stone 11.5c — JS-level ping/pong watchdog. Sends a `ping` every
   * 500ms; if no `pong` arrives within 1.5s the worker is treated
   * as hung (typically a `loop {}` inside an `akiva_app_dispatch`
   * call — the worker's single thread can't pump message events
   * while wasm runs). We `worker.terminate()` from the kernel side
   * (browser kills the OS thread), stage the package in the trap
   * queue for the next frame's drain, and let `Framework::
   * mark_package_trapped` do the rest.
   *
   * Why JS-level pong, not wasm: a wasm app's job is to process
   * dispatches, not to acknowledge heartbeats. Routing the pong
   * through wasm would put the heartbeat's liveness on the same
   * thread that we're trying to detect as wedged — pointless.
   */
  _startHeartbeat(proc) {
    proc.pingInterval = setInterval(() => {
      const now = performance.now();
      if (now - proc.lastPongAt > 1500) {
        this._log("error",
          `app ${proc.packageId}: no pong in ${(now - proc.lastPongAt).toFixed(0)}ms — terminating worker`);
        clearInterval(proc.pingInterval);
        try { proc.worker.terminate(); } catch (_) { /* best effort */ }
        this._pendingTraps.add(proc.packageId);
        this._processes.delete(proc.packageId);
        // _dispatches entries for the dead worker stay PENDING. They
        // can never resolve (no worker to reply), but they also can't
        // be polled — `dispatch()` returns 0n for unknown packages and
        // any in-flight pending_render slot in WasmActivity points at
        // a future_id that nobody else will create. The framework
        // marks the activity Trapped on the next frame's drain and
        // pops it off the stack; the orphaned _dispatches entry is
        // bounded by the activity count of the terminated worker.
        // Stone 11.5+1 GC pass will recycle them.
        return;
      }
      proc.worker.postMessage({ type: "ping", t: now });
    }, 500);
  }

  /**
   * Read the package's resource tables from storage and parse them
   * into plain-object form for the worker's init payload. Errors
   * are logged but don't block the spawn — a broken `colors.toml`
   * just means `@color/` refs return magenta.
   */
  _loadResources(packageId) {
    const strings = {};
    const colors = {};
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
    return { strings, colors };
  }

  /**
   * Inbound message handler for a peer Worker. Routes by `type`:
   * `dispatch_done` populates `_dispatches`; `side_effect` stages
   * into the matching `_pending*` map; `host_op` is wired in
   * 11.4c-3; `log` and `trapped` flow through the registry log.
   */
  _onWorkerMessage(packageId, msg) {
    if (!msg || typeof msg.type !== "string") return;
    switch (msg.type) {
      case "init_ok": {
        this._dbg("lifecycle", `${packageId}: worker init_ok`);
        // Stone 11.5c — only now do we arm the heartbeat. Before
        // init_ok the worker is busy compiling wasm; pings would
        // queue and the watchdog would terminate a perfectly
        // healthy worker at the first 1.5s tick.
        const proc = this._processes.get(packageId);
        if (proc && !proc.ready) {
          proc.ready = true;
          proc.lastPongAt = performance.now();
          this._startHeartbeat(proc);
        }
        break;
      }
      case "init_failed":
        this._log("error", `${packageId}: worker init_failed: ${msg.reason}`);
        break;
      case "dispatch_done": {
        const fid = typeof msg.dispatch_id === "bigint" ? msg.dispatch_id : BigInt(msg.dispatch_id);
        const entry = this._dispatches.get(fid);
        if (!entry) {
          this._log("warn", `${packageId}: dispatch_done for unknown id ${fid}`);
          return;
        }
        const futureId = typeof msg.future_id === "bigint" ? msg.future_id : BigInt(msg.future_id ?? 0);
        const bytes = msg.bytes instanceof Uint8Array ? msg.bytes
          : (msg.bytes ? new Uint8Array(msg.bytes) : null);
        // App-side future_id of 0 means "no result bytes" (ack-only
        // cmd). Any non-zero id and `bytes != null` means RENDER /
        // SAVE_STATE produced a stream.
        entry.status = STATUS.READY;
        entry.bytes = futureId !== 0n ? (bytes ?? null) : null;
        break;
      }
      case "side_effect":
        this._handleSideEffect(packageId, msg);
        break;
      case "host_op":
        this._handleHostOp(packageId, msg);
        break;
      case "pong": {
        // Stone 11.5c — heartbeat reply. Update the watchdog clock so
        // the next interval check sees the worker as alive.
        const proc = this._processes.get(packageId);
        if (proc) proc.lastPongAt = performance.now();
        break;
      }
      case "log":
        this._log(msg.level ?? "info", msg.text ?? "");
        break;
      case "trapped":
        this._log("error", `${packageId}: app trapped: ${msg.reason}`);
        // Stage for kernel pickup on next frame. mark_package_trapped
        // is idempotent on the framework side — if the same trap fires
        // multiple times before the kernel drains, the Set collapses.
        this._pendingTraps.add(packageId);
        break;
      default:
        this._log("warn", `${packageId}: unknown worker message type ${msg.type}`);
    }
  }

  /**
   * Stage the `_pending*` entry the legacy in-process `_dispatchFire`
   * handler used to populate. The kernel-side `hal_app_drain_*`
   * imports read these maps unchanged from pre-11.4c — only the
   * source moved (from in-process import callbacks to peer-worker
   * messages).
   */
  _handleSideEffect(packageId, msg) {
    const args = msg.args instanceof Uint8Array ? msg.args : new Uint8Array(msg.args ?? 0);
    const view = new DataView(args.buffer, args.byteOffset, args.byteLength);
    switch (msg.op_kind) {
      case FIRE.FINISH: {
        if (args.length < 4) return;
        const handle = view.getUint32(0, true);
        this._pendingFinish.add(handle);
        this._dbg("intent", `${packageId}: side_effect FINISH handle=${handle}`);
        return;
      }
      case FIRE.START_ACTIVITY: {
        if (!this._hasGrant(packageId, "akiva.permission.LAUNCH_ANY_APP")) {
          this._log("warn", `[${packageId}] side_effect START_ACTIVITY: denied — no LAUNCH_ANY_APP grant`);
          return;
        }
        if (args.length < 4) return;
        const handle = view.getUint32(0, true);
        const intentBytes = args.slice(4);
        if (intentBytes.length === 0) return;
        this._pendingIntent.set(handle, intentBytes);
        this._dbg("intent", `${packageId}: side_effect START_ACTIVITY handle=${handle} bytes=${intentBytes.length}`);
        return;
      }
      case FIRE.START_ACTIVITY_FOR_RESULT: {
        if (!this._hasGrant(packageId, "akiva.permission.LAUNCH_ANY_APP")) {
          this._log("warn", `[${packageId}] side_effect START_ACTIVITY_FOR_RESULT: denied — no LAUNCH_ANY_APP grant`);
          return;
        }
        if (args.length < 8) return;
        const handle = view.getUint32(0, true);
        const requestCode = view.getUint32(4, true);
        const intentBytes = args.slice(8);
        if (intentBytes.length === 0) return;
        this._pendingStartForResult.set(handle, { bytes: intentBytes, requestCode });
        this._dbg("intent", `${packageId}: side_effect START_ACTIVITY_FOR_RESULT handle=${handle} rc=${requestCode}`);
        return;
      }
      case FIRE.SET_RESULT: {
        if (args.length < 4) return;
        const handle = view.getUint32(0, true);
        const resultBytes = args.slice(4);
        if (resultBytes.length === 0) return;
        this._pendingSetResult.set(handle, resultBytes);
        this._dbg("intent", `${packageId}: side_effect SET_RESULT handle=${handle} bytes=${resultBytes.length}`);
        return;
      }
      default:
        this._log("warn", `[${packageId}] unknown side_effect op_kind=0x${(msg.op_kind ?? 0).toString(16)}`);
    }
  }

  _hasGrant(packageId, capability) {
    return this._grants.get(packageId)?.has(capability) ?? false;
  }

  /**
   * Stone 11.4c-3 — resolve a `host_op` request raised by an app
   * worker that needed a kernel-served io op (QUERY_PACKAGES,
   * STORAGE_GET, STORAGE_SET). Capability gating happens here
   * (mirrors the legacy single-worker dispatcher); the result is
   * posted back via `host_op_done` so the worker's own `io_poll`
   * resolves on the next tick.
   */
  _handleHostOp(packageId, msg) {
    const futureId = typeof msg.future_id === "bigint" ? msg.future_id : BigInt(msg.future_id ?? 0);
    const proc = this._processes.get(packageId);
    if (!proc) return;

    let status = STATUS.READY;
    let payload = new Uint8Array(0);

    switch (msg.op_kind) {
      case IO.QUERY_PACKAGES: {
        if (!this._hasGrant(packageId, "akiva.permission.QUERY_PACKAGES")) {
          this._log("warn", `[${packageId}] host_op QUERY_PACKAGES: denied — no QUERY_PACKAGES grant`);
          status = STATUS.ERROR;
          break;
        }
        // Slice so a later `setPackages` reassignment can't mutate
        // the staged payload out from under a pending poll.
        payload = this._packagesBlob.slice();
        break;
      }
      case IO.STORAGE_GET:
      case IO.STORAGE_SET: {
        if (!this._hasGrant(packageId, "akiva.permission.STORAGE_SANDBOX")) {
          this._log("warn", `[${packageId}] host_op STORAGE_*: denied — no STORAGE_SANDBOX grant`);
          status = STATUS.ERROR;
          break;
        }
        // Stone 11.6 wires the actual handler. Until then, ERROR.
        status = STATUS.ERROR;
        break;
      }
      default:
        this._log("warn", `[${packageId}] host_op unknown op_kind=0x${(msg.op_kind ?? 0).toString(16)}`);
        status = STATUS.ERROR;
    }

    proc.worker.postMessage(
      { type: "host_op_done", future_id: futureId, status, payload },
      [payload.buffer],
    );
  }

  /**
   * Copy `len` bytes out of kernel memory at `ptr` into an owned
   * `Uint8Array`. Necessary before any postMessage that may detach
   * the kernel buffer — `WebAssembly.Memory.grow` on either side
   * invalidates existing views.
   */
  _copyKernelSlice(ptr, len) {
    const kernelBuf = this._kernelMemory().buffer;
    const copy = new Uint8Array(len);
    copy.set(new Uint8Array(kernelBuf, ptr, len));
    return copy;
  }
}

// ── Worker spawn helper ─────────────────────────────────────────────────

function defaultSpawnWorker(_packageId) {
  // `import.meta.url` resolves to the kernel-worker's URL; the shell
  // sits next to it under runtime/web/. Module workers let the shell
  // `import` the shared op_kinds + renderer modules without bundling.
  return new Worker(new URL("./app_worker_shell.js", import.meta.url), { type: "module" });
}

// ── Constants ────────────────────────────────────────────────────────────

// Stone 11.3 / 11.4c — every app gets resource lookups + the unified-ABI
// trio. Capability gating happens inside the worker's dispatcher (one
// import serves many op_kinds), not at the import surface — kernel-side
// AppRegistry no longer builds imports at all (the worker does).
//
// AUTO_IMPORTS is preserved as the cross-side parity contract: the
// Rust `framework::permissions::AUTO_IMPORTS` round-trips against
// this list in the vitest mirror test.
export const AUTO_IMPORTS = Object.freeze([
  "akiva_host_resource_color",
  "akiva_host_resource_string",
  "akiva_host_resource_has_color",
  "akiva_host_resource_has_string",
  "akiva_host_fire",
  "akiva_host_io_start",
  "akiva_host_io_poll",
]);

// Capability → gated imports. Empty everywhere after 11.3 — gating
// happens inside the unified-ABI dispatcher inside the worker. The
// map is preserved as a tripwire: any future capability that lands a
// per-op import (e.g. a streaming surface that doesn't fit fire/io)
// must register here, otherwise the cross-side mirror test fails.
export const CAPABILITY_IMPORTS = Object.freeze({
  "akiva.permission.LAUNCH_ANY_APP":  Object.freeze([]),
  "akiva.permission.QUERY_PACKAGES":  Object.freeze([]),
  "akiva.permission.STORAGE_SANDBOX": Object.freeze([]),
  "akiva.permission.NOTIFICATIONS":   Object.freeze([]),
  "akiva.permission.NETWORK":         Object.freeze([]),
  "akiva.permission.CAMERA":          Object.freeze([]),
});

// ── Tiny TOML parsers for values files ──────────────────────────────────
//
// The values files are deliberately trivial — flat `key = "value"` pairs,
// `#` comments, nothing else. A real TOML parser on the worker side would
// pull in ~10 KB of JS for a grammar we never exercise. The framework
// (Rust side) uses the full `toml` crate, which catches anything our
// hand rolled parser misses and reports via `BootReport.resource_errors`.

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
    if (c === '\\' && inQuote) { i++; continue; }
    if (c === '"') { inQuote = !inQuote; continue; }
    if (c === '#' && !inQuote) return line.slice(0, i);
  }
  return line;
}

function parseStringsToml(src, target, onBad) {
  for (const line of valuesLines(src)) {
    const m = line.match(/^([A-Za-z_][A-Za-z0-9_]*)\s*=\s*"((?:[^"\\]|\\.)*)"\s*$/);
    if (!m) { onBad(line, `doesn't match key = "value"`); continue; }
    const value = m[2].replace(/\\"/g, '"').replace(/\\\\/g, "\\");
    target[m[1]] = value;
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
    target[m[1]] = argb;
  }
}

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
