// AppRegistry unit tests.
//
// Covers the semantic layer of the worker-side host — the seam that
// Rust-side `MockHost` tests can't reach (which exercises the kernel's
// `WasmHost` trait directly, not the JS class). The focus is on the
// three shared byte-transfer helpers added in Stone 9.4+polish:
//
//   _copyAppToKernel(handle, opLabel, outPtr, outCap, invoke)  — 2-phase read
//   _callAppWithBytes(handle, opLabel, bytes, invoke)          — 2-phase write
//   _copyKernelSlice(ptr, len)                                  — detach-safe copy
//
// plus the register/unregister/drain* state invariants. We fake the
// wasm instance with a plain JS object whose exports are vi.fn
// spies — real wasm instantiation is a separate concern and irrelevant
// to the semantic contract these tests pin down.

import { describe, it, expect, beforeEach, vi } from "vitest";
import { AppRegistry } from "../app_registry.js";

// ── Test harness ──────────────────────────────────────────────────────

const PACKAGE_ID = "com.test.app";
const CLASS_NAME = "Main";

/**
 * Build a fake wasm "process" matching what `_instantiateProcess` would
 * produce. `opts.allocFailAfter === 0` fails every alloc; `opts.alloc
 * FailAfter === 1` returns a real pointer once then fails. Handy for
 * simulating OOM mid-handshake.
 */
function makeFakeProc(opts = {}) {
  // One small WebAssembly.Memory so `proc.memory.buffer` is a real
  // ArrayBuffer — the helpers read/write through it with Uint8Array
  // views and we want a real-world slot, not a polyfill.
  const memory = new WebAssembly.Memory({ initial: 1 });

  let bumpNext = opts.allocBase ?? 1024;
  let allocCount = 0;
  const allocations = []; // records [ptr, size] in order for assertions

  const allocFn = (size) => {
    if (opts.allocFailAfter !== undefined && allocCount >= opts.allocFailAfter) {
      return 0;
    }
    allocCount += 1;
    const ptr = bumpNext;
    bumpNext += size;
    allocations.push([ptr, size]);
    return ptr;
  };

  const exports = {
    memory,
    akiva_app_alloc: vi.fn(allocFn),
    akiva_app_dealloc: vi.fn(),
    akiva_app_create: vi.fn(),
    akiva_app_start: vi.fn(),
    akiva_app_resume: vi.fn(),
    akiva_app_pause: vi.fn(),
    akiva_app_stop: vi.fn(),
    akiva_app_destroy: vi.fn(),
    akiva_app_pointer: vi.fn(),
    akiva_app_key: vi.fn(),
    akiva_app_resize: vi.fn(),
    akiva_app_render: vi.fn(),
    akiva_app_save_state: vi.fn(),
    akiva_app_restore_state: vi.fn(),
    akiva_app_new_intent: vi.fn(),
    akiva_app_activity_result: vi.fn(),
    akiva_app_abi_version: vi.fn(() => 0),
  };

  return {
    proc: { module: null, instance: { exports }, memory },
    exports,
    allocations,
    memory,
  };
}

/**
 * Build an `AppRegistry` with `fakeProc` wired up as the process for
 * `PACKAGE_ID`. Returns the registry, the spy-exports, and a fresh
 * `kernelMemory` the tests can read/write to inspect copy direction.
 */
function makeRegistry({ fakeProc, log } = {}) {
  const kernelMemory = new WebAssembly.Memory({ initial: 1 });
  const storage = { get: vi.fn(() => null) };
  const logFn = log ?? vi.fn();
  const registry = new AppRegistry({
    storage,
    kernelMemory: () => kernelMemory,
    log: logFn,
  });
  // Skip the real `_instantiateProcess` path — it needs bytes + a real
  // wasm compile. Seed the internal maps directly. This is the one
  // place these tests reach past the public API, and it's worth it:
  // it keeps the suite focused on semantics of the helpers without
  // tying every test to a hand-crafted wasm fixture.
  if (fakeProc) {
    registry._processes.set(PACKAGE_ID, fakeProc.proc);
    registry._resources.set(PACKAGE_ID, { strings: new Map(), colors: new Map() });
  }
  return { registry, kernelMemory, log: logFn, storage };
}

