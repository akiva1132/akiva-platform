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
// Wire decoder pinned to v3 of the SDK's encode (`SemanticTree::encode`).
// History:
//   v1 — base format (parent / role / bounds / flags + id/label/value).
//   v2 — added optional 28-byte `scroll` payload, gated by flag bit 3.
//   v3 — `scroll` payload grew to 29 bytes with a trailing axis enum
//        (0 = Vertical, 1 = Horizontal).
// The decoder reads v2 and v3 cleanly; v2 input defaults axis to
// "vertical". A version mismatch is logged at warn level and the
// message dropped — easier to diagnose than a half-decoded tree.

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

const WIRE_VERSION = 3;
// Single-byte enum from the SDK's `ScrollAxis` repr — must stay in
// sync with `sdk-rust/src/ui/mod.rs::ScrollAxis`.
const SCROLL_AXIS_NAMES = ["vertical", "horizontal"];

// Stable role table — byte values pinned in the SDK's `SemanticRole`
// enum. Translate to short strings so consumers can branch on names.
const ROLE_NAMES = ["group", "text", "button", "image", "rect", "spacer", "scroll"];

// `0xFFFFFFFF` is the SDK's sentinel for "this index field is null"
// (plain `Node::Scroll` populates first_visible / last_visible /
// item_count as `None`; `Node::LazyColumn` populates them).
const SCROLL_NULL_U32 = 0xFFFFFFFF;

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
  // Accept v1 (legacy: no scroll flag), v2 (28-byte scroll payload
  // without axis byte — back-compat path for rolling upgrades),
  // and v3 (current: 29-byte scroll payload with axis trailing byte).
  if (version !== 1 && version !== 2 && version !== WIRE_VERSION) return null;
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
    const hasId     = !!(flags & 0b0001);
    const hasLabel  = !!(flags & 0b0010);
    const hasValue  = !!(flags & 0b0100);
    const hasScroll = !!(flags & 0b1000);
    let id = null, label = null, value = null, scroll = null;
    if (hasId)    { const r = readStr(bytes, dv, p); if (!r) return null; id    = r.s; p = r.p; }
    if (hasLabel) { const r = readStr(bytes, dv, p); if (!r) return null; label = r.s; p = r.p; }
    if (hasValue) { const r = readStr(bytes, dv, p); if (!r) return null; value = r.s; p = r.p; }
    if (hasScroll) {
      if (p + 28 > bytes.byteLength) return null;
      const scrollY    = dv.getFloat32(p, true); p += 4;
      const contentH   = dv.getFloat32(p, true); p += 4;
      const viewportH  = dv.getFloat32(p, true); p += 4;
      const viewportW  = dv.getFloat32(p, true); p += 4;
      const firstU32   = dv.getUint32(p, true);  p += 4;
      const lastU32    = dv.getUint32(p, true);  p += 4;
      const countU32   = dv.getUint32(p, true);  p += 4;
      // v3 carries an axis enum byte; v2 input has no byte here so we
      // default to "vertical" (the pre-2d behaviour every v2 emitter
      // produced). Reject unknown axis values rather than silently
      // mapping to a default — a decoder that lies hides bugs.
      let axis = "vertical";
      if (version >= 3) {
        if (p + 1 > bytes.byteLength) return null;
        const axisByte = dv.getUint8(p); p += 1;
        const name = SCROLL_AXIS_NAMES[axisByte];
        if (!name) return null;
        axis = name;
      }
      const nullable = (v) => (v === SCROLL_NULL_U32 ? null : v);
      scroll = {
        scrollY,
        contentHeight: contentH,
        viewportHeight: viewportH,
        viewportWidth: viewportW,
        firstVisibleIndex: nullable(firstU32),
        lastVisibleIndex:  nullable(lastU32),
        itemCount:         nullable(countU32),
        axis,
      };
    }
    nodes[i] = {
      parent: parentI32 < 0 ? null : parentI32,
      role,
      x, y, w, h,
      id, label, value, scroll,
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
    /** Per-handle composite transform — last value posted by the
     *  framework's compositor. Applied to every rect returned from
     *  the query API so tests don't see surface-local coords during
     *  animations. Identity transform = no entry (cheaper than
     *  storing IDENTITY for every handle).
     *  @type {Map<number, {opacity:number, tx:number, ty:number, scale:number}>} */
    this._transforms = new Map();
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
    const prev = this._activities.get(key);
    this._activities.set(key, {
      packageId, handle,
      nodes: decoded.nodes,
      lastSeenMs: performance.now(),
      // Monotonic per-activity frame counter. Tests use this to wait
      // for "the activity has at least re-rendered once since I tapped"
      // without an arbitrary `waitForTimeout`. Rolls over at 2^53;
      // practically never during a session.
      frame: (prev?.frame ?? 0) + 1,
    });
  }

  /** Apply a composite transform from the framework's compositor pass.
   *  See `framework/src/compositor.rs::PaintOp::SetInspectorTransform`.
   *  We always store something, even at identity, so tests can
   *  distinguish "framework has reported a transform" (entry exists,
   *  may be identity) from "no entry yet, may still be transitioning"
   *  (used by `waitForStableLayout` to gate on the post-launch settle).
   *  The fast path in `makeMatch` checks for identity and skips the
   *  add — the storage cost is one Map entry per active activity. */
  applyTransform({ handle, opacity, translate_x, translate_y, scale }) {
    this._transforms.set(handle, {
      opacity, tx: translate_x, ty: translate_y, scale,
    });
  }

  _gcStale() {
    const now = performance.now();
    for (const [key, entry] of this._activities) {
      if (now - entry.lastSeenMs > this._staleMs) {
        this._activities.delete(key);
        // Drop the transform too — without an active activity it has
        // no consumer, and a stale value would offset a future
        // re-spawn that gets the same handle.
        this._transforms.delete(entry.handle);
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
          frame: a.frame,
        }));
      },
      /** Per-activity monotonic frame counter — used by tests to gate
       *  on "the activity has re-rendered since I tapped" without
       *  an arbitrary timeout. Returns 0 for unknown handles so
       *  test code can take `prev = frameOf(h)` then poll
       *  `frameOf(h) > prev` even before the activity has published
       *  its first frame. */
      frameOf(handle) {
        for (const a of self._activities.values()) {
          if (a.handle === handle) return a.frame;
        }
        return 0;
      },
      /** Per-activity composite transform — `null` if the framework
       *  has not yet reported any transform for `handle` (early in
       *  the activity's lifecycle), otherwise the latest values.
       *  `waitForStableLayout` distinguishes "no transform reported"
       *  from "reported and identity" so it doesn't false-positive
       *  during the gap between the activity's first
       *  `PUBLISH_SEMANTICS` and the framework's first
       *  `SetInspectorTransform`. */
      transformOf(handle) {
        const t = self._transforms.get(handle);
        return t ? { ...t } : null;
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
      /** Stage 2c.6 — scroll-region introspection. Returns a snapshot
       *  of the named scroll region's persistent state, or null when
       *  no `Node::Scroll` / `Node::LazyColumn` with that key has
       *  published in the most recent frame. Useful for tests:
       *    await page.evaluate(() => __akiva.scrollOf("feed").scrollY)
       *  Plain `Node::Scroll` has null firstVisibleIndex /
       *  lastVisibleIndex / itemCount; `Node::LazyColumn` populates
       *  the index range so a test can assert "visible window
       *  advanced by 5 rows after scroll". */
      scrollOf(key) {
        const node = self._findFirst((n) => n.role === "scroll" && n.id === key);
        if (!node || !node.scroll) return null;
        return {
          key,
          scrollY: node.scroll.scrollY,
          contentHeight: node.scroll.contentHeight,
          viewportHeight: node.scroll.viewportHeight,
          viewportWidth: node.scroll.viewportWidth,
          firstVisibleIndex: node.scroll.firstVisibleIndex,
          lastVisibleIndex:  node.scroll.lastVisibleIndex,
          itemCount: node.scroll.itemCount,
          // v2 inputs default to "vertical" (see decodeTree); v3+
          // carries the real axis. Carousel tests read this.
          axis: node.scroll.axis ?? "vertical",
          rect: { x: node.rect.x, y: node.rect.y, w: node.rect.w, h: node.rect.h },
        };
      },
    };
    if (this._global) this._global.__akiva = this._api;
  }

  _findFirst(pred) {
    for (const a of this._activities.values()) {
      for (const n of a.nodes) {
        if (pred(n)) return makeMatch(a, n, this._transforms.get(a.handle));
      }
    }
    return null;
  }

  _findAll(pred) {
    const out = [];
    for (const a of this._activities.values()) {
      for (const n of a.nodes) {
        if (pred(n)) out.push(makeMatch(a, n, this._transforms.get(a.handle)));
      }
    }
    return out;
  }
}

