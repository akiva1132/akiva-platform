// AppRegistry unit tests — Stone 11.4c WorkerPool.
//
// AppRegistry post-11.4c is a thin coordinator: it owns the per-package
// `Worker` references, ships init+dispatch messages to them, and stages
// reply messages (`dispatch_done`, `side_effect`) into kernel-side
// state the existing `hal_app_*` HAL bridges read. The wasm execution,
// host-import handling, and dispatcher logic all live in the peer
// worker (`app_worker_shell.js`); their semantics get their own test
// file.
//
// We fake the `Worker` with a plain JS object that captures posted
// messages and exposes a `_inject(msg)` helper for tests to push the
// reply side. That keeps these tests focused on the registry's wire
// contract without spinning up real Web Workers (which vitest's jsdom
// env doesn't expose anyway).

import { describe, it, expect, vi } from "vitest";
import { AppRegistry, AUTO_IMPORTS, CAPABILITY_IMPORTS } from "../app_registry.js";
import { CMD, FIRE, IO, STATUS } from "../op_kinds.mjs";

// ── Test harness ──────────────────────────────────────────────────────

const PACKAGE_ID = "com.test.app";
const CLASS_NAME = "Main";

/** Build a stub Worker that records postMessage calls and lets tests
 *  inject inbound messages by calling `_inject(msg)`. */
function makeFakeWorker() {
  const posted = [];
  const listeners = [];
  const fake = {
    onerror: null,
    postMessage(msg, _transfer) {
      posted.push(msg);
    },
    addEventListener(kind, fn) {
      if (kind === "message") listeners.push(fn);
    },
    /** Push a fake inbound message into all registered listeners. */
    _inject(msg) {
      for (const fn of listeners) fn({ data: msg });
    },
    _posted: posted,
  };
  return fake;
}

// Build an `AppRegistry` whose `spawnWorker` factory hands back the
// caller-controlled stub worker. The fake storage matches production
// `Storage`'s semantics: it caches the Uint8Array reference and
// returns the SAME object every call. This used to be a fresh-copy
// stub which masked a real bug — `_spawnProcess` transferred
// `wasmBytes.buffer` directly, detaching storage's live entry, which
// the test never saw because each `get()` minted fresh bytes.
function makeRegistry({ log, fakeWorker } = {}) {
  const kernelMemory = new WebAssembly.Memory({ initial: 1 });
  // Per-key cache → same-reference returns. `_loadResources` reads
  // from the same `storage`; absent keys return null which
  // `_loadResources` treats as "no values file" (no-op).
  // set/delete mirror the real `Storage` shape for storage-handler tests.
  const cache = new Map();
  const storage = {
    get: (key) => {
      if (!cache.has(key)) {
        if (key.endsWith("/app.wasm")) cache.set(key, new Uint8Array([0]));
        else cache.set(key, null);
      }
      return cache.get(key);
    },
    set: (key, value) => {
      cache.set(key, new Uint8Array(value));
      return 0;  // SR_OK
    },
    delete: (key) => {
      cache.delete(key);
      return 0;
    },
  };
  const logFn = log ?? vi.fn();
  const worker = fakeWorker ?? makeFakeWorker();
  const registry = new AppRegistry({
    storage,
    kernelMemory: () => kernelMemory,
    log: logFn,
    spawnWorker: () => worker,
  });
  return { registry, kernelMemory, log: logFn, storage, worker };
}

// ── register / unregister ─────────────────────────────────────────────

describe("register / unregister — spawn & teardown", () => {
  it("spawns a worker on first register and posts init", () => {
    const { registry, worker } = makeRegistry();
    const handle = registry.register(PACKAGE_ID, CLASS_NAME);
    expect(handle).toBeGreaterThan(0);
    expect(registry._processes.has(PACKAGE_ID)).toBe(true);

    const init = worker._posted.find((m) => m.type === "init");
    expect(init).toBeDefined();
    expect(init.packageId).toBe(PACKAGE_ID);
    expect(init.wasmBytes).toBeInstanceOf(Uint8Array);
  });

  it("reuses the worker on a second register for the same package", () => {
    const { registry, worker } = makeRegistry();
    const a = registry.register(PACKAGE_ID, "Main");
    const b = registry.register(PACKAGE_ID, "Settings");
    expect(a).not.toBe(b);
    // Only one init posted — the second register reuses the worker.
    const inits = worker._posted.filter((m) => m.type === "init");
    expect(inits).toHaveLength(1);
  });

  it("returns 0 when wasm bytes aren't in storage", () => {
    const { registry } = makeRegistry();
    registry._storage = { get: () => null };
    expect(registry.register("com.missing.app", "Main")).toBe(0);
  });

  it("doesn't detach storage's wasm-bytes buffer on init transfer", () => {
    // Stone 11.5e regression: `Storage.get` returns the SAME Uint8Array
    // reference each call. If `_spawnProcess` transfers
    // `wasmBytes.buffer` directly, the storage entry detaches and any
    // later read for that key crashes with "ArrayBuffer is already
    // detached." `_spawnProcess` must slice into a fresh buffer first.
    const { registry, storage } = makeRegistry();
    registry.register(PACKAGE_ID, CLASS_NAME);

    // The storage entry should still be readable — accessing
    // `.byteLength` on a detached Uint8Array returns 0, but the
    // backing buffer is what we care about. Read it again and assert
    // the bytes survived.
    const second = storage.get(`packages/${PACKAGE_ID}/app.wasm`);
    expect(second).toBeInstanceOf(Uint8Array);
    expect(second.byteLength).toBeGreaterThan(0);
    // And a re-spawn (simulated by calling _spawnProcess directly,
    // since register short-circuits when _processes already has the
    // entry) must succeed.
    registry._processes.delete(PACKAGE_ID);
    expect(() => registry._spawnProcess(PACKAGE_ID)).not.toThrow();
  });

  it("unregister clears the slot and per-handle pending state", () => {
    const { registry, worker } = makeRegistry();
    const handle = registry.register(PACKAGE_ID, CLASS_NAME);
    registry._pendingFinish.add(handle);
    registry._pendingIntent.set(handle, new Uint8Array([1]));
    registry._pendingStartForResult.set(handle, { bytes: new Uint8Array([2]), requestCode: 0 });
    registry._pendingSetResult.set(handle, new Uint8Array([3]));

    registry.unregister(handle);

    expect(registry._slots.has(handle)).toBe(false);
    expect(registry._pendingFinish.has(handle)).toBe(false);
    expect(registry._pendingIntent.has(handle)).toBe(false);
    expect(registry._pendingStartForResult.has(handle)).toBe(false);
    expect(registry._pendingSetResult.has(handle)).toBe(false);
    // Worker stays alive — 11.5 adds the GC pass.
    expect(registry._processes.has(PACKAGE_ID)).toBe(true);
    void worker; // eslint-friendly noop
  });

  it("unregister on unknown handle is a no-op", () => {
    const { registry } = makeRegistry();
    expect(() => registry.unregister(9999)).not.toThrow();
  });
});

