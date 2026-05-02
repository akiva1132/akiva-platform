// App worker shell — runs inside a Dedicated Worker, hosts a single
// app's `WebAssembly.Instance`, and speaks the post-message protocol
// the kernel-worker (`worker.js`) drives via its `WorkerPool`.
//
// One of these workers per installed package, spawned from the main
// kernel worker. Each app's wasm sees only this worker's memory —
// no shared state with the kernel or with other apps.
//
// ── Inbound messages (kernel-worker → app-worker) ────────────────────
//   { type:"init", packageId, wasmBytes, grants?, packagesBlob?,
//     resources? }
//     One-shot bootstrap. Compiles + instantiates the wasm. Replies
//     with `init_ok` or `init_failed`.
//
//   { type:"dispatch", dispatch_id, cmd_kind, payload }
//     Invoke the app's `akiva_app_dispatch(cmd_kind, ptr, len)`. If
//     the returned future_id is non-zero, the shell calls
//     `akiva_app_get_result` to fetch the bytes and ships them back
//     in `dispatch_done`.
//
//   { type:"host_op_done", future_id, status, payload }
//     Reply to a host_op the shell raised earlier (Stone 11.4c-4).
//     Stub for now — wired in 11.4c-4.
//
// ── Outbound messages (app-worker → kernel-worker) ───────────────────
//   { type:"init_ok" }
//   { type:"init_failed", reason }
//   { type:"dispatch_done", dispatch_id, future_id, bytes }
//     `bytes` is a Transferable Uint8Array (or null when no result
//     bytes were produced). Buffer ownership transfers — the shell
//     does not retain a reference.
//   { type:"log", level, text }
//   { type:"trapped", reason }
//     Set when an exported function throws. Stone 11.5 makes this
//     observable to the framework via `LifecycleState::Trapped`.
//   { type:"host_op", future_id, op_kind, args }
//     Stone 11.4c-4 — placeholder for now.
//   { type:"side_effect", kind, handle, payload }
//     Stone 11.4c-5 — placeholder for now.
//
// 11.4c-1 scope: the dispatch path + local stub host imports. Cross-
// worker host_op + side_effect channels land in 11.4c-4 / 11.4c-5.

import { FIRE, IO, STATUS, packPoll, isFireKind, isIoKind } from "./op_kinds.mjs";

// ── State (one per worker) ─────────────────────────────────────────────

/** @type {string | null} */
let packageId = null;
/** @type {WebAssembly.Module | null} */
let wasmModule = null;
/** @type {WebAssembly.Instance | null} */
let wasmInstance = null;
/** @type {WebAssembly.Memory | null} */
let wasmMemory = null;
/** @type {Set<string>} */
let grants = new Set();
/** @type {Uint8Array} */
let packagesBlob = new Uint8Array(0);
/** @type {{ strings: Map<string, string>, colors: Map<string, number> }} */
let resources = { strings: new Map(), colors: new Map() };

/**
 * Local future registry for `akiva_host_io_start` / `akiva_host_io_poll`.
 * Same shape as `AppRegistry._futures` in single-worker mode (Stone
 * 11.3c). Entries that need a kernel round-trip stay PENDING here
 * until `host_op_done` flips them — wired in 11.4c-4.
 *
 * @type {Map<bigint, { status: number, payload: Uint8Array }>}
 */
const futures = new Map();
/** @type {bigint} */
let nextFutureId = 1n;

const decoder = new TextDecoder("utf-8");
const encoder = new TextEncoder();

/**
 * Dedicated 1×1 OffscreenCanvas for `measureText`. Constructed lazily
 * on the first IO::TEXT_WIDTH (mirrors AppRegistry's pattern — keeps
 * `OffscreenCanvas` out of the constructor so the same source can run
 * under vitest jsdom in handler-unit tests).
 */
let measureCtx = null;
function ensureMeasureCtx() {
  if (measureCtx) return measureCtx;
  const c = new OffscreenCanvas(1, 1);
  measureCtx = c.getContext("2d");
  return measureCtx;
}

// ── Outbound helpers ───────────────────────────────────────────────────

function postLog(level, text) {
  postMessage({ type: "log", level, text: `[worker:${packageId ?? "?"}] ${text}` });
}

function postTrapped(reason) {
  postMessage({ type: "trapped", reason });
}

