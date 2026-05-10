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
import { Canvas2DBackend } from "./renderer/canvas2d.js";
import { applyPaintStream } from "./paint_decoder.mjs";
import { Inspector, inspectorEnabled } from "./inspector.js";

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
  // Stone 13.4 — follower-only fields. The leader keeps them at
  // their defaults (canvas was transferred to the worker) and never
  // touches the local Canvas2DBackend.
  /** @type {number | null} The displayId the kernel assigned to us. */
  displayId: null,
  /** @type {Canvas2DBackend | null} */
  followerBackend: null,
  /**
   * Stone 13.4 follower-promotion fix — a separate canvas element the
   * follower paints into, layered over `surfaceEl` with
   * `pointer-events: none` so clicks fall through to `surfaceEl`
   * (where the input listeners live). Why a separate canvas: once
   * `getContext("2d")` is called on `surfaceEl`, the browser
   * forbids `transferControlToOffscreen()` on it. Followers that
   * later get promoted to leader (when the leader tab closes) need
   * `surfaceEl` pristine so the transfer can happen. Layering keeps
   * the input pipeline (which is wired to `surfaceEl`) untouched.
   * @type {HTMLCanvasElement | null}
   */
  followerCanvas: null,
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

// Inspector — JS-only window into the SDK's semantic stream. Off by
// default; mounts only when `?inspector=1` is set (or
// `localStorage.akivaInspector === "1"`). When mounted, exposes
// `window.__akiva` with `findById`/`findByRole`/`findByText`/`snapshot`
// for tests + devtools. No DOM is projected — see runtime/web/inspector.js
// for the rationale.
const inspector = inspectorEnabled() ? new Inspector({}) : null;
if (inspector) {
  console.log("[akiva] inspector enabled — window.__akiva exposes the live semantic tree");
}

// Input + resize forwarding are hooked exactly once. The Session takes
// care of routing to the right place based on its current role.
installInputForwarding(surfaceEl);
installResizeHandler(surfaceEl);
installColorSchemeWatcher();

session.start().catch((err) => onPanic(`boot failed: ${err?.message ?? err}`));

// ── Role handling ────────────────────────────────────────────────────

function onRoleChange({ role, promoted }) {
  console.log(
    `[akiva] tab ${tabId.slice(0, 8)} — role=${role}${promoted ? " (promoted)" : ""}`,
  );

  if (role === "leader") {
    // Stone 13.4 — promoted-from-follower path: tear down the
    // follower's overlay canvas BEFORE touching `surfaceEl`. The
    // overlay canvas had a 2D rendering context attached; the
    // browser would throw "Cannot transfer control from a canvas
    // that has a rendering context" if we tried to transfer
    // surfaceEl while the overlay was still around (it isn't on
    // surfaceEl itself any more, but discipline says: clean up the
    // follower-mode artefacts first). The follower canvas is
    // separate from surfaceEl precisely so this cleanup is a DOM
    // remove rather than a "uncontextualize" no-op the browser
    // doesn't expose.
    teardownFollowerCanvas();

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
      // Compositor selection. WebGPU on a worker-side OffscreenCanvas
      // hits a black-screen bug on several mobile drivers (init succeeds,
      // draws encode, but the swapchain pixels never reach the visible
      // canvas). Default to Canvas2D on touch / mobile UAs; allow
      // explicit override via `?webgpu` (force on) or `?canvas2d`
      // (force off).
      const params = new URLSearchParams(location.search);
      const ua = (navigator.userAgent || "").toLowerCase();
      const isMobile = /android|iphone|ipad|ipod|mobile|tablet/.test(ua)
                    || (navigator.maxTouchPoints || 0) > 1;
      let preferGPU;
      if (params.has("canvas2d"))      preferGPU = false;
      else if (params.has("webgpu"))   preferGPU = true;
      else                             preferGPU = !isMobile;
      console.log(`[akiva] compositor preference: preferGPU=${preferGPU} (mobile=${isMobile})`);
      session.send(
        { type: "attach_display", tabId, surface: offscreen, metrics: state.metrics, preferGPU },
        [offscreen],
      );
    }
    bootEl.classList.add("hidden");
  } else if (role === "follower") {
    // Stone 13.4 — followers paint into a separate overlay canvas so
    // `surfaceEl` stays pristine in case we later get promoted to
    // leader. The overlay sits absolutely on top of `surfaceEl` with
    // `pointer-events: none`, so the existing input listeners
    // (bound to `surfaceEl`) keep firing.
    setupFollowerCanvas();
    session.send({
      type: "request_display", tabId,
      width_px: state.metrics.width_px,
      height_px: state.metrics.height_px,
      density: state.metrics.density,
    });
    bootEl.classList.add("hidden");
  }
}