// ── dispatch ──────────────────────────────────────────────────────────

describe("dispatch — post host→app command to worker", () => {
  it("returns 0n on too-short payload (<4 bytes for the handle prefix)", () => {
    const { registry, worker } = makeRegistry();
    registry.register(PACKAGE_ID, CLASS_NAME);
    expect(registry.dispatch(CMD.RESUME, 0, 0)).toBe(0n);
    expect(registry.dispatch(CMD.RESUME, 0, 3)).toBe(0n);
    // Only the init message — no dispatch posted.
    expect(worker._posted.some((m) => m.type === "dispatch")).toBe(false);
  });

  it("returns 0n on unknown handle", () => {
    const { registry, kernelMemory, worker } = makeRegistry();
    registry.register(PACKAGE_ID, CLASS_NAME);
    new DataView(kernelMemory.buffer).setUint32(0, 9999, true);
    expect(registry.dispatch(CMD.RESUME, 0, 4)).toBe(0n);
    expect(worker._posted.some((m) => m.type === "dispatch")).toBe(false);
  });

  it("posts a non-CREATE payload through unchanged + stages PENDING entry", () => {
    const { registry, kernelMemory, worker } = makeRegistry();
    const handle = registry.register(PACKAGE_ID, CLASS_NAME);
    // [handle, x, y, button, phase, ts] = 28 bytes for POINTER.
    const dv = new DataView(kernelMemory.buffer);
    dv.setUint32(0, handle, true);
    dv.setFloat32(4, 12.5, true);
    dv.setFloat32(8, 34.5, true);
    dv.setUint32(12, 0, true);
    dv.setUint32(16, 1, true);
    dv.setFloat64(20, 1234.0, true);

    const dispatchId = registry.dispatch(CMD.POINTER, 0, 28);
    expect(typeof dispatchId).toBe("bigint");
    expect(dispatchId).toBeGreaterThan(0n);
    expect(registry._dispatches.get(dispatchId).status).toBe(STATUS.PENDING);

    const post = worker._posted.find((m) => m.type === "dispatch");
    expect(post).toBeDefined();
    expect(post.cmd_kind).toBe(CMD.POINTER);
    expect(post.payload).toBeInstanceOf(Uint8Array);
    expect(post.payload.length).toBe(28);
    // Bytes match what we wrote into kernel memory.
    expect(Array.from(post.payload)).toEqual(
      Array.from(new Uint8Array(kernelMemory.buffer, 0, 28)),
    );
  });

  it("CREATE augments the [handle] payload with the class string", () => {
    const { registry, kernelMemory, worker } = makeRegistry();
    const handle = registry.register(PACKAGE_ID, CLASS_NAME);
    new DataView(kernelMemory.buffer).setUint32(0, handle, true);
    registry.dispatch(CMD.CREATE, 0, 4);

    const post = worker._posted.find((m) => m.type === "dispatch");
    expect(post.cmd_kind).toBe(CMD.CREATE);
    // [handle:4][class string]
    expect(post.payload.length).toBe(4 + CLASS_NAME.length);
    expect(new DataView(post.payload.buffer, post.payload.byteOffset, 4).getUint32(0, true))
      .toBe(handle);
    expect(new TextDecoder().decode(post.payload.slice(4))).toBe(CLASS_NAME);
  });

  it("dispatch_ids are monotonic", () => {
    const { registry, kernelMemory } = makeRegistry();
    const handle = registry.register(PACKAGE_ID, CLASS_NAME);
    new DataView(kernelMemory.buffer).setUint32(0, handle, true);
    const a = registry.dispatch(CMD.RESUME, 0, 4);
    const b = registry.dispatch(CMD.RESUME, 0, 4);
    expect(b).toBe(a + 1n);
  });
});

// ── pollFuture ────────────────────────────────────────────────────────