// ── Required app exports ───────────────────────────────────────────────
//
// P0 #4 added `akiva_app_poll_dispatch` to support async-resumable
// dispatches. The shell allocates a `dispatch_id`, hands it to wasm
// via the new `dispatch(dispatch_id, cmd, ptr, len)` ABI, polls via
// `poll_dispatch(dispatch_id)`, and re-polls on every `host_op_done`
// arrival until the dispatch reaches READY/ERROR.

const REQUIRED_EXPORTS = [
  "memory",
  "akiva_app_alloc",
  "akiva_app_dealloc",
  "akiva_app_abi_version",
  "akiva_app_dispatch",
  "akiva_app_poll_dispatch",
  "akiva_app_get_result",
  // Stone 11.6.5 (#11) — pumps `TaskScope::spawn`-queued futures.
  // Called after every dispatch resolution and every host_op_done so
  // tasks awaiting host ops wake when their reply lands.
  "akiva_app_drive_spawned",
];

/**
 * In-flight dispatches whose wasm-side future has returned PENDING and
 * is waiting on a `host_op_done`. Keyed by `dispatch_id` (the same id
 * `AppRegistry` allocated). Each entry holds the kernel-facing
 * dispatch metadata so we can post `dispatch_done` once the wasm
 * resolves.
 *
 * P0 #4 — re-polled every time `host_op_done` arrives, since any
 * pending dispatch could be the one waiting on this exact future.
 *
 * @type {Map<bigint, { cmd_kind: number }>}
 */
const pendingDispatches = new Map();

/** Status constants mirrored locally so the dispatch loop doesn't
 *  need to import op_kinds — the wasm uses the same numeric values. */
const POLL_STATUS = Object.freeze({
  PENDING: 0,
  READY:   1,
  ERROR:   2,
});

// ── Inbound dispatch ───────────────────────────────────────────────────

self.addEventListener("message", (e) => {
  const msg = e.data;
  switch (msg?.type) {
    case "init":
      handleInit(msg);
      break;
    case "dispatch":
      handleDispatch(msg);
      break;
    case "host_op_done":
      handleHostOpDone(msg);
      break;
    case "set_grants":
      grants = new Set(msg.grants ?? []);
      break;
    case "set_packages":
      packagesBlob = msg.packagesBlob instanceof Uint8Array ? msg.packagesBlob : new Uint8Array(0);
      break;
    case "ping":
      // Stone 11.5c — JS-level heartbeat. Replied to in the message
      // handler itself, BEFORE we ever touch the wasm instance, so a
      // dispatch that's about to recurse into wasm doesn't delay the
      // pong. If wasm is stuck in a `loop {}` the handler doesn't
      // run at all (single-threaded worker) — that's exactly what
      // the kernel's timeout uses to detect a hang.
      //
      // Phase 12.1 — piggyback the app's current
      // `WebAssembly.Memory.buffer.byteLength` on every pong so the
      // kernel-side `MemoryManager` gets a fresh sample without an
      // extra round-trip. Pre-init `wasmMemory` is null; reporting 0
      // matches "no instance yet, nothing to charge".
      postMessage({
        type: "pong",
        t: msg.t,
        memory_bytes: wasmMemory ? wasmMemory.buffer.byteLength : 0,
      });
      break;
    default:
      postLog("warn", `unknown message type: ${msg?.type}`);
  }
});

// ── init ───────────────────────────────────────────────────────────────

function handleInit(msg) {
  packageId = msg.packageId;
  grants = new Set(msg.grants ?? []);
  packagesBlob = msg.packagesBlob instanceof Uint8Array ? msg.packagesBlob : new Uint8Array(0);
  if (msg.resources) {
    resources = {
      strings: new Map(Object.entries(msg.resources.strings ?? {})),
      colors: new Map(Object.entries(msg.resources.colors ?? {})),
    };
  }

  // Sync compile is intentional: the worker's incoming message queue
  // is FIFO, so a dispatch arriving during init can't be processed
  // before this handler returns. An async (await-based) version
  // would yield the event loop mid-compile and let `handleDispatch`
  // run with `wasmInstance` still null — the app would silently
  // miss CREATE/RESIZE/START/RESUME. The cold-start cost (~50-200ms
  // per app) is paid once per package and dominated by the network
  // fetch of `app.wasm`, not the compile itself.
  let module, instance;
  try {
    module = new WebAssembly.Module(msg.wasmBytes);
    instance = new WebAssembly.Instance(module, buildHostImports());
  } catch (err) {
    postMessage({ type: "init_failed", reason: `instantiate: ${err?.message ?? err}` });
    return;
  }

  const exports = instance.exports;
  for (const req of REQUIRED_EXPORTS) {
    if (req === "memory") {
      if (!(exports.memory instanceof WebAssembly.Memory)) {
        postMessage({ type: "init_failed", reason: `missing 'memory' export` });
        return;
      }
    } else if (typeof exports[req] !== "function") {
      postMessage({ type: "init_failed", reason: `missing export '${req}'` });
      return;
    }
  }

  wasmModule = module;
  wasmInstance = instance;
  wasmMemory = exports.memory;
  postMessage({ type: "init_ok" });
}