/** Pre-seed a slot so `drain*` / `newIntent` / `restoreState` have a
 *  handle to find. `register()` can't be called directly because it
 *  goes through `_instantiateProcess`. */
function seedHandle(registry, handle = 1) {
  registry._slots.set(handle, { packageId: PACKAGE_ID, class: CLASS_NAME });
  return handle;
}

// ── Tests ─────────────────────────────────────────────────────────────

describe("drainFinish — single-shot finish flag", () => {
  it("returns false when nothing pending", () => {
    const { registry } = makeRegistry();
    expect(registry.drainFinish(1)).toBe(false);
  });

  it("returns true once after the host import fires, then false", () => {
    const fake = makeFakeProc();
    const { registry } = makeRegistry({ fakeProc: fake });
    const handle = seedHandle(registry);

    registry._pendingFinish.add(handle);
    expect(registry.drainFinish(handle)).toBe(true);
    expect(registry.drainFinish(handle)).toBe(false);
  });

  it("doesn't leak across handles", () => {
    const { registry } = makeRegistry();
    registry._pendingFinish.add(1);
    expect(registry.drainFinish(2)).toBe(false);
    expect(registry.drainFinish(1)).toBe(true);
  });
});

describe("drainIntent — single-shot intent blob", () => {
  it("returns null when nothing pending", () => {
    const { registry } = makeRegistry();
    expect(registry.drainIntent(1)).toBe(null);
  });

  it("returns the staged blob and leaves it armed until _consumeIntent", () => {
    // The blob stays in the map across reads so a `len > outCap`
    // retry on the kernel side still finds it. Explicit consume is
    // the contract (fired by the worker's halAppDrainIntent after a
    // successful copy).
    const { registry } = makeRegistry();
    const blob = new Uint8Array([1, 2, 3, 4]);
    registry._pendingIntent.set(1, blob);

    expect(registry.drainIntent(1)).toBe(blob);
    expect(registry.drainIntent(1)).toBe(blob); // still armed
    registry._consumeIntent(1);
    expect(registry.drainIntent(1)).toBe(null);
  });
});

describe("unregister — cleans up per-handle transient state", () => {
  it("drops _pendingFinish and _pendingIntent for the handle", () => {
    const fake = makeFakeProc();
    const { registry } = makeRegistry({ fakeProc: fake });
    const handle = seedHandle(registry);
    registry._pendingFinish.add(handle);
    registry._pendingIntent.set(handle, new Uint8Array([1]));

    registry.unregister(handle);

    expect(registry._pendingFinish.has(handle)).toBe(false);
    expect(registry._pendingIntent.has(handle)).toBe(false);
    expect(registry._slots.has(handle)).toBe(false);
  });

  it("is a no-op on an unknown handle", () => {
    const { registry } = makeRegistry();
    expect(() => registry.unregister(9999)).not.toThrow();
  });
});