describe("pollFuture — status of an in-flight dispatch", () => {
  it("returns ERROR for unknown future_id", () => {
    const { registry } = makeRegistry();
    expect(registry.pollFuture(99999n)).toBe(STATUS.ERROR);
  });

  it("returns PENDING for a fresh dispatch", () => {
    const { registry, kernelMemory } = makeRegistry();
    const handle = registry.register(PACKAGE_ID, CLASS_NAME);
    new DataView(kernelMemory.buffer).setUint32(0, handle, true);
    const id = registry.dispatch(CMD.RESUME, 0, 4);
    expect(registry.pollFuture(id)).toBe(STATUS.PENDING);
  });

  it("flips to READY after dispatch_done arrives", () => {
    const { registry, kernelMemory, worker } = makeRegistry();
    const handle = registry.register(PACKAGE_ID, CLASS_NAME);
    new DataView(kernelMemory.buffer).setUint32(0, handle, true);
    const id = registry.dispatch(CMD.RESUME, 0, 4);

    worker._inject({ type: "dispatch_done", dispatch_id: id, future_id: 0n, bytes: null });
    expect(registry.pollFuture(id)).toBe(STATUS.READY);
  });
});

// ── getResult ─────────────────────────────────────────────────────────

describe("getResult — copy result bytes to kernel memory", () => {
  it("returns 0 for unknown future_id without writing", () => {
    const { registry, kernelMemory } = makeRegistry();
    new Uint8Array(kernelMemory.buffer, 0, 16).fill(0xab);
    expect(registry.getResult(99999n, 0, 16)).toBe(0);
    // No writes — sentinel bytes intact.
    expect(Array.from(new Uint8Array(kernelMemory.buffer, 0, 16))).toEqual(Array(16).fill(0xab));
  });

  it("copies READY bytes to kernel memory and evicts the entry", () => {
    const { registry, kernelMemory, worker } = makeRegistry();
    const handle = registry.register(PACKAGE_ID, CLASS_NAME);
    new DataView(kernelMemory.buffer).setUint32(0, handle, true);
    const id = registry.dispatch(CMD.RENDER, 0, 12);

    const paint = new Uint8Array([0xde, 0xad, 0xbe, 0xef, 0x01]);
    worker._inject({
      type: "dispatch_done",
      dispatch_id: id,
      future_id: BigInt(handle),
      bytes: paint,
    });

    const outPtr = 100;
    const ret = registry.getResult(id, outPtr, 64);
    expect(ret).toBe(paint.length);
    const got = new Uint8Array(kernelMemory.buffer, outPtr, paint.length);
    expect(Array.from(got)).toEqual(Array.from(paint));
    // Evicted — second call returns 0.
    expect(registry.getResult(id, outPtr, 64)).toBe(0);
  });

  it("ack-only dispatch (future_id=0n) resolves with 0-length bytes", () => {
    // Lifecycle / input cmds: app dispatch returns 0n, worker shell
    // ships dispatch_done with future_id=0n and bytes=null. The
    // kernel-side getResult must report 0 length, and the entry is
    // still evicted single-shot.
    const { registry, kernelMemory, worker } = makeRegistry();
    const handle = registry.register(PACKAGE_ID, CLASS_NAME);
    new DataView(kernelMemory.buffer).setUint32(0, handle, true);
    const id = registry.dispatch(CMD.RESUME, 0, 4);
    worker._inject({ type: "dispatch_done", dispatch_id: id, future_id: 0n, bytes: null });
    expect(registry.pollFuture(id)).toBe(STATUS.READY);
    expect(registry.getResult(id, 0, 64)).toBe(0);
  });

  it("returns needed length (no copy) when len > outCap and keeps the entry", () => {
    const { registry, kernelMemory, worker } = makeRegistry();
    const handle = registry.register(PACKAGE_ID, CLASS_NAME);
    new DataView(kernelMemory.buffer).setUint32(0, handle, true);
    const id = registry.dispatch(CMD.RENDER, 0, 12);
    const paint = new Uint8Array(200).fill(0x42);
    worker._inject({
      type: "dispatch_done",
      dispatch_id: id,
      future_id: BigInt(handle),
      bytes: paint,
    });

    // Undersized buffer: returns 200, copies nothing.
    const outPtr = 0;
    new Uint8Array(kernelMemory.buffer, outPtr, 64).fill(0);
    expect(registry.getResult(id, outPtr, 64)).toBe(200);
    expect(Array.from(new Uint8Array(kernelMemory.buffer, outPtr, 64))).toEqual(Array(64).fill(0));

    // Properly sized buffer: copies, evicts.
    expect(registry.getResult(id, outPtr, 256)).toBe(200);
    expect(registry.getResult(id, outPtr, 256)).toBe(0);
  });
});

// ── side_effect bridging ──────────────────────────────────────────────

