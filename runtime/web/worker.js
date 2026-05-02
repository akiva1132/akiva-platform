// Worker HAL.
//
// Hosts the kernel wasm module. One of these runs in the leader tab's
// Dedicated Worker. It:
//   1. Fetches and instantiates `akiva-kernel.wasm`, supplying imports.
//   2. Drives the frame loop (requestAnimationFrame via rAF-bridge).
//   3. Exposes a renderer backend (Canvas2D in Phase 1a; see
//      ARCHITECTURE.md §10 for the pluggable trait).
//   4. Relays input events from tabs and draws output back through
//      OffscreenCanvas.
//
// The worker never touches the DOM. All "display" state is owned here;
// the main thread is a thin input/output surface.

import { Canvas2DBackend } from "./renderer/canvas2d.js";
import { Storage, SR_BAD_KEY } from "./storage.js";
import { AppRegistry } from "./app_registry.js";

/** @type {WebAssembly.Instance | null} */
let instance = null;
/** @type {WebAssembly.Memory | null} */
let memory = null;
/** Primary display — Phase 1a supports exactly one. */
let primary = null;
/** Renderer backend instance. */
let backend = null;
/** Kernel init state. */
let booted = false;
/** Persistent KV store. Initialised before akiva_boot. */
const storage = new Storage();
/** App wasm registry. Lazily binds to kernel memory once the kernel is booted. */
const apps = new AppRegistry({
  storage,
  kernelMemory: () => memory,
  log: (level, text) => self.postMessage({ type: "log", level: levelCode(level), text }),
});
/** UTF-8 decoder for host-side reading of kernel strings. */
const decoder = new TextDecoder("utf-8");

// ── Message handling from the main thread ───────────────────────────────

self.addEventListener("message", (e) => {
  const msg = e.data;
  switch (msg?.type) {
    case "init":
      // Forward the initial debug categories (if any) before boot so
      // the first lifecycle events — register, shell's on_create —
      // are observable. Arrives as `{ debug: ["lifecycle", "intent"] }`
      // parsed from the tab's `?debug=...` query on the main thread.
      if (Array.isArray(msg.debug)) {
        apps.setDebugCategories(msg.debug);
      }
      bootKernel(msg.kernelUrl, msg.tabId).catch((err) => {
        self.postMessage({ type: "panic", text: `boot failed: ${err?.message ?? err}` });
      });
      break;
    case "set_debug":
      // Live toggle: supports flipping categories at runtime without
      // rebooting the worker (`postMessage({type:'set_debug',debug:['lifecycle']})`
      // from devtools console on the main thread works).
      apps.setDebugCategories(Array.isArray(msg.debug) ? msg.debug : []);
      log("info", `debug categories set: ${(msg.debug ?? []).join(",") || "(none)"}`);
      break;
    case "attach_display":
      attachDisplay(msg);
      break;
    case "register_follower_display":
      // MVP: followers just reserve a slot; compositor multiplexing lands
      // in Phase 2.
      log("info", `follower display registered for tab ${msg.tabId}`);
      break;
    case "resize":
      if (primary?.tabId === msg.tabId && backend) {
        backend.resize(msg.metrics.width_px, msg.metrics.height_px, msg.metrics.density);
        postEvent(3 /* resize */, encodeResize(msg.metrics));
      }
      break;
    case "event":
      forwardEvent(msg);
      break;
    default:
      log("warn", `unknown message type: ${msg?.type}`);
  }
});

