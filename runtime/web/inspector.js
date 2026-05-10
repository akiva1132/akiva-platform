// Akiva inspector — JS-only window into what the SDK is rendering.
//
// Each frame the SDK publishes a flat semantic tree (see
// `sdk-rust/src/ui/semantics.rs`) of what the user sees: role,
// surface-local bounds, label, optional id. This module decodes that
// stream and parks the latest tree per `(packageId, handle)` in
// memory, then publishes the tree as `window.__akiva` so tests
// (Playwright via `page.evaluate`) and devtools sessions can ask
// "what's on screen right now?".
//
// Why no DOM projection. An earlier iteration mirrored the tree into
// hidden ARIA-decorated `<div>` elements so Playwright's `getByRole`
// queries would work. That coupled the OS to a browser-only fallback
// path Akiva will eventually run without (native shells, embedded
// surfaces) and ran a 60 Hz DOM rebuild on every paint. The pure JS
// object form is faster, has zero browser-DOM dependency, and forces
// the test infrastructure to go through the same protocol any other
// inspector consumer (devtools panel, screen reader bridge, AT-API)
// would.
//
// Off by default. The inspector only mounts when `?inspector=1` is in
// the URL (or `localStorage.akivaInspector === "1"` for sticky dev
// sessions). Production tabs receive the messages but ignore them, so
// a forgotten flag is a perf miss not a correctness bug.
//
// Wire decoder pinned to v1 of the SDK's encode (`SemanticTree::encode`).
// A version mismatch is logged at warn level and the message is dropped
// — easier to diagnose than a half-decoded tree.

/** @typedef {object} SemanticNode
 *  @property {number|null} parent  Index of parent in `nodes`, or null for root.
 *  @property {string}      role    "group" | "text" | "button" | "image" | "rect" | "spacer"
 *  @property {number}      x       Surface-local left, in canvas physical pixels.
 *  @property {number}      y       Surface-local top.
 *  @property {number}      w
 *  @property {number}      h
 *  @property {string|null} id      Stable widget id (AKML `id="..."` / `Ui::button(id, ...)`).
 *  @property {string|null} label   Visible text / accessible name.
 *  @property {string|null} value   Live state value (reserved).
 */

/** @typedef {object} InspectorMatch
 *  @property {string} packageId
 *  @property {number} handle
 *  @property {string} role
 *  @property {string|null} id
 *  @property {string|null} label
 *  @property {string|null} value
 *  @property {{x:number,y:number,w:number,h:number}} rect  Canvas-space bounds.
 */

const WIRE_VERSION = 1;

// Stable role table — byte values pinned in the SDK's `SemanticRole`
// enum. Translate to short strings so consumers can branch on names.
const ROLE_NAMES = ["group", "text", "button", "image", "rect", "spacer"];

/** True iff the inspector should mount on this page load. Honours
 *  both the URL flag (`?inspector=1`) and a sticky localStorage
 *  override so dev sessions can persist across reloads without
 *  retyping the query string. */
export function inspectorEnabled() {
  try {
    const params = new URLSearchParams(globalThis.location?.search ?? "");
    if (params.get("inspector") === "1") return true;
    if (params.get("inspector") === "0") return false;     // explicit disable beats sticky
    return globalThis.localStorage?.getItem("akivaInspector") === "1";
  } catch {
    // Sandboxed environments (some test runners) throw on URLSearchParams /
    // localStorage; treat as "off" rather than crashing the tab.
    return false;
  }
}

/** Decode `SemanticTree::encode` bytes (v1 wire format). Returns
 *  `{ version, nodes }` on success, `null` on a truncated/malformed
 *  blob — same loose contract as the SDK-side decoder. */
export function decodeTree(bytes) {
  if (!bytes || bytes.byteLength < 5) return null;
  const dv = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  let p = 0;
  const version = dv.getUint8(p); p += 1;
  if (version !== WIRE_VERSION) return null;
  const count = dv.getUint32(p, true); p += 4;
  const nodes = new Array(count);
  for (let i = 0; i < count; i++) {
    if (p + 22 > bytes.byteLength) return null;
    const parentI32 = dv.getInt32(p, true); p += 4;
    const roleByte  = dv.getUint8(p);       p += 1;
    const x = dv.getFloat32(p, true); p += 4;
    const y = dv.getFloat32(p, true); p += 4;
    const w = dv.getFloat32(p, true); p += 4;
    const h = dv.getFloat32(p, true); p += 4;
    const flags = dv.getUint8(p); p += 1;
    const role = ROLE_NAMES[roleByte];
    if (!role) return null;
    const hasId    = !!(flags & 0b001);
    const hasLabel = !!(flags & 0b010);
    const hasValue = !!(flags & 0b100);
    let id = null, label = null, value = null;
    if (hasId)    { const r = readStr(bytes, dv, p); if (!r) return null; id    = r.s; p = r.p; }
    if (hasLabel) { const r = readStr(bytes, dv, p); if (!r) return null; label = r.s; p = r.p; }
    if (hasValue) { const r = readStr(bytes, dv, p); if (!r) return null; value = r.s; p = r.p; }
    nodes[i] = {
      parent: parentI32 < 0 ? null : parentI32,
      role,
      x, y, w, h,
      id, label, value,
    };
  }
  return { version, nodes };
}