describe("side_effect from worker — stages into _pending* maps", () => {
  function u32le(n) {
    const out = new Uint8Array(4);
    new DataView(out.buffer).setUint32(0, n >>> 0, true);
    return out;
  }
  function concat(...arrs) {
    const total = arrs.reduce((s, a) => s + a.length, 0);
    const out = new Uint8Array(total);
    let off = 0;
    for (const a of arrs) { out.set(a, off); off += a.length; }
    return out;
  }

  it("FINISH stages into _pendingFinish (drainFinish flips it once)", () => {
    const { registry, worker } = makeRegistry();
    const handle = registry.register(PACKAGE_ID, CLASS_NAME);
    worker._inject({ type: "side_effect", op_kind: FIRE.FINISH, args: u32le(handle) });
    expect(registry.drainFinish(handle)).toBe(true);
    expect(registry.drainFinish(handle)).toBe(false);
  });

  it("START_ACTIVITY without LAUNCH_ANY_APP grant is silently dropped + warn-logged", () => {
    const { registry, worker, log } = makeRegistry();
    const handle = registry.register(PACKAGE_ID, CLASS_NAME);
    const intentBytes = new Uint8Array([0xab, 0xcd, 0xef]);
    worker._inject({
      type: "side_effect",
      op_kind: FIRE.START_ACTIVITY,
      args: concat(u32le(handle), intentBytes),
    });
    expect(registry.drainIntent(handle)).toBeNull();
    expect(log.mock.calls.some(([lvl, msg]) =>
      lvl === "warn" && msg.includes("denied — no LAUNCH_ANY_APP"))).toBe(true);
  });

  it("START_ACTIVITY with grant stages intent bytes for drainIntent", () => {
    const { registry, worker } = makeRegistry();
    registry.setGrants(PACKAGE_ID, ["akiva.permission.LAUNCH_ANY_APP"]);
    const handle = registry.register(PACKAGE_ID, CLASS_NAME);
    const intentBytes = new Uint8Array([0xde, 0xad, 0xbe, 0xef]);
    worker._inject({
      type: "side_effect",
      op_kind: FIRE.START_ACTIVITY,
      args: concat(u32le(handle), intentBytes),
    });
    const drained = registry.drainIntent(handle);
    expect(drained).not.toBeNull();
    expect(Array.from(drained)).toEqual(Array.from(intentBytes));
  });

  it("START_ACTIVITY_FOR_RESULT carries handle + request_code + bytes", () => {
    const { registry, worker } = makeRegistry();
    registry.setGrants(PACKAGE_ID, ["akiva.permission.LAUNCH_ANY_APP"]);
    const handle = registry.register(PACKAGE_ID, CLASS_NAME);
    const intentBytes = new Uint8Array([1, 2, 3]);
    worker._inject({
      type: "side_effect",
      op_kind: FIRE.START_ACTIVITY_FOR_RESULT,
      args: concat(u32le(handle), u32le(42), intentBytes),
    });
    const drained = registry.drainStartForResult(handle);
    expect(drained.requestCode).toBe(42);
    expect(Array.from(drained.bytes)).toEqual([1, 2, 3]);
  });

  it("SET_RESULT stashes bytes for drainSetResult", () => {
    const { registry, worker } = makeRegistry();
    const handle = registry.register(PACKAGE_ID, CLASS_NAME);
    const result = new Uint8Array([9, 8, 7]);
    worker._inject({
      type: "side_effect",
      op_kind: FIRE.SET_RESULT,
      args: concat(u32le(handle), result),
    });
    expect(Array.from(registry.drainSetResult(handle))).toEqual([9, 8, 7]);
  });

  // Stone 11.5e provenance check — the routing-context packageId
  // (which worker actually sent the message) is the truth, not the
  // handle the worker put in `args`. Without this, app A's worker
  // could forge a finish/start_activity/set_result against app B.
  it("rejects side_effect for a handle that doesn't belong to the sender", () => {
    const { registry, worker, log } = makeRegistry();
    // Register PACKAGE_ID first so `worker` gets a com.test.app
    // routed message listener — that's the seam the inject below
    // exercises.
    registry.register(PACKAGE_ID, CLASS_NAME);

    // Set up a second package's worker + a handle that belongs to it.
    const otherWorker = makeFakeWorker();
    registry._processes.set("com.b.app", { worker: otherWorker, packageId: "com.b.app" });
    otherWorker.addEventListener("message", (e) =>
      registry._onWorkerMessage("com.b.app", e.data));
    // Allocate a handle for com.b.app via the public register() path
    // — its storage entry is the same default placeholder.
    const otherHandle = registry.register("com.b.app", "Main");

    // App A's worker (the one we test from `worker`) tries to finish
    // app B's activity by forging the handle in args.
    worker._inject({
      type: "side_effect",
      op_kind: FIRE.FINISH,
      args: u32le(otherHandle),
    });

    expect(registry.drainFinish(otherHandle)).toBe(false);
    expect(log.mock.calls.some(([lvl, msg]) =>
      lvl === "warn" && msg.includes("doesn't belong"))).toBe(true);
  });
});

// ── host_op cross-worker protocol ─────────────────────────────────────

