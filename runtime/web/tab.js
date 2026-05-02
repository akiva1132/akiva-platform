// Tab HAL.
//
// Each browser tab runs this module. Responsibilities:
//   1. Own the canvas and input surface.
//   2. Run a leader-election Session (see leader.js).
//   3. On becoming leader (either immediately or via promotion from
//      follower on failover): transfer the canvas to a Dedicated Worker
//      that hosts the kernel wasm.
//   4. On becoming follower: stay main-thread-side, pipe input + resize
//      to the leader via the broadcast channel.
//
// The Session abstracts transport. We call `session.send(...)` the same
// way regardless of role; it routes to the worker (leader) or the
// BroadcastChannel (follower).
//
// Lifecycle of the canvas:
//   - From mount until we become leader: main-thread owned, sized in JS.
//   - After becoming leader: ownership transferred to the worker via
//     `transferControlToOffscreen()`. All width/height changes from here
//     on are sent as "resize" messages; the worker applies them inside
//     its own context.
//
// See docs/runtime-web.md for the full protocol.

import { Session } from "./leader.js";

const KERNEL_URL = "./akiva-kernel.wasm";
const WORKER_URL = new URL("./worker.js", import.meta.url);
const CHANNEL = "akiva.os";
const LEADER_LOCK = "akiva.os.leader";

/**
 * Parse the `?debug=<csv>` query param into the worker's debug-category
 * list. Accepts `*` as a wildcard that enables every category. Missing
 * or empty → silent runtime, matching the default. Examples:
 *   ?debug=lifecycle              → ["lifecycle"]
 *   ?debug=lifecycle,intent,paint → three categories
 *   ?debug=*                      → ["*"] (wildcard)
 */
function parseDebugCategories() {
  const raw = new URLSearchParams(window.location.search).get("debug");
  if (!raw) return [];
  return raw.split(",").map((s) => s.trim()).filter(Boolean);
}

const surfaceEl = document.getElementById("akiva-surface");
const bootEl = document.getElementById("akiva-boot");

const tabId = crypto.randomUUID();

const dpr = () => self.devicePixelRatio || 1;

function computeMetrics() {
  const ratio = dpr();
  return {
    width_px:  Math.max(1, Math.floor(window.innerWidth  * ratio)),
    height_px: Math.max(1, Math.floor(window.innerHeight * ratio)),
    density:   ratio,
  };
}

/** Canvas sizing is valid only while the canvas is main-thread-owned.
 *  Once transferControlToOffscreen has been called, this function must
 *  not be called again — resize is handled inside the worker. */
function applyCanvasSize(canvas, metrics) {
  canvas.width  = metrics.width_px;
  canvas.height = metrics.height_px;
}

/** Per-tab mutable state that survives role transitions. */
const state = {
  ownsCanvas: true,   // true until we transferControlToOffscreen
  canvasTransferred: false,
  metrics: computeMetrics(),
};

// Initial canvas size. Safe regardless of role; after promotion we stop
// touching it from here.
applyCanvasSize(surfaceEl, state.metrics);

const session = new Session({
  channel: CHANNEL,
  lockName: LEADER_LOCK,
  tabId,
  kernelUrl: KERNEL_URL,
  workerUrl: WORKER_URL,
  debugCategories: parseDebugCategories(),
});

session.addEventListener("role", (e) => onRoleChange(e.detail));
session.addEventListener("message", (e) => onKernelMessage(e.detail));
session.addEventListener("panic", (e) => onPanic(e.detail.text));

// Input + resize forwarding are hooked exactly once. The Session takes
// care of routing to the right place based on its current role.
installInputForwarding(surfaceEl);
installResizeHandler(surfaceEl);

session.start().catch((err) => onPanic(`boot failed: ${err?.message ?? err}`));

// ── Role handling ────────────────────────────────────────────────────

function onRoleChange({ role, promoted }) {
  console.log(
    `[akiva] tab ${tabId.slice(0, 8)} — role=${role}${promoted ? " (promoted)" : ""}`,
  );

  if (role === "leader") {
    if (!state.canvasTransferred) {
      // Fresh metrics at transfer time — viewport may have changed while
      // we were follower (resize during wait).
      state.metrics = computeMetrics();
      applyCanvasSize(surfaceEl, state.metrics);

      console.log(`[akiva] transferring canvas to worker (${state.metrics.width_px}x${state.metrics.height_px})`);
      let offscreen;
      try {
        offscreen = surfaceEl.transferControlToOffscreen();
      } catch (err) {
        console.error("[akiva] transferControlToOffscreen failed:", err);
        onPanic(`canvas transfer failed: ${err?.message ?? err}`);
        return;
      }
      state.canvasTransferred = true;
      state.ownsCanvas = false;
      session.send(
        { type: "attach_display", tabId, surface: offscreen, metrics: state.metrics },
        [offscreen],
      );
    }
    bootEl.classList.add("hidden");
  } else if (role === "follower") {
    session.send({ type: "register_follower_display", tabId, metrics: state.metrics });
    bootEl.classList.add("hidden");
  }
}