async function bootKernel(kernelUrl, _tabId) {
  // Storage must be ready before the kernel can call hal_storage_* from
  // within akiva_boot. The load itself is async (OPFS handle creation),
  // but every subsequent kernel-visible operation is synchronous.
  await storage.load((level, text) => self.postMessage({ type: "log", level: levelCode(level), text }));

  // Dev-mode: pull any apps staged under runtime/web/dev-apps/ into
  // storage so PackageManager rehydrates them during `akiva_boot`. Safe
  // to run on every boot — already-installed packages are skipped.
  await installDevApps((level, text) => self.postMessage({ type: "log", level: levelCode(level), text }));

  const resp = await fetch(kernelUrl);
  if (!resp.ok) throw new Error(`fetch ${kernelUrl}: ${resp.status}`);
  const bytes = await resp.arrayBuffer();

  const imports = buildImports();
  const { instance: inst } = await WebAssembly.instantiate(bytes, imports);
  instance = inst;
  memory = inst.exports.memory;

  inst.exports.akiva_boot();
  booted = true;
  self.postMessage({ type: "boot_ok" });

  // Kick the frame loop once we have a display.
  if (primary) startFrameLoop();
}

// ── Dev-mode app installation ────────────────────────────────────────────
//
// Dev-loop bootstrap: for each directory staged under `runtime/web/dev-apps/`
// (by `akiva build` or `akiva install`), fetch its `manifest.toml` and
// `app.wasm` and write them into OPFS at the keys PackageManager expects.
// Already-installed packages get their wasm refreshed but are not duplicated
// in the index. A missing or 404ing app is logged and skipped — not every
// dev checkout has every app staged.
//
// The directory list comes from the devserver's dynamic
// `./dev-apps/index.json` endpoint (Stone 8.4) — no hardcoded names live
// here anymore. Production boots (no devserver, OPFS-resident packages)
// skip this path entirely: the fetch 404s and discoverDevApps returns [].

async function discoverDevApps(log) {
  try {
    const resp = await fetch("./dev-apps/index.json");
    if (!resp.ok) {
      // 404 is production (no devserver, no dev-apps tree) — stay
      // quiet. Anything else means the devserver is up but the
      // endpoint is misbehaving (stale binary from before Stone 8.4,
      // say). Surface it so the dev loop isn't a silent mystery.
      if (resp.status === 404) {
        log("info", "dev-apps/index.json: 404 (production boot or stale devserver — no dev apps to install)");
      } else {
        log("warn", `dev-apps/index.json: ${resp.status}`);
      }
      return [];
    }
    const body = await resp.json();
    if (!Array.isArray(body?.dirs)) {
      log("warn", "dev-apps/index.json: malformed (expected {\"dirs\":[...]})");
      return [];
    }
    log("info", `dev-apps discovered: ${body.dirs.length === 0 ? "(empty)" : body.dirs.join(", ")}`);
    return body.dirs;
  } catch (err) {
    log("warn", `dev-apps/index.json: ${err?.message ?? err}`);
    return [];
  }
}

async function installDevApps(log) {
  const names = await discoverDevApps(log);
  for (const name of names) {
    try {
      await installOneDevApp(name, log);
    } catch (err) {
      log("warn", `dev install '${name}': ${err?.message ?? err}`);
    }
  }
}

