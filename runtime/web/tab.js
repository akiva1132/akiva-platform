// Tab HAL.
//
// Each browser tab runs this module. The tab is responsible for:
//   1. Owning the canvas and input surface.
//   2. Participating in leader election (see leader.js).
//   3. If leader: spawning the Dedicated Worker that hosts the kernel wasm.
//   4. If follower: using the broadcast channel as the kernel transport.
//   5. Transferring an OffscreenCanvas to the worker so all rendering
//      happens off-main-thread. Followers don't transfer — they receive
//      frame deltas from the leader and render onto their own canvas.
//
// Matches ARCHITECTURE.md §9 (single-instance architecture) and §10
// (renderer backend) — the pluggable backend lives inside the worker.

import { electLeader } from "./leader.js";

const KERNEL_URL = "./akiva-kernel.wasm";
const CHANNEL = "akiva.os";
const LEADER_LOCK = "akiva.os.leader";

const surfaceEl = document.getElementById("akiva-surface");
const bootEl = document.getElementById("akiva-boot");

const tabId = crypto.randomUUID();

const dpr = () => self.devicePixelRatio || 1;
function sizeCanvasToViewport(canvas) {
  const ratio = dpr();
  const w = Math.max(1, Math.floor(window.innerWidth * ratio));
  const h = Math.max(1, Math.floor(window.innerHeight * ratio));
  if (canvas.width !== w) canvas.width = w;
  if (canvas.height !== h) canvas.height = h;
  return { width_px: w, height_px: h, density: ratio };
}

/** Uniform transport interface over Worker (leader) or BroadcastChannel (follower). */
function makeTransport(role, port) {
  if (role === "leader") {
    return {
      send: (msg, transfer = []) => port.postMessage(msg, transfer),
      on:   (handler) => port.addEventListener("message", (e) => handler(e.data)),
    };
  }
  // Follower: wrap broadcast channel so messages are namespaced and
  // addressed to/from the leader.
  return {
    send: (msg) => port.postMessage({ type: "follower_msg", payload: msg }),
    on:   (handler) => port.addEventListener("message", (e) => {
      if (e.data?.type === "leader_msg") handler(e.data.payload);
    }),
  };
}

function attachCanvas(transport, role, canvas) {
  const metrics = sizeCanvasToViewport(canvas);
  if (role === "leader") {
    // Only the leader can transfer the canvas to the worker it owns.
    const offscreen = canvas.transferControlToOffscreen();
    transport.send(
      { type: "attach_display", tabId, surface: offscreen, metrics },
      [offscreen],
    );
  } else {
    // Follower: keep the canvas main-thread-side; the leader pushes frame
    // deltas back which we apply locally (Phase 2). For now the follower
    // canvas just shows the boot banner.
    transport.send({ type: "register_follower_display", tabId, metrics });
  }

  window.addEventListener("resize", () => {
    const m = sizeCanvasToViewport(canvas);
    transport.send({ type: "resize", tabId, metrics: m });
  });
}

function forwardInput(transport, canvas) {
  const rect = () => canvas.getBoundingClientRect();

  const pointerEvent = (phase) => (e) => {
    const r = rect(); const ratio = dpr();
    transport.send({
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
    transport.send({
      type: "event", kind: "key", tabId,
      code: e.keyCode | 0, modifiers: encodeModifiers(e), phase,
    });
    if (!shouldPassThroughKey(e)) e.preventDefault();
  };
  canvas.addEventListener("keydown", keyEvent("down"));
  canvas.addEventListener("keyup", keyEvent("up"));

  document.addEventListener("visibilitychange", () => {
    transport.send({
      type: "event", kind: "visibility", tabId,
      visible: document.visibilityState === "visible",
    });
  });
}

function encodeModifiers(e) {
  return (e.shiftKey ? 1 : 0) | (e.ctrlKey ? 2 : 0) | (e.altKey ? 4 : 0) | (e.metaKey ? 8 : 0);
}
function shouldPassThroughKey(e) {
  return e.key === "F5" || e.key === "F12" || (e.ctrlKey && (e.key === "r" || e.key === "R"));
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
      bootEl.classList.remove("hidden");
      bootEl.textContent = `Akiva crashed: ${msg.text}`;
      break;
    default:
      break;
  }
}

async function boot() {
  const { role, port } = await electLeader({
    channel: CHANNEL,
    lockName: LEADER_LOCK,
    spawnLeader: () => {
      const worker = new Worker(new URL("./worker.js", import.meta.url), { type: "module" });
      worker.postMessage({ type: "init", kernelUrl: KERNEL_URL, tabId });
      return worker;
    },
  });

  console.log(`[akiva] tab ${tabId.slice(0, 8)} — role=${role}`);

  const transport = makeTransport(role, port);
  transport.on(onKernelMessage);
  attachCanvas(transport, role, surfaceEl);
  forwardInput(transport, surfaceEl);
}

boot().catch((err) => {
  console.error("[akiva] boot failed", err);
  bootEl.textContent = `Akiva failed to boot: ${err?.message ?? err}`;
});
