// Leader election + transport session.
//
// A single `Session` instance per tab coordinates with all other tabs of
// the same origin. It handles:
//
//   - Initial election: race the Web Lock while probing the broadcast
//     channel for an existing leader.
//   - Steady-state transport: leader talks directly to its worker;
//     follower talks to the leader over the broadcast channel.
//   - Failover: if the current leader dies, its Web Lock is released by
//     the browser. Every follower's pending lock request is queued FIFO;
//     the next one in line is promoted to leader automatically.
//
// See docs/runtime-web.md for the full protocol and the failover
// walkthrough.
//
// Events emitted on the Session (as CustomEvent):
//   - "role":    detail = { role: "leader"|"follower", promoted: boolean }
//   - "message": detail = the kernel-side message payload
//   - "panic":   detail = { text: string }  (leader crashed irrecoverably)

const PROBE_TIMEOUT_MS = 150;
const LOCK_WAIT_SOFT_MS = 25;

export class Session extends EventTarget {
  /**
   * @param {{
   *   channel: string,     // BroadcastChannel name
   *   lockName: string,    // Web Lock name
   *   tabId: string,       // stable ID of this tab
   *   kernelUrl: string,   // URL of akiva-kernel.wasm
   *   workerUrl: URL,      // URL of worker.js
   * }} opts
   */
  constructor(opts) {
    super();
    this.opts = opts;
    this.role = "electing";           // "electing" | "follower" | "leader"
    this.worker = null;
    this.bc = new BroadcastChannel(opts.channel);
    this._onLockAcquired = null;
    this._boundBcListener = this._onBroadcast.bind(this);
    this.bc.addEventListener("message", this._boundBcListener);
  }

  /** Kick off election. Resolves once we have a role (leader or follower). */
  async start() {
    // Fire the lock request in the background. The callback fires the
    // moment the lock is available — either immediately (no prior leader)
    // or later (after a prior leader's tab closes).
    const lockAcquired = new Promise((resolve) => { this._onLockAcquired = resolve; });
    navigator.locks.request(this.opts.lockName, { mode: "exclusive" }, async () => {
      this._becomeLeader();
      this._onLockAcquired?.();
      // Hold the lock for the lifetime of this tab. The browser releases
      // it on tab close, unblocking the next follower in line.
      await new Promise(() => {});
    }).catch((err) => {
      this._emit("panic", { text: `lock request failed: ${err?.message ?? err}` });
    });

    // Race: did we get the lock fast (meaning no prior leader)?
    const gotLockFast = await Promise.race([
      lockAcquired.then(() => true),
      delay(LOCK_WAIT_SOFT_MS).then(() => false),
    ]);
    if (gotLockFast) return;

    // Slow lock → someone else is leader. Confirm via probe.
    const existingLeader = await this._probeForLeader();
    if (existingLeader) {
      this._becomeFollower();
      return;
    }

    // No probe reply and no lock yet. Two legitimate cases:
    //   1. Prior leader died mid-boot; our lock will be granted soon.
    //   2. Prior leader is slow / throttled. Either way, wait it out.
    await lockAcquired;
  }

  /** Send a message toward the kernel. Behaviour depends on role. */
  send(message, transferList = []) {
    if (this.role === "leader") {
      this.worker.postMessage(message, transferList);
    } else if (this.role === "follower") {
      // BroadcastChannel cannot transfer objects. This is a leaky
      // abstraction on purpose — callers that need zero-copy transfer
      // (OffscreenCanvas) must be leader before calling. See tab.js for
      // how we defer canvas transfer until after promotion.
      if (transferList.length) {
        console.warn("[session] follower send dropped transferList:", transferList);
      }
      this.bc.postMessage({ type: "follower_msg", payload: message });
    } else {
      console.warn("[session] send() called before role assigned");
    }
  }

  /** Teardown. Idempotent. */
  dispose() {
    this.bc.removeEventListener("message", this._boundBcListener);
    this.bc.close();
    if (this.worker) {
      try { this.worker.terminate(); } catch {}
      this.worker = null;
    }
  }

  // ── Internals ────────────────────────────────────────────────────────

  _becomeFollower() {
    if (this.role !== "electing") return;
    this.role = "follower";
    this._emit("role", { role: "follower", promoted: false });
  }

  _becomeLeader() {
    const promoted = this.role === "follower";
    this.role = "leader";

    this.worker = new Worker(this.opts.workerUrl, { type: "module" });
    this.worker.postMessage({
      type: "init",
      kernelUrl: this.opts.kernelUrl,
      tabId: this.opts.tabId,
    });
    this.worker.addEventListener("message", (e) => {
      // Deliver locally.
      this._emit("message", e.data);
      // Relay to followers. BroadcastChannel is our one-to-many fanout.
      this.bc.postMessage({ type: "leader_msg", payload: e.data });
    });

    this._emit("role", { role: "leader", promoted });
  }

  _onBroadcast(e) {
    const msg = e.data;
    if (!msg || typeof msg !== "object") return;

    if (this.role === "leader") {
      if (msg.type === "probe") {
        // New tab is probing. Confirm we're alive.
        this.bc.postMessage({ type: "leader_here", probeId: msg.probeId });
      } else if (msg.type === "follower_msg") {
        this.worker?.postMessage(msg.payload);
      }
      return;
    }

    // Follower: consume relayed leader messages.
    if (msg.type === "leader_msg") {
      this._emit("message", msg.payload);
    }
    // Probe answers are handled inline in _probeForLeader; we don't care here.
  }

  _probeForLeader() {
    return new Promise((resolve) => {
      const probeId = crypto.randomUUID();
      const timeout = setTimeout(() => {
        this.bc.removeEventListener("message", onMsg);
        resolve(false);
      }, PROBE_TIMEOUT_MS);

      const onMsg = (e) => {
        if (e.data?.type === "leader_here" && e.data.probeId === probeId) {
          clearTimeout(timeout);
          this.bc.removeEventListener("message", onMsg);
          resolve(true);
        }
      };
      this.bc.addEventListener("message", onMsg);
      this.bc.postMessage({ type: "probe", probeId });
    });
  }

  _emit(name, detail) {
    this.dispatchEvent(new CustomEvent(name, { detail }));
  }
}

function delay(ms) { return new Promise((r) => setTimeout(r, ms)); }