async function installOneDevApp(name, log) {
  const manifestResp = await fetch(`./dev-apps/${name}/manifest.toml`);
  if (!manifestResp.ok) {
    // Not fatal — app simply isn't staged. `apps/${name}/build.sh` fixes that.
    if (manifestResp.status !== 404) {
      log("warn", `dev-apps/${name}/manifest.toml: ${manifestResp.status}`);
    }
    return;
  }
  const manifestText = await manifestResp.text();

  // Crude TOML field extraction — we don't have a parser in the worker,
  // and pulling one in just to read the id is overkill. The kernel-side
  // manifest parser will reject anything malformed at load-time anyway.
  const idMatch = manifestText.match(/^\s*id\s*=\s*"([^"]+)"/m);
  if (!idMatch) {
    log("warn", `dev-apps/${name}/manifest.toml: no [app].id`);
    return;
  }
  const packageId = idMatch[1];

  // Dev mode: always overwrite on boot. Otherwise a `build.sh` rebuild
  // wouldn't actually reach the running app — OPFS would keep serving the
  // first wasm we ever staged. Re-installing is a local fetch + a couple
  // of OPFS writes, which is cheap.
  const wasmResp = await fetch(`./dev-apps/${name}/app.wasm`);
  if (!wasmResp.ok) {
    log("warn", `dev-apps/${name}/app.wasm: ${wasmResp.status} (did you run apps/${name}/build.sh?)`);
    return;
  }
  const wasmBytes = new Uint8Array(await wasmResp.arrayBuffer());

  const enc = new TextEncoder();
  const dec = new TextDecoder("utf-8");
  storage.set(`packages/${packageId}/manifest.toml`, enc.encode(manifestText));
  storage.set(`packages/${packageId}/app.wasm`, wasmBytes);

  // Resource values files are optional. If the dev-apps stage dir has
  // them, copy into OPFS alongside the manifest so the framework's
  // ResourceManager picks them up at boot. Either file may be absent;
  // the 404 path is a quiet no-op.
  for (const leaf of ["strings", "colors"]) {
    const resp = await fetch(`./dev-apps/${name}/res/values/${leaf}.toml`);
    if (resp.ok) {
      const text = await resp.text();
      storage.set(`packages/${packageId}/res/values/${leaf}.toml`, enc.encode(text));
    } else if (resp.status !== 404) {
      log("warn", `dev-apps/${name}/res/values/${leaf}.toml: ${resp.status}`);
    }
  }

  const existing = storage.get("sys/packages/index");
  const ids = existing
    ? dec.decode(existing).split("\n").filter((s) => s.length > 0)
    : [];
  const fresh = !ids.includes(packageId);
  if (fresh) {
    ids.push(packageId);
    ids.sort();
    storage.set("sys/packages/index", enc.encode(ids.join("\n")));
  }
  log("info", `dev install: ${packageId} (${wasmBytes.byteLength} bytes${fresh ? ", new" : ", refreshed"})`);
}

function attachDisplay(msg) {
  primary = {
    tabId: msg.tabId,
    surface: msg.surface,
    metrics: msg.metrics,
  };
  backend = new Canvas2DBackend();
  backend.init(msg.surface, msg.metrics);

  if (booted && instance) startFrameLoop();
}

// ── Frame loop ──────────────────────────────────────────────────────────

let rafId = 0;
function startFrameLoop() {
  if (rafId) return;
  const tick = (ts) => {
    try {
      instance.exports.akiva_frame(ts);
    } catch (err) {
      self.postMessage({ type: "panic", text: String(err?.message ?? err) });
      return;
    }
    rafId = self.requestAnimationFrame ? self.requestAnimationFrame(tick) : setTimeout(() => tick(performance.now()), 16);
  };
  rafId = self.requestAnimationFrame ? self.requestAnimationFrame(tick) : setTimeout(() => tick(performance.now()), 16);
}

// ── Kernel → host imports ───────────────────────────────────────────────