function onKernelMessage(msg) {
  switch (msg?.type) {
    case "boot_ok":
      bootEl.classList.add("hidden");
      break;
    case "log":
      console.log(`[kernel ${msg.level}] ${msg.text}`);
      break;
    case "panic":
      onPanic(msg.text);
      break;
  }
}

function onPanic(text) {
  console.error("[akiva] panic:", text);
  bootEl.classList.remove("hidden");
  bootEl.textContent = `Akiva crashed: ${text}`;
}

// ── Input forwarding ─────────────────────────────────────────────────

function installInputForwarding(canvas) {
  const rect = () => canvas.getBoundingClientRect();

  const pointerEvent = (phase) => (e) => {
    const r = rect(); const ratio = dpr();
    session.send({
      type: "event", kind: "pointer", tabId,
      x: (e.clientX - r.left) * ratio,
      y: (e.clientY - r.top) * ratio,
      button: e.button | 0, phase,
    });
    if (phase === "down") canvas.focus();
    e.preventDefault();
  };
  canvas.addEventListener("pointerdown", pointerEvent("down"));
  canvas.addEventListener("pointermove", pointerEvent("move"));
  canvas.addEventListener("pointerup", pointerEvent("up"));
  canvas.addEventListener("pointercancel", pointerEvent("cancel"));

  const keyEvent = (phase) => (e) => {
    session.send({
      type: "event", kind: "key", tabId,
      code: e.keyCode | 0, modifiers: encodeModifiers(e), phase,
    });
    if (!shouldPassThroughKey(e)) e.preventDefault();
  };
  canvas.addEventListener("keydown", keyEvent("down"));
  canvas.addEventListener("keyup", keyEvent("up"));

  document.addEventListener("visibilitychange", () => {
    const visible = document.visibilityState === "visible";
    session.send({
      type: "event", kind: "visibility", tabId,
      visible,
    });
    // P0 #2 — graceful shutdown: page going background means the user
    // is on their way out (tab switch, OS-level swipe-up on mobile,
    // mid-route through a close). Fire a checkpoint so the foreground
    // activity's in-memory state has a chance to make it to OPFS
    // before the tab is throttled or terminated. visibilitychange is
    // the recommended hook over `beforeunload` — Safari ignores
    // beforeunload on bfcache transitions, and Chrome on mobile
    // doesn't always fire it on swipe-close.
    if (!visible) {
      session.send({ type: "checkpoint", tabId });
    }
  });

  // P0 #2 — last-resort checkpoint when the tab is unloading. `pagehide`
  // is the modern replacement for `beforeunload` and fires reliably on
  // tab close, navigation away, and bfcache eviction. The save bytes
  // travel async over the worker boundary, so on hard close they may
  // not land before the page dies — the visibilitychange path above
  // is the one that actually saves state in practice. This handler
  // exists to cover edge cases where visibility fires "visible" right
  // before unload (e.g. the user clicks the close button while the
  // page is foregrounded).
  window.addEventListener("pagehide", (e) => {
    // bfcache restore reuses the same Session — only checkpoint when
    // we're being terminated, not when we're being suspended. The
    // bfcache path is rare on apps that hold a Web Lock anyway.
    if (!e.persisted) {
      session.send({ type: "checkpoint", tabId });
    }
  });
}

function installResizeHandler(canvas) {
  // Stone 11.6.9 #2 — ResizeObserver + devicePixelContentBoxSize (Baseline
  // 2022). The plain `resize` window event misses zoom-level changes
  // (Ctrl+/-) because the viewport CSS size doesn't change, only DPR does;
  // ResizeObserver fires on both. `devicePixelContentBoxSize` returns the
  // canvas's physical-pixel box directly — no need to multiply CSS size by
  // DPR ourselves, and no rounding mismatch on fractional zoom levels.
  // Fallback path covers the (today vanishingly rare) browser without
  // devicePixelContentBoxSize support.
  const ro = new ResizeObserver((entries) => {
    const entry = entries[0];
    if (!entry) return;
    const dprNow = dpr();
    const dpcb = entry.devicePixelContentBoxSize?.[0];
    let width_px, height_px;
    if (dpcb) {
      width_px  = Math.max(1, dpcb.inlineSize);
      height_px = Math.max(1, dpcb.blockSize);
    } else {
      width_px  = Math.max(1, Math.round(canvas.clientWidth  * dprNow));
      height_px = Math.max(1, Math.round(canvas.clientHeight * dprNow));
    }
    const m = { width_px, height_px, density: dprNow };
    state.metrics = m;
    if (state.ownsCanvas) applyCanvasSize(canvas, m);
    session.send({ type: "resize", tabId, metrics: m });
  });
  ro.observe(canvas);
}

function encodeModifiers(e) {
  return (e.shiftKey ? 1 : 0) | (e.ctrlKey ? 2 : 0) | (e.altKey ? 4 : 0) | (e.metaKey ? 8 : 0);
}
function shouldPassThroughKey(e) {
  return e.key === "F5" || e.key === "F12" || (e.ctrlKey && (e.key === "r" || e.key === "R"));
}