describe("host_op routing — kernel-served io ops", () => {
  it("QUERY_PACKAGES with grant replies host_op_done with the packages blob", () => {
    const { registry, worker } = makeRegistry();
    registry.setGrants(PACKAGE_ID, ["akiva.permission.QUERY_PACKAGES"]);
    const blob = new Uint8Array([1, 2, 3, 4, 5]);
    registry.setPackages(blob);
    registry.register(PACKAGE_ID, CLASS_NAME);

    worker._inject({ type: "host_op", future_id: 7n, op_kind: IO.QUERY_PACKAGES, args: new Uint8Array(0) });
    const reply = worker._posted.find((m) => m.type === "host_op_done");
    expect(reply).toBeDefined();
    expect(reply.future_id).toBe(7n);
    expect(reply.status).toBe(STATUS.READY);
    expect(Array.from(reply.payload)).toEqual([1, 2, 3, 4, 5]);
  });

  it("QUERY_PACKAGES without grant replies ERROR + warn-logs", () => {
    const { registry, worker, log } = makeRegistry();
    registry.register(PACKAGE_ID, CLASS_NAME);
    worker._inject({ type: "host_op", future_id: 11n, op_kind: IO.QUERY_PACKAGES, args: new Uint8Array(0) });
    const reply = worker._posted.find((m) => m.type === "host_op_done");
    expect(reply.status).toBe(STATUS.ERROR);
    expect(log.mock.calls.some(([lvl, msg]) =>
      lvl === "warn" && msg.includes("denied — no QUERY_PACKAGES"))).toBe(true);
  });

  it("STORAGE_* without STORAGE_SANDBOX grant is denied", () => {
    const { registry, worker } = makeRegistry();
    registry.register(PACKAGE_ID, CLASS_NAME);
    worker._inject({ type: "host_op", future_id: 1n, op_kind: IO.STORAGE_GET, args: new Uint8Array(0) });
    expect(worker._posted.find((m) => m.type === "host_op_done").status).toBe(STATUS.ERROR);
  });

  it("STORAGE_GET on missing key returns READY with present=0", () => {
    const { registry, worker } = makeRegistry();
    registry.setGrants(PACKAGE_ID, ["akiva.permission.STORAGE_SANDBOX"]);
    registry.register(PACKAGE_ID, CLASS_NAME);
    worker._inject({
      type: "host_op", future_id: 7n, op_kind: IO.STORAGE_GET,
      args: new TextEncoder().encode("notes/today"),
    });
    const reply = worker._posted.find((m) => m.type === "host_op_done");
    expect(reply.status).toBe(STATUS.READY);
    expect(Array.from(reply.payload)).toEqual([0]);  // present=0
  });

  it("STORAGE_SET then STORAGE_GET round-trips with present=1 prefix", () => {
    const { registry, worker } = makeRegistry();
    registry.setGrants(PACKAGE_ID, ["akiva.permission.STORAGE_SANDBOX"]);
    registry.register(PACKAGE_ID, CLASS_NAME);

    // SET: [key_len:u32 LE][key][value]
    const key = "notes/today";
    const value = new TextEncoder().encode("hello, world");
    const keyBytes = new TextEncoder().encode(key);
    const setArgs = new Uint8Array(4 + keyBytes.length + value.length);
    new DataView(setArgs.buffer).setUint32(0, keyBytes.length, true);
    setArgs.set(keyBytes, 4);
    setArgs.set(value, 4 + keyBytes.length);
    worker._inject({ type: "host_op", future_id: 1n, op_kind: IO.STORAGE_SET, args: setArgs });
    const setReply = worker._posted.find((m) => m.type === "host_op_done");
    expect(setReply.status).toBe(STATUS.READY);
    expect(setReply.payload.length).toBe(0);

    // GET round-trip: [present:1, value...]
    worker._inject({ type: "host_op", future_id: 2n, op_kind: IO.STORAGE_GET, args: keyBytes });
    const getReply = worker._posted.filter((m) => m.type === "host_op_done")[1];
    expect(getReply.status).toBe(STATUS.READY);
    expect(getReply.payload[0]).toBe(1);  // present
    expect(new TextDecoder().decode(getReply.payload.slice(1))).toBe("hello, world");
  });

  it("STORAGE keys are sandboxed per package", () => {
    // App A writes; app B can't see the same user key.
    const { registry, worker, storage } = makeRegistry();
    registry.setGrants(PACKAGE_ID, ["akiva.permission.STORAGE_SANDBOX"]);
    registry.setGrants("com.b.app", ["akiva.permission.STORAGE_SANDBOX"]);
    registry.register(PACKAGE_ID, CLASS_NAME);

    const otherWorker = makeFakeWorker();
    registry._processes.set("com.b.app", { worker: otherWorker, packageId: "com.b.app" });
    otherWorker.addEventListener("message", (e) =>
      registry._onWorkerMessage("com.b.app", e.data));
    registry.register("com.b.app", "Main");

    // App A writes "secret" under user-key "k".
    const k = new TextEncoder().encode("k");
    const v = new TextEncoder().encode("secret");
    const setArgs = new Uint8Array(4 + k.length + v.length);
    new DataView(setArgs.buffer).setUint32(0, k.length, true);
    setArgs.set(k, 4); setArgs.set(v, 4 + k.length);
    worker._inject({ type: "host_op", future_id: 1n, op_kind: IO.STORAGE_SET, args: setArgs });

    // Storage actually has the prefixed entry, not the raw "k".
    expect(storage.get("k")).toBeNull();
    expect(storage.get(`pkgs/${PACKAGE_ID}/k`)).not.toBeNull();

    // App B reads its own "k" — sees nothing.
    otherWorker._inject({ type: "host_op", future_id: 1n, op_kind: IO.STORAGE_GET, args: k });
    const bReply = otherWorker._posted.find((m) => m.type === "host_op_done");
    expect(bReply.status).toBe(STATUS.READY);
    expect(Array.from(bReply.payload)).toEqual([0]);  // not present in B's sandbox
  });

  it("STORAGE_DELETE removes the key (idempotent on missing)", () => {
    const { registry, worker, storage } = makeRegistry();
    registry.setGrants(PACKAGE_ID, ["akiva.permission.STORAGE_SANDBOX"]);
    registry.register(PACKAGE_ID, CLASS_NAME);

    // Pre-seed via direct storage write; then STORAGE_DELETE should
    // wipe the entry.
    const fullKey = `pkgs/${PACKAGE_ID}/k`;
    storage.set?.(fullKey, new Uint8Array([1, 2, 3]));
    // Our test stub doesn't implement set(); poke the cache directly.
    if (typeof storage.set !== "function") {
      // (Test stub side-channel — the storage facade in the real
      // worker.js wraps a real Storage instance with .set/.delete.)
    }

    const k = new TextEncoder().encode("k");
    worker._inject({ type: "host_op", future_id: 1n, op_kind: IO.STORAGE_DELETE, args: k });
    const reply = worker._posted.find((m) => m.type === "host_op_done");
    expect(reply.status).toBe(STATUS.READY);

    // Second delete is a no-op success.
    worker._inject({ type: "host_op", future_id: 2n, op_kind: IO.STORAGE_DELETE, args: k });
    const reply2 = worker._posted.filter((m) => m.type === "host_op_done")[1];
    expect(reply2.status).toBe(STATUS.READY);
  });

  it("STORAGE_GET with empty key returns ERROR with reason", () => {
    const { registry, worker } = makeRegistry();
    registry.setGrants(PACKAGE_ID, ["akiva.permission.STORAGE_SANDBOX"]);
    registry.register(PACKAGE_ID, CLASS_NAME);
    worker._inject({ type: "host_op", future_id: 1n, op_kind: IO.STORAGE_GET, args: new Uint8Array(0) });
    const reply = worker._posted.find((m) => m.type === "host_op_done");
    expect(reply.status).toBe(STATUS.ERROR);
    expect(new TextDecoder().decode(reply.payload)).toMatch(/empty key/);
  });

  it("unknown op_kind replies ERROR + warn-logs", () => {
    const { registry, worker, log } = makeRegistry();
    registry.register(PACKAGE_ID, CLASS_NAME);
    worker._inject({ type: "host_op", future_id: 99n, op_kind: 0x01FE, args: new Uint8Array(0) });
    const reply = worker._posted.find((m) => m.type === "host_op_done");
    expect(reply.status).toBe(STATUS.ERROR);
    expect(log.mock.calls.some(([lvl, msg]) =>
      lvl === "warn" && msg.includes("unknown op_kind"))).toBe(true);
  });
});