describe("_copyAppToKernel — 2-phase buffer read", () => {
  it("returns 0 on handle miss without touching the allocator", () => {
    const fake = makeFakeProc();
    const { registry } = makeRegistry({ fakeProc: fake });
    const out = registry._copyAppToKernel(
      /* handle */ 42,
      "render",
      /* outPtr */ 0,
      /* outCap */ 1024,
      () => 0,
    );
    expect(out).toBe(0);
    expect(fake.exports.akiva_app_alloc).not.toHaveBeenCalled();
  });

  it("copies app bytes into kernel memory when len <= outCap", () => {
    const fake = makeFakeProc();
    const { registry, kernelMemory } = makeRegistry({ fakeProc: fake });
    const handle = seedHandle(registry);

    // Simulate the app writing "hello" at some app-memory offset and
    // returning that pointer + setting *lenPtr = 5.
    const appSrc = 2048;
    const appBytes = new Uint8Array(fake.memory.buffer);
    appBytes.set([0x68, 0x65, 0x6c, 0x6c, 0x6f], appSrc); // 'hello'

    const invoke = (proc, lenPtr) => {
      new DataView(proc.memory.buffer).setUint32(lenPtr, 5, /* le */ true);
      return appSrc;
    };
    const outPtr = 512;
    const ret = registry._copyAppToKernel(handle, "render", outPtr, 128, invoke);
    expect(ret).toBe(5);

    const copied = new Uint8Array(kernelMemory.buffer, outPtr, 5);
    expect(Array.from(copied)).toEqual([0x68, 0x65, 0x6c, 0x6c, 0x6f]);

    // The len-ptr allocation must be freed — exactly one alloc(4) +
    // one matching dealloc, regardless of the app's other allocs.
    expect(fake.exports.akiva_app_alloc).toHaveBeenCalledWith(4);
    expect(fake.exports.akiva_app_dealloc).toHaveBeenCalledTimes(1);
  });

  it("returns len unchanged when len > outCap (retry signal, no copy)", () => {
    const fake = makeFakeProc();
    const { registry, kernelMemory } = makeRegistry({ fakeProc: fake });
    const handle = seedHandle(registry);

    // App claims 9999 bytes but the kernel's buffer only has 100.
    // The helper must return 9999 as the "realloc to this size" hint
    // without writing anything into the output.
    const invoke = (proc, lenPtr) => {
      new DataView(proc.memory.buffer).setUint32(lenPtr, 9999, true);
      return 4096;
    };
    // Zero out the first 16 bytes of kernel memory so we can verify
    // nothing leaked in.
    const kb = new Uint8Array(kernelMemory.buffer, 0, 16);
    kb.fill(0);

    const ret = registry._copyAppToKernel(handle, "render", 0, 100, invoke);
    expect(ret).toBe(9999);
    expect(Array.from(kb)).toEqual(Array(16).fill(0));
  });

  it("returns 0 + logs when the export throws, still freeing the len ptr", () => {
    const log = vi.fn();
    const fake = makeFakeProc();
    const { registry } = makeRegistry({ fakeProc: fake, log });
    const handle = seedHandle(registry);

    const invoke = () => { throw new Error("app boom"); };
    const ret = registry._copyAppToKernel(handle, "render", 0, 1024, invoke);
    expect(ret).toBe(0);
    expect(log).toHaveBeenCalledWith(
      "error",
      expect.stringMatching(/render.*app boom/),
    );
    // Catch happens OUTSIDE the try/finally that releases the len-ptr,
    // so dealloc(4) should still have run before the error surfaced.
    expect(fake.exports.akiva_app_dealloc).toHaveBeenCalledTimes(1);
  });

  it("returns 0 + logs when alloc(4) fails", () => {
    const log = vi.fn();
    const fake = makeFakeProc({ allocFailAfter: 0 });
    const { registry } = makeRegistry({ fakeProc: fake, log });
    const handle = seedHandle(registry);

    const invoke = vi.fn();
    const ret = registry._copyAppToKernel(handle, "render", 0, 1024, invoke);
    expect(ret).toBe(0);
    expect(invoke).not.toHaveBeenCalled();
    expect(log).toHaveBeenCalledWith(
      "warn",
      expect.stringMatching(/alloc.*render/),
    );
  });
});