// ── dispatch ───────────────────────────────────────────────────────────

function handleDispatch(msg) {
  if (!wasmInstance) {
    postDispatchDone(msg.dispatch_id, null);
    return;
  }
  const { dispatch_id, cmd_kind, payload } = msg;
  const bytes = payload instanceof Uint8Array ? payload : new Uint8Array(payload ?? 0);
  const dispatchId = typeof dispatch_id === "bigint" ? dispatch_id : BigInt(dispatch_id);

  // Two-phase buffer write: alloc, copy, dispatch. The wasm reads
  // (or doesn't) the bytes synchronously inside `akiva_app_dispatch`,
  // so we can dealloc the input buffer right after — its contents
  // have either been consumed or rejected before the call returns.
  const exports = wasmInstance.exports;
  let appPtr = 0;
  let acceptStatus = 1;  // non-zero = rejected; updated below
  try {
    if (bytes.length > 0) {
      appPtr = exports.akiva_app_alloc(bytes.length);
      if (appPtr === 0) {
        postLog("warn", `alloc(${bytes.length}) for dispatch(${cmd_kind}) returned null`);
        postDispatchDone(dispatchId, null);
        return;
      }
      new Uint8Array(wasmMemory.buffer, appPtr, bytes.length).set(bytes);
    }
    acceptStatus = exports.akiva_app_dispatch(dispatchId, cmd_kind, appPtr, bytes.length);
  } catch (err) {
    postLog("error", `dispatch(${cmd_kind}) trapped: ${err?.message ?? err}`);
    postTrapped(`dispatch(${cmd_kind}): ${err?.message ?? err}`);
    postDispatchDone(dispatchId, null);
    return;
  } finally {
    if (appPtr) {
      try { exports.akiva_app_dealloc(appPtr, bytes.length); }
      catch (_) { /* best effort */ }
    }
  }
  if (acceptStatus !== 0) {
    // Hard refusal — bad payload, unknown cmd_kind, OOM in slot
    // construction. Treat as ack with no bytes.
    postLog("warn", `dispatch(${cmd_kind}) refused: status=${acceptStatus}`);
    postDispatchDone(dispatchId, null);
    return;
  }

  // Try to drive the dispatch to terminal status synchronously. If
  // it parks on a host_op, we'll re-poll once host_op_done arrives.
  pendingDispatches.set(dispatchId, { cmd_kind });
  driveDispatch(dispatchId);
  // Stone 11.6.5 (#11) — the dispatch body may have spawned tasks
  // (e.g. on_resume calling `ctx.resumed_scope().spawn(...)`). Pump
  // them once now so first-poll-Ready spawns resolve before we
  // return; pending ones stay armed for the next host_op_done.
  driveSpawned();
}

/**
 * Re-poll `dispatchId` and finalise it if the wasm has reached a
 * terminal state. Called once from `handleDispatch` and again from
 * `handleHostOpDone` for every still-pending dispatch (any of them
 * could have been waiting on the host op that just resolved).
 *
 * No-op if `dispatchId` is no longer pending (already finalised in
 * a prior call).
 */