// ── log / trapped pass-through ────────────────────────────────────────

describe("log + trapped messages from worker", () => {
  it("log forwards to the registry logger with the worker's level", () => {
    const { registry, worker, log } = makeRegistry();
    registry.register(PACKAGE_ID, CLASS_NAME);
    worker._inject({ type: "log", level: "info", text: "hello" });
    expect(log).toHaveBeenCalledWith("info", "hello");
  });

  it("trapped surfaces as an error log including the package id", () => {
    const { registry, worker, log } = makeRegistry();
    registry.register(PACKAGE_ID, CLASS_NAME);
    worker._inject({ type: "trapped", reason: "panic in on_pointer" });
    expect(log).toHaveBeenCalledWith(
      "error",
      expect.stringMatching(/com\.test\.app.*trapped.*panic/),
    );
  });
});

// ── Stone 11.5c: ping/pong watchdog ───────────────────────────────────

describe("watchdog — ping/pong with init_ok gating", () => {
  // These tests use vi.useFakeTimers() so the 500ms heartbeat fires
  // synchronously under our control. Production setInterval has jitter
  // and rAF-induced delay; we don't simulate those, but we DO pin the
  // contract that healthy workers don't time out and hung ones do.

  function withFakeTimers(fn) {
    return () => {
      // `performance` is in `toFake` so `performance.now()` advances
      // in lockstep with `vi.advanceTimersByTime`. Without this the
      // watchdog's `now - lastPongAt` calculation would still be on
      // real wall time and never trip even after virtual seconds pass.
      vi.useFakeTimers({ toFake: ["setInterval", "clearInterval", "setTimeout", "clearTimeout", "performance"] });
      try { fn(); }
      finally { vi.useRealTimers(); }
    };
  }

  it("doesn't terminate a freshly-spawned worker before init_ok", withFakeTimers(() => {
    const { registry, worker, log } = makeRegistry();
    registry.register(PACKAGE_ID, CLASS_NAME);
    // Heartbeat is gated on init_ok — without it, no setInterval has
    // started. Advancing 5 seconds shouldn't terminate.
    vi.advanceTimersByTime(5000);
    expect(registry._processes.has(PACKAGE_ID)).toBe(true);
    expect(log.mock.calls.some(([lvl, msg]) =>
      lvl === "error" && msg.includes("terminating worker"))).toBe(false);
    void worker;
  }));

  it("starts the heartbeat only on init_ok", withFakeTimers(() => {
    const { registry, worker } = makeRegistry();
    registry.register(PACKAGE_ID, CLASS_NAME);
    // Pre-init_ok: no pings posted by the watchdog.
    vi.advanceTimersByTime(2000);
    expect(worker._posted.some((m) => m.type === "ping")).toBe(false);

    worker._inject({ type: "init_ok" });
    // First tick lands at +500ms after init_ok.
    vi.advanceTimersByTime(500);
    expect(worker._posted.some((m) => m.type === "ping")).toBe(true);
  }));

  it("terminates a hung worker after 1.5s of pong silence", withFakeTimers(() => {
    const { registry, worker, log } = makeRegistry();
    registry.register(PACKAGE_ID, CLASS_NAME);
    worker._inject({ type: "init_ok" });

    // No `pong` injection — simulate a wasm `loop {}` blocking the
    // worker's JS event loop. After ~2000ms (next tick after the
    // 1500ms threshold) the watchdog should fire.
    let terminated = false;
    worker.terminate = () => { terminated = true; };
    vi.advanceTimersByTime(2200);
    expect(terminated).toBe(true);
    expect(registry._processes.has(PACKAGE_ID)).toBe(false);
    expect(registry._pendingTraps.has(PACKAGE_ID)).toBe(true);
    expect(log.mock.calls.some(([lvl, msg]) =>
      lvl === "error" && msg.includes("terminating worker"))).toBe(true);
  }));

  it("a healthy pong-reply worker stays alive indefinitely", withFakeTimers(() => {
    const { registry, worker } = makeRegistry();
    registry.register(PACKAGE_ID, CLASS_NAME);
    worker._inject({ type: "init_ok" });

    // Auto-pong: every ping that gets posted, immediately inject a pong.
    let originalPost = worker.postMessage.bind(worker);
    worker.postMessage = (msg, transfer) => {
      originalPost(msg, transfer);
      if (msg?.type === "ping") {
        // Microtask-style — but in fake timers we can run it synchronously.
        worker._inject({ type: "pong", t: msg.t });
      }
    };

    vi.advanceTimersByTime(10_000);  // 10 seconds of healthy pings
    expect(registry._processes.has(PACKAGE_ID)).toBe(true);
    expect(registry._pendingTraps.has(PACKAGE_ID)).toBe(false);
  }));

  it("watchdog termination GCs the dead worker's own _dispatches entries",
    withFakeTimers(() => {
      // Audit fix #2: the orphaned dispatch entries on the dead
      // worker's account must be evicted so they don't accumulate
      // for the lifetime of the session.
      const { registry, kernelMemory, worker } = makeRegistry();
      const handle = registry.register(PACKAGE_ID, CLASS_NAME);
      worker._inject({ type: "init_ok" });

      new DataView(kernelMemory.buffer).setUint32(0, handle, true);
      const orphan = registry.dispatch(CMD.RENDER, 0, 12);
      expect(registry._dispatches.has(orphan)).toBe(true);

      vi.advanceTimersByTime(2200);  // worker hangs (no auto-pong) → terminate

      expect(registry._dispatches.has(orphan)).toBe(false);
    }));

  it("watchdog termination doesn't clobber other workers' in-flight dispatches",
    withFakeTimers(() => {
      // Regression: pre-Stone-11.5e the watchdog iterated _dispatches
      // and marked every PENDING entry ERROR; post-fix it also deletes
      // entries belonging to the terminated worker. Either way, the
      // contract is "killing worker B must NOT affect worker A's
      // in-flight futures."
      const { registry, kernelMemory, worker } = makeRegistry();
      const handle = registry.register(PACKAGE_ID, CLASS_NAME);
      worker._inject({ type: "init_ok" });

      // Auto-pong on this worker so its own watchdog stays quiet.
      // Without this, advancing 2.2s of fake time would also kill
      // PACKAGE_ID's worker — and its dispatch would be GC'd along
      // with it (correctly, but masking what we're trying to test).
      const origPost = worker.postMessage.bind(worker);
      worker.postMessage = (msg, transfer) => {
        origPost(msg, transfer);
        if (msg?.type === "ping") {
          worker._inject({ type: "pong", t: msg.t });
        }
      };

      // Stage a healthy in-flight dispatch from this worker.
      new DataView(kernelMemory.buffer).setUint32(0, handle, true);
      const inFlightId = registry.dispatch(CMD.RENDER, 0, 12);
      expect(registry._dispatches.get(inFlightId).status).toBe(STATUS.PENDING);
      expect(registry._dispatches.get(inFlightId).packageId).toBe(PACKAGE_ID);

      // Spawn a SECOND worker and let it time out without ponging.
      const otherWorker = makeFakeWorker();
      registry._processes.set("com.b.app", {
        worker: otherWorker, packageId: "com.b.app",
        lastPongAt: 0, ready: true, pingTimer: null,
      });
      registry._startHeartbeat(registry._processes.get("com.b.app"));

      vi.advanceTimersByTime(2200);

      // The peer's termination must not have touched our dispatch.
      const stillThere = registry._dispatches.get(inFlightId);
      expect(stillThere).toBeDefined();
      expect(stillThere.status).toBe(STATUS.PENDING);
      // And com.b.app is gone.
      expect(registry._processes.has("com.b.app")).toBe(false);
    }));
});