function buildImports() {
  const env = {
    hal_log: (level, ptr, len) => {
      const text = readUtf8(ptr, len);
      self.postMessage({ type: "log", level, text });
    },

    hal_panic: (ptr, len) => {
      const text = readUtf8(ptr, len);
      self.postMessage({ type: "panic", text });
      // The wasm trap will follow; nothing more to do here.
      throw new Error(`akiva panic: ${text}`);
    },

    hal_now_ms: () => performance.now(),

    hal_surface_width:   () => primary?.metrics?.width_px ?? 1,
    hal_surface_height:  () => primary?.metrics?.height_px ?? 1,
    hal_surface_density: () => primary?.metrics?.density ?? 1,

    hal_draw_clear:   (rgba) => backend?.clear(rgba >>> 0),
    hal_draw_rect:    (x, y, w, h, rgba) => backend?.fillRect(x, y, w, h, rgba >>> 0),
    hal_draw_text:    (x, y, size, rgba, ptr, len) => {
      const text = readUtf8(ptr, len);
      backend?.drawText(x, y, size, rgba >>> 0, text);
    },
    hal_draw_present: () => backend?.present(),

    hal_storage_read:   (kptr, klen, outLenPtr) => halStorageRead(kptr, klen, outLenPtr),
    hal_storage_write:  (kptr, klen, vptr, vlen) => halStorageWrite(kptr, klen, vptr, vlen),
    hal_storage_delete: (kptr, klen) => halStorageDelete(kptr, klen),

    hal_post_to_display: (displayId, ptr, len) => {
      const bytes = readBytes(ptr, len);
      self.postMessage({ type: "display_frame", displayId, bytes }, [bytes.buffer]);
    },

    // ── App runtime (Phase 2b.3) ─────────────────────────────────────────
    // Contract matches kernel/src/host.rs `hal_app_*`. Routes to the
    // `AppRegistry` which owns one WebAssembly.Instance per installed
    // package and one handle per framework-side activity slot.
    hal_app_register: (pidPtr, pidLen, clsPtr, clsLen) =>
      apps.register(readUtf8(pidPtr, pidLen), readUtf8(clsPtr, clsLen)),
    hal_app_unregister: (handle) => apps.unregister(handle),

    // Stone 11.4b/c — unified host→app dispatch. The kernel-side
    // `KernelWasmHost` builds a `[handle: u32 LE, ...]` payload and
    // calls `hal_app_dispatch` with the cmd_kind that identifies the
    // lifecycle / input / render / state / intent transition. The JS
    // side allocates a kernel-side future_id, posts the payload to
    // the package's app-worker, and returns the id immediately —
    // every dispatch is async post-11.4c. The kernel polls via
    // `hal_app_poll_future` until READY/ERROR, then optionally fetches
    // result bytes via `hal_app_get_result` (RENDER/SAVE_STATE).
    hal_app_dispatch:    (cmdKind, payloadPtr, payloadLen) =>
      apps.dispatch(cmdKind, payloadPtr, payloadLen),
    hal_app_poll_future: (futureId) => apps.pollFuture(futureId),
    hal_app_get_result:  (futureId, outPtr, outCap) =>
      apps.getResult(futureId, outPtr, outCap),

    // Stone 9.3 — read + clear the pending-finish flag for `handle`.
    // Returns 1 if the app called `akiva_host_finish` since the last
    // drain, 0 otherwise.
    hal_app_drain_finish: (handle) => (apps.drainFinish(handle) ? 1 : 0),

    // Stone 9.4 — drain the pending intent blob for `handle` and copy
    // it into `outPtr`. Returns the total blob size. If the return
    // value exceeds `outCap` nothing was copied and the kernel retries
    // with a larger buffer; the blob stays armed until a call actually
    // consumes it.
    hal_app_drain_intent: (handle, outPtr, outCap) => halAppDrainIntent(handle, outPtr, outCap),

    // Stone 9.5 — two-phase drains for the for-result flow.
    hal_app_drain_start_for_result: (handle, outPtr, outCap, rcOut) =>
      halAppDrainStartForResult(handle, outPtr, outCap, rcOut),
    hal_app_drain_set_result: (handle, outPtr, outCap) =>
      halAppDrainSetResult(handle, outPtr, outCap),

    // Stone 10.3b — push per-package permission grants. Called from
    // `framework::boot` after rehydration, once per installed package,
    // before any activity is launched. `grantsPtr..grantsLen` is the
    // LF-separated UTF-8 name list that `PackageManager::install`
    // persists to `sys/permissions/{id}`; empty payload = "no grants".
    hal_host_set_grants: (pkgPtr, pkgLen, grantsPtr, grantsLen) => {
      const packageId = readUtf8(pkgPtr, pkgLen);
      const blob = grantsLen === 0 ? "" : readUtf8(grantsPtr, grantsLen);
      const grants = blob.length === 0 ? [] : blob.split("\n").filter((s) => s.length > 0);
      apps.setGrants(packageId, grants);
    },

    // Stone 11.0a — push the snapshot of installed packages. The kernel
    // hands us the framework's already-encoded blob (length-prefixed; see
    // framework/src/pkgs_wire.rs). We don't decode here — we copy the
    // bytes out of kernel memory and hand them to AppRegistry, which
    // serves them straight back when an app calls
    // `akiva_host_query_packages`. Empty payload = "no packages".
    hal_host_set_packages: (bytesPtr, bytesLen) => {
      const bytes = bytesLen === 0 ? new Uint8Array(0) : readBytes(bytesPtr, bytesLen);
      apps.setPackages(bytes);
    },
  };
  return { env };
}

