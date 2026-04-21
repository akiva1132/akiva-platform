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

    hal_storage_read:   (_kptr, _klen, _outLenPtr) => 0, // Phase 2
    hal_storage_write:  (_kptr, _klen, _vptr, _vlen) => 1,
    hal_storage_delete: (_kptr, _klen) => 1,

    hal_post_to_display: (displayId, ptr, len) => {
      const bytes = readBytes(ptr, len);
      self.postMessage({ type: "display_frame", displayId, bytes }, [bytes.buffer]);
    },
  };
  return { env };
}

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