describe("_callAppWithBytes — 2-phase buffer write", () => {
  it("returns 1 on handle miss without allocating", () => {
    const fake = makeFakeProc();
    const { registry } = makeRegistry({ fakeProc: fake });
    const invoke = vi.fn();
    const rc = registry._callAppWithBytes(
      9999,
      "restore_state",
      new Uint8Array([1, 2, 3]),
      invoke,
    );
    expect(rc).toBe(1);
    expect(fake.exports.akiva_app_alloc).not.toHaveBeenCalled();
    expect(invoke).not.toHaveBeenCalled();
  });

  it("returns 0 + skips alloc on empty payload", () => {
    const fake = makeFakeProc();
    const { registry } = makeRegistry({ fakeProc: fake });
    const handle = seedHandle(registry);
    const invoke = vi.fn();

    const rc = registry._callAppWithBytes(handle, "restore_state", new Uint8Array(), invoke);
    expect(rc).toBe(0);
    expect(invoke).not.toHaveBeenCalled();
    expect(fake.exports.akiva_app_alloc).not.toHaveBeenCalled();
  });

  it("returns 2 + logs when alloc fails", () => {
    const log = vi.fn();
    const fake = makeFakeProc({ allocFailAfter: 0 });
    const { registry } = makeRegistry({ fakeProc: fake, log });
    const handle = seedHandle(registry);
    const invoke = vi.fn();

    const rc = registry._callAppWithBytes(handle, "restore_state", new Uint8Array([9]), invoke);
    expect(rc).toBe(2);
    expect(invoke).not.toHaveBeenCalled();
    expect(log).toHaveBeenCalledWith(
      "warn",
      expect.stringMatching(/alloc.*restore_state/),
    );
  });

  it("copies bytes into app memory, invokes, and deallocs — success path", () => {
    const fake = makeFakeProc();
    const { registry } = makeRegistry({ fakeProc: fake });
    const handle = seedHandle(registry);
    const payload = new Uint8Array([0xaa, 0xbb, 0xcc, 0xdd]);
    const seen = { ptr: null, len: null };

    const invoke = (proc, appPtr, len) => {
      // Read back what was copied to prove the helper actually wrote
      // it in before invoking.
      const view = new Uint8Array(proc.memory.buffer, appPtr, len);
      seen.ptr = appPtr;
      seen.len = len;
      seen.bytes = Array.from(view);
    };

    const rc = registry._callAppWithBytes(handle, "restore_state", payload, invoke);
    expect(rc).toBe(0);
    expect(seen.len).toBe(4);
    expect(seen.bytes).toEqual([0xaa, 0xbb, 0xcc, 0xdd]);
    // Exactly one alloc + one dealloc, both for the same size.
    expect(fake.exports.akiva_app_alloc).toHaveBeenCalledWith(4);
    expect(fake.exports.akiva_app_dealloc).toHaveBeenCalledWith(seen.ptr, 4);
  });

  it("returns 3 when the app export throws, still deallocing", () => {
    const log = vi.fn();
    const fake = makeFakeProc();
    const { registry } = makeRegistry({ fakeProc: fake, log });
    const handle = seedHandle(registry);

    const invoke = () => { throw new Error("new_intent trap"); };
    const rc = registry._callAppWithBytes(handle, "new_intent", new Uint8Array([1]), invoke);
    expect(rc).toBe(3);
    expect(fake.exports.akiva_app_dealloc).toHaveBeenCalledTimes(1);
    expect(log).toHaveBeenCalledWith(
      "error",
      expect.stringMatching(/new_intent.*trap/),
    );
  });
});

