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
import { AppRegistry, AUTO_IMPORTS, CAPABILITY_IMPORTS } from "../app_registry.js";
import { FIRE, IO, STATUS, CMD, packPoll } from "../op_kinds.mjs";

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
    // Stone 11.4b — unified dispatch + result fetcher.
    akiva_app_dispatch: vi.fn(() => 0n),
    akiva_app_get_result: vi.fn(() => 0),
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

// ── Stone 10.3b: capability-based import filtering ───────────────────────

describe("setGrants — per-package permission tracking", () => {
  it("stores the grant set for later filter consultation", () => {
    const { registry } = makeRegistry();
    registry.setGrants(PACKAGE_ID, ["akiva.permission.LAUNCH_ANY_APP"]);
    expect(registry._grants.get(PACKAGE_ID)).toEqual(
      new Set(["akiva.permission.LAUNCH_ANY_APP"]),
    );
  });

  it("is last-write-wins (upgrade semantics: new full set replaces old)", () => {
    // An upgrade via PackageManager::install re-pushes the full new
    // list; the previous set must be replaced, not merged. A naïve
    // add-only implementation would preserve permissions the new
    // manifest dropped — a stealth-grant hazard this test guards.
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

  it("treats empty list as 'auto imports only'", () => {
    const { registry } = makeRegistry();
    registry.setGrants(PACKAGE_ID, []);
    expect(registry._grants.get(PACKAGE_ID).size).toBe(0);
  });
});

describe("_buildAppImports — capability filter", () => {
  it("passes every AUTO import through untouched regardless of grants", () => {
    // Auto imports are foundational — log, text_width, resource lookups,
    // finish, set_result. Gating them would break every app's render
    // path. This test pins that AUTO stays unconditional.
    const { registry } = makeRegistry();
    // No setGrants call at all — the most restrictive possible state.
    const filtered = registry._buildAppImports(PACKAGE_ID).env;
    for (const name of AUTO_IMPORTS) {
      expect(typeof filtered[name]).toBe("function");
      // The returned function's name differs between passthrough and
      // stub — assert it's NOT the stub by checking the name pattern
      // (stubs are named `permissionDeniedStub`).
      expect(filtered[name].name).not.toBe("permissionDeniedStub");
    }
  });

  it("every capability list is empty (gating moved into dispatcher in 11.3-final)", () => {
    // After 11.3-final the per-op imports that LAUNCH_ANY_APP and
    // QUERY_PACKAGES used to gate (start_activity{,_for_result},
    // query_packages) are gone — every fire/io op runs through the
    // unified-ABI multiplexers and capability checks happen inside
    // `_dispatchFire` / `_dispatchIoStart`. The capability map is
    // preserved as a tripwire: any future per-op import that doesn't
    // fit the fire/io shape must register here, and a non-empty list
    // means we have to update the dispatcher to know about it. The
    // dedicated dispatcher tests further down enforce the gating.
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

describe("Stone 11.3c — unified-ABI dispatcher", () => {
  // The three multiplexers (`fire` / `io_start` / `io_poll`) route every
  // host-mediated op through a single switch on op_kind. Per-op behavior
  // matches the legacy per-op imports — same internal state mutation
  // for fire ops (pendingFinish/pendingIntent/etc), same payload shape
  // for io ops. These tests pin both that the dispatch routes correctly
  // and that capability gating happens *inside* the dispatcher (since
  // one import serves many op_kinds).

  /**
   * Allocate a region in the fake proc's memory and write `bytes` into
   * it. Returns the pointer for the dispatcher to read. Mirrors the
   * "host writes args into app memory before calling the import" flow
   * — except in tests we drive both ends, so this is just a memory
   * write.
   */
  function writeArgs(fakeProc, bytes) {
    const ptr = fakeProc.exports.akiva_app_alloc(bytes.length);
    new Uint8Array(fakeProc.proc.memory.buffer, ptr, bytes.length).set(bytes);
    return { ptr, len: bytes.length };
  }

  function u32le(n) {
    const out = new Uint8Array(4);
    new DataView(out.buffer).setUint32(0, n >>> 0, true);
    return out;
  }
  function f32le(n) {
    const out = new Uint8Array(4);
    new DataView(out.buffer).setFloat32(0, n, true);
    return out;
  }
  function concat(...arrs) {
    const total = arrs.reduce((s, a) => s + a.length, 0);
    const out = new Uint8Array(total);
    let off = 0;
    for (const a of arrs) { out.set(a, off); off += a.length; }
    return out;
  }

  // ── multiplexers are AUTO imports ──

  it("multiplexers are AUTO imports (no gating at import surface)", () => {
    // Mirrors the Rust-side `unified_abi_trio_is_in_auto_imports` test.
    for (const name of ["akiva_host_fire", "akiva_host_io_start", "akiva_host_io_poll"]) {
      expect(AUTO_IMPORTS).toContain(name);
    }
  });

  // ── fire dispatch ──

  describe("fire dispatch", () => {
    it("LOG forwards to the registry logger", () => {
      const fake = makeFakeProc();
      const { registry, log } = makeRegistry({ fakeProc: fake });
      const fire = registry._buildAppImports(PACKAGE_ID).env.akiva_host_fire;
      const text = "hello from app";
      const args = concat(u32le(2 /* info */), new TextEncoder().encode(text));
      const { ptr, len } = writeArgs(fake, args);
      expect(fire(FIRE.LOG, ptr, len)).toBe(0);
      expect(log).toHaveBeenCalledWith("info", `[${PACKAGE_ID}] ${text}`);
    });

    it("FINISH stages the handle for drainFinish", () => {
      const fake = makeFakeProc();
      const { registry } = makeRegistry({ fakeProc: fake });
      const fire = registry._buildAppImports(PACKAGE_ID).env.akiva_host_fire;
      const { ptr, len } = writeArgs(fake, u32le(7));
      expect(fire(FIRE.FINISH, ptr, len)).toBe(0);
      expect(registry.drainFinish(7)).toBe(true);
      expect(registry.drainFinish(7)).toBe(false);  // single-shot
    });

    it("START_ACTIVITY without LAUNCH_ANY_APP grant is denied", () => {
      const fake = makeFakeProc();
      const { registry, log } = makeRegistry({ fakeProc: fake });
      const fire = registry._buildAppImports(PACKAGE_ID).env.akiva_host_fire;
      const args = concat(u32le(1), new Uint8Array([0xab, 0xcd]));
      const { ptr, len } = writeArgs(fake, args);
      expect(fire(FIRE.START_ACTIVITY, ptr, len)).toBe(2);  // 2 = denied
      expect(log.mock.calls.some(([lvl, msg]) =>
        lvl === "warn" && msg.includes("denied — no LAUNCH_ANY_APP"))).toBe(true);
      expect(registry.drainIntent(1)).toBe(null);
    });

    it("START_ACTIVITY with the grant stages intent bytes", () => {
      const fake = makeFakeProc();
      const { registry } = makeRegistry({ fakeProc: fake });
      registry.setGrants(PACKAGE_ID, ["akiva.permission.LAUNCH_ANY_APP"]);
      const fire = registry._buildAppImports(PACKAGE_ID).env.akiva_host_fire;
      const intentBytes = new Uint8Array([0xde, 0xad, 0xbe, 0xef]);
      const args = concat(u32le(1), intentBytes);
      const { ptr, len } = writeArgs(fake, args);
      expect(fire(FIRE.START_ACTIVITY, ptr, len)).toBe(0);
      const drained = registry.drainIntent(1);
      expect(drained).not.toBe(null);
      expect(Array.from(drained)).toEqual(Array.from(intentBytes));
    });

    it("START_ACTIVITY_FOR_RESULT carries request_code", () => {
      const fake = makeFakeProc();
      const { registry } = makeRegistry({ fakeProc: fake });
      registry.setGrants(PACKAGE_ID, ["akiva.permission.LAUNCH_ANY_APP"]);
      const fire = registry._buildAppImports(PACKAGE_ID).env.akiva_host_fire;
      const intentBytes = new Uint8Array([1, 2, 3]);
      const args = concat(u32le(5 /* handle */), u32le(42 /* request_code */), intentBytes);
      const { ptr, len } = writeArgs(fake, args);
      expect(fire(FIRE.START_ACTIVITY_FOR_RESULT, ptr, len)).toBe(0);
      const drained = registry.drainStartForResult(5);
      expect(drained).not.toBe(null);
      expect(drained.requestCode).toBe(42);
      expect(Array.from(drained.bytes)).toEqual([1, 2, 3]);
    });

    it("SET_RESULT stashes result bytes for drain", () => {
      const fake = makeFakeProc();
      const { registry } = makeRegistry({ fakeProc: fake });
      const fire = registry._buildAppImports(PACKAGE_ID).env.akiva_host_fire;
      const result = new Uint8Array([9, 8, 7]);
      const args = concat(u32le(3), result);
      const { ptr, len } = writeArgs(fake, args);
      expect(fire(FIRE.SET_RESULT, ptr, len)).toBe(0);
      expect(Array.from(registry.drainSetResult(3))).toEqual([9, 8, 7]);
    });

    it("rejects unknown fire op_kind in range", () => {
      const fake = makeFakeProc();
      const { registry, log } = makeRegistry({ fakeProc: fake });
      const fire = registry._buildAppImports(PACKAGE_ID).env.akiva_host_fire;
      const { ptr, len } = writeArgs(fake, new Uint8Array([0]));
      expect(fire(0x00FE, ptr, len)).toBe(1);
      expect(log.mock.calls.some(([lvl, msg]) =>
        lvl === "warn" && msg.includes("unknown op_kind"))).toBe(true);
    });

    it("rejects op_kind from the wrong range (io kind passed to fire)", () => {
      const fake = makeFakeProc();
      const { registry, log } = makeRegistry({ fakeProc: fake });
      const fire = registry._buildAppImports(PACKAGE_ID).env.akiva_host_fire;
      const { ptr, len } = writeArgs(fake, new Uint8Array([0]));
      expect(fire(IO.TEXT_WIDTH, ptr, len)).toBe(1);
      expect(log.mock.calls.some(([lvl, msg]) =>
        lvl === "warn" && msg.includes("not a fire op_kind"))).toBe(true);
    });
  });

  // ── io dispatch + FutureRegistry ──

  describe("io dispatch + FutureRegistry", () => {
    it("TEXT_WIDTH resolves Ready on first poll with f32 LE width", () => {
      const fake = makeFakeProc();
      const { registry } = makeRegistry({ fakeProc: fake });
      // Force a deterministic width measurement: stub measureTextWidth.
      registry.measureTextWidth = (text, size) => text.length * size * 0.6;
      const env = registry._buildAppImports(PACKAGE_ID).env;
      const args = concat(f32le(20.0), new TextEncoder().encode("abcd"));
      const { ptr, len } = writeArgs(fake, args);
      const futureId = env.akiva_host_io_start(IO.TEXT_WIDTH, ptr, len);
      expect(futureId).not.toBe(0n);

      const outBuf = fake.exports.akiva_app_alloc(4);
      const packed = env.akiva_host_io_poll(futureId, outBuf, 4);
      expect(packed).toBe(packPoll(STATUS.READY, 4));
      const width = new DataView(fake.proc.memory.buffer).getFloat32(outBuf, true);
      expect(width).toBeCloseTo(4 * 20.0 * 0.6);
    });

    it("QUERY_PACKAGES without grant returns 0 future_id and logs", () => {
      const fake = makeFakeProc();
      const { registry, log } = makeRegistry({ fakeProc: fake });
      const env = registry._buildAppImports(PACKAGE_ID).env;
      const { ptr, len } = writeArgs(fake, new Uint8Array(0));
      const futureId = env.akiva_host_io_start(IO.QUERY_PACKAGES, ptr, len);
      expect(futureId).toBe(0n);
      expect(log.mock.calls.some(([lvl, msg]) =>
        lvl === "warn" && msg.includes("denied — no QUERY_PACKAGES"))).toBe(true);
    });

    it("QUERY_PACKAGES with grant resolves Ready with packagesBlob", () => {
      const fake = makeFakeProc();
      const { registry } = makeRegistry({ fakeProc: fake });
      registry.setGrants(PACKAGE_ID, ["akiva.permission.QUERY_PACKAGES"]);
      registry.setPackages(new Uint8Array([1, 2, 3, 4, 5]));
      const env = registry._buildAppImports(PACKAGE_ID).env;
      const futureId = env.akiva_host_io_start(IO.QUERY_PACKAGES, 0, 0);
      expect(futureId).not.toBe(0n);

      const outBuf = fake.exports.akiva_app_alloc(16);
      const packed = env.akiva_host_io_poll(futureId, outBuf, 16);
      expect(packed).toBe(packPoll(STATUS.READY, 5));
      const written = Array.from(new Uint8Array(fake.proc.memory.buffer, outBuf, 5));
      expect(written).toEqual([1, 2, 3, 4, 5]);
    });

    it("two-phase retry: payload too large returns length, retains entry", () => {
      const fake = makeFakeProc();
      const { registry } = makeRegistry({ fakeProc: fake });
      registry.setGrants(PACKAGE_ID, ["akiva.permission.QUERY_PACKAGES"]);
      registry.setPackages(new Uint8Array(100));
      const env = registry._buildAppImports(PACKAGE_ID).env;
      const futureId = env.akiva_host_io_start(IO.QUERY_PACKAGES, 0, 0);

      // First poll with an undersized buffer — must report required
      // length, write nothing, and KEEP the entry alive for retry.
      const undersized = fake.exports.akiva_app_alloc(8);
      const first = env.akiva_host_io_poll(futureId, undersized, 8);
      expect(first).toBe(packPoll(STATUS.READY, 100));
      // Buffer remains zeroed — host did not write.
      const after = Array.from(new Uint8Array(fake.proc.memory.buffer, undersized, 8));
      expect(after.every(b => b === 0)).toBe(true);

      // Second poll with sized buffer succeeds and evicts.
      const sized = fake.exports.akiva_app_alloc(100);
      const second = env.akiva_host_io_poll(futureId, sized, 100);
      expect(second).toBe(packPoll(STATUS.READY, 100));

      // Third poll on the same id is now ERROR (entry consumed).
      const third = env.akiva_host_io_poll(futureId, sized, 100);
      expect(third).toBe(packPoll(STATUS.ERROR, 0));
    });

    it("STORAGE_GET/SET resolve Error until storage HAL lands", () => {
      const fake = makeFakeProc();
      const { registry } = makeRegistry({ fakeProc: fake });
      registry.setGrants(PACKAGE_ID, ["akiva.permission.STORAGE_SANDBOX"]);
      const env = registry._buildAppImports(PACKAGE_ID).env;
      for (const kind of [IO.STORAGE_GET, IO.STORAGE_SET]) {
        const futureId = env.akiva_host_io_start(kind, 0, 0);
        expect(futureId).not.toBe(0n);
        const out = fake.exports.akiva_app_alloc(0);
        expect(env.akiva_host_io_poll(futureId, out, 0)).toBe(packPoll(STATUS.ERROR, 0));
      }
    });

    it("polling an unknown future_id returns ERROR", () => {
      const fake = makeFakeProc();
      const { registry } = makeRegistry({ fakeProc: fake });
      const env = registry._buildAppImports(PACKAGE_ID).env;
      expect(env.akiva_host_io_poll(99999n, 0, 0)).toBe(packPoll(STATUS.ERROR, 0));
    });

    it("cross-package poll attempt is rejected", () => {
      // Two packages share the registry. Package A starts a future,
      // package B tries to poll it. The dispatcher must refuse without
      // confirming the future even exists.
      const fakeA = makeFakeProc();
      const fakeB = makeFakeProc();
      const { registry, log } = makeRegistry({ fakeProc: fakeA });
      registry._processes.set("com.b.app", fakeB.proc);
      registry._resources.set("com.b.app", { strings: new Map(), colors: new Map() });
      registry.setGrants(PACKAGE_ID, ["akiva.permission.QUERY_PACKAGES"]);
      registry.setPackages(new Uint8Array([1]));

      const envA = registry._buildAppImports(PACKAGE_ID).env;
      const futureId = envA.akiva_host_io_start(IO.QUERY_PACKAGES, 0, 0);
      const envB = registry._buildAppImports("com.b.app").env;
      const out = fakeB.exports.akiva_app_alloc(8);
      expect(envB.akiva_host_io_poll(futureId, out, 8)).toBe(packPoll(STATUS.ERROR, 0));
      expect(log.mock.calls.some(([lvl, msg]) =>
        lvl === "warn" && msg.includes("not owned by this package"))).toBe(true);

      // Original package can still poll its own future.
      const outA = fakeA.exports.akiva_app_alloc(8);
      expect(envA.akiva_host_io_poll(futureId, outA, 8))
        .toBe(packPoll(STATUS.READY, 1));
    });

    it("rejects op_kind from the wrong range (fire kind passed to io_start)", () => {
      const fake = makeFakeProc();
      const { registry, log } = makeRegistry({ fakeProc: fake });
      const env = registry._buildAppImports(PACKAGE_ID).env;
      expect(env.akiva_host_io_start(FIRE.LOG, 0, 0)).toBe(0n);
      expect(log.mock.calls.some(([lvl, msg]) =>
        lvl === "warn" && msg.includes("not an io op_kind"))).toBe(true);
    });

    it("rejects unknown io op_kind in range", () => {
      const fake = makeFakeProc();
      const { registry, log } = makeRegistry({ fakeProc: fake });
      const env = registry._buildAppImports(PACKAGE_ID).env;
      expect(env.akiva_host_io_start(0x01FE, 0, 0)).toBe(0n);
      expect(log.mock.calls.some(([lvl, msg]) =>
        lvl === "warn" && msg.includes("unknown op_kind"))).toBe(true);
    });

    it("future_ids are monotonic and unique within a registry", () => {
      const fake = makeFakeProc();
      const { registry } = makeRegistry({ fakeProc: fake });
      registry.measureTextWidth = () => 1.0;
      const env = registry._buildAppImports(PACKAGE_ID).env;
      const args = concat(f32le(10.0), new TextEncoder().encode("x"));
      const { ptr, len } = writeArgs(fake, args);
      const a = env.akiva_host_io_start(IO.TEXT_WIDTH, ptr, len);
      const b = env.akiva_host_io_start(IO.TEXT_WIDTH, ptr, len);
      const c = env.akiva_host_io_start(IO.TEXT_WIDTH, ptr, len);
      expect(b).toBe(a + 1n);
      expect(c).toBe(b + 1n);
    });
  });
});

describe("setPackages — package-list snapshot (Stone 11.0a)", () => {
  it("stores the bytes verbatim for later query_packages calls", () => {
    const { registry } = makeRegistry();
    const blob = new Uint8Array([1, 2, 3, 4]);
    registry.setPackages(blob);
    expect(registry._packagesBlob).toBe(blob);
  });

  it("is last-write-wins (re-push replaces, doesn't merge)", () => {
    // Reboot or future install/uninstall flow re-pushes the full set.
    // A naive append-only implementation would leak old entries.
    const { registry } = makeRegistry();
    registry.setPackages(new Uint8Array([1, 2, 3]));
    registry.setPackages(new Uint8Array([9, 9]));
    expect(Array.from(registry._packagesBlob)).toEqual([9, 9]);
  });

  it("treats non-Uint8Array as empty (defensive against bridge bugs)", () => {
    const { registry } = makeRegistry();
    registry.setPackages(null);
    expect(registry._packagesBlob.length).toBe(0);
  });
});

// `akiva_host_query_packages` no longer exists post-11.3-final. The
// equivalent two-phase contract (length-too-small returns required
// size, fitting size copies and evicts) is exercised by the dispatcher
// tests in the `Stone 11.3c — unified-ABI dispatcher > io dispatch +
// FutureRegistry` block above (see "two-phase retry: payload too large
// returns length, retains entry").

// ── Stone 11.4b: unified host→app dispatch ─────────────────────────────

describe("dispatch — host→app multiplexer", () => {
  it("returns 0n on too-short payload (< 4 bytes for the handle)", () => {
    const fake = makeFakeProc();
    const { registry } = makeRegistry({ fakeProc: fake });
    seedHandle(registry);
    expect(registry.dispatch(CMD.RESUME, 0, 0)).toBe(0n);
    expect(registry.dispatch(CMD.RESUME, 0, 3)).toBe(0n);
    expect(fake.exports.akiva_app_dispatch).not.toHaveBeenCalled();
  });

  it("returns 0n on unknown handle", () => {
    const fake = makeFakeProc();
    const { registry, kernelMemory } = makeRegistry({ fakeProc: fake });
    seedHandle(registry, 1);
    // Write handle=42 (unknown) to kernel memory at offset 0.
    new DataView(kernelMemory.buffer).setUint32(0, 42, true);
    expect(registry.dispatch(CMD.RESUME, 0, 4)).toBe(0n);
    expect(fake.exports.akiva_app_dispatch).not.toHaveBeenCalled();
  });

  it("forwards a non-CREATE payload unchanged to akiva_app_dispatch", () => {
    const fake = makeFakeProc();
    const { registry, kernelMemory } = makeRegistry({ fakeProc: fake });
    const handle = seedHandle(registry, 7);
    // Build a POINTER payload [handle, x, y, button, phase, ts] = 28 bytes.
    const dv = new DataView(kernelMemory.buffer);
    dv.setUint32(0, handle, true);
    dv.setFloat32(4, 12.5, true);
    dv.setFloat32(8, 34.5, true);
    dv.setUint32(12, 0, true);   // button
    dv.setUint32(16, 1, true);   // phase = Move
    dv.setFloat64(20, 1234.0, true);
    fake.exports.akiva_app_dispatch.mockReturnValueOnce(0n);

    const ret = registry.dispatch(CMD.POINTER, 0, 28);
    expect(ret).toBe(0n);
    expect(fake.exports.akiva_app_dispatch).toHaveBeenCalledTimes(1);
    const [cmdKind, appPtr, len] = fake.exports.akiva_app_dispatch.mock.calls[0];
    expect(cmdKind).toBe(CMD.POINTER);
    expect(len).toBe(28);
    // The bytes the app saw must match what we put in kernel memory.
    const appBytes = new Uint8Array(fake.proc.memory.buffer, appPtr, len);
    const kernelBytes = new Uint8Array(kernelMemory.buffer, 0, 28);
    expect(Array.from(appBytes)).toEqual(Array.from(kernelBytes));
  });

  it("CREATE augments the [handle] payload with the class string", () => {
    const fake = makeFakeProc();
    const { registry, kernelMemory } = makeRegistry({ fakeProc: fake });
    const handle = seedHandle(registry, 3);
    // Kernel emits only [handle:u32 LE] for CREATE.
    new DataView(kernelMemory.buffer).setUint32(0, handle, true);

    registry.dispatch(CMD.CREATE, 0, 4);
    const [cmdKind, appPtr, len] = fake.exports.akiva_app_dispatch.mock.calls[0];
    expect(cmdKind).toBe(CMD.CREATE);
    // 4 bytes handle + class string ("Main" = 4 bytes) = 8.
    expect(len).toBe(4 + CLASS_NAME.length);
    const appBytes = new Uint8Array(fake.proc.memory.buffer, appPtr, len);
    // Handle prefix preserved.
    expect(new DataView(fake.proc.memory.buffer, appPtr, 4).getUint32(0, true))
      .toBe(handle);
    // Class string follows.
    const cls = new TextDecoder().decode(appBytes.slice(4));
    expect(cls).toBe(CLASS_NAME);
  });

  it("returns the app's BigInt future_id and frees the alloc", () => {
    const fake = makeFakeProc();
    const { registry, kernelMemory } = makeRegistry({ fakeProc: fake });
    const handle = seedHandle(registry, 11);
    new DataView(kernelMemory.buffer).setUint32(0, handle, true);
    fake.exports.akiva_app_dispatch.mockReturnValueOnce(11n);

    const ret = registry.dispatch(CMD.RENDER, 0, 12);
    expect(ret).toBe(11n);
    // Buffer-life lasts only the dispatch call — 11.4b dealloc-on-return.
    expect(fake.exports.akiva_app_dealloc).toHaveBeenCalledTimes(1);
  });

  it("returns 0n + warns when alloc fails, no dispatch invocation", () => {
    const fake = makeFakeProc({ allocFailAfter: 0 });
    const log = vi.fn();
    const { registry, kernelMemory } = makeRegistry({ fakeProc: fake, log });
    const handle = seedHandle(registry, 5);
    new DataView(kernelMemory.buffer).setUint32(0, handle, true);

    expect(registry.dispatch(CMD.RESUME, 0, 4)).toBe(0n);
    expect(fake.exports.akiva_app_dispatch).not.toHaveBeenCalled();
    expect(log).toHaveBeenCalledWith(
      "warn",
      expect.stringContaining("alloc"),
    );
  });

  it("returns 0n + logs and still frees on app dispatch throw", () => {
    const fake = makeFakeProc();
    const log = vi.fn();
    const { registry, kernelMemory } = makeRegistry({ fakeProc: fake, log });
    const handle = seedHandle(registry, 9);
    new DataView(kernelMemory.buffer).setUint32(0, handle, true);
    fake.exports.akiva_app_dispatch.mockImplementationOnce(() => {
      throw new Error("simulated trap");
    });

    expect(registry.dispatch(CMD.PAUSE, 0, 4)).toBe(0n);
    expect(fake.exports.akiva_app_dealloc).toHaveBeenCalledTimes(1);
    expect(log).toHaveBeenCalledWith(
      "error",
      expect.stringContaining("simulated trap"),
    );
  });
});

describe("getResult — fetch dispatch result bytes", () => {
  it("decodes the handle from low 32 bits of future_id and copies app bytes", () => {
    const fake = makeFakeProc();
    const { registry, kernelMemory } = makeRegistry({ fakeProc: fake });
    const handle = seedHandle(registry, 4);
    // Pre-place result bytes in app memory at a known address. The
    // _copyAppToKernel helper allocates a 4-byte len ptr, calls the
    // export with that ptr, reads len from app memory, and copies
    // src[0..len] into kernel memory. Mock: write 5 to *lenPtr and
    // return a pointer to "hello" in app memory.
    const HELLO_PTR = 5000;
    const helloBytes = new TextEncoder().encode("hello");
    new Uint8Array(fake.proc.memory.buffer, HELLO_PTR, helloBytes.length).set(helloBytes);

    fake.exports.akiva_app_get_result.mockImplementationOnce((_fid, lenPtr) => {
      new DataView(fake.proc.memory.buffer).setUint32(lenPtr, helloBytes.length, true);
      return HELLO_PTR;
    });

    const outPtr = 100;
    const ret = registry.getResult(BigInt(handle), outPtr, 64);
    expect(ret).toBe(5);
    const got = new Uint8Array(kernelMemory.buffer, outPtr, 5);
    expect(new TextDecoder().decode(got)).toBe("hello");
  });

  it("returns the needed length without copying when len > outCap", () => {
    const fake = makeFakeProc();
    const { registry } = makeRegistry({ fakeProc: fake });
    const handle = seedHandle(registry, 4);
    fake.exports.akiva_app_get_result.mockImplementationOnce((_fid, lenPtr) => {
      new DataView(fake.proc.memory.buffer).setUint32(lenPtr, 1024, true);
      return 5000;
    });
    // outCap=64 < 1024; helper must return needed length and not copy.
    expect(registry.getResult(BigInt(handle), 0, 64)).toBe(1024);
  });

  it("returns 0 on unknown handle (decoded from future_id)", () => {
    const fake = makeFakeProc();
    const { registry } = makeRegistry({ fakeProc: fake });
    seedHandle(registry, 1);
    // future_id encodes handle=999 — not seeded.
    expect(registry.getResult(999n, 0, 64)).toBe(0);
    expect(fake.exports.akiva_app_get_result).not.toHaveBeenCalled();
  });
});