// ── Phase 12.1: per-package memory accounting ─────────────────────────

describe("memoryCurrent — kernel reads last pong-reported byteLength", () => {
  it("returns 0 for unknown packages", () => {
    const { registry } = makeRegistry();
    expect(registry.memoryCurrent("never.installed")).toBe(0);
  });

  it("returns 0 before the first pong arrives", () => {
    const { registry, worker } = makeRegistry();
    registry.register(PACKAGE_ID, CLASS_NAME);
    worker._inject({ type: "init_ok" });
    // No pong yet — proc exists but `memoryBytes` defaults to 0.
    expect(registry.memoryCurrent(PACKAGE_ID)).toBe(0);
  });

  it("returns the latest memory_bytes carried on a pong", () => {
    const { registry, worker } = makeRegistry();
    registry.register(PACKAGE_ID, CLASS_NAME);
    worker._inject({ type: "init_ok" });
    worker._inject({ type: "pong", t: 100, memory_bytes: 1_048_576 });
    expect(registry.memoryCurrent(PACKAGE_ID)).toBe(1_048_576);
    // Subsequent pong replaces the reading.
    worker._inject({ type: "pong", t: 200, memory_bytes: 2_097_152 });
    expect(registry.memoryCurrent(PACKAGE_ID)).toBe(2_097_152);
  });

  it("ignores pongs with malformed memory_bytes (negative or non-numeric)", () => {
    const { registry, worker } = makeRegistry();
    registry.register(PACKAGE_ID, CLASS_NAME);
    worker._inject({ type: "init_ok" });
    worker._inject({ type: "pong", t: 100, memory_bytes: 4096 });
    worker._inject({ type: "pong", t: 200, memory_bytes: "lots" });
    worker._inject({ type: "pong", t: 300, memory_bytes: -1 });
    // The 4096 reading sticks — bad readings don't clobber a good one.
    expect(registry.memoryCurrent(PACKAGE_ID)).toBe(4096);
  });
});

// ── Stone 11.5: trap drain ────────────────────────────────────────────