describe("newIntent / restoreState — rc pass-through", () => {
  // These are thin wrappers over _callAppWithBytes + _copyKernelSlice;
  // the point of covering them is to pin the contract that a zero-len
  // payload fast-paths to success only when the handle is known, and
  // that the copy-from-kernel step does happen (so a subsequent
  // kernel-memory grow wouldn't detach the bytes mid-flight).

  it("newIntent — zero-len + valid handle → 0", () => {
    const fake = makeFakeProc();
    const { registry } = makeRegistry({ fakeProc: fake });
    const handle = seedHandle(registry);
    expect(registry.newIntent(handle, 0, 0)).toBe(0);
    expect(fake.exports.akiva_app_new_intent).not.toHaveBeenCalled();
  });

  it("newIntent — zero-len + unknown handle → 1", () => {
    const { registry } = makeRegistry({ fakeProc: makeFakeProc() });
    expect(registry.newIntent(9999, 0, 0)).toBe(1);
  });

  it("newIntent — copies kernel bytes then fires app export", () => {
    const fake = makeFakeProc();
    const { registry, kernelMemory } = makeRegistry({ fakeProc: fake });
    const handle = seedHandle(registry);

    // Stage 4 bytes in kernel memory at offset 100.
    new Uint8Array(kernelMemory.buffer, 100, 4).set([0x11, 0x22, 0x33, 0x44]);

    let seenBytes = null;
    fake.exports.akiva_app_new_intent.mockImplementation((h, ptr, len) => {
      seenBytes = Array.from(new Uint8Array(fake.memory.buffer, ptr, len));
    });

    const rc = registry.newIntent(handle, 100, 4);
    expect(rc).toBe(0);
    expect(seenBytes).toEqual([0x11, 0x22, 0x33, 0x44]);
  });

  it("restoreState — zero-len short-circuits without calling invoke", () => {
    const fake = makeFakeProc();
    const { registry } = makeRegistry({ fakeProc: fake });
    const handle = seedHandle(registry);
    expect(registry.restoreState(handle, 0, 0)).toBe(0);
    expect(fake.exports.akiva_app_restore_state).not.toHaveBeenCalled();
  });
});

describe("debug categories — toggleable per-site tracing", () => {
  it("is silent by default (empty set)", () => {
    const log = vi.fn();
    const { registry } = makeRegistry({ log });
    registry._dbg("lifecycle", "should not fire");
    expect(log).not.toHaveBeenCalled();
  });

  it("fires only for enabled categories", () => {
    const log = vi.fn();
    const { registry } = makeRegistry({ log });
    registry.setDebugCategories(["intent"]);

    registry._dbg("lifecycle", "quiet");
    registry._dbg("intent", "loud");
    expect(log).toHaveBeenCalledTimes(1);
    expect(log).toHaveBeenCalledWith("debug", "[intent] loud");
  });

  it("wildcard '*' enables every category", () => {
    const log = vi.fn();
    const { registry } = makeRegistry({ log });
    registry.setDebugCategories(["*"]);

    registry._dbg("lifecycle", "a");
    registry._dbg("paint", "b");
    registry._dbg("intent", "c");
    expect(log).toHaveBeenCalledTimes(3);
  });

  it("setDebugCategories resets rather than accumulates", () => {
    const log = vi.fn();
    const { registry } = makeRegistry({ log });
    registry.setDebugCategories(["lifecycle"]);
    registry.setDebugCategories(["intent"]); // overrides
    registry._dbg("lifecycle", "should be silent now");
    expect(log).not.toHaveBeenCalled();
  });

  it("drainFinish emits when category is on", () => {
    const log = vi.fn();
    const fake = makeFakeProc();
    const { registry } = makeRegistry({ fakeProc: fake, log });
    const handle = seedHandle(registry);
    registry.setDebugCategories(["intent"]);
    registry._pendingFinish.add(handle);

    registry.drainFinish(handle);
    expect(log).toHaveBeenCalledWith(
      "debug",
      expect.stringMatching(/^\[intent\] drain_finish\[handle=1\]/),
    );
  });
});

describe("_copyKernelSlice — detach-safe copy", () => {
  it("returns a freshly-owned Uint8Array (not a view into kernel memory)", () => {
    const { registry, kernelMemory } = makeRegistry();
    new Uint8Array(kernelMemory.buffer, 0, 3).set([1, 2, 3]);

    const copy = registry._copyKernelSlice(0, 3);
    expect(Array.from(copy)).toEqual([1, 2, 3]);

    // Mutate the kernel buffer — the copy must not see the change,
    // which is the whole point of doing this before any call that
    // might detach the buffer via Memory.grow.
    new Uint8Array(kernelMemory.buffer, 0, 3).set([9, 9, 9]);
    expect(Array.from(copy)).toEqual([1, 2, 3]);
  });
});