/**
 * Stone 13.4 follower-promotion fix — create the overlay canvas the
 * follower paints into. Sized to match `surfaceEl`'s current pixel
 * box. Idempotent: a second call with an existing overlay just
 * resyncs dimensions.
 */
function setupFollowerCanvas() {
  if (!state.followerCanvas) {
    const c = document.createElement("canvas");
    c.id = "akiva-follower-surface";
    Object.assign(c.style, {
      position: "fixed",
      inset: "0",
      width: "100vw",
      height: "100vh",
      display: "block",
      // Pass clicks through to `surfaceEl` underneath, where the
      // input listeners are installed. The follower canvas is
      // visual-only.
      pointerEvents: "none",
      // Z above surfaceEl so the user sees its paint stream.
      zIndex: "1",
    });
    document.body.appendChild(c);
    state.followerCanvas = c;
  }
  state.followerCanvas.width  = state.metrics.width_px;
  state.followerCanvas.height = state.metrics.height_px;
  state.followerBackend = new Canvas2DBackend();
  state.followerBackend.init(state.followerCanvas, state.metrics);
}

/**
 * Stone 13.4 follower-promotion fix — drop the follower's overlay
 * canvas + Canvas2DBackend. Called on role=leader (including
 * promotion) so the underlying `surfaceEl` is ready for
 * `transferControlToOffscreen`.
 */
function teardownFollowerCanvas() {
  if (state.followerCanvas) {
    state.followerCanvas.remove();
    state.followerCanvas = null;
  }
  state.followerBackend = null;
}

function onKernelMessage(msg) {
  switch (msg?.type) {
    case "boot_ok":
      bootEl.classList.add("hidden");
      // Test infra — flip a body class so e2e helpers have a single
      // unambiguous "kernel ready" signal. Pre-this the boot splash's
      // `.hidden` class doubled as a readiness marker, but `.hidden`
      // gets added on role assignment (which happens BEFORE the
      // worker even spawns the wasm), so a Playwright wait on it
      // returned long before the kernel could route input. With
      // `body.akiva-booted` flipped on `boot_ok` we get the first
      // moment the kernel can answer dispatches.
      document.body.classList.add("akiva-booted");
      break;
    case "log":
      console.log(`[kernel ${msg.level}] ${msg.text}`);
      break;
    case "panic":
      onPanic(msg.text);
      break;
    case "display_assigned":
      // Stone 13.4 — kernel allocated this follower a displayId.
      // From now on we filter incoming `display_frame` messages by
      // it. The leader auto-launches a fresh task on the assigned
      // display; tab.js doesn't need to know which app — it just
      // paints whatever bytes arrive.
      if (msg.tabId === tabId) {
        state.displayId = msg.displayId;
        console.log(`[akiva] follower ${tabId.slice(0, 8)} got displayId=${msg.displayId}`);
      }
      break;
    case "display_frame":
      // Stone 13.4 — paint stream for one of the displays. Followers
      // act on frames matching their own displayId; the leader
      // ignores these (its canvas is painted by the worker directly
      // via hal_draw_*).
      if (state.followerBackend && state.displayId === msg.displayId && msg.bytes) {
        applyPaintStream(msg.bytes, state.followerBackend);
      }
      break;
    // Stone 14.3 — Notifications. Only the leader's kernel produces
    // these; followers never receive them (the kernel-worker stub
    // they run doesn't dispatch app code), so the duplicate-
    // notification problem doesn't arise even with multiple tabs.
    case "notify_request_permission":
      handleNotifyRequestPermission(msg);
      break;
    case "notify_post":
      handleNotifyPost(msg);
      break;
    case "notify_close":
      handleNotifyCloseFromKernel(msg);
      break;
    // Stone 14.4 — Clipboard.
    case "clipboard_read_text":
      handleClipboardRead(msg);
      break;
    case "clipboard_write_text":
      handleClipboardWrite(msg);
      break;
    // Stone 14.5 — Vibration (fire-and-forget).
    case "vibrate":
      handleVibrate(msg);
      break;
    // Stone 14.6 — Geolocation.
    case "geo_get_position":
      handleGeoGetPosition(msg);
      break;
    // Test-infra phase — semantic-tree projection. Always emitted by
    // the SDK; ignored unless the inspector is mounted (gated by
    // `?inspector=1`). See runtime/web/inspector.js.
    case "semantics":
      inspector?.apply(msg);
      break;
    // Test-infra — per-frame composite transform for an activity
    // handle. The inspector applies this to rects returned from
    // findById/findByRole/findByText so tests get post-composite
    // coordinates even mid-animation. Production tabs (no inspector)
    // discard the message.
    case "surface_transform":
      inspector?.applyTransform(msg);
      break;
  }
}