function makeMatch(a, n, transform) {
  // Apply the activity's current composite transform (if any) so the
  // rect is in post-composite screen coordinates. The SDK publishes
  // surface-local bounds; the compositor applies translate/scale
  // during the screen draw. Without this, mid-animation queries
  // would hand tests stale coords. Order matches the GPU shader's:
  // scale around the surface origin, then translate.
  let { x, y, w, h } = n;
  // Identity transforms get a fast path — `applyTransform` always
  // stores something so the wait helpers can distinguish "framework
  // hasn't reported yet" from "reported and identity", but the apply
  // cost is bounded.
  if (transform && (
    transform.tx !== 0 || transform.ty !== 0 ||
    transform.scale !== 1 || transform.opacity !== 1
  )) {
    x = x * transform.scale + transform.tx;
    y = y * transform.scale + transform.ty;
    w = w * transform.scale;
    h = h * transform.scale;
  }
  return {
    packageId: a.packageId,
    handle: a.handle,
    role: n.role,
    id: n.id,
    label: n.label,
    value: n.value,
    rect: { x, y, w, h },
    // Stage 2c.6 — scroll metadata threaded straight through so
    // `__akiva.scrollOf(key)` can read it off a match without a
    // second lookup. `null` for non-scroll roles, populated for
    // SemanticRole::Scroll (both plain Scroll and LazyColumn).
    scroll: n.scroll ?? null,
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