function driveDispatch(dispatchId) {
  if (!pendingDispatches.has(dispatchId)) return;
  const exports = wasmInstance.exports;
  let packed;
  try {
    packed = exports.akiva_app_poll_dispatch(dispatchId);
  } catch (err) {
    postLog("error", `poll_dispatch(${dispatchId}) trapped: ${err?.message ?? err}`);
    postTrapped(`poll_dispatch: ${err?.message ?? err}`);
    pendingDispatches.delete(dispatchId);
    postDispatchDone(dispatchId, null);
    return;
  }
  // Packed encoding: low 32 bits status, high 32 bits length. We
  // only need status to gate the next step; length tells us whether
  // to fetch bytes via get_result.
  const status = Number(BigInt.asUintN(32, BigInt(packed)));
  if (status === POLL_STATUS.PENDING) {
    return;  // leave in pendingDispatches; re-poll on next host_op_done
  }
  pendingDispatches.delete(dispatchId);
  if (status === POLL_STATUS.ERROR) {
    postLog("error", `dispatch(${dispatchId}) reported ERROR`);
    postDispatchDone(dispatchId, null);
    return;
  }
  // status === READY
  let resultBytes = null;
  try {
    resultBytes = fetchResult(dispatchId);
  } catch (err) {
    postLog("error", `get_result(${dispatchId}) trapped: ${err?.message ?? err}`);
    postTrapped(`get_result: ${err?.message ?? err}`);
    postDispatchDone(dispatchId, null);
    return;
  }
  postDispatchDone(dispatchId, resultBytes);
}

function postDispatchDone(dispatchId, resultBytes) {
  const transfer = resultBytes ? [resultBytes.buffer] : [];
  postMessage(
    { type: "dispatch_done", dispatch_id: dispatchId, future_id: 0n, bytes: resultBytes },
    transfer,
  );
}

/**
 * Fetch the bytes the app produced for a READY dispatch. Two-phase
 * buffer: app writes length to a 4-byte slot we allocate, returns
 * the source pointer, we slice into a fresh Uint8Array (so the buffer
 * is transferable without aliasing live wasm memory).
 */
function fetchResult(dispatchId) {
  const exports = wasmInstance.exports;
  const lenPtr = exports.akiva_app_alloc(4);
  if (lenPtr === 0) {
    postLog("warn", `alloc(4) for get_result(${dispatchId}) returned null`);
    return null;
  }
  let len, srcPtr;
  try {
    srcPtr = exports.akiva_app_get_result(dispatchId, lenPtr);
    len = new DataView(wasmMemory.buffer).getUint32(lenPtr, true);
  } finally {
    try { exports.akiva_app_dealloc(lenPtr, 4); }
    catch (_) { /* best effort */ }
  }
  if (len === 0 || srcPtr === 0) return null;
  // slice() gives us an owned ArrayBuffer we can transfer; a view
  // into wasm memory cannot be transferred.
  return new Uint8Array(wasmMemory.buffer, srcPtr, len).slice();
}

// ── host_op_done ───────────────────────────────────────────────────────

function handleHostOpDone(msg) {
  const fid = typeof msg.future_id === "bigint" ? msg.future_id : BigInt(msg.future_id);
  const entry = futures.get(fid);
  if (!entry) {
    postLog("warn", `host_op_done for unknown future_id ${fid}`);
    return;
  }
  entry.status = msg.status;
  entry.payload = msg.payload instanceof Uint8Array ? msg.payload : new Uint8Array(0);
  // P0 #4 — re-drive every pending dispatch. Any one of them could
  // have been parked on this exact future, and re-polling them now
  // is the cheapest way to find out (the wasm side caches terminal
  // results, so calls into already-resolved dispatches are no-ops).
  if (pendingDispatches.size > 0) {
    // Snapshot the keys — driveDispatch may delete entries during
    // iteration if a dispatch finalises.
    const ids = [...pendingDispatches.keys()];
    for (const id of ids) {
      driveDispatch(id);
    }
  }
  // Stone 11.6.5 (#11) — pump TaskScope-spawned tasks too. A spawned
  // task may have parked on this exact host_op (storage::read,
  // text_width, query_packages...), and there's no per-task wake-up
  // signal; the host op resolution is itself the trigger.
  driveSpawned();
}

/**
 * Stone 11.6.5 (#11) — call the wasm export that polls every
 * `TaskScope`-spawned future across every live activity slot.
 * Cheap when no spawns exist (the wasm side returns 0 immediately).
 * Traps inside the export are reported as a hard worker fault — a
 * spawned future panicking takes down the whole worker today, same
 * fault model as a callback panicking.
 */