// ── Stone 14.4 — Clipboard ───────────────────────────────────────────────

function handleClipboardRead({ capId }) {
  if (!navigator.clipboard?.readText) {
    session.send({ type: "clipboard_read_done", capId, text: null, reason: "navigator.clipboard.readText unavailable" });
    return;
  }
  navigator.clipboard.readText().then((text) => {
    session.send({ type: "clipboard_read_done", capId, text, reason: "" });
  }).catch((err) => {
    session.send({ type: "clipboard_read_done", capId, text: null, reason: String(err?.message ?? err) });
  });
}

function handleClipboardWrite({ capId, text }) {
  if (!navigator.clipboard?.writeText) {
    session.send({ type: "clipboard_write_done", capId, ok: false, reason: "navigator.clipboard.writeText unavailable" });
    return;
  }
  navigator.clipboard.writeText(text ?? "").then(() => {
    session.send({ type: "clipboard_write_done", capId, ok: true, reason: "" });
  }).catch((err) => {
    session.send({ type: "clipboard_write_done", capId, ok: false, reason: String(err?.message ?? err) });
  });
}

// ── Stone 14.5 — Vibration ───────────────────────────────────────────────

function handleVibrate({ pattern }) {
  if (typeof navigator.vibrate !== "function") return;
  // Browsers silently no-op on devices without haptic hardware; we
  // pass the pattern through and accept the same contract.
  try {
    if (Array.isArray(pattern) && pattern.length > 0) {
      navigator.vibrate(pattern);
    } else {
      // Empty pattern cancels in-progress vibration.
      navigator.vibrate(0);
    }
  } catch (_err) { /* ignore — fire-and-forget */ }
}

// ── Stone 14.6 — Geolocation ─────────────────────────────────────────────

function handleGeoGetPosition({ capId }) {
  if (!navigator.geolocation?.getCurrentPosition) {
    session.send({ type: "geo_get_position_done", capId, position: null, reason: "geolocation API unavailable" });
    return;
  }
  navigator.geolocation.getCurrentPosition(
    (pos) => {
      const c = pos.coords ?? {};
      session.send({
        type: "geo_get_position_done",
        capId,
        position: {
          latitude:  c.latitude  ?? 0,
          longitude: c.longitude ?? 0,
          accuracy:  c.accuracy  ?? 0,
          altitude:  c.altitude  ?? null,
          speed:     c.speed     ?? null,
          heading:   c.heading   ?? null,
          timestamp: pos.timestamp ?? Date.now(),
        },
        reason: "",
      });
    },
    (err) => {
      session.send({
        type: "geo_get_position_done",
        capId,
        position: null,
        reason: err?.message ?? `geolocation error code ${err?.code}`,
      });
    },
    { enableHighAccuracy: false, timeout: 10_000, maximumAge: 30_000 },
  );
}