function halAppDrainIntent(handle, outPtr, outCap) {
  const bytes = apps.drainIntent(handle);
  if (!bytes || bytes.length === 0) return 0;
  if (bytes.length > outCap) {
    // Two-phase contract: buffer too small, leave the blob armed and
    // report the required size so the kernel can retry.
    return bytes.length;
  }
  new Uint8Array(memory.buffer, outPtr, bytes.length).set(bytes);
  apps._consumeIntent(handle);
  return bytes.length;
}

function halAppDrainStartForResult(handle, outPtr, outCap, rcOut) {
  const entry = apps.drainStartForResult(handle);
  if (!entry) return 0;
  const { bytes, requestCode } = entry;
  // rcOut is always written, regardless of whether the buffer fits —
  // so a retry (which also hits this function) sees a consistent
  // request_code on both calls.
  new DataView(memory.buffer).setUint32(rcOut, requestCode, true);
  if (bytes.length > outCap) return bytes.length;
  new Uint8Array(memory.buffer, outPtr, bytes.length).set(bytes);
  apps._consumeStartForResult(handle);
  return bytes.length;
}

function halAppDrainSetResult(handle, outPtr, outCap) {
  const bytes = apps.drainSetResult(handle);
  if (!bytes || bytes.length === 0) return 0;
  if (bytes.length > outCap) return bytes.length;
  new Uint8Array(memory.buffer, outPtr, bytes.length).set(bytes);
  apps._consumeSetResult(handle);
  return bytes.length;
}

// ── Storage bridge ──────────────────────────────────────────────────────
//
// Kernel-facing signatures (see kernel/src/host.rs):
//   hal_storage_read(kptr, klen, out_len_ptr) -> *u8
//     returns null on miss; otherwise a kernel-allocated buffer whose
//     length has been written to *out_len_ptr. Kernel owns and frees it.
//   hal_storage_write(kptr, klen, vptr, vlen) -> u32   // 0 = ok
//   hal_storage_delete(kptr, klen) -> u32              // 0 = ok

function halStorageRead(kptr, klen, outLenPtr) {
  const key = readUtf8(kptr, klen);
  if (!key) { writeUsize(outLenPtr, 0); return 0; }
  const val = storage.get(key);
  if (val === null) { writeUsize(outLenPtr, 0); return 0; }
  // Hand the kernel a copy in its own memory so it controls the lifetime.
  const ptr = instance.exports.akiva_alloc(val.byteLength);
  if (ptr === 0) { writeUsize(outLenPtr, 0); return 0; }
  new Uint8Array(memory.buffer, ptr, val.byteLength).set(val);
  writeUsize(outLenPtr, val.byteLength);
  return ptr;
}

function halStorageWrite(kptr, klen, vptr, vlen) {
  const key = readUtf8(kptr, klen);
  if (!key) return SR_BAD_KEY;
  // Copy immediately — set() copies again defensively, but reading from
  // wasm memory via a view is only safe before any allocation/growth.
  const val = new Uint8Array(memory.buffer, vptr, vlen).slice();
  return storage.set(key, val);
}