// ── Stone 9.5: start_activity_for_result + set_result plumbing ────────

describe("drainStartForResult — single-shot for-result launch", () => {
  it("returns null when nothing pending", () => {
    const { registry } = makeRegistry();
    expect(registry.drainStartForResult(1)).toBe(null);
  });

  it("returns the staged {bytes, requestCode} until _consumeStartForResult", () => {
    const { registry } = makeRegistry();
    const bytes = new Uint8Array([10, 20, 30]);
    registry._pendingStartForResult.set(1, { bytes, requestCode: 7 });

    const first = registry.drainStartForResult(1);
    expect(first.bytes).toBe(bytes);
    expect(first.requestCode).toBe(7);

    // Still armed for a retry (kernel may probe for size first).
    expect(registry.drainStartForResult(1)).not.toBe(null);

    registry._consumeStartForResult(1);
    expect(registry.drainStartForResult(1)).toBe(null);
  });

  it("is wiped on unregister alongside the other pending maps", () => {
    const fake = makeFakeProc();
    const { registry } = makeRegistry({ fakeProc: fake });
    const handle = seedHandle(registry);
    registry._pendingStartForResult.set(handle, { bytes: new Uint8Array([1]), requestCode: 0 });
    registry._pendingSetResult.set(handle, new Uint8Array([2]));

    registry.unregister(handle);

    expect(registry._pendingStartForResult.has(handle)).toBe(false);
    expect(registry._pendingSetResult.has(handle)).toBe(false);
  });
});

describe("drainSetResult — stashed result bytes", () => {
  it("returns null when nothing stashed", () => {
    const { registry } = makeRegistry();
    expect(registry.drainSetResult(1)).toBe(null);
  });

  it("last set_result call wins (overwrite semantics)", () => {
    // The host import stashes by `set()`; multiple calls in one lifetime
    // collapse to the last. This test pins that policy via direct map
    // writes, then via the drain path — same behaviour both routes.
    const { registry } = makeRegistry();
    registry._pendingSetResult.set(1, new Uint8Array([0xAA]));
    registry._pendingSetResult.set(1, new Uint8Array([0xBB, 0xCC]));
    const drained = registry.drainSetResult(1);
    expect(Array.from(drained)).toEqual([0xBB, 0xCC]);
  });
});

describe("activityResult — deliver result blob to caller app", () => {
  it("returns 1 (handle miss) when the caller handle isn't registered", () => {
    const { registry } = makeRegistry();
    expect(registry.activityResult(999, 7, 0, 0)).toBe(1);
  });

  it("invokes akiva_app_activity_result with the request_code and copied bytes", () => {
    const fake = makeFakeProc();
    const { registry, kernelMemory } = makeRegistry({ fakeProc: fake });
    const handle = seedHandle(registry);
    // Stage bytes in the kernel buffer; activityResult copies them
    // into the app's memory via _callAppWithBytes.
    const payload = [0x00, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0xDE, 0xAD];
    new Uint8Array(kernelMemory.buffer, 0, payload.length).set(payload);

    const rc = registry.activityResult(handle, 42, 0, payload.length);
    expect(rc).toBe(0);

    expect(fake.exports.akiva_app_activity_result).toHaveBeenCalledTimes(1);
    const [calledHandle, calledRc, appPtr, appLen] =
      fake.exports.akiva_app_activity_result.mock.calls[0];
    expect(calledHandle).toBe(handle);
    expect(calledRc).toBe(42);
    expect(appLen).toBe(payload.length);
    // The bytes at appPtr in the app's memory must match what the
    // kernel handed us (`_callAppWithBytes` copies through
    // `akiva_app_alloc` first, then populates the slice).
    const appBytes = new Uint8Array(fake.memory.buffer, appPtr, appLen);
    expect(Array.from(appBytes)).toEqual(payload);
  });
});