// ── Stone 14.3 — Notifications ───────────────────────────────────────────
//
// All Notification API calls live on the main thread because
// `Notification.requestPermission()` requires a Window context (it
// throws or auto-denies in workers on most browsers). We keep an
// id-keyed map of live `Notification` instances so the kernel can
// close them later.
//
// Reply protocol (mirror of `app_registry.js::completeNotifyPost` /
// `completeNotifyPermission`):
//   { type: "notify_done", notifyId, ok: bool, reason: string }
//   { type: "notify_perm_done", notifyId, status: "default"|"granted"|"denied" }

/** @type {Map<number, Notification>} */
const liveNotifications = new Map();

function handleNotifyRequestPermission({ notifyId }) {
  if (typeof Notification === "undefined") {
    session.send({ type: "notify_perm_done", notifyId, status: "denied" });
    return;
  }
  // Promise form is supported on every browser that exposes the API
  // since 2019. The legacy callback form would also work but the
  // `then` chain composes more cleanly with the async reply.
  Promise.resolve(Notification.requestPermission()).then((status) => {
    session.send({ type: "notify_perm_done", notifyId, status });
  }).catch(() => {
    session.send({ type: "notify_perm_done", notifyId, status: "denied" });
  });
}

function handleNotifyPost({ notifyId, title, body, tag }) {
  if (typeof Notification === "undefined") {
    session.send({ type: "notify_done", notifyId, ok: false, reason: "Notification API unavailable" });
    return;
  }
  if (Notification.permission !== "granted") {
    session.send({
      type: "notify_done", notifyId, ok: false,
      reason: `permission ${Notification.permission}`,
    });
    return;
  }
  let n;
  try {
    n = new Notification(title, {
      body: body ?? "",
      tag: tag || undefined,
    });
  } catch (err) {
    session.send({
      type: "notify_done", notifyId, ok: false,
      reason: String(err?.message ?? err),
    });
    return;
  }
  liveNotifications.set(notifyId, n);
  // Drop the entry from our map once the user dismisses the
  // notification themselves — keeps the map tight and matches the
  // browser's own lifecycle (the underlying object is still valid
  // after `close`, but we don't need to track it).
  n.addEventListener("close", () => liveNotifications.delete(notifyId));
  session.send({ type: "notify_done", notifyId, ok: true, reason: "" });
}

function handleNotifyCloseFromKernel({ notifyId }) {
  const n = liveNotifications.get(notifyId);
  if (!n) return;
  liveNotifications.delete(notifyId);
  try { n.close(); } catch (_) { /* ignore */ }
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
    // Foundation gap — also surface the typed character on `keydown`
    // when `e.key` is a single printable character. Modifier-driven
    // shortcuts (Ctrl-C, Cmd-V) and named keys ("Enter", "ArrowDown",
    // "Backspace") stay on the key channel only; apps that want to
    // accept typing override `Activity::on_text_input` and read the
    // text from there. IME-composed text arrives via the
    // `compositionend` listener below, not here.
    if (phase === "down" && isSingleCharText(e)) {
      session.send({ type: "event", kind: "text", tabId, text: e.key });
    }
    if (!shouldPassThroughKey(e)) e.preventDefault();
  };
  canvas.addEventListener("keydown", keyEvent("down"));
  canvas.addEventListener("keyup", keyEvent("up"));

  // Browser back-button → kernel back. Mobile users (and laptop users
  // with no Escape key handy) reach for the OS-level back gesture; we
  // bridge it to the framework's KEY_CODE_BACK (27) so `dispatch_key`
  // pops the activity stack just like Escape does. Pattern: seed one
  // sentinel state at boot so popstate fires on the first back; the
  // handler re-pushes after acting so subsequent backs keep firing
  // without the browser actually leaving the page.
  history.pushState({ akiva: 1 }, "", location.href);
  window.addEventListener("popstate", () => {
    session.send({ type: "event", kind: "key", tabId, code: 27, modifiers: 0, phase: "down" });
    session.send({ type: "event", kind: "key", tabId, code: 27, modifiers: 0, phase: "up"   });
    history.pushState({ akiva: 1 }, "", location.href);
  });

  // IME / multi-codepoint paste path. `compositionend.data` carries
  // the committed text — for Latin layouts this is what the user
  // typed during composition (dead keys → accented char), for CJK /
  // Hebrew niqqud / emoji it can be a multi-character string. Browsers
  // suppress `keydown` during composition to avoid double-firing, so
  // we listen for the commit explicitly.
  canvas.addEventListener("compositionend", (e) => {
    if (typeof e.data === "string" && e.data.length > 0) {
      session.send({ type: "event", kind: "text", tabId, text: e.data });
    }
  });

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
      // Stone 13.4 — release our claimed display so the kernel can
      // free its slot. Best-effort: on hard close the BC may not
      // flush before the page dies, but the leader's worker GCs
      // followerDisplays on probe-timeout regardless.
      if (state.displayId !== null) {
        session.send({ type: "release_display", tabId, displayId: state.displayId });
      }
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
    // Stone 13.4 — followers paint into the overlay canvas, not
    // `surfaceEl`. Resize the overlay's pixel grid alongside the
    // backend so subsequent `display_frame` decodes target the
    // right dimensions. The leader path goes through `worker.js`
    // (worker.js calls `backend.resize` against the OffscreenCanvas
    // it owns).
    if (state.followerBackend && state.followerCanvas) {
      state.followerBackend.resize(width_px, height_px, dprNow);
    }
    session.send({ type: "resize", tabId, metrics: m });
  });
  ro.observe(canvas);
}