function driveSpawned() {
  if (!wasmInstance) return;
  try {
    wasmInstance.exports.akiva_app_drive_spawned();
  } catch (err) {
    postLog("error", `drive_spawned trapped: ${err?.message ?? err}`);
    postTrapped(`drive_spawned: ${err?.message ?? err}`);
  }
}

// ── Host imports the app links against ────────────────────────────────
//
// Same surface as `AppRegistry._buildAppImports` in single-worker
// mode pre-11.4c. Capability gating happens inside the dispatcher,
// not at the import boundary — the imports themselves are always
// installed.

function buildHostImports() {
  return {
    env: {
      // ── Resources (always available) ─────────────────────────────
      akiva_host_resource_color: (ptr, len) => {
        const name = readAppUtf8(ptr, len);
        if (name === null) return MISSING_COLOR;
        const v = resources.colors.get(name);
        if (v === undefined) {
          postLog("warn", `@color/${name}: undefined (using magenta placeholder)`);
          return MISSING_COLOR;
        }
        return v >>> 0;
      },
      akiva_host_resource_string: (ptr, len, outPtr, outCap) => {
        const name = readAppUtf8(ptr, len);
        if (name === null) return 0;
        let value = resources.strings.get(name);
        if (value === undefined) {
          postLog("warn", `@string/${name}: undefined (using placeholder)`);
          value = `⟨missing:${name}⟩`;
        }
        const bytes = encoder.encode(value);
        if (bytes.length > outCap) return bytes.length;
        new Uint8Array(wasmMemory.buffer, outPtr, bytes.length).set(bytes);
        return bytes.length;
      },
      akiva_host_resource_has_color: (ptr, len) => {
        const name = readAppUtf8(ptr, len);
        return name !== null && resources.colors.has(name) ? 1 : 0;
      },
      akiva_host_resource_has_string: (ptr, len) => {
        const name = readAppUtf8(ptr, len);
        return name !== null && resources.strings.has(name) ? 1 : 0;
      },

      // ── Unified-ABI multiplexers ─────────────────────────────────
      akiva_host_fire: (opKind, argsPtr, argsLen) => {
        const args = readAppBytes(argsPtr, argsLen);
        if (args === null) return 1;
        return dispatchFire(opKind, args);
      },
      akiva_host_io_start: (opKind, argsPtr, argsLen) => {
        const args = readAppBytes(argsPtr, argsLen);
        if (args === null) return 0n;
        return dispatchIoStart(opKind, args);
      },
      akiva_host_io_poll: (futureId, outPtr, outCap) => {
        const fid = typeof futureId === "bigint" ? futureId : BigInt(futureId);
        return pollFuture(fid, outPtr, outCap);
      },
    },
  };
}

// ── Fire dispatch ──────────────────────────────────────────────────────

function dispatchFire(opKind, args) {
  if (!isFireKind(opKind)) {
    postLog("warn", `fire(0x${opKind.toString(16)}): not a fire op_kind`);
    return 1;
  }
  switch (opKind) {
    case FIRE.LOG: {
      if (args.length < 4) return 1;
      const view = new DataView(args.buffer, args.byteOffset, args.byteLength);
      const level = view.getUint32(0, true);
      const text = decoder.decode(args.subarray(4));
      postMessage({ type: "log", level: levelName(level), text });
      return 0;
    }
    case FIRE.FINISH: {
      // No grant required — finish is the app shutting itself down.
      // Forward to kernel so `hal_app_drain_finish` flips on the
      // next ctx-bearing callback.
      if (args.length < 4) return 1;
      postSideEffect(opKind, args);
      return 0;
    }
    case FIRE.START_ACTIVITY:
    case FIRE.START_ACTIVITY_FOR_RESULT: {
      if (!grants.has("akiva.permission.LAUNCH_ANY_APP")) {
        postLog("warn", `fire(0x${opKind.toString(16)}): denied — no LAUNCH_ANY_APP grant`);
        return 2;
      }
      const minLen = opKind === FIRE.START_ACTIVITY ? 4 : 8;
      if (args.length < minLen) return 1;
      postSideEffect(opKind, args);
      return 0;
    }
    case FIRE.SET_RESULT: {
      if (args.length < 4) return 1;
      postSideEffect(opKind, args);
      return 0;
    }
    default:
      postLog("warn", `fire(0x${opKind.toString(16)}): unknown op_kind`);
      return 1;
  }
}