function halStorageDelete(kptr, klen) {
  const key = readUtf8(kptr, klen);
  if (!key) return SR_BAD_KEY;
  return storage.delete(key);
}

// Writes a usize (32-bit on wasm32) into kernel memory.
function writeUsize(ptr, value) {
  new DataView(memory.buffer).setUint32(ptr, value >>> 0, true);
}

// Flush + release the OPFS handle if the worker is being torn down.
// Browsers auto-release on worker termination, but flushing first
// reduces the window where a crash leaves the file mid-write.
self.addEventListener("unload", () => storage.close());

// ── Event forwarding ────────────────────────────────────────────────────

const EVENT_POINTER = 1;
const EVENT_KEY = 2;
const EVENT_RESIZE = 3;
const EVENT_VISIBILITY = 4;

function forwardEvent(msg) {
  if (!booted || !instance) return;
  switch (msg.kind) {
    case "pointer": {
      const phaseId = { down: 0, move: 1, up: 2, cancel: 3 }[msg.phase] ?? 3;
      const buf = new ArrayBuffer(10);
      const dv = new DataView(buf);
      dv.setFloat32(0, msg.x, true);
      dv.setFloat32(4, msg.y, true);
      dv.setUint8(8, msg.button & 0xff);
      dv.setUint8(9, phaseId);
      postEvent(EVENT_POINTER, new Uint8Array(buf));
      break;
    }
    case "key": {
      const phaseId = { down: 0, repeat: 1, up: 2 }[msg.phase] ?? 0;
      const buf = new ArrayBuffer(6);
      const dv = new DataView(buf);
      dv.setUint32(0, msg.code, true);
      dv.setUint8(4, msg.modifiers & 0xff);
      dv.setUint8(5, phaseId);
      postEvent(EVENT_KEY, new Uint8Array(buf));
      break;
    }
    case "visibility": {
      const buf = new Uint8Array([msg.visible ? 1 : 0]);
      postEvent(EVENT_VISIBILITY, buf);
      break;
    }
  }
}

function encodeResize(m) {
  const buf = new ArrayBuffer(12);
  const dv = new DataView(buf);
  dv.setUint32(0, m.width_px >>> 0, true);
  dv.setUint32(4, m.height_px >>> 0, true);
  dv.setUint32(8, Math.round((m.density || 1) * 256) >>> 0, true);
  return new Uint8Array(buf);
}

function postEvent(kind, bytes) {
  // Silently drop events that land before the kernel wasm is live. Boot
  // is async (storage load + dev install + fetch + instantiate), and the
  // leader may have forwarded resize/pointer events in the meantime.
  // Reading `instance.exports` before instantiate resolves throws
  // "Cannot read properties of null", which the session reports as a
  // worker panic — dropping is correct because the kernel will read the
  // fresh surface state from the HAL on its very first frame anyway.
  if (!booted || !instance) return;
  const len = bytes.length;
  const ptr = instance.exports.akiva_alloc(len);
  if (ptr === 0) { log("warn", `akiva_alloc(${len}) → null, dropping event`); return; }
  new Uint8Array(memory.buffer, ptr, len).set(bytes);
  instance.exports.akiva_on_event(kind, ptr, len);
  instance.exports.akiva_dealloc(ptr, len);
}

// ── Helpers ─────────────────────────────────────────────────────────────

function readUtf8(ptr, len) {
  if (!memory || len === 0) return "";
  return decoder.decode(new Uint8Array(memory.buffer, ptr, len));
}
function readBytes(ptr, len) {
  return new Uint8Array(memory.buffer, ptr, len).slice();
}

function log(level, text) {
  self.postMessage({ type: "log", level: levelCode(level), text });
}
function levelCode(name) {
  return { trace: 0, debug: 1, info: 2, warn: 3, error: 4 }[name] ?? 2;
}
