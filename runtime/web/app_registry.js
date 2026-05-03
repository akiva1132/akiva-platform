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
import { WATCHDOG, effectiveWatchdog } from "./config.js";

/**
 * Package id of the system shell — the launcher activity the
 * framework brings up at boot. Hot-spawned by `_warmupWorkers` so
 * the first paint isn't gated on a wasm compile; every other
 * package spawns lazily via `register()` on first use (Stone
 * 11.6.7 #4).
 */
const HOME_PACKAGE_ID = "sys.shell";

export class AppRegistry {
  /**
   * @param {object} deps
   * @param {{ get(key: string): Uint8Array | null }} deps.storage  Worker Storage (OPFS-backed).
   * @param {() => WebAssembly.Memory} deps.kernelMemory  Returns the live kernel memory reference; re-read every call because wasm memory can grow and the ArrayBuffer detaches.
   * @param {(level: string, text: string) => void} deps.log  Logger (level name, not numeric code).
   * @param {(packageId: string) => Worker} [deps.spawnWorker]  Override for tests; defaults to `new Worker("./app_worker_shell.js", {type: "module"})`.
   * @param {(msg: object) => void} [deps.sendToTab]  Stone 14.3 — bridge to the main-thread tab for capabilities that the worker can't dispatch on its own (browser Notification API requires a Window context for `requestPermission`). Default no-op so tests that don't exercise notifications can construct an AppRegistry without supplying it.
   */
  constructor({ storage, kernelMemory, log, spawnWorker, sendToTab }) {
    this._storage = storage;
    this._kernelMemory = kernelMemory;
    this._log = log;
    this._spawnWorker = spawnWorker ?? defaultSpawnWorker;
    this._sendToTab = sendToTab ?? ((_msg) => {});
    /**
     * Stone 13.0prep C2 — renderer backend reference. AppRegistry calls
     * `registerDrawable(id, bitmap)` after async PNG decode and
     * `forgetDrawable(id)` on uninstall. Wired post-construction by
     * `worker.js` once the backend's surface is live (see `setBackend`).
     * Null-safe: drawables that resolve before the backend is wired
     * stay queued in `_pendingDrawableRegistrations` and flush on
     * `setBackend`.
     */
    this._backend = null;
    /** @type {Array<{id: number, bitmap: ImageBitmap}>} */
    this._pendingDrawableRegistrations = [];

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

    /**
     * Stone 11.6.7 #5 — per-package watchdog timeout overrides
     * threaded from `manifest.runtime.watchdog_timeout_ms`. The
     * kernel pushes one entry per package that declares the field
     * via `setWatchdogTimeoutMs(packageId, ms)`; absent entries fall
     * back to the global `WATCHDOG.TIMEOUT_MS` (or its
     * `globalThis.__akivaConfig.watchdog.TIMEOUT_MS` override).
     *
     * @type {Map<string, number>}
     */
    this._watchdogTimeoutMs = new Map();

    /**
     * Stone 11.6.9 #3 — current value of the night-mode device
     * config qualifier. `false` selects only `values/`; `true`
     * overlays `values-night/` on top of `values/` per-key.
     * Toggled by `setNightMode()` from the kernel-worker after
     * `tab.js` observes `matchMedia('(prefers-color-scheme: dark)')`.
     * @type {boolean}
     */
    this._nightMode = false;

    this._decoder = new TextDecoder("utf-8");
    this._encoder = new TextEncoder();

    /**
     * Stone 13.2 — pending dialog futures the kernel/framework has
     * accepted but the user hasn't resolved yet. Keyed by `handle`
     * since the framework only tracks one open dialog per activity
     * handle (see `framework::dialog::DialogManager`); the value is
     * the (packageId, workerFutureId) pair we route the eventual
     * `host_op_done` reply to once `hal_dialog_complete` fires.
     *
     * @type {Map<number, { packageId: string, futureId: bigint }>}
     */
    this._pendingDialogs = new Map();

    /**
     * Stone 14.1 — open WebSockets, keyed by the opaque handle we
     * hand back to apps on `IO::WS_OPEN`. Each entry pins:
     * - `ws`         the live `WebSocket` instance
     * - `packageId`  the app that owns the handle (rejected if any
     *                other worker tries to act on it)
     * - `recvQueue`  buffered events waiting for the next
     *                `IO::WS_RECV` (text/binary/closed). Capped at
     *                `WS_RECV_QUEUE_CAP`; overflow force-closes the
     *                socket with code 1009 ("message too big") per
     *                RFC 6455 §7.4.1.
     * - `pendingRecv` the (futureId) of an in-flight `WS_RECV` the
     *                app issued before any event arrived. At most
     *                one — the SDK serialises recvs by construction
     *                (each `recv()` `.await`s before issuing the
     *                next), but if a misbehaving app double-issues
     *                we drop the older one with ERROR.
     * - `closed`    `true` once a Closed event has been delivered;
     *                further sends are no-ops, further recvs return
     *                the cached close synthetically.
     *
     * @type {Map<number, { ws: WebSocket, packageId: string, recvQueue: Array<Uint8Array>, pendingRecv: { futureId: bigint } | null, closed: boolean }>}
     */
    this._wsHandles = new Map();
    /** Monotonic WebSocket handle allocator. Starts at 1; 0 is the "rejected" sentinel. */
    this._nextWsHandle = 1;

    /**
     * Stone 14.2 — open WebTransport sessions, keyed by handle.
     * Each entry pins:
     * - `wt`         the live `WebTransport` instance
     * - `packageId`  owning package (provenance)
     * - `dgQueue`    buffered datagrams (Uint8Array per entry)
     * - `pendingDg`  in-flight `WT_RECV_DATAGRAM` future awaiting
     *                the next datagram. At most one — the SDK
     *                serialises by construction.
     * - `closed`     `true` once a Closed event has been delivered
     * - `dgPumpAbort` AbortController that stops the datagram
     *                ReadableStream pump when the entry is reaped.
     *
     * @type {Map<number, { wt: WebTransport, packageId: string, dgQueue: Array<Uint8Array>, pendingDg: { futureId: bigint } | null, closed: boolean, dgPumpAbort: AbortController }>}
     */
    this._wtHandles = new Map();
    /**
     * Stone 14.2 — open bidirectional streams across all sessions.
     * Stream handles share the same monotonic counter as WS / WT
     * handles (no namespace collision because they live in different
     * maps and the SDK presents them through different types).
     *
     * - `reader`/`writer`  underlying `ReadableStreamDefaultReader`
     *                      and `WritableStreamDefaultWriter`
     * - `parentWt`         handle of the owning `WebTransport`
     * - `recvQueue`/`pendingRecv`  same queue / one-pending-recv
     *                      shape as WS
     * - `closed`           `true` once Fin or Error has been
     *                      delivered or [`fire::WT_CLOSE_STREAM`]
     *                      tore down the writer.
     *
     * @type {Map<number, { reader: any, writer: any, packageId: string, parentWt: number, recvQueue: Array<Uint8Array>, pendingRecv: { futureId: bigint } | null, closed: boolean }>}
     */
    this._wtStreams = new Map();

    /**
     * Stone 14.3 — pending notification ops awaiting tab reply.
     * Keyed by the in-flight `notify_id` we allocate here; value is
     * the (packageId, futureId) pair we route the eventual
     * `notify_done` / `notify_perm_done` reply to.
     *
     * @type {Map<number, { packageId: string, futureId: bigint, kind: "post" | "perm" }>}
     */
    this._pendingNotifs = new Map();
    /** Monotonic notify id allocator (host-side) shared with the tab side. */
    this._nextNotifyId = 1;

    /**
     * Stone 14.4 / 14.6 — pending main-thread cap requests
     * (clipboard read/write, geo get_position). Keyed by an
     * AppRegistry-allocated `cap_id` that the tab echoes back; value
     * is the routing entry. Distinct from `_pendingNotifs` because
     * notifications carry their own id namespace that the app holds
     * after the round-trip; cap requests are one-shot — the id is
     * internal-only.
     *
     * @type {Map<number, { packageId: string, futureId: bigint, kind: "clip_read" | "clip_write" | "geo_get" }>}
     */
    this._pendingCaps = new Map();
    /** Monotonic cap id allocator. Disjoint from notify ids by virtue
     *  of the separate map; the `type` field on each tab message is
     *  what disambiguates routing. */
    this._nextCapId = 1;
    /**
     * Stone 14.3 — owners of in-flight notifications, by id. Cleared
     * once the tab confirms `notify_post` succeeded (the id then
     * becomes meaningful) or fails (entry removed). `_handleNotifyClose`
     * uses this to verify the close handle belongs to the calling
     * package before forwarding to the tab.
     *
     * @type {Map<number, string>}  notify_id → packageId
     */
    this._notifyOwners = new Map();
    /**
     * Stone 13.2 — kernel-side bridge symbols. Looked up lazily on the
     * first dialog open after `setKernelInstance` has wired them in
     * from `worker.js`. Null until then; SHOW_DIALOG arriving before
     * boot returns a synthetic Dismissed.
     *
     * `akiva_open_dialog` signature: `(handle: u32, future_id: u64,
     * args_ptr: u32, args_len: u32) -> u32`. The `u64` future_id is
     * passed as a BigInt at the JS boundary.
     *
     * @type {null | { exports: { akiva_alloc(n: number): number, akiva_dealloc(p: number, n: number): void, akiva_open_dialog(handle: number, futureId: bigint, ptr: number, len: number): number } }}
     */
    this._kernelInstance = null;

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

    // Stone 13.0prep C2 — async drawable PNG decode. Walk the
    // package list and fire `_loadDrawableBitmaps` for each. Same
    // setTimeout(0) deferral so the first paint isn't blocked.
    // Decode is per-app independent and runs in parallel.
    setTimeout(() => this._loadAllDrawables(), 0);
  }

  /** Stone 13.0prep C2 — fan out drawable decode across every
   *  installed package the snapshot lists. Errors per-app are logged
   *  inside `_loadDrawableBitmaps`; the orchestrator just races them
   *  in parallel. */
  _loadAllDrawables() {
    const blob = this._packagesBlob;
    if (blob.length < 4) return;
    const decoder = this._decoder;
    const view = new DataView(blob.buffer, blob.byteOffset, blob.byteLength);
    const count = view.getUint32(0, true);
    let off = 4;
    for (let i = 0; i < count; i++) {
      if (off + 2 > blob.length) return;
      const idLen = view.getUint16(off, true); off += 2;
      if (off + idLen > blob.length) return;
      const id = decoder.decode(blob.subarray(off, off + idLen));
      off += idLen;
      if (off + 2 > blob.length) return;
      const classLen = view.getUint16(off, true); off += 2 + classLen;
      if (off + 2 > blob.length) return;
      const nameLen = view.getUint16(off, true); off += 2 + nameLen;
      // Fire-and-forget; failures inside log + skip.
      this._loadDrawableBitmaps(id).catch((err) =>
        this._log("warn", `[${id}] drawable decode pass failed: ${err?.message ?? err}`));
    }
  }