function readStr(bytes, dv, p) {
  if (p + 2 > bytes.byteLength) return null;
  const len = dv.getUint16(p, true);
  p += 2;
  if (p + len > bytes.byteLength) return null;
  const s = new TextDecoder("utf-8").decode(
    new Uint8Array(bytes.buffer, bytes.byteOffset + p, len),
  );
  return { s, p: p + len };
}

/** Per-page inspector. Holds the most recent decoded tree per
 *  `(packageId, handle)` and exposes a query API on `window.__akiva`.
 *  Activities that stop publishing age out after `staleMs`. */
export class Inspector {
  /**
   * @param {object} [opts]
   * @param {number} [opts.staleMs]  Subtrees not refreshed within this many ms are dropped. Default 2 000.
   * @param {object} [opts.global]   Global object to attach `__akiva` to. Defaults to `globalThis`.
   */
  constructor({ staleMs = 2000, global = globalThis } = {}) {
    this._staleMs = staleMs;
    this._global = global;
    /** @type {Map<string, { packageId: string, handle: number, nodes: SemanticNode[], lastSeenMs: number }>} */
    this._activities = new Map();
    this._gcTimer = setInterval(() => this._gcStale(), 1000);
    this._installApi();
  }

  /** Apply one frame's tree from `(packageId, handle)`. The previous
   *  tree for the same key is replaced wholesale — no diffing, no
   *  identity tracking across frames. The published JSON snapshot is
   *  always the latest frame, which is what tests expect: query
   *  immediately after an interaction, see the post-update state. */
  apply({ packageId, handle, tree }) {
    const decoded = decodeTree(tree);
    if (!decoded) {
      console.warn("[akiva-inspector] dropped malformed tree from", packageId, handle);
      return;
    }
    const key = `${packageId}#${handle}`;
    this._activities.set(key, {
      packageId, handle,
      nodes: decoded.nodes,
      lastSeenMs: performance.now(),
    });
  }

  _gcStale() {
    const now = performance.now();
    for (const [key, entry] of this._activities) {
      if (now - entry.lastSeenMs > this._staleMs) {
        this._activities.delete(key);
      }
    }
  }

  /** Tear down — used in tests. Production tabs let the page unload
   *  GC handle the cleanup. */
  destroy() {
    clearInterval(this._gcTimer);
    this._activities.clear();
    if (this._global && this._global.__akiva === this._api) {
      try { delete this._global.__akiva; } catch { this._global.__akiva = undefined; }
    }
  }

  _installApi() {
    const self = this;
    /** @type {object} */
    this._api = {
      /** Snapshot every active activity's full node list. Returned
       *  objects are plain copies — mutating them does not affect the
       *  inspector. */
      snapshot() {
        return Array.from(self._activities.values()).map((a) => ({
          packageId: a.packageId,
          handle: a.handle,
          lastSeenMs: a.lastSeenMs,
          nodes: a.nodes.map((n) => ({ ...n })),
        }));
      },
      /** Lightweight directory for debugging — packageId + handle +
       *  count, no node payload. */
      list() {
        return Array.from(self._activities.values()).map((a) => ({
          packageId: a.packageId,
          handle: a.handle,
          nodeCount: a.nodes.length,
          lastSeenMs: a.lastSeenMs,
        }));
      },
      /** Find first node across all activities whose id matches.
       *  Returns null when no widget by that id has published yet. */
      findById(id) { return self._findFirst((n) => n.id === id); },
      /** Find first node with the given role; if `name` is given, the
       *  node's `label` must match. Default match is exact equality;
       *  pass `{ exact: false }` for substring match. */
      findByRole(role, opts = {}) {
        return self._findFirst((n) =>
          n.role === role &&
          (opts.name === undefined || matchesName(n.label, opts.name, opts.exact ?? true))
        );
      },
      /** Find first text-role node whose visible text matches. */
      findByText(text, opts = {}) {
        return self._findFirst((n) =>
          n.role === "text" && matchesName(n.label, text, opts.exact ?? true)
        );
      },
      /** Return every match for a structured query. */
      queryAll(q = {}) { return self._findAll((n) => matchesQuery(n, q)); },
    };
    if (this._global) this._global.__akiva = this._api;
  }

  _findFirst(pred) {
    for (const a of this._activities.values()) {
      for (const n of a.nodes) {
        if (pred(n)) return makeMatch(a, n);
      }
    }
    return null;
  }

  _findAll(pred) {
    const out = [];
    for (const a of this._activities.values()) {
      for (const n of a.nodes) {
        if (pred(n)) out.push(makeMatch(a, n));
      }
    }
    return out;
  }
}

function makeMatch(a, n) {
  return {
    packageId: a.packageId,
    handle: a.handle,
    role: n.role,
    id: n.id,
    label: n.label,
    value: n.value,
    rect: { x: n.x, y: n.y, w: n.w, h: n.h },
  };
}

function matchesName(actual, expected, exact) {
  if (actual === null || actual === undefined) return false;
  return exact ? actual === expected : actual.includes(expected);
}

function matchesQuery(n, q) {
  if (q.role !== undefined && n.role !== q.role) return false;
  if (q.id !== undefined && n.id !== q.id) return false;
  const exact = q.exact ?? true;
  if (q.label !== undefined && !matchesName(n.label, q.label, exact)) return false;
  if (q.text !== undefined && (n.role !== "text" || !matchesName(n.label, q.text, exact))) return false;
  return true;
}