function installColorSchemeWatcher() {
  // Stone 11.6.9 #3 — observe the OS-level dark-mode preference.
  // `matchMedia` is universally supported; the `change` event is
  // emitted whenever the user (or system) flips the setting at
  // runtime. We post the value through the Session so it lands in
  // the kernel-worker just like resize / input forwarding does;
  // the kernel turns it into a `set_resources` broadcast and an
  // `akiva_config_change(night)` export call.
  //
  // Initial send is unconditional — at boot the kernel hasn't
  // observed any signal and assumes light mode, so we always
  // surface the actual current value once even when night=false,
  // to make the kernel-side state explicit rather than implicit.
  const mq = window.matchMedia("(prefers-color-scheme: dark)");
  const send = () => session.send({ type: "config-change", tabId, night: mq.matches });
  mq.addEventListener("change", send);
  send();
}

function encodeModifiers(e) {
  return (e.shiftKey ? 1 : 0) | (e.ctrlKey ? 2 : 0) | (e.altKey ? 4 : 0) | (e.metaKey ? 8 : 0);
}
function shouldPassThroughKey(e) {
  return e.key === "F5" || e.key === "F12" || (e.ctrlKey && (e.key === "r" || e.key === "R"));
}

/**
 * Foundation gap — should this `keydown` also fire a text event?
 *
 * The text channel exists for typed input (Notes / Search / chat).
 * Three filters narrow `keydown` down to "user typed a character":
 *
 *   1. **Single-codepoint `e.key`.** Single printable character — `"a"`,
 *      `"A"`, `"!"`. Named keys land in `e.key` as multi-letter strings:
 *      `"Enter"`, `"Backspace"`, `"ArrowDown"`, `"F1"`. We use the
 *      Unicode-aware iterator length so a multi-byte emoji that the
 *      browser delivered as a single keydown ("👋", `e.key.length === 2`
 *      under UTF-16) still counts. Native composition-end multi-char
 *      payloads land via `compositionend` instead.
 *   2. **No Ctrl / Meta.** Ctrl-C, Cmd-V, etc. are shortcuts, not text.
 *      Apps that genuinely want Ctrl-letter as typed input are unusual
 *      enough that we make them parse the existing `e.key` from the
 *      key channel themselves.
 *   3. **`e.isComposing` false.** Browsers fire keydown(229)
 *      throughout an IME composition; the committed text already arrives
 *      via `compositionend`, so swallowing the in-flight keydowns
 *      avoids duplicates and partial-character delivery.
 */
function isSingleCharText(e) {
  if (e.ctrlKey || e.metaKey) return false;
  if (e.isComposing) return false;
  const key = e.key;
  if (typeof key !== "string" || key.length === 0) return false;
  // Reject named keys — they're always more than one Unicode scalar.
  // [...key].length collapses surrogate pairs so emoji counts as 1.
  return [...key].length === 1;
}