/**
 * Ship a side-effect to the kernel-worker for handle-keyed staging
 * (FINISH/START_ACTIVITY/SET_RESULT). The args buffer is transferred
 * — caller must not retain a reference after calling this.
 */
function postSideEffect(opKind, args) {
  postMessage(
    { type: "side_effect", op_kind: opKind, args },
    [args.buffer],
  );
}

// ── IO start/poll dispatch ─────────────────────────────────────────────

function dispatchIoStart(opKind, args) {
  if (!isIoKind(opKind)) {
    postLog("warn", `io_start(0x${opKind.toString(16)}): not an io op_kind`);
    return 0n;
  }
  switch (opKind) {
    case IO.TEXT_WIDTH: {
      // Resolved locally — every worker has its own OffscreenCanvas
      // for `measureText`, so no kernel round-trip needed.
      if (args.length < 4) return 0n;
      const view = new DataView(args.buffer, args.byteOffset, args.byteLength);
      const size = view.getFloat32(0, true);
      const text = decoder.decode(args.subarray(4));
      const ctx = ensureMeasureCtx();
      ctx.font = `${size}px sans-serif`;
      const width = ctx.measureText(text).width;
      const out = new Uint8Array(4);
      new DataView(out.buffer).setFloat32(0, width, true);
      const fid = nextFutureId++;
      futures.set(fid, { status: STATUS.READY, payload: out });
      return fid;
    }
    case IO.QUERY_PACKAGES: {
      // Resolved locally — the kernel pushes the snapshot at `init`
      // and again on every `setPackages` (install/uninstall). The
      // SDK's `read_blob` polls just once and gives up on PENDING,
      // so a kernel round-trip would race the first frame; serving
      // the cached blob here makes the call synchronously READY in
      // the same dispatch the wasm raised it.
      if (!grants.has("akiva.permission.QUERY_PACKAGES")) {
        postLog("warn", `io_start(QUERY_PACKAGES): denied — no QUERY_PACKAGES grant`);
        return 0n;
      }
      const fid = nextFutureId++;
      futures.set(fid, { status: STATUS.READY, payload: packagesBlob.slice() });
      return fid;
    }
    case IO.STORAGE_GET:
    case IO.STORAGE_SET: {
      // Storage ops genuinely need the kernel — backed by OPFS the
      // app-worker can't see directly (Stone 11.6 wires read-only
      // handles into the worker, but writes still proxy here).
      // Allocate a local future_id, stage as PENDING, post host_op,
      // and let host_op_done flip it for the next io_poll.
      const fid = nextFutureId++;
      futures.set(fid, { status: STATUS.PENDING, payload: new Uint8Array(0) });
      postMessage(
        { type: "host_op", future_id: fid, op_kind: opKind, args },
        [args.buffer],
      );
      return fid;
    }
    default:
      postLog("warn", `io_start(0x${opKind.toString(16)}): unknown op_kind`);
      return 0n;
  }
}

function pollFuture(futureId, outPtr, outCap) {
  const entry = futures.get(futureId);
  if (!entry) return packPoll(STATUS.ERROR, 0);
  if (entry.status === STATUS.PENDING) return packPoll(STATUS.PENDING, 0);
  const len = entry.payload.length;
  if (len > outCap) return packPoll(entry.status, len);
  if (len > 0 && wasmMemory) {
    new Uint8Array(wasmMemory.buffer, outPtr, len).set(entry.payload);
  }
  futures.delete(futureId);
  return packPoll(entry.status, len);
}

// ── App-memory readers ─────────────────────────────────────────────────

function readAppUtf8(ptr, len) {
  if (!wasmMemory || len === 0) return "";
  return decoder.decode(new Uint8Array(wasmMemory.buffer, ptr, len));
}

function readAppBytes(ptr, len) {
  if (!wasmMemory) return null;
  if (len === 0) return new Uint8Array(0);
  const buf = wasmMemory.buffer;
  if (ptr < 0 || ptr + len > buf.byteLength) return null;
  const copy = new Uint8Array(len);
  copy.set(new Uint8Array(buf, ptr, len));
  return copy;
}

// ── Constants ──────────────────────────────────────────────────────────

const MISSING_COLOR = 0xFFFF00FF;

const LEVEL_NAMES = ["trace", "debug", "info", "warn", "error"];
function levelName(n) {
  return LEVEL_NAMES[n] ?? "info";
}