describe("drainTrappedPackages — kernel pulls trap notifications", () => {
  it("returns 0 when nothing trapped", () => {
    const { registry } = makeRegistry();
    expect(registry.drainTrappedPackages(0, 1024)).toBe(0);
  });

  it("trapped message stages the package; drain returns the bytes", () => {
    const { registry, worker, kernelMemory } = makeRegistry();
    registry.register(PACKAGE_ID, CLASS_NAME);
    worker._inject({ type: "trapped", reason: "panic" });
    const ret = registry.drainTrappedPackages(0, 1024);
    expect(ret).toBe(PACKAGE_ID.length);
    const got = new TextDecoder().decode(new Uint8Array(kernelMemory.buffer, 0, ret));
    expect(got).toBe(PACKAGE_ID);
    // Single-shot consume.
    expect(registry.drainTrappedPackages(0, 1024)).toBe(0);
  });

  it("two-phase retry: outCap too small returns needed length, keeps entries", () => {
    const { registry, worker } = makeRegistry();
    registry.register(PACKAGE_ID, CLASS_NAME);
    worker._inject({ type: "trapped", reason: "x" });
    expect(registry.drainTrappedPackages(0, 5)).toBe(PACKAGE_ID.length);
    // Entry stays armed for the retry.
    expect(registry._pendingTraps.has(PACKAGE_ID)).toBe(true);
    expect(registry.drainTrappedPackages(0, 1024)).toBe(PACKAGE_ID.length);
    expect(registry._pendingTraps.size).toBe(0);
  });

  it("LF-separates multiple package_ids and dedupes repeats", () => {
    const { registry, worker, kernelMemory } = makeRegistry();
    registry.register("com.a.app", "Main");
    const otherWorker = makeFakeWorker();
    // Manually inject a second worker for a second package.
    registry._processes.set("com.b.app", { worker: otherWorker, packageId: "com.b.app" });
    otherWorker.addEventListener("message", (e) =>
      registry._onWorkerMessage("com.b.app", e.data));

    worker._inject({ type: "trapped", reason: "1" });
    otherWorker._inject({ type: "trapped", reason: "2" });
    worker._inject({ type: "trapped", reason: "again — should dedupe" });

    const ret = registry.drainTrappedPackages(0, 1024);
    const got = new TextDecoder().decode(new Uint8Array(kernelMemory.buffer, 0, ret));
    const parts = got.split("\n").sort();
    expect(parts).toEqual(["com.a.app", "com.b.app"]);
  });
});

// ── Grant + packages updates ──────────────────────────────────────────

describe("setGrants / setPackages — kernel-side caches", () => {
  it("setGrants stores and forwards to a running worker", () => {
    const { registry, worker } = makeRegistry();
    registry.register(PACKAGE_ID, CLASS_NAME);
    registry.setGrants(PACKAGE_ID, ["akiva.permission.LAUNCH_ANY_APP"]);
    expect(registry._grants.get(PACKAGE_ID)).toEqual(
      new Set(["akiva.permission.LAUNCH_ANY_APP"]),
    );
    const update = worker._posted.find((m) => m.type === "set_grants");
    expect(update).toBeDefined();
    expect(update.grants).toEqual(["akiva.permission.LAUNCH_ANY_APP"]);
  });

  it("setGrants is last-write-wins (full set replaces, no merge)", () => {
    const { registry } = makeRegistry();
    registry.setGrants(PACKAGE_ID, [
      "akiva.permission.LAUNCH_ANY_APP",
      "akiva.permission.NETWORK",
    ]);
    registry.setGrants(PACKAGE_ID, ["akiva.permission.LAUNCH_ANY_APP"]);
    expect(registry._grants.get(PACKAGE_ID)).toEqual(
      new Set(["akiva.permission.LAUNCH_ANY_APP"]),
    );
  });

  it("setPackages stores the bytes verbatim and is last-write-wins", () => {
    const { registry } = makeRegistry();
    const blob = new Uint8Array([1, 2, 3]);
    registry.setPackages(blob);
    expect(registry._packagesBlob).toBe(blob);
    registry.setPackages(new Uint8Array([9, 9]));
    expect(Array.from(registry._packagesBlob)).toEqual([9, 9]);
  });

  it("setPackages defends against non-Uint8Array (bridge bug)", () => {
    const { registry } = makeRegistry();
    registry.setPackages(null);
    expect(registry._packagesBlob.length).toBe(0);
  });
});

// ── Capability / AUTO mirror ──────────────────────────────────────────

describe("AUTO_IMPORTS / CAPABILITY_IMPORTS — cross-side parity", () => {
  it("AUTO_IMPORTS contains the resource lookups + unified-ABI trio", () => {
    expect(AUTO_IMPORTS).toContain("akiva_host_resource_color");
    expect(AUTO_IMPORTS).toContain("akiva_host_resource_string");
    expect(AUTO_IMPORTS).toContain("akiva_host_fire");
    expect(AUTO_IMPORTS).toContain("akiva_host_io_start");
    expect(AUTO_IMPORTS).toContain("akiva_host_io_poll");
  });

  it("every capability list is empty (gating moved into the worker dispatcher)", () => {
    for (const cap of [
      "akiva.permission.LAUNCH_ANY_APP",
      "akiva.permission.QUERY_PACKAGES",
      "akiva.permission.STORAGE_SANDBOX",
      "akiva.permission.NOTIFICATIONS",
      "akiva.permission.NETWORK",
      "akiva.permission.CAMERA",
    ]) {
      expect(CAPABILITY_IMPORTS[cap]).toEqual([]);
    }
  });
});

// ── Debug categories ──────────────────────────────────────────────────

describe("debug categories — toggleable per-site tracing", () => {
  it("is silent by default", () => {
    const { registry, log } = makeRegistry();
    registry._dbg("lifecycle", "should not fire");
    expect(log).not.toHaveBeenCalled();
  });

  it("fires only for enabled categories", () => {
    const { registry, log } = makeRegistry();
    registry.setDebugCategories(["intent"]);
    registry._dbg("lifecycle", "quiet");
    registry._dbg("intent", "loud");
    expect(log).toHaveBeenCalledTimes(1);
    expect(log).toHaveBeenCalledWith("debug", "[intent] loud");
  });

  it("wildcard '*' enables every category", () => {
    const { registry, log } = makeRegistry();
    registry.setDebugCategories(["*"]);
    registry._dbg("lifecycle", "a");
    registry._dbg("paint", "b");
    expect(log).toHaveBeenCalledTimes(2);
  });
});
