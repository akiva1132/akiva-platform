// Leader election.
//
// Pattern: Web Locks for primacy + BroadcastChannel for discovery and
// follower↔leader messaging. ARCHITECTURE.md §9 walks through why this
// beats SharedWorker (Safari support, lifetime guarantees).
//
//   1. Every tab requests `navigator.locks.request(LEADER_LOCK, exclusive)`.
//      The winner becomes leader — its callback stays active until the
//      tab closes, at which point the lock is released and a follower
//      takes over automatically.
//   2. On tab startup, a short probe on the broadcast channel discovers
//      whether there already is a leader. Followers then use the same
//      channel as their transport.
//
// Phase 2 upgrades follower↔leader transport from BroadcastChannel to
// per-tab MessagePorts. BroadcastChannel cannot transfer ports, so the
// upgrade path goes: leader posts a Worker-hosted MessagePort URL that
// the follower opens via `SharedWorker` (feature-detected), or the
// follower negotiates a dedicated ReadableStream pair.

const PROBE_TIMEOUT_MS = 120;

/**
 * @param {{
 *   channel: string,
 *   lockName: string,
 *   spawnLeader: () => Worker,
 * }} opts
 * @returns {Promise<{
 *   role: "leader"|"follower",
 *   port: Worker|BroadcastChannel
 * }>}
 */
export async function electLeader(opts) {
  const bc = new BroadcastChannel(opts.channel);

  // 1. Probe: is there already a leader?
  const leaderReady = await probeForLeader(bc);
  if (leaderReady) {
    // We're a follower; use the broadcast channel as our transport.
    return { role: "follower", port: bc };
  }

  // 2. Race for the lock. Whoever wins becomes leader.
  return await raceForLock(opts, bc);
}

function probeForLeader(bc) {
  return new Promise((resolve) => {
    const probeId = crypto.randomUUID();
    const timeout = setTimeout(() => {
      bc.removeEventListener("message", onMsg);
      resolve(false);
    }, PROBE_TIMEOUT_MS);

    const onMsg = (e) => {
      if (e.data?.type === "leader_here" && e.data.probeId === probeId) {
        clearTimeout(timeout);
        bc.removeEventListener("message", onMsg);
        resolve(true);
      }
    };
    bc.addEventListener("message", onMsg);
    bc.postMessage({ type: "probe", probeId });
  });
}

function raceForLock(opts, bc) {
  return new Promise((resolve, reject) => {
    navigator.locks.request(opts.lockName, { mode: "exclusive" }, async (lock) => {
      if (!lock) { reject(new Error("leader lock unavailable")); return; }

      const worker = opts.spawnLeader();

      // Answer future probes so newly-opened tabs find us.
      bc.addEventListener("message", (e) => {
        if (e.data?.type === "probe") {
          bc.postMessage({ type: "leader_here", probeId: e.data.probeId });
        } else if (e.data?.type === "follower_msg") {
          // Forward follower messages into the worker.
          worker.postMessage(e.data.payload);
        }
      });

      // Forward worker-originated messages out to followers.
      worker.addEventListener("message", (wev) => {
        bc.postMessage({ type: "leader_msg", payload: wev.data });
      });

      resolve({ role: "leader", port: worker });

      // Hold the lock for the lifetime of this tab. We never resolve;
      // when the tab closes the lock is released and a follower takes over.
      await new Promise(() => {});
    }).catch(reject);
  });
}