  /**
   * Stone 11.6.7 #4 — pre-spawn only the system shell (`sys.shell`).
   *
   * Pre-Stone 11.6.7, this method walked the entire `_packagesBlob`
   * and spawned a worker for every installed package on boot. With
   * 4 apps that was fine; with 100 apps it's 100 idle workers
   * burning ~10 MiB of wasm memory each before the user has clicked
   * anything. `register(packageId, ...)` already lazily spawns on
   * first use (see line ~276); the only worker that genuinely needs
   * to be hot at boot is the home, because the framework launches
   * it immediately and a cold compile delays the first paint.
   *
   * Now: scan the snapshot, find `sys.shell`, spawn it. Every other
   * package waits for `register` to demand a worker. Cold-start
   * cost (~150ms wasm compile per app) shifts to first-launch — a
   * fair price for not running 95 idle workers.
   *
   * Future refinement: persist a "recently-used apps" hint to
   * storage and pre-warm those too. Out of scope for this stone.
   */
  _warmupWorkers() {
    const blob = this._packagesBlob;
    if (blob.length < 4) return;
    // SCHEMA: this format is owned by `wire/src/pkgs.rs` in the
    // Rust workspace (Stone 11.6.7 #2). Both framework and SDK
    // pull encode/decode from `akiva-wire`; this JS reader mirrors
    // the same layout because it can't depend on a Rust crate.
    // Layout per entry:
    //   [u16 LE id_len][id][u16 LE class_len][class][u16 LE name_len][name]
    const decoder = this._decoder;
    const view = new DataView(blob.buffer, blob.byteOffset, blob.byteLength);
    const count = view.getUint32(0, true);
    let off = 4;
    for (let i = 0; i < count; i++) {
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

      if (id === HOME_PACKAGE_ID && !this._processes.has(id)) {
        // Pre-warm the home. Fire-and-forget; failures log inside
        // `_spawnProcess`. Every other package id is intentionally
        // skipped — it spawns lazily on its first `register` call.
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

    const tStart = performance.now();
    let proc = this._processes.get(packageId);
    const reused = !!proc;
    if (!proc) {
      proc = this._spawnProcess(packageId);
      if (!proc) return 0;
    }

    const handle = this._nextHandle++;
    this._slots.set(handle, { packageId, class: className });
    this._log("info",
      `[time] register(${packageId}/${className}) → handle ${handle} (${reused ? "reused" : "fresh-spawn"}, +${(performance.now() - tStart).toFixed(1)}ms)`);
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
    // Track packageId per dispatch so the watchdog/trap GC can evict
    // entries belonging to a terminated worker without touching
    // healthy peers' in-flight futures.
    this._dispatches.set(dispatchId, {
      status: STATUS.PENDING,
      bytes: null,
      packageId: slot.packageId,
      cmdKind,
      postedAt: performance.now(),
    });
    proc.worker.postMessage(
      { type: "dispatch", dispatch_id: dispatchId, cmd_kind: cmdKind, payload },
      [payload.buffer],
    );
    // [time] log first dispatch per package — the gap between init_ok
    // and first dispatch is host-side framework work (rehydrate, intent
    // resolve, etc.); the gap between first dispatch and dispatch_done
    // is the worker's first wasm execution.
    if (proc.spawnStart && !proc.firstDispatchAt) {
      proc.firstDispatchAt = performance.now();
      const dt = (proc.firstDispatchAt - proc.spawnStart).toFixed(1);
      this._log("info",
        `[time] ${slot.packageId}: first dispatch (cmd=0x${cmdKind.toString(16)}) at t=${dt}ms`);
    }
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

  /**
   * Phase 12.1 — last-known `WebAssembly.Memory.buffer.byteLength`
   * for `packageId`. Returns `0` when the package has no spawned
   * process or the process has not yet replied to a pong (init not
   * yet done, or first pong still in flight). Read by the kernel-
   * side `hal_app_memory_current` import once per frame.
   *
   * Cheap — O(1) Map lookup plus a property access. Memory readings
   * advance lazily through the heartbeat (every 500ms by default),
   * which is plenty for an accounting view that only changes on
   * `WebAssembly.Memory.grow`.
   */
  memoryCurrent(packageId) {
    const proc = this._processes.get(packageId);
    return proc?.memoryBytes ?? 0;
  }

  /**
   * Phase 12.2 — last-known cumulative CPU time (ms) for `packageId`.
   * Returns `0` when the package has no spawned process or the
   * worker has not yet replied to a pong. Read by the kernel-side
   * `hal_app_cpu_ms` import once per frame; the kernel computes the
   * delta and forwards through `Framework::record_app_cpu`.
   *
   * The reading is monotonic per worker lifetime — a worker restart
   * resets it to 0, which the kernel-side `CpuBudget` clamps as a
   * negative delta and ignores (no rate poison).
   */
  cpuMs(packageId) {
    const proc = this._processes.get(packageId);
    return proc?.cpuMs ?? 0;
  }

  /**
   * Phase 12.3 — terminate the worker for `packageId` on demand. The
   * Low-Memory Killer (kernel-side `drive_lmk_into`) calls this for
   * every victim it elects: same `worker.terminate()` + dispatch
   * cleanup as the watchdog kill path, **without** adding the package
   * to `_pendingTraps`.
   *
   * Why no `_pendingTraps`: the LMK path runs framework-side cleanup
   * (`Framework::evict_package`) directly from the kernel, so routing
   * the trap mark through the next-frame drain would be redundant.
   * The watchdog path keeps using `_pendingTraps` because it has no
   * concurrent kernel-side counterpart.
   *
   * Returns `1` on a successful terminate, `0` if no worker was spawned
   * (idempotent — a second call returns `0`).
   */
  evictPackage(packageId) {
    const proc = this._processes.get(packageId);
    if (!proc) return 0;
    this._log("info", `lmk: terminating worker for ${packageId}`);
    try { proc.worker.terminate(); } catch (_) { /* best effort */ }
    if (proc.pingTimer) {
      clearTimeout(proc.pingTimer);
      proc.pingTimer = null;
    }
    this._processes.delete(packageId);
    // Evict every dispatch entry that belonged to the dead worker —
    // same surgical pruning as the watchdog kill (audit fix #2).
    for (const [fid, entry] of this._dispatches) {
      if (entry.packageId === packageId) {
        this._dispatches.delete(fid);
      }
    }
    // Stone 14.1 — close any WebSocket the dead app left open so the
    // browser doesn't hold an orphan connection. Symmetric with the
    // watchdog kill path further down.
    this._disposeWsForPackage(packageId);
    this._disposeNotificationsForPackage(packageId);
    this._disposeCapsForPackage(packageId);
    return 1;
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
    const spawnStart = performance.now();
    this._log("info", `[time] ${packageId}: spawn → init_post t=0ms (wasm ${wasmCopy.byteLength}B)`);
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

    const proc = { worker, packageId, lastPongAt: 0, ready: false, pingTimer: null, memoryBytes: 0, cpuMs: 0, warned: false, spawnStart };
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
   * **Why JS-level pong, not wasm:** the wasm app's job is to
   * process dispatches, not heartbeats. Routing the pong through
   * wasm would put the heartbeat's liveness on the same thread
   * we're trying to detect as wedged — pointless.
   *
   * **Why self-scheduling setTimeout, not setInterval:** setInterval
   * fires on a fixed schedule even if the previous callback hasn't
   * finished, which can stack overlapping ticks when the kernel-
   * worker is busy. setTimeout chained to itself only re-arms after
   * the current tick returns — no overlap, accepts whatever
   * scheduling slack the runtime imposes.
   */
  _startHeartbeat(proc) {
    const tick = () => {
      // Worker may have been terminated by another path between
      // ticks — bail cleanly.
      if (this._processes.get(proc.packageId) !== proc) return;
      const now = performance.now();
      // Stone 11.6.7 #5 — resolve TIMEOUT_MS / WARN_MS / HEARTBEAT_MS
      // each tick so a runtime override
      // (`globalThis.__akivaConfig.watchdog`) or a per-package
      // manifest.runtime.watchdog_timeout_ms takes effect without
      // restarting the worker. Per-package wins over global which
      // wins over defaults; see `effectiveWatchdog` in config.js.
      const cfg = effectiveWatchdog(this._watchdogTimeoutMs.get(proc.packageId));
      const elapsed = now - proc.lastPongAt;
      if (elapsed >= cfg.TIMEOUT_MS) {
        this._log("error",
          `app ${proc.packageId}: no pong in ${elapsed.toFixed(0)}ms — terminating worker`);
        try { proc.worker.terminate(); } catch (_) { /* best effort */ }
        this._pendingTraps.add(proc.packageId);
        this._processes.delete(proc.packageId);
        // Audit fix #2 — evict every _dispatches entry that belonged
        // to the dead worker. Tracking `packageId` per entry (added
        // in `dispatch()`) lets us prune surgically without touching
        // healthy peers' pending futures. Without this, an
        // unbounded number of in-flight render/save_state futures
        // accumulate when an app crashes.
        for (const [fid, entry] of this._dispatches) {
          if (entry.packageId === proc.packageId) {
            this._dispatches.delete(fid);
          }
        }
        this._disposeWsForPackage(proc.packageId);
        this._disposeNotificationsForPackage(proc.packageId);
        this._disposeCapsForPackage(proc.packageId);
        return;
      }
      // WARN — log once per crossing (proc.warned latches until a
      // pong resets it, so a worker stuck around the warn band
      // doesn't spam the log every heartbeat).
      if (elapsed >= cfg.WARN_MS) {
        if (!proc.warned) {
          this._log("warn",
            `app ${proc.packageId}: no pong in ${elapsed.toFixed(0)}ms (warn threshold ${cfg.WARN_MS}ms; will terminate at ${cfg.TIMEOUT_MS}ms)`);
          proc.warned = true;
        }
      } else if (proc.warned) {
        // Pong arrived since the warning — clear the latch so a
        // future stall logs again. lastPongAt is updated in the
        // pong handler; a recent enough pong drops elapsed below
        // WARN_MS.
        proc.warned = false;
      }
      proc.worker.postMessage({ type: "ping", t: now });
      proc.pingTimer = setTimeout(tick, cfg.HEARTBEAT_MS);
    };
    // Initial schedule uses the global heartbeat — per-package
    // overrides only adjust TIMEOUT_MS / WARN_MS, not HEARTBEAT_MS.
    proc.pingTimer = setTimeout(tick, effectiveWatchdog().HEARTBEAT_MS);
  }

  /**
   * Stone 11.6.7 #3 — broadcast a `storage_invalidate` to a package's
   * worker after a kernel-side write or delete. The worker uses the
   * message to drop any cached readonly `SyncAccessHandle` it holds
   * for that key (so its next read either re-opens fresh or falls
   * back to the kernel host_op channel).
   *
   * `userKey === null` means "uninstall — drop everything for this
   * package". The worker is expected to close its package-directory
   * handle entirely.
   *
   * No-op if the package has no spawned worker yet (lazy-spawn means
   * a never-launched package has no readers to invalidate).
   */
  broadcastStorageInvalidate(packageId, userKey) {
    const proc = this._processes.get(packageId);
    if (!proc) return;
    try {
      proc.worker.postMessage({
        type: "storage_invalidate",
        userKey: userKey ?? null,
      });
    } catch (err) {
      this._log("warn", `[${packageId}] storage_invalidate post failed: ${err?.message ?? err}`);
    }
  }

  /**
   * Stone 11.6.7 #5 — register a per-package watchdog timeout
   * override (from `manifest.runtime.watchdog_timeout_ms`). Called
   * by the kernel via the `hal_app_set_watchdog_timeout_ms` HAL
   * during boot for each package that declared the field. Absent
   * entries fall back to the global default.
   *
   * Pass `0` (or a non-finite value) to clear an override.
   */
  setWatchdogTimeoutMs(packageId, ms) {
    if (typeof ms !== "number" || !Number.isFinite(ms) || ms <= 0) {
      this._watchdogTimeoutMs.delete(packageId);
      return;
    }
    this._watchdogTimeoutMs.set(packageId, ms);
  }

  /**
   * Read the package's resource tables from storage and parse them
   * into plain-object form for the worker's init payload. Errors
   * are logged but don't block the spawn — a broken `colors.toml`
   * just means `@color/` refs return magenta.
   *
   * Stone 11.6.9 #3 — qualifier-aware. Always loads `values/`. When
   * `this._nightMode` is true, also loads `values-night/` and
   * overlays each entry on top (last-write-wins per key). An app
   * without a `values-night/` directory simply behaves as before.
   */
  _loadResources(packageId) {
    const strings = {};
    const colors = {};
    const tryParse = (qualifierDir, leaf, parseFn, target) => {
      const key = `packages/${packageId}/res/${qualifierDir}/${leaf}.toml`;
      const bytes = this._storage.get(key);
      if (!bytes) return;
      let text;
      try {
        text = this._decoder.decode(bytes);
      } catch (err) {
        this._log("warn", `[${packageId}] ${qualifierDir}/${leaf}.toml: not valid UTF-8`);
        return;
      }
      try {
        parseFn(text, target, (key, reason) =>
          this._log("warn", `[${packageId}] ${qualifierDir}/${leaf}.toml: '${key}' ${reason}`));
      } catch (err) {
        this._log("warn", `[${packageId}] ${qualifierDir}/${leaf}.toml: ${err?.message ?? err}`);
      }
    };
    // Default `values/` first — populates the base map. Then night
    // overlay (if enabled) writes on top, so its keys win and
    // anything it doesn't define falls through.
    tryParse("values", "strings", parseStringsToml, strings);
    tryParse("values", "colors",  parseColorsToml,  colors);
    if (this._nightMode) {
      tryParse("values-night", "strings", parseStringsToml, strings);
      tryParse("values-night", "colors",  parseColorsToml,  colors);
    }
    // Stone 13.0prep C — enumerate `@drawable/` references. Each PNG
    // staged at `packages/<id>/res/drawable/<name>.png` becomes a
    // sync-allocated `drawable_id`. Actual ImageBitmap registration
    // (PNG → bitmap → renderer.registerDrawable) is async and lives
    // in `_loadDrawableBitmaps` below; the {name → id} map exposed
    // here is what the worker needs synchronously.
    const drawables = this._buildDrawableIndex(packageId);
    return { strings, colors, drawables };
  }

  /**
   * Stone 13.0prep C2 — wire the renderer backend so async drawable
   * loads can register their decoded ImageBitmaps. Called from
   * `worker.js` after `Canvas2DBackend` is initialised. Idempotent;
   * flushes any registrations that completed before the wire.
   */
  setBackend(backend) {
    this._backend = backend;
    if (backend && this._pendingDrawableRegistrations.length > 0) {
      for (const { id, bitmap } of this._pendingDrawableRegistrations) {
        try { backend.registerDrawable(id, bitmap); }
        catch (err) { this._log("warn", `registerDrawable(${id}) failed: ${err?.message ?? err}`); }
      }
      this._pendingDrawableRegistrations.length = 0;
    }
  }

  /**
   * Stone 13.2 — kernel instance reference for the dialog opening
   * bridge. `worker.js` calls this once after `WebAssembly.instantiate`
   * resolves; we use the exports to allocate a kernel buffer, copy the
   * dialog payload, and call `akiva_open_dialog`.
   *
   * @param {WebAssembly.Instance} instance
   */
  setKernelInstance(instance) {
    this._kernelInstance = instance;
  }

  /**
   * Stone 13.2 — drive a SHOW_DIALOG host_op into the kernel. Returns
   * `true` if the kernel accepted it (the framework will resolve the
   * future later via `hal_dialog_complete`); `false` if it must be
   * resolved here as Dismissed (kernel not booted, payload bad).
   *
   * `args` is the SDK-encoded payload: `[handle:u32 LE | title_len:u16
   *  | title | body_len:u16 | body | btn_count:u8 | (btn_len:u16 |
   *  btn)*]`. This method strips the handle prefix and forwards the
   * rest to `akiva_open_dialog`, which the kernel decodes back.
   *
   * @param {string} packageId
   * @param {bigint} workerFutureId
   * @param {Uint8Array} args
   * @returns {boolean}
   */
  _openDialog(packageId, workerFutureId, args) {
    if (!this._kernelInstance) {
      this._log("warn", `[${packageId}] SHOW_DIALOG before kernel boot — auto-dismissing`);
      return false;
    }
    if (args.length < 4) {
      this._log("warn", `[${packageId}] SHOW_DIALOG args truncated (${args.length} < 4)`);
      return false;
    }
    const view = new DataView(args.buffer, args.byteOffset, args.byteLength);
    const handle = view.getUint32(0, true);
    if (handle === 0) {
      this._log("warn", `[${packageId}] SHOW_DIALOG handle is 0`);
      return false;
    }
    if (this._pendingDialogs.has(handle)) {
      // The framework's open() also handles this by resolving the
      // prior — but record-keeping on this side needs to follow suit
      // so the future routing matches the most recent open. The prior
      // entry's host_op_done will land via hal_dialog_complete with
      // Dismissed kind shortly.
      this._log("info", `[${packageId}] SHOW_DIALOG replacing in-flight dialog on handle ${handle}`);
    }
    const payloadBytes = args.subarray(4);
    const exp = this._kernelInstance.exports;
    const ptr = exp.akiva_alloc(payloadBytes.length);
    if (!ptr) {
      this._log("warn", `[${packageId}] SHOW_DIALOG kernel alloc(${payloadBytes.length}) failed`);
      return false;
    }
    try {
      const kernelBuf = this._kernelMemory().buffer;
      new Uint8Array(kernelBuf, ptr, payloadBytes.length).set(payloadBytes);
      const rc = exp.akiva_open_dialog(handle, workerFutureId, ptr, payloadBytes.length);
      exp.akiva_dealloc(ptr, payloadBytes.length);
      if (rc !== 0) {
        this._log("warn", `[${packageId}] akiva_open_dialog rc=${rc}`);
        return false;
      }
      this._pendingDialogs.set(handle, { packageId, futureId: workerFutureId });
      return true;
    } catch (err) {
      try { exp.akiva_dealloc(ptr, payloadBytes.length); } catch {}
      this._log("warn", `[${packageId}] akiva_open_dialog threw: ${err?.message ?? err}`);
      return false;
    }
  }

  /**
   * Stone 13.2 — kernel calls back here for every dialog the user
   * resolved (button tap or tap-outside). We look up the pending
   * routing entry, post `host_op_done` to the originating worker,
   * and clear the entry.
   *
   * `kind`: 0 = Button (`btnIdx` meaningful), 1 = Dismissed.
   *
   * @param {number} handle
   * @param {bigint} _futureId  recorded for future use; we look up by handle today
   * @param {number} kind
   * @param {number} btnIdx
   */
  completeDialog(handle, _futureId, kind, btnIdx) {
    const entry = this._pendingDialogs.get(handle);
    if (!entry) {
      // Stale — could happen if a watchdog tore the worker down between
      // open and resolve. Drop on the floor.
      this._dbg("dialog", `complete(${handle}) without pending — dropping`);
      return;
    }
    this._pendingDialogs.delete(handle);
    const proc = this._processes.get(entry.packageId);
    if (!proc) {
      this._dbg("dialog", `complete(${handle}) packageId=${entry.packageId} has no process`);
      return;
    }
    // Wire payload: [kind: u8, btn_idx: u8]. Mirrored on the SDK side
    // by `dialog::poll_dialog_inner` which decodes back to DialogResult.
    const payload = new Uint8Array(2);
    payload[0] = kind & 0xff;
    payload[1] = btnIdx & 0xff;
    proc.worker.postMessage(
      { type: "host_op_done", future_id: entry.futureId, status: STATUS.READY, payload },
      [payload.buffer],
    );
  }

  /**
   * Stone 13.0prep C2 — async side. For every drawable PNG in storage
   * for `packageId`, decode it to an `ImageBitmap` and register with
   * the renderer backend. Fire-and-forget — the worker spawn does not
   * wait for this. Until each registration lands, the renderer paints
   * the magenta placeholder for that id.
   *
   * Decode failures are logged but non-fatal: the id stays valid
   * (resolution succeeds), the renderer continues painting placeholder.
   */
  async _loadDrawableBitmaps(packageId) {
    const prefix = `packages/${packageId}/res/drawable/`;
    const keys = this._storage.keysWithPrefix?.(prefix) ?? [];
    const index = this._buildDrawableIndex(packageId);
    for (const key of keys) {
      const tail = key.slice(prefix.length);
      if (!tail.endsWith(".png")) continue;
      const name = tail.slice(0, -".png".length);
      const id = index[name];
      if (!id) continue;  // not in index (shouldn't happen) — skip

      const bytes = this._storage.get(key);
      if (!bytes) continue;
      try {
        const blob = new Blob([bytes], { type: "image/png" });
        const bitmap = await createImageBitmap(blob);
        if (this._backend) {
          this._backend.registerDrawable(id, bitmap);
        } else {
          this._pendingDrawableRegistrations.push({ id, bitmap });
        }
      } catch (err) {
        this._log("warn", `@drawable/${name}: decode failed (${err?.message ?? err})`);
      }
    }
  }

  /**
   * Stone 13.0prep C — sync side. Enumerate the package's drawable
   * filenames in storage and assign each a stable per-package u32 id.
   * Ids start at 1 (id 0 is the missing-drawable sentinel). Idempotent
   * across calls because the assignment is deterministic — alphabetical
   * sort, sequential numbering — so a re-load (night-mode flip,
   * uninstall/reinstall) yields the same id for the same name.
   *
   * The actual PNG decode + ImageBitmap upload is async and happens
   * separately in `_loadDrawableBitmaps`; until that resolves, the
   * renderer's drawable cache is empty and the `OP_DRAW_IMAGE` path
   * paints magenta placeholders for the assigned ids. Apps see the id
   * resolution succeed (non-zero) but visually get the placeholder
   * during the brief async-load window — usually invisible because
   * boot does the loading before the first frame is composed.
   */
  _buildDrawableIndex(packageId) {
    const prefix = `packages/${packageId}/res/drawable/`;
    const keys = this._storage.keysWithPrefix?.(prefix) ?? [];
    const names = [];
    for (const key of keys) {
      const tail = key.slice(prefix.length);
      // Accept `<name>.png` only — other extensions ignored for now;
      // adding webp/avif is a one-line change once we decide on the
      // extension policy.
      if (tail.endsWith(".png")) {
        names.push(tail.slice(0, -".png".length));
      }
    }
    names.sort();  // deterministic id assignment
    const out = {};
    let nextId = 1;  // 0 is the missing sentinel
    for (const name of names) out[name] = nextId++;
    return out;
  }

  /**
   * Stone 11.6.9 #3 — toggle the night-mode qualifier. Re-resolves
   * resources for every running app and posts `set_resources` so
   * `@string/` / `@color/` lookups pick up the new overlay on the
   * next paint, without restarting any worker.
   *
   * No-op when `night` already matches the current state — saves a
   * pile of postMessage/parse work on duplicate matchMedia
   * notifications (browsers do fire spurious change events when the
   * underlying setting hasn't actually moved).
   *
   * @param {boolean} night
   * @returns {boolean} true if the value changed, false if no-op.
   */
  setNightMode(night) {
    const next = !!night;
    if (this._nightMode === next) return false;
    this._nightMode = next;
    for (const [packageId, proc] of this._processes) {
      const resources = this._loadResources(packageId);
      proc.worker.postMessage({ type: "set_resources", resources });
    }
    this._dbg("lifecycle", `night-mode → ${next ? "on" : "off"}`);
    return true;
  }

  /** Read-only accessor — the kernel-worker queries this when
   *  building the host_op response for any future `night-mode` query
   *  primitive, and tests use it to assert state after a toggle. */
  get nightMode() { return this._nightMode; }

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
          if (proc.spawnStart) {
            const dt = (performance.now() - proc.spawnStart).toFixed(1);
            this._log("info", `[time] ${packageId}: init_ok at t=${dt}ms (wasm compiled+instantiated)`);
          }
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
        // [time] log first dispatch_done per package.
        const proc = this._processes.get(packageId);
        if (proc?.spawnStart && !proc.firstDoneAt) {
          proc.firstDoneAt = performance.now();
          const dt = (proc.firstDoneAt - proc.spawnStart).toFixed(1);
          const wait = entry.postedAt ? (proc.firstDoneAt - entry.postedAt).toFixed(1) : "?";
          this._log("info",
            `[time] ${packageId}: first dispatch_done (cmd=0x${entry.cmdKind?.toString(16) ?? "?"}) at t=${dt}ms (wait=${wait}ms)`);
        }
        // P0 #4 — the worker shell already returns `bytes=null` for
        // ack-only dispatches and a populated `Uint8Array` for RENDER /
        // SAVE_STATE. The legacy `future_id` field is unused under the
        // new protocol; trust `bytes` directly.
        const bytes = msg.bytes instanceof Uint8Array ? msg.bytes
          : (msg.bytes ? new Uint8Array(msg.bytes) : null);
        entry.status = STATUS.READY;
        entry.bytes = bytes;
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
        //
        // Phase 12.1 — pong also carries the worker's current
        // `WebAssembly.Memory.buffer.byteLength`. Stash it on the
        // process record so `memoryCurrent()` (called from the
        // kernel-side HAL) can return a fresh value without
        // round-tripping into the worker each frame.
        //
        // Phase 12.2 — same for `cpu_ms` (cumulative time the worker
        // has spent inside wasm exports). Kernel-side `CpuBudget`
        // derives the recent rate from successive snapshots.
        const proc = this._processes.get(packageId);
        if (proc) {
          proc.lastPongAt = performance.now();
          if (typeof msg.memory_bytes === "number" && msg.memory_bytes >= 0) {
            proc.memoryBytes = msg.memory_bytes;
          }
          if (typeof msg.cpu_ms === "number" && msg.cpu_ms >= 0 && Number.isFinite(msg.cpu_ms)) {
            proc.cpuMs = msg.cpu_ms;
          }
        }
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
   *
   * **Provenance check.** The handle in `args` is supplied by the
   * worker; without verification, app A's worker could send a
   * `side_effect` claiming app B's handle and forge a `finish` /
   * `start_activity` / `set_result` call against B. This mirrors
   * Android's Binder UID check at the IPC boundary: the kernel
   * doesn't trust the message's "I'm acting on behalf of N" claim
   * unless the routing context confirms it. Here the routing context
   * is `packageId` (which worker actually posted the message), and
   * the slot's owner is the truth — handle that doesn't belong to
   * `packageId` is dropped with a warn-level log.
   */
  _handleSideEffect(packageId, msg) {
    const args = msg.args instanceof Uint8Array ? msg.args : new Uint8Array(msg.args ?? 0);
    if (args.length < 4) return;

    // Stone 14.1/14.2 — net fire ops use a separate handle namespace
    // (WebSocket / WebTransport / stream handles allocated by
    // AppRegistry, not activity slot ids). Branch out before the
    // activity-handle ownership check.
    switch (msg.op_kind) {
      case FIRE.WS_SEND:
        this._handleWsSend(packageId, args);
        return;
      case FIRE.WS_CLOSE:
        this._handleWsClose(packageId, args);
        return;
      case FIRE.WT_SEND_DATAGRAM:
        this._handleWtSendDatagram(packageId, args);
        return;
      case FIRE.WT_SEND_STREAM:
        this._handleWtSendStream(packageId, args);
        return;
      case FIRE.WT_CLOSE_STREAM:
        this._handleWtCloseStream(packageId, args);
        return;
      case FIRE.WT_CLOSE:
        this._handleWtClose(packageId, args);
        return;
      case FIRE.NOTIFY_CLOSE:
        this._handleNotifyClose(packageId, args);
        return;
      case FIRE.VIBRATE:
        this._handleVibrate(packageId, args);
        return;
    }

    const view = new DataView(args.buffer, args.byteOffset, args.byteLength);
    const handle = view.getUint32(0, true);
    if (!this._handleBelongsTo(handle, packageId)) {
      this._log("warn",
        `[${packageId}] side_effect op_kind=0x${(msg.op_kind ?? 0).toString(16)} ` +
        `claimed handle=${handle} which doesn't belong to this package — dropped`);
      return;
    }
    switch (msg.op_kind) {
      case FIRE.FINISH: {
        this._pendingFinish.add(handle);
        this._dbg("intent", `${packageId}: side_effect FINISH handle=${handle}`);
        return;
      }
      case FIRE.START_ACTIVITY: {
        if (!this._hasGrant(packageId, "akiva.permission.LAUNCH_ANY_APP")) {
          this._log("warn", `[${packageId}] side_effect START_ACTIVITY: denied — no LAUNCH_ANY_APP grant`);
          return;
        }
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
        const requestCode = view.getUint32(4, true);
        const intentBytes = args.slice(8);
        if (intentBytes.length === 0) return;
        this._pendingStartForResult.set(handle, { bytes: intentBytes, requestCode });
        this._dbg("intent", `${packageId}: side_effect START_ACTIVITY_FOR_RESULT handle=${handle} rc=${requestCode}`);
        return;
      }
      case FIRE.SET_RESULT: {
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

  /**
   * Provenance check: `handle` was assigned by `register(packageId,…)`
   * to a specific package, and the binding never moves. Returns true
   * only when the routing-context `packageId` matches the slot's
   * recorded owner. Used by every cross-worker hook that takes a
   * handle from app-supplied bytes (side_effect, host_op).
   */
  _handleBelongsTo(handle, packageId) {
    const slot = this._slots.get(handle);
    return slot != null && slot.packageId === packageId;
  }

  _hasGrant(packageId, capability) {
    return this._grants.get(packageId)?.has(capability) ?? false;
  }

  /**
   * Stone 11.6 — namespace a user-supplied key under the package's
   * sandbox prefix. The kernel-worker's `Storage` is a flat KV map
   * (no path traversal possible — keys are opaque strings — but
   * an unprefixed key would still collide across packages, and
   * inspecting OPFS for support would lump everyone into one map).
   *
   * Choice of prefix: `pkgs/{package_id}/{user_key}`. Same shape
   * as `packages/...` reserved for app wasm + manifest, but in a
   * sibling namespace to keep the install side unambiguous.
   *
   * Reserved characters in `package_id` (none — it's UTF-8 from a
   * manifest field that PackageManager validates) plus the `/`
   * separator pin a unique prefix; any user key is appended as-is.
   */
  _sandboxKey(packageId, userKey) {
    return `pkgs/${packageId}/${userKey}`;
  }

  /**
   * Stone 11.6 — kernel-side dispatch for the three storage ops.
   * Returns `{ status, payload }` matching the `host_op_done` reply
   * shape. Empty user key is rejected (the storage layer would
   * accept it but it's a footgun; SDK already trims trailing
   * whitespace in the read API).
   */
  _dispatchStorage(packageId, opKind, args) {
    switch (opKind) {
      case IO.STORAGE_GET: {
        const key = this._decoder.decode(args);
        if (key.length === 0) {
          return { status: STATUS.ERROR, payload: this._encoder.encode("storage: empty key") };
        }
        const val = this._storage.get(this._sandboxKey(packageId, key));
        if (val === null || val === undefined) {
          // Not found → present:0, no value bytes.
          return { status: STATUS.READY, payload: new Uint8Array([0]) };
        }
        // Found → present:1, then value bytes. 1-byte prefix lets the
        // SDK distinguish "key absent" from "empty value present"
        // without paying a 4-byte length prefix.
        const out = new Uint8Array(1 + val.length);
        out[0] = 1;
        out.set(val, 1);
        return { status: STATUS.READY, payload: out };
      }
      case IO.STORAGE_SET: {
        if (args.length < 4) {
          return { status: STATUS.ERROR, payload: this._encoder.encode("storage: set args truncated") };
        }
        const view = new DataView(args.buffer, args.byteOffset, args.byteLength);
        const keyLen = view.getUint32(0, true);
        if (keyLen === 0 || 4 + keyLen > args.length) {
          return { status: STATUS.ERROR, payload: this._encoder.encode("storage: bad key length") };
        }
        const key = this._decoder.decode(args.subarray(4, 4 + keyLen));
        const value = args.slice(4 + keyLen);
        const rc = this._storage.set(this._sandboxKey(packageId, key), value);
        if (rc !== 0) {
          return { status: STATUS.ERROR, payload: this._encoder.encode(`storage: set rc=${rc}`) };
        }
        return { status: STATUS.READY, payload: new Uint8Array(0) };
      }
      case IO.STORAGE_DELETE: {
        const key = this._decoder.decode(args);
        if (key.length === 0) {
          return { status: STATUS.ERROR, payload: this._encoder.encode("storage: empty key") };
        }
        // Idempotent: storage.delete on a missing key returns SR_OK
        // already (see runtime/web/storage.js).
        this._storage.delete(this._sandboxKey(packageId, key));
        return { status: STATUS.READY, payload: new Uint8Array(0) };
      }
      case IO.STORAGE_LIST: {
        // Empty prefix is legal — means "list every key the package owns".
        // Sandbox the prefix to the package's namespace so a buggy or
        // hostile app can't enumerate `sys/` or another package's keys.
        const userPrefix = this._decoder.decode(args);
        const sandboxedPrefix = this._sandboxKey(packageId, userPrefix);
        const sandboxStrip = `pkgs/${packageId}/`;
        const allKeys = this._storage.keysWithPrefix(sandboxedPrefix);
        // Strip the per-package prefix before handing back to the app:
        // apps work in their own keyspace, and exposing `pkgs/<id>/`
        // would let one package's keys be passed verbatim to another's
        // `read` call. Order is whatever Map iteration gives — apps
        // sort caller-side if they care.
        const userKeys = allKeys
          .map((k) => k.startsWith(sandboxStrip) ? k.slice(sandboxStrip.length) : null)
          .filter((k) => k !== null && k.length > 0);

        // Wire format: [count: u32 LE | (klen: u16 LE | key: utf8...){count}].
        // Cap result at 64 KiB so a runaway directory listing can't
        // blow the kernel↔worker postMessage buffer; apps with more
        // keys than that should paginate by prefix.
        const RESULT_CAP = 64 * 1024;
        const encodedKeys = userKeys.map((k) => this._encoder.encode(k));
        let total = 4;
        for (const e of encodedKeys) total += 2 + e.byteLength;
        if (total > RESULT_CAP) {
          return {
            status: STATUS.ERROR,
            payload: this._encoder.encode(
              `storage: too many keys (would emit ${total} bytes; cap is ${RESULT_CAP})`,
            ),
          };
        }
        const out = new Uint8Array(total);
        const dv = new DataView(out.buffer);
        dv.setUint32(0, encodedKeys.length, true);
        let off = 4;
        for (const e of encodedKeys) {
          dv.setUint16(off, e.byteLength, true);
          off += 2;
          out.set(e, off);
          off += e.byteLength;
        }
        return { status: STATUS.READY, payload: out };
      }
      default:
        return { status: STATUS.ERROR, payload: this._encoder.encode(`storage: unknown op_kind 0x${opKind.toString(16)}`) };
    }
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
      case IO.STORAGE_SET:
      case IO.STORAGE_DELETE:
      case IO.STORAGE_LIST: {
        if (!this._hasGrant(packageId, "akiva.permission.STORAGE_SANDBOX")) {
          this._log("warn", `[${packageId}] host_op STORAGE_*: denied — no STORAGE_SANDBOX grant`);
          status = STATUS.ERROR;
          break;
        }
        const args = msg.args instanceof Uint8Array ? msg.args : new Uint8Array(0);
        const result = this._dispatchStorage(packageId, msg.op_kind, args);
        status = result.status;
        payload = result.payload;
        break;
      }
      case IO.SHOW_DIALOG: {
        // Stone 13.2 — fire into the kernel/framework. On success we
        // DO NOT post host_op_done now; the framework drains
        // resolutions later and calls back via `hal_dialog_complete`,
        // which routes the reply through `completeDialog`. On failure
        // we synthesize a Dismissed reply so the SDK future doesn't
        // hang.
        const args = msg.args instanceof Uint8Array ? msg.args : new Uint8Array(0);
        const accepted = this._openDialog(packageId, futureId, args);
        if (accepted) return;
        // Auto-dismiss path: kind=1 (Dismissed), btn_idx=0
        payload = new Uint8Array([1, 0]);
        status = STATUS.READY;
        break;
      }
      // Stone 14.1 — Network ops, all gated by NETWORK. Each goes
      // async (real fetch / WebSocket events), so we don't post
      // host_op_done from this synchronous switch — the dedicated
      // helper resolves it once the underlying browser primitive
      // fires its callback.
      case IO.HTTP_FETCH: {
        if (!this._hasGrant(packageId, "akiva.permission.NETWORK")) {
          this._log("warn", `[${packageId}] host_op HTTP_FETCH: denied — no NETWORK grant`);
          status = STATUS.ERROR;
          payload = this._encoder.encode("net: denied (no NETWORK grant)");
          break;
        }
        const args = msg.args instanceof Uint8Array ? msg.args : new Uint8Array(0);
        this._handleHttpFetch(packageId, futureId, args);
        return; // helper posts host_op_done asynchronously
      }
      case IO.WS_OPEN: {
        if (!this._hasGrant(packageId, "akiva.permission.NETWORK")) {
          this._log("warn", `[${packageId}] host_op WS_OPEN: denied — no NETWORK grant`);
          status = STATUS.ERROR;
          payload = this._encoder.encode("net: denied (no NETWORK grant)");
          break;
        }
        const args = msg.args instanceof Uint8Array ? msg.args : new Uint8Array(0);
        this._handleWsOpen(packageId, futureId, args);
        return; // resolves on ws.onopen / ws.onerror
      }
      case IO.WS_RECV: {
        if (!this._hasGrant(packageId, "akiva.permission.NETWORK")) {
          this._log("warn", `[${packageId}] host_op WS_RECV: denied — no NETWORK grant`);
          status = STATUS.ERROR;
          payload = this._encoder.encode("net: denied (no NETWORK grant)");
          break;
        }
        const args = msg.args instanceof Uint8Array ? msg.args : new Uint8Array(0);
        this._handleWsRecv(packageId, futureId, args);
        return; // resolves on next ws.onmessage / onclose
      }
      // Stone 14.2 — WebTransport ops, gated by NETWORK same as WS.
      case IO.WT_OPEN: {
        if (!this._hasGrant(packageId, "akiva.permission.NETWORK")) {
          status = STATUS.ERROR;
          payload = this._encoder.encode("net: denied (no NETWORK grant)");
          break;
        }
        const args = msg.args instanceof Uint8Array ? msg.args : new Uint8Array(0);
        this._handleWtOpen(packageId, futureId, args);
        return;
      }
      case IO.WT_RECV_DATAGRAM: {
        if (!this._hasGrant(packageId, "akiva.permission.NETWORK")) {
          status = STATUS.ERROR;
          payload = this._encoder.encode("net: denied (no NETWORK grant)");
          break;
        }
        const args = msg.args instanceof Uint8Array ? msg.args : new Uint8Array(0);
        this._handleWtRecvDatagram(packageId, futureId, args);
        return;
      }
      case IO.WT_OPEN_BIDI: {
        if (!this._hasGrant(packageId, "akiva.permission.NETWORK")) {
          status = STATUS.ERROR;
          payload = this._encoder.encode("net: denied (no NETWORK grant)");
          break;
        }
        const args = msg.args instanceof Uint8Array ? msg.args : new Uint8Array(0);
        this._handleWtOpenBidi(packageId, futureId, args);
        return;
      }
      case IO.WT_RECV_STREAM: {
        if (!this._hasGrant(packageId, "akiva.permission.NETWORK")) {
          status = STATUS.ERROR;
          payload = this._encoder.encode("net: denied (no NETWORK grant)");
          break;
        }
        const args = msg.args instanceof Uint8Array ? msg.args : new Uint8Array(0);
        this._handleWtRecvStream(packageId, futureId, args);
        return;
      }
      // Stone 14.3 — Notifications. Bridged to the main-thread tab
      // because `Notification.requestPermission` requires a Window
      // context and many platforms route notifications through the
      // tab's lifecycle. NOTIFICATIONS grant gate.
      case IO.NOTIFY_REQUEST_PERMISSION: {
        if (!this._hasGrant(packageId, "akiva.permission.NOTIFICATIONS")) {
          this._log("warn", `[${packageId}] host_op NOTIFY_REQUEST_PERMISSION: denied — no NOTIFICATIONS grant`);
          status = STATUS.ERROR;
          payload = this._encoder.encode("notify: denied (no NOTIFICATIONS grant)");
          break;
        }
        this._handleNotifyRequestPermission(packageId, futureId);
        return;
      }
      case IO.NOTIFY_POST: {
        if (!this._hasGrant(packageId, "akiva.permission.NOTIFICATIONS")) {
          this._log("warn", `[${packageId}] host_op NOTIFY_POST: denied — no NOTIFICATIONS grant`);
          status = STATUS.ERROR;
          payload = this._encoder.encode("notify: denied (no NOTIFICATIONS grant)");
          break;
        }
        const args = msg.args instanceof Uint8Array ? msg.args : new Uint8Array(0);
        this._handleNotifyPost(packageId, futureId, args);
        return;
      }
      // Stone 14.4 — Clipboard.
      case IO.CLIPBOARD_READ_TEXT: {
        if (!this._hasGrant(packageId, "akiva.permission.CLIPBOARD")) {
          status = STATUS.ERROR;
          payload = this._encoder.encode("clipboard: denied (no CLIPBOARD grant)");
          break;
        }
        this._handleClipboardRead(packageId, futureId);
        return;
      }
      case IO.CLIPBOARD_WRITE_TEXT: {
        if (!this._hasGrant(packageId, "akiva.permission.CLIPBOARD")) {
          status = STATUS.ERROR;
          payload = this._encoder.encode("clipboard: denied (no CLIPBOARD grant)");
          break;
        }
        const args = msg.args instanceof Uint8Array ? msg.args : new Uint8Array(0);
        this._handleClipboardWrite(packageId, futureId, args);
        return;
      }
      // Stone 14.6 — Geolocation.
      case IO.GEO_GET_POSITION: {
        if (!this._hasGrant(packageId, "akiva.permission.LOCATION")) {
          status = STATUS.ERROR;
          payload = this._encoder.encode("geo: denied (no LOCATION grant)");
          break;
        }
        this._handleGeoGetPosition(packageId, futureId);
        return;
      }
      default:
        this._log("warn", `[${packageId}] host_op unknown op_kind=0x${(msg.op_kind ?? 0).toString(16)}`);
        status = STATUS.ERROR;
    }

    // Stone 11.6.7 #3 — `Storage.set/delete` is now async-persisted
    // (in-memory mirror updates sync, OPFS file flush queued for
    // background drain). To preserve `WriteHandle::await` semantics
    // ("when this resolves the write is durable"), await the storage
    // drain for SET / DELETE before posting `host_op_done`. GET and
    // every other op skip the wait — they have no durability obligation.
    const needsDrain =
      (msg.op_kind === IO.STORAGE_SET || msg.op_kind === IO.STORAGE_DELETE) &&
      typeof this._storage.drain === "function";
    if (needsDrain) {
      this._storage.drain().then(() => {
        // Worker may have died between submit and flush — don't crash on
        // a stale postMessage call.
        if (this._processes.get(packageId) !== proc) return;
        proc.worker.postMessage(
          { type: "host_op_done", future_id: futureId, status, payload },
          [payload.buffer],
        );
      });
      return;
    }

    proc.worker.postMessage(
      { type: "host_op_done", future_id: futureId, status, payload },
      [payload.buffer],
    );
  }

  // ── Stone 14.1 — Network capability ──────────────────────────────────
  //
  // HTTP_FETCH, WS_OPEN, WS_RECV all dispatch into browser network
  // primitives (`fetch` / `WebSocket`) and resolve the originating
  // `host_op` future once the primitive completes. WS_SEND / WS_CLOSE
  // arrive as fire ops via `_handleSideEffect` and operate on an
  // already-open handle.
  //
  // Each handle is bound to its allocating packageId; cross-package
  // attempts are rejected with a logged warning and either an ERROR
  // host_op_done (for ops that owe a reply) or a silent drop (for
  // fire ops). The same provenance discipline as the activity-handle
  // path in `_handleSideEffect`.

  /** RFC 6455 §7.4.1 close codes the host issues. */
  static get _WS_CLOSE_CODES() {
    return Object.freeze({
      NORMAL: 1000,
      ABNORMAL: 1006,        // never sent on the wire; synthetic for
                             // close events the browser surfaces
                             // without a code (transport drop).
      MESSAGE_TOO_BIG: 1009,
    });
  }

  /** Cap on per-handle buffered receive events. Matches the SDK's
   *  expectation that `recv()` is awaited promptly; an app that
   *  ignores recv for too long under heavy traffic gets force-closed
   *  rather than letting the host eat unbounded memory. 64 events
   *  covers small bursts; sustained higher rates are an app bug. */
  static get _WS_RECV_QUEUE_CAP() { return 64; }

  /**
   * Run an HTTP request on behalf of `packageId` and resolve its
   * `host_op` future when the browser's `fetch` settles. Failure
   * modes:
   *  - parse error in the args blob → STATUS::ERROR with reason
   *  - fetch rejected (DNS, CORS preflight refusal, TLS) → ERROR
   *  - any thrown error reading the response body → ERROR
   *
   * The fetch always uses `mode: "cors"` — there is no
   * non-CORS path from a Worker. Apps targeting non-CORS servers
   * must proxy through their own backend.
   */
  _handleHttpFetch(packageId, futureId, args) {
    let req;
    try {
      req = decodeHttpRequest(args, this._decoder);
    } catch (err) {
      this._postHostOpDone(packageId, futureId, STATUS.ERROR,
        this._encoder.encode(`http: bad request: ${err?.message ?? err}`));
      return;
    }

    const method = HTTP_METHODS[req.method];
    if (!method) {
      this._postHostOpDone(packageId, futureId, STATUS.ERROR,
        this._encoder.encode(`http: unknown method code ${req.method}`));
      return;
    }

    const proc = this._processes.get(packageId);

    const init = {
      method,
      headers: req.headers,
      // GET / HEAD must not have a body per the fetch spec.
      body: (method === "GET" || method === "HEAD") ? undefined : req.body,
      mode: "cors",
      credentials: "omit",
      redirect: "follow",
    };

    fetch(req.url, init).then(async (resp) => {
      const headerPairs = [];
      resp.headers.forEach((v, k) => headerPairs.push([k, v]));
      const body = new Uint8Array(await resp.arrayBuffer());
      const out = encodeHttpResponse(resp.status, headerPairs, body);
      // Worker may have died between issue and settle.
      if (this._processes.get(packageId) !== proc) return;
      this._postHostOpDone(packageId, futureId, STATUS.READY, out);
    }).catch((err) => {
      if (this._processes.get(packageId) !== proc) return;
      this._postHostOpDone(packageId, futureId, STATUS.ERROR,
        this._encoder.encode(`http: ${err?.message ?? err}`));
    });
  }

  /**
   * Open a WebSocket on behalf of `packageId`. Allocate a handle
   * synchronously, store it, and resolve the open future once the
   * `open` event fires. `error` before `open` resolves the future
   * with STATUS::ERROR; `error` after `open` enqueues a Closed event
   * so `recv()` surfaces it cleanly.
   */
  _handleWsOpen(packageId, futureId, args) {
    if (args.length < 2) {
      this._postHostOpDone(packageId, futureId, STATUS.ERROR,
        this._encoder.encode("ws: open args truncated"));
      return;
    }
    const view = new DataView(args.buffer, args.byteOffset, args.byteLength);
    const urlLen = view.getUint16(0, true);
    if (2 + urlLen > args.length) {
      this._postHostOpDone(packageId, futureId, STATUS.ERROR,
        this._encoder.encode("ws: url length exceeds args"));
      return;
    }
    const url = this._decoder.decode(args.subarray(2, 2 + urlLen));
    // Subprotocols (1 byte count + entries) — currently always 0.
    // Carried in the wire shape for forward compat; we ignore them.
    const protoOff = 2 + urlLen;
    const protocols = [];
    if (protoOff < args.length) {
      const count = args[protoOff];
      let cur = protoOff + 1;
      for (let i = 0; i < count && cur + 2 <= args.length; i++) {
        const pl = view.getUint16(cur, true); cur += 2;
        if (cur + pl > args.length) break;
        protocols.push(this._decoder.decode(args.subarray(cur, cur + pl)));
        cur += pl;
      }
    }

    const wsHandle = this._nextWsHandle++;
    let ws;
    try {
      ws = protocols.length > 0 ? new WebSocket(url, protocols) : new WebSocket(url);
    } catch (err) {
      this._postHostOpDone(packageId, futureId, STATUS.ERROR,
        this._encoder.encode(`ws: construct failed: ${err?.message ?? err}`));
      return;
    }
    ws.binaryType = "arraybuffer";

    const entry = {
      ws,
      packageId,
      recvQueue: [],
      pendingRecv: null,
      closed: false,
    };
    this._wsHandles.set(wsHandle, entry);

    const proc = this._processes.get(packageId);
    let openSettled = false;

    ws.addEventListener("open", () => {
      if (openSettled) return;
      openSettled = true;
      if (this._processes.get(packageId) !== proc) {
        // Worker died before connection landed. Tear down the
        // socket so we don't keep an orphan connection alive.
        try { ws.close(1000, ""); } catch (_) { /* ignore */ }
        this._wsHandles.delete(wsHandle);
        return;
      }
      const out = new Uint8Array(4);
      new DataView(out.buffer).setUint32(0, wsHandle, true);
      this._postHostOpDone(packageId, futureId, STATUS.READY, out);
    });

    ws.addEventListener("message", (ev) => this._onWsMessage(wsHandle, ev));
    ws.addEventListener("close", (ev) => this._onWsClose(wsHandle, ev));
    ws.addEventListener("error", () => {
      // Browser-level error: if open hasn't happened, settle the
      // open future with ERROR; otherwise let the close handler
      // surface it as a Closed event (the browser fires close
      // after error in nearly all cases).
      if (!openSettled) {
        openSettled = true;
        this._wsHandles.delete(wsHandle);
        if (this._processes.get(packageId) !== proc) return;
        this._postHostOpDone(packageId, futureId, STATUS.ERROR,
          this._encoder.encode("ws: handshake failed"));
      }
    });
  }

  /**
   * Pull the next event off the per-handle queue, or park the
   * future for the next message/close to arrive.
   */
  _handleWsRecv(packageId, futureId, args) {
    if (args.length < 4) {
      this._postHostOpDone(packageId, futureId, STATUS.ERROR,
        this._encoder.encode("ws: recv args truncated"));
      return;
    }
    const view = new DataView(args.buffer, args.byteOffset, args.byteLength);
    const wsHandle = view.getUint32(0, true);
    const entry = this._wsHandles.get(wsHandle);
    if (!entry || entry.packageId !== packageId) {
      this._postHostOpDone(packageId, futureId, STATUS.ERROR,
        this._encoder.encode("ws: unknown handle"));
      return;
    }
    if (entry.recvQueue.length > 0) {
      const ev = entry.recvQueue.shift();
      this._postHostOpDone(packageId, futureId, STATUS.READY, ev);
      // After a Closed event we leave the entry around so further
      // recvs return the same Closed state via the queue's pre-fill;
      // see _onWsClose.
      return;
    }
    if (entry.pendingRecv) {
      // Two recvs in flight is a malformed app pattern (the SDK
      // serialises). Resolve the older one with ERROR so it doesn't
      // hang, then park this new one.
      this._postHostOpDone(entry.packageId, entry.pendingRecv.futureId,
        STATUS.ERROR,
        this._encoder.encode("ws: superseded by another recv"));
    }
    entry.pendingRecv = { futureId };
  }

  /**
   * App raised `FIRE::WS_SEND` — write a frame on the underlying
   * socket. Wire: `[handle:4 | kind:1 | data...]`. Silently drops
   * if the handle is unknown / not ours / closed; that's the
   * fire-and-forget contract.
   */
  _handleWsSend(packageId, args) {
    if (args.length < 5) return;
    const view = new DataView(args.buffer, args.byteOffset, args.byteLength);
    const wsHandle = view.getUint32(0, true);
    const entry = this._wsHandles.get(wsHandle);
    if (!entry || entry.packageId !== packageId) {
      this._log("warn", `[${packageId}] ws_send: unknown or foreign handle ${wsHandle} — dropped`);
      return;
    }
    if (entry.closed) return;
    const kind = args[4];
    const payload = args.subarray(5);
    try {
      if (kind === 0) {
        entry.ws.send(this._decoder.decode(payload));
      } else {
        // Slice into a fresh ArrayBuffer — `args` may be a view into
        // a buffer the postMessage transferred to us, but `WebSocket.send`
        // expects an ArrayBuffer / Blob / ArrayBufferView; slicing also
        // ensures the host doesn't hold a reference to caller-owned data.
        entry.ws.send(payload.slice().buffer);
      }
    } catch (err) {
      this._log("warn", `[${packageId}] ws_send failed on handle ${wsHandle}: ${err?.message ?? err}`);
    }
  }

  /**
   * App raised `FIRE::WS_CLOSE`. Idempotent — second close is a no-op.
   */
  _handleWsClose(packageId, args) {
    if (args.length < 8) return;
    const view = new DataView(args.buffer, args.byteOffset, args.byteLength);
    const wsHandle = view.getUint32(0, true);
    const entry = this._wsHandles.get(wsHandle);
    if (!entry || entry.packageId !== packageId) {
      this._log("warn", `[${packageId}] ws_close: unknown or foreign handle ${wsHandle} — dropped`);
      return;
    }
    const code = view.getUint16(4, true);
    const reasonLen = view.getUint16(6, true);
    const reasonBytes = (8 + reasonLen <= args.length)
      ? args.subarray(8, 8 + reasonLen) : new Uint8Array(0);
    const reason = reasonBytes.length > 0 ? this._decoder.decode(reasonBytes) : "";
    try {
      entry.ws.close(code, reason);
    } catch (err) {
      this._log("warn", `[${packageId}] ws_close threw: ${err?.message ?? err}`);
    }
  }

  /** Fired when the underlying socket emits a message frame. */
  _onWsMessage(wsHandle, ev) {
    const entry = this._wsHandles.get(wsHandle);
    if (!entry || entry.closed) return;
    let payload;
    if (typeof ev.data === "string") {
      const bodyBytes = this._encoder.encode(ev.data);
      payload = new Uint8Array(1 + bodyBytes.length);
      payload[0] = 0; // text
      payload.set(bodyBytes, 1);
    } else if (ev.data instanceof ArrayBuffer) {
      const view = new Uint8Array(ev.data);
      payload = new Uint8Array(1 + view.length);
      payload[0] = 1; // binary
      payload.set(view, 1);
    } else {
      // Blob (some browsers default to this). Drop quietly — apps
      // that need binary should call `WebSocket.binaryType =
      // "arraybuffer"` (we already set it) so this branch is dead
      // in practice.
      this._log("warn", `[${entry.packageId}] ws#${wsHandle}: unsupported message data type`);
      return;
    }
    if (entry.pendingRecv) {
      const { futureId } = entry.pendingRecv;
      entry.pendingRecv = null;
      this._postHostOpDone(entry.packageId, futureId, STATUS.READY, payload);
      return;
    }
    if (entry.recvQueue.length >= AppRegistry._WS_RECV_QUEUE_CAP) {
      // Force-close per RFC 6455 §7.4.1 code 1009.
      this._log("warn", `[${entry.packageId}] ws#${wsHandle}: recv queue overflow → force-close 1009`);
      try { entry.ws.close(AppRegistry._WS_CLOSE_CODES.MESSAGE_TOO_BIG, "queue overflow"); } catch (_) {}
      return;
    }
    entry.recvQueue.push(payload);
  }

  /** Fired when the underlying socket closes (graceful or transport). */
  _onWsClose(wsHandle, ev) {
    const entry = this._wsHandles.get(wsHandle);
    if (!entry) return;
    entry.closed = true;
    const reasonStr = typeof ev?.reason === "string" ? ev.reason : "";
    const reasonBytes = this._encoder.encode(reasonStr);
    const code = (typeof ev?.code === "number" && ev.code > 0)
      ? ev.code : AppRegistry._WS_CLOSE_CODES.ABNORMAL;
    const payload = new Uint8Array(1 + 2 + reasonBytes.length);
    payload[0] = 2; // closed
    new DataView(payload.buffer).setUint16(1, code, true);
    payload.set(reasonBytes, 3);
    if (entry.pendingRecv) {
      const { futureId } = entry.pendingRecv;
      entry.pendingRecv = null;
      this._postHostOpDone(entry.packageId, futureId, STATUS.READY, payload);
    } else {
      // Cap-bypass: a Closed event always lands even if the queue is
      // full, so apps that ignored recv still see the close on the
      // next call. Replace the queue tail rather than dropping the
      // close (the close is the most useful event in that branch).
      if (entry.recvQueue.length >= AppRegistry._WS_RECV_QUEUE_CAP) {
        entry.recvQueue.pop();
      }
      entry.recvQueue.push(payload);
    }
    // We deliberately don't `_wsHandles.delete` here — further
    // `recv()` calls on the handle should still surface the cached
    // Closed event from the queue. The entry is reaped when the
    // owning package terminates (`_disposeWsForPackage`).
  }

  /**
   * Force-close every WebSocket owned by `packageId`. Called from
   * the worker-termination paths (`evictPackage`, the watchdog kill,
   * unregister-on-uninstall) so a dead app doesn't leak browser-side
   * connections.
   */
  _disposeWsForPackage(packageId) {
    for (const [handle, entry] of this._wsHandles) {
      if (entry.packageId !== packageId) continue;
      try { entry.ws.close(1000, ""); } catch (_) { /* ignore */ }
      // Resolve any in-flight recv with a synthetic Closed so the
      // app's pending future doesn't hang past worker termination.
      // (Not strictly necessary if the worker is gone, but avoids
      // pinning the future entry in any zombie state map.)
      this._wsHandles.delete(handle);
    }
    // Stone 14.2 — same idea for WebTransport. Sessions and the
    // streams under them go away with the package.
    for (const [streamHandle, entry] of this._wtStreams) {
      if (entry.packageId !== packageId) continue;
      try { entry.reader.cancel("package terminated"); } catch (_) {}
      try { entry.writer.close(); } catch (_) {}
      this._wtStreams.delete(streamHandle);
    }
    for (const [handle, entry] of this._wtHandles) {
      if (entry.packageId !== packageId) continue;
      try { entry.dgPumpAbort.abort(); } catch (_) {}
      try { entry.wt.close({ closeCode: 0, reason: "" }); } catch (_) {}
      this._wtHandles.delete(handle);
    }
  }

  // ── Stone 14.2 — WebTransport handlers ────────────────────────────────

  /**
   * Open a WebTransport session. Returns immediately after kicking
   * off the handshake; resolves the futureId once `wt.ready` settles.
   */
  _handleWtOpen(packageId, futureId, args) {
    if (typeof globalThis.WebTransport !== "function") {
      this._postHostOpDone(packageId, futureId, STATUS.ERROR,
        this._encoder.encode("wt: WebTransport not supported in this runtime"));
      return;
    }
    if (args.length < 2) {
      this._postHostOpDone(packageId, futureId, STATUS.ERROR,
        this._encoder.encode("wt: open args truncated"));
      return;
    }
    const view = new DataView(args.buffer, args.byteOffset, args.byteLength);
    const urlLen = view.getUint16(0, true);
    if (2 + urlLen > args.length) {
      this._postHostOpDone(packageId, futureId, STATUS.ERROR,
        this._encoder.encode("wt: url length exceeds args"));
      return;
    }
    const url = this._decoder.decode(args.subarray(2, 2 + urlLen));

    let wt;
    try {
      wt = new globalThis.WebTransport(url);
    } catch (err) {
      this._postHostOpDone(packageId, futureId, STATUS.ERROR,
        this._encoder.encode(`wt: construct failed: ${err?.message ?? err}`));
      return;
    }

    const wtHandle = this._nextWsHandle++;
    const dgPumpAbort = new AbortController();
    const entry = {
      wt,
      packageId,
      dgQueue: [],
      pendingDg: null,
      closed: false,
      dgPumpAbort,
    };
    this._wtHandles.set(wtHandle, entry);

    const proc = this._processes.get(packageId);
    let readySettled = false;

    wt.ready.then(() => {
      if (readySettled) return;
      readySettled = true;
      if (this._processes.get(packageId) !== proc) {
        // Worker terminated mid-handshake — tear down.
        try { wt.close({ closeCode: 0, reason: "" }); } catch (_) {}
        this._wtHandles.delete(wtHandle);
        return;
      }
      // Start the datagram pump.
      this._pumpWtDatagrams(wtHandle, entry);
      // Wire transport-level closed → enqueue Closed event.
      wt.closed.then((info) => this._onWtClosed(wtHandle, info, false))
              .catch((info) => this._onWtClosed(wtHandle, info, true));
      const out = new Uint8Array(4);
      new DataView(out.buffer).setUint32(0, wtHandle, true);
      this._postHostOpDone(packageId, futureId, STATUS.READY, out);
    }).catch((err) => {
      if (readySettled) return;
      readySettled = true;
      this._wtHandles.delete(wtHandle);
      if (this._processes.get(packageId) !== proc) return;
      this._postHostOpDone(packageId, futureId, STATUS.ERROR,
        this._encoder.encode(`wt: ready rejected: ${err?.message ?? err}`));
    });
  }

  /** Run the datagram ReadableStream pump until aborted or session
   *  closes. Each chunk becomes one entry in `dgQueue`; if a
   *  `pendingDg` recv is parked we satisfy it instead of queueing. */
  async _pumpWtDatagrams(wtHandle, entry) {
    const reader = entry.wt.datagrams.readable.getReader();
    try {
      while (!entry.dgPumpAbort.signal.aborted) {
        const { value, done } = await reader.read();
        if (done) return;
        if (entry.closed) return;
        const chunk = value instanceof Uint8Array ? value : new Uint8Array(value);
        const payload = new Uint8Array(1 + chunk.length);
        payload[0] = 0; // datagram bytes
        payload.set(chunk, 1);
        if (entry.pendingDg) {
          const { futureId } = entry.pendingDg;
          entry.pendingDg = null;
          this._postHostOpDone(entry.packageId, futureId, STATUS.READY, payload);
          continue;
        }
        if (entry.dgQueue.length >= AppRegistry._WS_RECV_QUEUE_CAP) {
          // Drop oldest — datagrams are unreliable by design.
          entry.dgQueue.shift();
        }
        entry.dgQueue.push(payload);
      }
    } catch (err) {
      this._log("warn", `[${entry.packageId}] wt#${wtHandle} datagram pump error: ${err?.message ?? err}`);
    } finally {
      try { reader.releaseLock(); } catch (_) {}
    }
  }

  _onWtClosed(wtHandle, info, _wasError) {
    const entry = this._wtHandles.get(wtHandle);
    if (!entry || entry.closed) return;
    entry.closed = true;
    const reasonStr = typeof info?.reason === "string" ? info.reason : "";
    const reasonBytes = this._encoder.encode(reasonStr);
    const code = (typeof info?.closeCode === "number") ? info.closeCode : 0;
    const payload = new Uint8Array(1 + 4 + reasonBytes.length);
    payload[0] = 1; // closed
    new DataView(payload.buffer).setUint32(1, code, true);
    payload.set(reasonBytes, 5);
    if (entry.pendingDg) {
      const { futureId } = entry.pendingDg;
      entry.pendingDg = null;
      this._postHostOpDone(entry.packageId, futureId, STATUS.READY, payload);
    } else {
      if (entry.dgQueue.length >= AppRegistry._WS_RECV_QUEUE_CAP) {
        entry.dgQueue.pop();
      }
      entry.dgQueue.push(payload);
    }
    // Streams under this session inherit the close — surface Fin
    // via their queues so any pending recvs unblock.
    for (const [streamHandle, sEntry] of this._wtStreams) {
      if (sEntry.parentWt !== wtHandle || sEntry.closed) continue;
      this._enqueueWtStreamEvent(streamHandle, sEntry, new Uint8Array([1])); // fin
    }
  }

  _handleWtRecvDatagram(packageId, futureId, args) {
    if (args.length < 4) {
      this._postHostOpDone(packageId, futureId, STATUS.ERROR,
        this._encoder.encode("wt: recv_datagram args truncated"));
      return;
    }
    const view = new DataView(args.buffer, args.byteOffset, args.byteLength);
    const wtHandle = view.getUint32(0, true);
    const entry = this._wtHandles.get(wtHandle);
    if (!entry || entry.packageId !== packageId) {
      this._postHostOpDone(packageId, futureId, STATUS.ERROR,
        this._encoder.encode("wt: unknown handle"));
      return;
    }
    if (entry.dgQueue.length > 0) {
      const ev = entry.dgQueue.shift();
      this._postHostOpDone(packageId, futureId, STATUS.READY, ev);
      return;
    }
    if (entry.pendingDg) {
      this._postHostOpDone(entry.packageId, entry.pendingDg.futureId,
        STATUS.ERROR,
        this._encoder.encode("wt: superseded by another recv"));
    }
    entry.pendingDg = { futureId };
  }

  _handleWtOpenBidi(packageId, futureId, args) {
    if (args.length < 4) {
      this._postHostOpDone(packageId, futureId, STATUS.ERROR,
        this._encoder.encode("wt: open_bidi args truncated"));
      return;
    }
    const view = new DataView(args.buffer, args.byteOffset, args.byteLength);
    const wtHandle = view.getUint32(0, true);
    const entry = this._wtHandles.get(wtHandle);
    if (!entry || entry.packageId !== packageId) {
      this._postHostOpDone(packageId, futureId, STATUS.ERROR,
        this._encoder.encode("wt: unknown handle"));
      return;
    }
    if (entry.closed) {
      this._postHostOpDone(packageId, futureId, STATUS.ERROR,
        this._encoder.encode("wt: session closed"));
      return;
    }

    entry.wt.createBidirectionalStream().then((stream) => {
      const proc = this._processes.get(packageId);
      if (!proc) {
        try { stream.readable.cancel("package gone"); } catch (_) {}
        try { stream.writable.close(); } catch (_) {}
        return;
      }
      const reader = stream.readable.getReader();
      const writer = stream.writable.getWriter();
      const streamHandle = this._nextWsHandle++;
      const sEntry = {
        reader,
        writer,
        packageId,
        parentWt: wtHandle,
        recvQueue: [],
        pendingRecv: null,
        closed: false,
      };
      this._wtStreams.set(streamHandle, sEntry);
      this._pumpWtStream(streamHandle, sEntry);
      const out = new Uint8Array(4);
      new DataView(out.buffer).setUint32(0, streamHandle, true);
      this._postHostOpDone(packageId, futureId, STATUS.READY, out);
    }).catch((err) => {
      if (this._processes.get(packageId)) {
        this._postHostOpDone(packageId, futureId, STATUS.ERROR,
          this._encoder.encode(`wt: open_bidi rejected: ${err?.message ?? err}`));
      }
    });
  }

  /** Drain a stream's reader; surface Bytes / Fin / Error events. */
  async _pumpWtStream(streamHandle, sEntry) {
    try {
      // eslint-disable-next-line no-constant-condition
      while (true) {
        const { value, done } = await sEntry.reader.read();
        if (sEntry.closed) return;
        if (done) {
          this._enqueueWtStreamEvent(streamHandle, sEntry, new Uint8Array([1])); // fin
          return;
        }
        const chunk = value instanceof Uint8Array ? value : new Uint8Array(value);
        const payload = new Uint8Array(1 + chunk.length);
        payload[0] = 0; // bytes
        payload.set(chunk, 1);
        this._enqueueWtStreamEvent(streamHandle, sEntry, payload);
      }
    } catch (err) {
      const reasonBytes = this._encoder.encode(String(err?.message ?? err));
      const payload = new Uint8Array(1 + reasonBytes.length);
      payload[0] = 2; // error
      payload.set(reasonBytes, 1);
      this._enqueueWtStreamEvent(streamHandle, sEntry, payload);
    } finally {
      try { sEntry.reader.releaseLock(); } catch (_) {}
    }
  }

  _enqueueWtStreamEvent(_streamHandle, sEntry, payload) {
    if (sEntry.pendingRecv) {
      const { futureId } = sEntry.pendingRecv;
      sEntry.pendingRecv = null;
      this._postHostOpDone(sEntry.packageId, futureId, STATUS.READY, payload);
      return;
    }
    if (sEntry.recvQueue.length >= AppRegistry._WS_RECV_QUEUE_CAP) {
      // Streams are reliable — dropping bytes silently would lie. Replace
      // the tail with an Error event so the next recv surfaces overflow.
      const errBytes = this._encoder.encode("wt: stream queue overflow");
      const errPayload = new Uint8Array(1 + errBytes.length);
      errPayload[0] = 2;
      errPayload.set(errBytes, 1);
      sEntry.recvQueue.length = 0;
      sEntry.recvQueue.push(errPayload);
      return;
    }
    sEntry.recvQueue.push(payload);
  }

  _handleWtRecvStream(packageId, futureId, args) {
    if (args.length < 4) {
      this._postHostOpDone(packageId, futureId, STATUS.ERROR,
        this._encoder.encode("wt: recv_stream args truncated"));
      return;
    }
    const view = new DataView(args.buffer, args.byteOffset, args.byteLength);
    const streamHandle = view.getUint32(0, true);
    const sEntry = this._wtStreams.get(streamHandle);
    if (!sEntry || sEntry.packageId !== packageId) {
      this._postHostOpDone(packageId, futureId, STATUS.ERROR,
        this._encoder.encode("wt: unknown stream handle"));
      return;
    }
    if (sEntry.recvQueue.length > 0) {
      const ev = sEntry.recvQueue.shift();
      this._postHostOpDone(packageId, futureId, STATUS.READY, ev);
      return;
    }
    if (sEntry.pendingRecv) {
      this._postHostOpDone(sEntry.packageId, sEntry.pendingRecv.futureId,
        STATUS.ERROR,
        this._encoder.encode("wt: stream recv superseded"));
    }
    sEntry.pendingRecv = { futureId };
  }

  _handleWtSendDatagram(packageId, args) {
    if (args.length < 4) return;
    const view = new DataView(args.buffer, args.byteOffset, args.byteLength);
    const wtHandle = view.getUint32(0, true);
    const entry = this._wtHandles.get(wtHandle);
    if (!entry || entry.packageId !== packageId) {
      this._log("warn", `[${packageId}] wt_send_datagram: unknown or foreign handle ${wtHandle} — dropped`);
      return;
    }
    if (entry.closed) return;
    const data = args.subarray(4);
    const buf = data.length > 0 ? data.slice() : new Uint8Array(0);
    // Datagrams are best-effort — backpressure is handled by silently
    // dropping rather than queueing in the writer. Browser
    // `WritableStream.getWriter().write()` returns a promise that
    // resolves when the chunk is accepted; we ignore the promise to
    // keep the SDK's fire-and-forget contract.
    try {
      entry.wt.datagrams.writable.getWriter().write(buf).catch((err) => {
        this._log("warn", `[${packageId}] wt#${wtHandle} datagram write rejected: ${err?.message ?? err}`);
      });
    } catch (err) {
      this._log("warn", `[${packageId}] wt#${wtHandle} datagram write threw: ${err?.message ?? err}`);
    }
  }

  _handleWtSendStream(packageId, args) {
    if (args.length < 4) return;
    const view = new DataView(args.buffer, args.byteOffset, args.byteLength);
    const streamHandle = view.getUint32(0, true);
    const sEntry = this._wtStreams.get(streamHandle);
    if (!sEntry || sEntry.packageId !== packageId) {
      this._log("warn", `[${packageId}] wt_send_stream: unknown/foreign stream ${streamHandle} — dropped`);
      return;
    }
    if (sEntry.closed) return;
    const data = args.subarray(4);
    const buf = data.length > 0 ? data.slice() : new Uint8Array(0);
    sEntry.writer.write(buf).catch((err) => {
      this._log("warn", `[${packageId}] wt stream#${streamHandle} write rejected: ${err?.message ?? err}`);
    });
  }

  _handleWtCloseStream(packageId, args) {
    if (args.length < 8) return;
    const view = new DataView(args.buffer, args.byteOffset, args.byteLength);
    const streamHandle = view.getUint32(0, true);
    const sEntry = this._wtStreams.get(streamHandle);
    if (!sEntry || sEntry.packageId !== packageId) {
      this._log("warn", `[${packageId}] wt_close_stream: unknown/foreign stream ${streamHandle} — dropped`);
      return;
    }
    if (sEntry.closed) return;
    sEntry.closed = true;
    sEntry.writer.close().catch(() => {});
  }

  // ── Stone 14.3 — Notifications bridge ────────────────────────────────
  //
  // Notifications go through the main-thread tab because
  // `Notification.requestPermission` requires a Window context (it
  // throws or auto-denies in workers on every browser that supports
  // worker-side `Notification` at all). The tab side does the actual
  // browser API calls and replies via the worker's message channel
  // (`completeNotifyPost` / `completeNotifyPermission` below).
  //
  // ID allocator: `_nextNotifyId` is shared across both kinds so
  // app workers can rely on a single monotonic namespace per
  // package.

  _handleNotifyRequestPermission(packageId, futureId) {
    const notifyId = this._nextNotifyId++;
    this._pendingNotifs.set(notifyId, { packageId, futureId, kind: "perm" });
    this._sendToTab({ type: "notify_request_permission", notifyId, packageId });
  }

  _handleNotifyPost(packageId, futureId, args) {
    let parsed;
    try {
      parsed = decodeNotifyPostArgs(args, this._decoder);
    } catch (err) {
      this._postHostOpDone(packageId, futureId, STATUS.ERROR,
        this._encoder.encode(`notify: bad args: ${err?.message ?? err}`));
      return;
    }
    if (!parsed.title) {
      this._postHostOpDone(packageId, futureId, STATUS.ERROR,
        this._encoder.encode("notify: empty title"));
      return;
    }
    const notifyId = this._nextNotifyId++;
    this._pendingNotifs.set(notifyId, { packageId, futureId, kind: "post" });
    this._notifyOwners.set(notifyId, packageId);
    this._sendToTab({
      type: "notify_post",
      notifyId,
      packageId,
      title: parsed.title,
      body: parsed.body,
      tag: parsed.tag,
    });
  }

  _handleNotifyClose(packageId, args) {
    if (args.length < 4) return;
    const view = new DataView(args.buffer, args.byteOffset, args.byteLength);
    const notifyId = view.getUint32(0, true);
    const owner = this._notifyOwners.get(notifyId);
    if (owner !== packageId) {
      this._log("warn", `[${packageId}] notify_close: unknown or foreign notify_id ${notifyId} — dropped`);
      return;
    }
    this._notifyOwners.delete(notifyId);
    this._sendToTab({ type: "notify_close", notifyId, packageId });
  }

  /**
   * Called by `worker.js` when the main-thread tab replies to a
   * `notify_post` / `notify_close`. `notifyId` keys back to the
   * pending future; payload is the success bit + (on error) reason.
   */
  completeNotifyPost(notifyId, ok, reason) {
    const pending = this._pendingNotifs.get(notifyId);
    if (!pending) return;
    this._pendingNotifs.delete(notifyId);
    if (pending.kind !== "post") return;
    if (ok) {
      const out = new Uint8Array(4);
      new DataView(out.buffer).setUint32(0, notifyId, true);
      this._postHostOpDone(pending.packageId, pending.futureId, STATUS.READY, out);
    } else {
      this._notifyOwners.delete(notifyId);
      this._postHostOpDone(pending.packageId, pending.futureId, STATUS.ERROR,
        this._encoder.encode(`notify: ${reason || "host refused"}`));
    }
  }

  /**
   * Called by `worker.js` with the user's permission decision for a
   * `notify_request_permission` round-trip. `status` is one of
   * `"default"`, `"granted"`, `"denied"`.
   */
  completeNotifyPermission(notifyId, status) {
    const pending = this._pendingNotifs.get(notifyId);
    if (!pending) return;
    this._pendingNotifs.delete(notifyId);
    if (pending.kind !== "perm") return;
    const code = status === "granted" ? 1 : status === "denied" ? 2 : 0;
    this._postHostOpDone(pending.packageId, pending.futureId, STATUS.READY,
      new Uint8Array([code]));
  }

  // ── Stone 14.4 — Clipboard bridge ────────────────────────────────────

  _handleClipboardRead(packageId, futureId) {
    const capId = this._nextCapId++;
    this._pendingCaps.set(capId, { packageId, futureId, kind: "clip_read" });
    this._sendToTab({ type: "clipboard_read_text", capId, packageId });
  }

  _handleClipboardWrite(packageId, futureId, args) {
    const text = this._decoder.decode(args);
    const capId = this._nextCapId++;
    this._pendingCaps.set(capId, { packageId, futureId, kind: "clip_write" });
    this._sendToTab({ type: "clipboard_write_text", capId, packageId, text });
  }

  /** Called by `worker.js` after the tab finishes a clipboard read.
   *  `text` is `null` on browser refusal; `reason` (when set)
   *  carries the host's error message. */
  completeClipboardRead(capId, text, reason) {
    const pending = this._pendingCaps.get(capId);
    if (!pending) return;
    this._pendingCaps.delete(capId);
    if (pending.kind !== "clip_read") return;
    if (text == null) {
      this._postHostOpDone(pending.packageId, pending.futureId, STATUS.ERROR,
        this._encoder.encode(reason || "clipboard read refused"));
    } else {
      this._postHostOpDone(pending.packageId, pending.futureId, STATUS.READY,
        this._encoder.encode(text));
    }
  }

  /** Called by `worker.js` after the tab finishes a clipboard write. */
  completeClipboardWrite(capId, ok, reason) {
    const pending = this._pendingCaps.get(capId);
    if (!pending) return;
    this._pendingCaps.delete(capId);
    if (pending.kind !== "clip_write") return;
    if (ok) {
      this._postHostOpDone(pending.packageId, pending.futureId, STATUS.READY, new Uint8Array(0));
    } else {
      this._postHostOpDone(pending.packageId, pending.futureId, STATUS.ERROR,
        this._encoder.encode(reason || "clipboard write refused"));
    }
  }

  // ── Stone 14.5 — Vibration (fire-and-forget) ─────────────────────────

  _handleVibrate(packageId, args) {
    if (args.length < 2) return;
    const view = new DataView(args.buffer, args.byteOffset, args.byteLength);
    const count = view.getUint16(0, true);
    if (2 + count * 4 > args.length) {
      this._log("warn", `[${packageId}] vibrate: pattern truncated — dropped`);
      return;
    }
    const pattern = new Array(count);
    for (let i = 0; i < count; i++) {
      pattern[i] = view.getUint32(2 + i * 4, true);
    }
    // No cap_id needed — fire-and-forget; the tab dispatches
    // synchronously.
    this._sendToTab({ type: "vibrate", packageId, pattern });
  }

  // ── Stone 14.6 — Geolocation bridge ──────────────────────────────────

  _handleGeoGetPosition(packageId, futureId) {
    const capId = this._nextCapId++;
    this._pendingCaps.set(capId, { packageId, futureId, kind: "geo_get" });
    this._sendToTab({ type: "geo_get_position", capId, packageId });
  }

  /** Called by `worker.js` after the browser settles a geolocation
   *  request. `position` is an object mirroring the wire fields, or
   *  `null` if the read failed; `reason` is the browser's reason
   *  text. */
  completeGeoGetPosition(capId, position, reason) {
    const pending = this._pendingCaps.get(capId);
    if (!pending) return;
    this._pendingCaps.delete(capId);
    if (pending.kind !== "geo_get") return;
    if (position == null) {
      this._postHostOpDone(pending.packageId, pending.futureId, STATUS.ERROR,
        this._encoder.encode(reason || "geolocation refused"));
      return;
    }
    const out = encodeGeoPosition(position);
    this._postHostOpDone(pending.packageId, pending.futureId, STATUS.READY, out);
  }

  /** Drop every pending cap_req owned by `packageId`. */
  _disposeCapsForPackage(packageId) {
    for (const [id, entry] of this._pendingCaps) {
      if (entry.packageId !== packageId) continue;
      this._pendingCaps.delete(id);
    }
  }

  /** Stone 14.3 — drop every pending notification owned by a
   *  terminated package so its replies don't leak past worker
   *  death. Symmetric with `_disposeWsForPackage`. */
  _disposeNotificationsForPackage(packageId) {
    for (const [id, entry] of this._pendingNotifs) {
      if (entry.packageId !== packageId) continue;
      this._pendingNotifs.delete(id);
    }
    for (const [id, owner] of this._notifyOwners) {
      if (owner !== packageId) continue;
      this._notifyOwners.delete(id);
      // Tell the tab to dismiss too — orphans on the OS notification
      // shade are ugly.
      this._sendToTab({ type: "notify_close", notifyId: id, packageId });
    }
  }

  _handleWtClose(packageId, args) {
    if (args.length < 10) return;
    const view = new DataView(args.buffer, args.byteOffset, args.byteLength);
    const wtHandle = view.getUint32(0, true);
    const entry = this._wtHandles.get(wtHandle);
    if (!entry || entry.packageId !== packageId) {
      this._log("warn", `[${packageId}] wt_close: unknown/foreign handle ${wtHandle} — dropped`);
      return;
    }
    const code = view.getUint32(4, true);
    const reasonLen = view.getUint16(8, true);
    const reasonBytes = (10 + reasonLen <= args.length)
      ? args.subarray(10, 10 + reasonLen) : new Uint8Array(0);
    const reason = reasonBytes.length > 0 ? this._decoder.decode(reasonBytes) : "";
    try {
      entry.wt.close({ closeCode: code, reason });
    } catch (err) {
      this._log("warn", `[${packageId}] wt_close threw: ${err?.message ?? err}`);
    }
  }

  /** Post a `host_op_done` reply to `packageId`'s worker, transferring
   *  the payload buffer for zero-copy. Stale-worker safe — silently
   *  drops if the worker is no longer the registered process. */
  _postHostOpDone(packageId, futureId, status, payload) {
    const proc = this._processes.get(packageId);
    if (!proc) return;
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

// ── Stone 14.1 — HTTP wire format helpers ──────────────────────────────
//
// Mirrors `framework::wire::op_kinds::io::HTTP_FETCH` and the SDK's
// `net::encode_http_args` / `decode_http_response`. The shape:
//
//   request:
//     [method:1]
//     [url_len:2 | url:utf8...]
//     [hdr_count:2]
//     hdr_count × [name_len:2 | name:utf8 | val_len:2 | val:utf8]
//     [body: bytes...]   // remainder
//
//   response:
//     [status:2]
//     [hdr_count:2]
//     hdr_count × same shape as request headers
//     [body: bytes...]

const HTTP_METHODS = Object.freeze({
  0: "GET",
  1: "POST",
  2: "PUT",
  3: "DELETE",
  4: "PATCH",
  5: "HEAD",
});

function decodeHttpRequest(args, decoder) {
  if (args.length < 1) throw new Error("empty");
  const view = new DataView(args.buffer, args.byteOffset, args.byteLength);
  const method = args[0];
  let cur = 1;
  if (cur + 2 > args.length) throw new Error("url len truncated");
  const urlLen = view.getUint16(cur, true); cur += 2;
  if (cur + urlLen > args.length) throw new Error("url truncated");
  const url = decoder.decode(args.subarray(cur, cur + urlLen)); cur += urlLen;
  if (cur + 2 > args.length) throw new Error("header count truncated");
  const hdrCount = view.getUint16(cur, true); cur += 2;
  const headers = [];
  for (let i = 0; i < hdrCount; i++) {
    if (cur + 2 > args.length) throw new Error("header name len truncated");
    const nl = view.getUint16(cur, true); cur += 2;
    if (cur + nl > args.length) throw new Error("header name truncated");
    const name = decoder.decode(args.subarray(cur, cur + nl)); cur += nl;
    if (cur + 2 > args.length) throw new Error("header val len truncated");
    const vl = view.getUint16(cur, true); cur += 2;
    if (cur + vl > args.length) throw new Error("header val truncated");
    const value = decoder.decode(args.subarray(cur, cur + vl)); cur += vl;
    headers.push([name, value]);
  }
  const body = args.slice(cur);
  return { method, url, headers, body };
}

// Stone 14.3 — wire format helper for `IO::NOTIFY_POST` args. Mirror
// of `akiva_sdk::notification::encode_post_args`.
function decodeNotifyPostArgs(args, decoder) {
  if (args.length < 2) throw new Error("title len truncated");
  const view = new DataView(args.buffer, args.byteOffset, args.byteLength);
  let cur = 0;
  const titleLen = view.getUint16(cur, true); cur += 2;
  if (cur + titleLen > args.length) throw new Error("title truncated");
  const title = decoder.decode(args.subarray(cur, cur + titleLen)); cur += titleLen;
  if (cur + 2 > args.length) throw new Error("body len truncated");
  const bodyLen = view.getUint16(cur, true); cur += 2;
  if (cur + bodyLen > args.length) throw new Error("body truncated");
  const body = decoder.decode(args.subarray(cur, cur + bodyLen)); cur += bodyLen;
  if (cur + 2 > args.length) throw new Error("tag len truncated");
  const tagLen = view.getUint16(cur, true); cur += 2;
  if (cur + tagLen > args.length) throw new Error("tag truncated");
  const tag = decoder.decode(args.subarray(cur, cur + tagLen));
  return { title, body, tag };
}

// Stone 14.6 — encode a `Position`-shaped JS object into the wire
// format documented on `IO::GEO_GET_POSITION`. Mirror of
// `akiva_sdk::geolocation::decode_position`.
function encodeGeoPosition(p) {
  const enc = new TextEncoder();
  // Compute size: 3 × f64 (24) + 3 optional triplets (max 9 × f64 + 3 × u8 = 75) + ts (8).
  // Allocate the max conservatively; we'll slice.
  const buf = new ArrayBuffer(24 + 27 + 8);
  const view = new DataView(buf);
  let cur = 0;
  view.setFloat64(cur, p.latitude, true); cur += 8;
  view.setFloat64(cur, p.longitude, true); cur += 8;
  view.setFloat64(cur, p.accuracy, true); cur += 8;
  if (typeof p.altitude === "number" && Number.isFinite(p.altitude)) {
    view.setUint8(cur, 1); cur += 1;
    view.setFloat64(cur, p.altitude, true); cur += 8;
  } else {
    view.setUint8(cur, 0); cur += 1;
  }
  if (typeof p.speed === "number" && Number.isFinite(p.speed)) {
    view.setUint8(cur, 1); cur += 1;
    view.setFloat64(cur, p.speed, true); cur += 8;
  } else {
    view.setUint8(cur, 0); cur += 1;
  }
  if (typeof p.heading === "number" && Number.isFinite(p.heading)) {
    view.setUint8(cur, 1); cur += 1;
    view.setFloat64(cur, p.heading, true); cur += 8;
  } else {
    view.setUint8(cur, 0); cur += 1;
  }
  view.setFloat64(cur, p.timestamp ?? 0, true); cur += 8;
  // Suppress unused warning; `enc` is here to keep parity with other
  // encoders that might add string fields later.
  void enc;
  return new Uint8Array(buf, 0, cur);
}

function encodeHttpResponse(status, headerPairs, body) {
  // Cap the header set to 256 entries — `Response.headers.forEach`
  // on a typical server returns ~10. The cap is pure overflow
  // protection; it doesn't drop interesting headers in practice.
  const hdrCount = Math.min(headerPairs.length, 0xFFFF);
  let size = 2 + 2;
  for (let i = 0; i < hdrCount; i++) {
    size += 2 + headerPairs[i][0].length + 2 + headerPairs[i][1].length;
  }
  size += body.length;
  const out = new Uint8Array(size);
  const view = new DataView(out.buffer);
  view.setUint16(0, status, true);
  view.setUint16(2, hdrCount, true);
  let cur = 4;
  const enc = new TextEncoder();
  for (let i = 0; i < hdrCount; i++) {
    const [name, value] = headerPairs[i];
    const nb = enc.encode(name);
    const vb = enc.encode(value);
    view.setUint16(cur, nb.length, true); cur += 2;
    out.set(nb, cur); cur += nb.length;
    view.setUint16(cur, vb.length, true); cur += 2;
    out.set(vb, cur); cur += vb.length;
  }
  out.set(body, cur);
  return out;
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
  "akiva.permission.CLIPBOARD":       Object.freeze([]),
  "akiva.permission.VIBRATE":         Object.freeze([]),
  "akiva.permission.LOCATION":        Object.freeze([]),
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
