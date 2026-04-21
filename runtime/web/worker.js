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
      bootKernel(msg.kernelUrl, msg.tabId).catch((err) => {
        self.postMessage({ type: "panic", text: `boot failed: ${err?.message ?? err}` });
      });
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
// Phase 2b.4 bootstrap before we have a real registry or CLI install path.
// For each name in DEV_APPS, fetches `dev-apps/{name}/manifest.toml` and
// `dev-apps/{name}/app.wasm`, and if the package isn't already in the
// index, writes the three storage keys the PackageManager expects. A
// missing or 404ing app is logged and skipped — not every dev checkout
// will have built every app.

const DEV_APPS = ["hello"];

async function installDevApps(log) {
  for (const name of DEV_APPS) {
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

  // Already installed? Skip the fetch + write.
  const enc = new TextEncoder();
  const dec = new TextDecoder("utf-8");
  const existing = storage.get("sys/packages/index");
  const ids = existing
    ? dec.decode(existing).split("\n").filter((s) => s.length > 0)
    : [];
  if (ids.includes(packageId)) return;

  const wasmResp = await fetch(`./dev-apps/${name}/app.wasm`);
  if (!wasmResp.ok) {
    log("warn", `dev-apps/${name}/app.wasm: ${wasmResp.status} (did you run apps/${name}/build.sh?)`);
    return;
  }
  const wasmBytes = new Uint8Array(await wasmResp.arrayBuffer());

  storage.set(`packages/${packageId}/manifest.toml`, enc.encode(manifestText));
  storage.set(`packages/${packageId}/app.wasm`, wasmBytes);

  ids.push(packageId);
  ids.sort();
  storage.set("sys/packages/index", enc.encode(ids.join("\n")));
  log("info", `dev install: ${packageId} (${wasmBytes.byteLength} bytes)`);
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
    hal_app_lifecycle:  (handle, transition) => apps.lifecycle(handle, transition),
    hal_app_pointer:    (handle, x, y, button, phase, ts) => apps.pointer(handle, x, y, button, phase, ts),
    hal_app_key:        (handle, code, mods, phase, ts) => apps.key(handle, code, mods, phase, ts),
    hal_app_resize:     (handle, w, h, density) => apps.resize(handle, w, h, density),
    hal_app_render:     (handle, ts, outPtr, outCap) => apps.render(handle, ts, outPtr, outCap),
  };
  return { env };
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
