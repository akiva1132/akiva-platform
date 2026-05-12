// @vitest-environment happy-dom
//
// Inspector — decoder + window.__akiva query API.
//
// The wire format is owned by `sdk-rust/src/ui/semantics.rs` (v1).
// This file pins the JS decoder against hand-built byte streams that
// match the SDK's encode output, then verifies the inspector exposes a
// query surface that maps the latest published tree to lookups by id /
// role / text. Tests downstream (Playwright e2e) drive the calculator
// through this same API.
//
// Scope is JS only — Rust-side encode/decode round-trips are pinned in
// the SDK's own `#[cfg(test)]` block. The thing this file plugs is
// "SDK bytes → queryable in-memory tree": if the decoder loses a field
// or the inspector's match logic regresses, e2e tests downstream
// silently fail to find their widgets, with no clear pointer to the
// cause.

import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import { decodeTree, Inspector, inspectorEnabled } from "../inspector.js";

/** Build the v1 wire format by hand for one node (root, no parent).
 *  Mirrors the SDK's `SemanticTree::encode` closely enough that the
 *  same byte stream re-encoded would match. */
function encodeNode({
  parent = -1, role, x, y, w, h,
  id = null, label = null, value = null,
}) {
  const parts = [];
  const pBuf = new ArrayBuffer(4);
  new DataView(pBuf).setInt32(0, parent, true);
  parts.push(new Uint8Array(pBuf));
  parts.push(new Uint8Array([role]));
  const rectBuf = new ArrayBuffer(16);
  const rectDv = new DataView(rectBuf);
  rectDv.setFloat32(0, x, true);
  rectDv.setFloat32(4, y, true);
  rectDv.setFloat32(8, w, true);
  rectDv.setFloat32(12, h, true);
  parts.push(new Uint8Array(rectBuf));
  let flags = 0;
  if (id !== null)    flags |= 0b001;
  if (label !== null) flags |= 0b010;
  if (value !== null) flags |= 0b100;
  parts.push(new Uint8Array([flags]));
  for (const s of [id, label, value]) {
    if (s === null) continue;
    const b = new TextEncoder().encode(s);
    const lenBuf = new ArrayBuffer(2);
    new DataView(lenBuf).setUint16(0, b.byteLength, true);
    parts.push(new Uint8Array(lenBuf));
    parts.push(b);
  }
  const total = parts.reduce((n, p) => n + p.byteLength, 0);
  const out = new Uint8Array(total);
  let off = 0;
  for (const p of parts) { out.set(p, off); off += p.byteLength; }
  return out;
}

/** Wrap one or more `encodeNode` outputs into a full v1 tree blob. */
function encodeTree(nodes) {
  const header = new Uint8Array(5);
  header[0] = 1;
  new DataView(header.buffer).setUint32(1, nodes.length, true);
  const total = 5 + nodes.reduce((n, b) => n + b.byteLength, 0);
  const out = new Uint8Array(total);
  out.set(header, 0);
  let off = 5;
  for (const b of nodes) { out.set(b, off); off += b.byteLength; }
  return out;
}

/** Stage 2c.6 — encode one node with the optional scroll payload (v2).
 *  `scroll` has shape `{scrollY, contentH, viewportH, viewportW,
 *  firstVisible|null, lastVisible|null, itemCount|null}`. */
function encodeNodeV2({ parent = -1, role, x, y, w, h, id = null, scroll }) {
  const parts = [];
  const pBuf = new ArrayBuffer(4);
  new DataView(pBuf).setInt32(0, parent, true);
  parts.push(new Uint8Array(pBuf));
  parts.push(new Uint8Array([role]));
  const rectBuf = new ArrayBuffer(16);
  const rectDv = new DataView(rectBuf);
  rectDv.setFloat32(0, x, true);
  rectDv.setFloat32(4, y, true);
  rectDv.setFloat32(8, w, true);
  rectDv.setFloat32(12, h, true);
  parts.push(new Uint8Array(rectBuf));
  let flags = 0;
  if (id !== null) flags |= 0b0001;
  if (scroll)      flags |= 0b1000;
  parts.push(new Uint8Array([flags]));
  if (id !== null) {
    const b = new TextEncoder().encode(id);
    const lenBuf = new ArrayBuffer(2);
    new DataView(lenBuf).setUint16(0, b.byteLength, true);
    parts.push(new Uint8Array(lenBuf));
    parts.push(b);
  }
  if (scroll) {
    const sBuf = new ArrayBuffer(28);
    const sDv = new DataView(sBuf);
    sDv.setFloat32(0,  scroll.scrollY,    true);
    sDv.setFloat32(4,  scroll.contentH,   true);
    sDv.setFloat32(8,  scroll.viewportH,  true);
    sDv.setFloat32(12, scroll.viewportW,  true);
    sDv.setUint32(16,  scroll.firstVisible ?? 0xFFFFFFFF, true);
    sDv.setUint32(20,  scroll.lastVisible  ?? 0xFFFFFFFF, true);
    sDv.setUint32(24,  scroll.itemCount    ?? 0xFFFFFFFF, true);
    parts.push(new Uint8Array(sBuf));
  }
  const total = parts.reduce((n, p) => n + p.byteLength, 0);
  const out = new Uint8Array(total);
  let off = 0;
  for (const p of parts) { out.set(p, off); off += p.byteLength; }
  return out;
}

function encodeTreeV2(nodes) {
  const header = new Uint8Array(5);
  header[0] = 2;
  new DataView(header.buffer).setUint32(1, nodes.length, true);
  const total = 5 + nodes.reduce((n, b) => n + b.byteLength, 0);
  const out = new Uint8Array(total);
  out.set(header, 0);
  let off = 5;
  for (const b of nodes) { out.set(b, off); off += b.byteLength; }
  return out;
}

/** Stage 2d.4 — encode one node with the v3 scroll payload (29 bytes
 *  with a trailing axis enum). `axis` is the raw byte (0=vertical,
 *  1=horizontal, anything else is the test triggering invalid-axis
 *  rejection). */
function encodeNodeV3({ parent = -1, role, x, y, w, h, id = null, scroll, axis = 0 }) {
  const parts = [];
  const pBuf = new ArrayBuffer(4);
  new DataView(pBuf).setInt32(0, parent, true);
  parts.push(new Uint8Array(pBuf));
  parts.push(new Uint8Array([role]));
  const rectBuf = new ArrayBuffer(16);
  const rectDv = new DataView(rectBuf);
  rectDv.setFloat32(0, x, true);
  rectDv.setFloat32(4, y, true);
  rectDv.setFloat32(8, w, true);
  rectDv.setFloat32(12, h, true);
  parts.push(new Uint8Array(rectBuf));
  let flags = 0;
  if (id !== null) flags |= 0b0001;
  if (scroll)      flags |= 0b1000;
  parts.push(new Uint8Array([flags]));
  if (id !== null) {
    const b = new TextEncoder().encode(id);
    const lenBuf = new ArrayBuffer(2);
    new DataView(lenBuf).setUint16(0, b.byteLength, true);
    parts.push(new Uint8Array(lenBuf));
    parts.push(b);
  }
  if (scroll) {
    const sBuf = new ArrayBuffer(28);
    const sDv = new DataView(sBuf);
    sDv.setFloat32(0,  scroll.scrollY,    true);
    sDv.setFloat32(4,  scroll.contentH,   true);
    sDv.setFloat32(8,  scroll.viewportH,  true);
    sDv.setFloat32(12, scroll.viewportW,  true);
    sDv.setUint32(16,  scroll.firstVisible ?? 0xFFFFFFFF, true);
    sDv.setUint32(20,  scroll.lastVisible  ?? 0xFFFFFFFF, true);
    sDv.setUint32(24,  scroll.itemCount    ?? 0xFFFFFFFF, true);
    parts.push(new Uint8Array(sBuf));
    parts.push(new Uint8Array([axis]));
  }
  const total = parts.reduce((n, p) => n + p.byteLength, 0);
  const out = new Uint8Array(total);
  let off = 0;
  for (const p of parts) { out.set(p, off); off += p.byteLength; }
  return out;
}

function encodeTreeV3(nodes) {
  const header = new Uint8Array(5);
  header[0] = 3;
  new DataView(header.buffer).setUint32(1, nodes.length, true);
  const total = 5 + nodes.reduce((n, b) => n + b.byteLength, 0);
  const out = new Uint8Array(total);
  out.set(header, 0);
  let off = 5;
  for (const b of nodes) { out.set(b, off); off += b.byteLength; }
  return out;
}

describe("decodeTree", () => {
  it("returns null on empty/short input", () => {
    expect(decodeTree(new Uint8Array(0))).toBeNull();
    expect(decodeTree(new Uint8Array([1, 0, 0, 0]))).toBeNull();
  });

  it("rejects unknown wire version", () => {
    const bytes = new Uint8Array(5);
    bytes[0] = 99;
    expect(decodeTree(bytes)).toBeNull();
  });

  it("decodes an empty tree (count=0)", () => {
    const bytes = encodeTree([]);
    expect(decodeTree(bytes)).toEqual({ version: 1, nodes: [] });
  });

  it("decodes a single root button with id + label", () => {
    const node = encodeNode({
      role: 2, x: 10, y: 20, w: 100, h: 40,
      id: "ok", label: "OK",
    });
    const out = decodeTree(encodeTree([node]));
    expect(out.nodes[0]).toEqual({
      parent: null, role: "button",
      x: 10, y: 20, w: 100, h: 40,
      id: "ok", label: "OK", value: null, scroll: null,
    });
  });

  it("decodes a nested tree with parent pointers", () => {
    const root  = encodeNode({ parent: -1, role: 0, x: 0, y: 0, w: 800, h: 600 });
    const text  = encodeNode({ parent: 0,  role: 1, x: 16, y: 16, w: 200, h: 24, label: "Hello" });
    const btn   = encodeNode({ parent: 0,  role: 2, x: 16, y: 60, w: 80,  h: 36, id: "go", label: "Go" });
    const out = decodeTree(encodeTree([root, text, btn]));
    expect(out.nodes).toHaveLength(3);
    expect(out.nodes[0].role).toBe("group");
    expect(out.nodes[0].parent).toBeNull();
    expect(out.nodes[1]).toMatchObject({ parent: 0, role: "text", label: "Hello" });
    expect(out.nodes[2]).toMatchObject({ parent: 0, role: "button", id: "go", label: "Go" });
  });

  it("returns null on truncated string payload", () => {
    const buf = new Uint8Array(5 + 4 + 1 + 16 + 1 + 2 + 4);
    buf[0] = 1;
    new DataView(buf.buffer).setUint32(1, 1, true);
    new DataView(buf.buffer).setInt32(5, -1, true);
    buf[9] = 1;                                             // role=text
    buf[26] = 0b010;                                        // flags: has_label
    new DataView(buf.buffer).setUint16(27, 100, true);     // claims 100 bytes, supplies 4
    expect(decodeTree(buf)).toBeNull();
  });

  it("rejects unknown role byte", () => {
    const bad = encodeNode({ role: 99, x: 0, y: 0, w: 0, h: 0 });
    expect(decodeTree(encodeTree([bad]))).toBeNull();
  });

  it("decodes a v2 scroll node with full metadata (LazyColumn shape)", () => {
    const node = encodeNodeV2({
      role: 6,
      x: 0, y: 0, w: 200, h: 400,
      id: "feed",
      scroll: {
        scrollY: 312.5, contentH: 8400, viewportH: 400, viewportW: 200,
        firstVisible: 8, lastVisible: 17, itemCount: 80,
      },
    });
    const out = decodeTree(encodeTreeV2([node]));
    expect(out.version).toBe(2);
    // v2 input → axis defaults to "vertical" (the only orientation
    // the wire could express pre-Stage 2d).
    expect(out.nodes[0]).toEqual({
      parent: null, role: "scroll",
      x: 0, y: 0, w: 200, h: 400,
      id: "feed", label: null, value: null,
      scroll: {
        scrollY: 312.5, contentHeight: 8400, viewportHeight: 400, viewportWidth: 200,
        firstVisibleIndex: 8, lastVisibleIndex: 17, itemCount: 80,
        axis: "vertical",
      },
    });
  });

  it("decodes a v2 scroll node with null index fields (plain Scroll shape)", () => {
    const node = encodeNodeV2({
      role: 6,
      x: 0, y: 0, w: 200, h: 400,
      id: "body",
      scroll: {
        scrollY: 50, contentH: 1000, viewportH: 400, viewportW: 200,
        // first/last/count null = 0xFFFFFFFF on the wire
      },
    });
    const out = decodeTree(encodeTreeV2([node]));
    const s = out.nodes[0].scroll;
    expect(s.firstVisibleIndex).toBeNull();
    expect(s.lastVisibleIndex).toBeNull();
    expect(s.itemCount).toBeNull();
    expect(s.axis).toBe("vertical");
  });

  it("rejects v2 node with scroll flag but no payload", () => {
    const buf = new Uint8Array(5 + 4 + 1 + 16 + 1);
    buf[0] = 2;
    new DataView(buf.buffer).setUint32(1, 1, true);
    new DataView(buf.buffer).setInt32(5, -1, true);
    buf[9] = 6; // Scroll role
    buf[26] = 0b1000;  // flag claims scroll payload follows…
    // …but no 28 bytes follow.
    expect(decodeTree(buf)).toBeNull();
  });

  it("decodes a v3 horizontal scroll node (LazyRow shape)", () => {
    // Stage 2d.4 — the 29th byte carries axis (0=vertical, 1=horizontal).
    const node = encodeNodeV3({
      role: 6,
      x: 0, y: 0, w: 400, h: 200,
      id: "carousel",
      scroll: {
        scrollY: 800, contentH: 4800, viewportH: 400, viewportW: 200,
        firstVisible: 2, lastVisible: 4, itemCount: 12,
      },
      axis: 1, // horizontal
    });
    const out = decodeTree(encodeTreeV3([node]));
    expect(out.version).toBe(3);
    expect(out.nodes[0].scroll.axis).toBe("horizontal");
    expect(out.nodes[0].scroll.scrollY).toBe(800);
    expect(out.nodes[0].scroll.itemCount).toBe(12);
  });

  it("rejects v3 node missing the axis byte", () => {
    // v3 header + scroll flag + 28 of the 29 payload bytes —
    // trailing axis byte missing, decoder must refuse.
    const buf = new Uint8Array(5 + 4 + 1 + 16 + 1 + 28);
    buf[0] = 3;
    new DataView(buf.buffer).setUint32(1, 1, true);
    new DataView(buf.buffer).setInt32(5, -1, true);
    buf[9] = 6; // Scroll role
    buf[26] = 0b1000;
    // 28 zero bytes follow; no axis byte.
    expect(decodeTree(buf)).toBeNull();
  });

  it("rejects v3 node with invalid axis byte", () => {
    const node = encodeNodeV3({
      role: 6,
      x: 0, y: 0, w: 200, h: 400,
      id: "x",
      scroll: { scrollY: 0, contentH: 0, viewportH: 0, viewportW: 0 },
      axis: 99,
    });
    expect(decodeTree(encodeTreeV3([node]))).toBeNull();
  });
});

describe("Inspector", () => {
  /** @type {Inspector} */
  let insp;
  /** @type {object} */
  let g;

  beforeEach(() => {
    // Stand-in global so __akiva mutation doesn't leak between tests.
    g = {};
    insp = new Inspector({ staleMs: 60_000, global: g });
  });

  afterEach(() => {
    insp.destroy();
  });

  it("exposes the API on the supplied global", () => {
    expect(typeof g.__akiva).toBe("object");
    expect(typeof g.__akiva.snapshot).toBe("function");
    expect(typeof g.__akiva.findById).toBe("function");
    expect(typeof g.__akiva.findByRole).toBe("function");
    expect(typeof g.__akiva.findByText).toBe("function");
    expect(typeof g.__akiva.queryAll).toBe("function");
  });

  it("findById returns the matching node with rect + activity", () => {
    const tree = encodeTree([
      encodeNode({ role: 2, x: 10, y: 20, w: 100, h: 40, id: "go", label: "Go" }),
    ]);
    insp.apply({ packageId: "com.test.app", handle: 7, tree });
    const m = g.__akiva.findById("go");
    expect(m).not.toBeNull();
    expect(m.role).toBe("button");
    expect(m.id).toBe("go");
    expect(m.label).toBe("Go");
    expect(m.rect).toEqual({ x: 10, y: 20, w: 100, h: 40 });
    expect(m.packageId).toBe("com.test.app");
    expect(m.handle).toBe(7);
  });

  it("findById returns null for unknown id", () => {
    insp.apply({
      packageId: "p", handle: 1,
      tree: encodeTree([encodeNode({ role: 2, x: 0, y: 0, w: 1, h: 1, id: "a", label: "A" })]),
    });
    expect(g.__akiva.findById("missing")).toBeNull();
  });

  it("findByRole + name matches on the visible label", () => {
    insp.apply({
      packageId: "p", handle: 1,
      tree: encodeTree([
        encodeNode({ role: 2, x: 0, y: 0, w: 80, h: 36, id: "plus", label: "+" }),
        encodeNode({ role: 2, x: 0, y: 0, w: 80, h: 36, id: "eq",   label: "=" }),
      ]),
    });
    expect(g.__akiva.findByRole("button", { name: "+" }).id).toBe("plus");
    expect(g.__akiva.findByRole("button", { name: "=" }).id).toBe("eq");
    expect(g.__akiva.findByRole("button", { name: "?" })).toBeNull();
  });

  it("findByRole without a name returns the first match", () => {
    insp.apply({
      packageId: "p", handle: 1,
      tree: encodeTree([
        encodeNode({ role: 0, x: 0, y: 0, w: 800, h: 600 }),
        encodeNode({ parent: 0, role: 2, x: 0, y: 0, w: 80, h: 36, id: "btn", label: "X" }),
      ]),
    });
    expect(g.__akiva.findByRole("button").id).toBe("btn");
    expect(g.__akiva.findByRole("group").role).toBe("group");
  });

  it("findByText matches text-role nodes by visible content", () => {
    insp.apply({
      packageId: "p", handle: 1,
      tree: encodeTree([
        encodeNode({ role: 1, x: 0, y: 0, w: 200, h: 24, label: "Hello, world" }),
        encodeNode({ role: 2, x: 0, y: 0, w: 80,  h: 36, id: "b", label: "Hello, world" }),
      ]),
    });
    const m = g.__akiva.findByText("Hello, world");
    expect(m).not.toBeNull();
    expect(m.role).toBe("text");
    // Button with the same label is *not* matched by findByText.
    expect(m.id).toBeNull();
  });

  it("findByText supports substring match via { exact: false }", () => {
    insp.apply({
      packageId: "p", handle: 1,
      tree: encodeTree([
        encodeNode({ role: 1, x: 0, y: 0, w: 200, h: 24, label: "Result: 56" }),
      ]),
    });
    expect(g.__akiva.findByText("56", { exact: true })).toBeNull();
    expect(g.__akiva.findByText("56", { exact: false })).not.toBeNull();
  });

  it("snapshot returns plain copies that can't mutate the inspector", () => {
    insp.apply({
      packageId: "p", handle: 1,
      tree: encodeTree([encodeNode({ role: 2, x: 0, y: 0, w: 1, h: 1, id: "a", label: "A" })]),
    });
    const snap = g.__akiva.snapshot();
    expect(snap).toHaveLength(1);
    expect(snap[0].nodes[0].label).toBe("A");
    snap[0].nodes[0].label = "MUTATED";
    expect(g.__akiva.findById("a").label).toBe("A");
  });

  it("apply replaces an activity's previous tree wholesale", () => {
    insp.apply({
      packageId: "p", handle: 1,
      tree: encodeTree([encodeNode({ role: 2, x: 0, y: 0, w: 1, h: 1, id: "a", label: "A" })]),
    });
    expect(g.__akiva.findById("a")).not.toBeNull();
    insp.apply({
      packageId: "p", handle: 1,
      tree: encodeTree([encodeNode({ role: 2, x: 0, y: 0, w: 1, h: 1, id: "b", label: "B" })]),
    });
    expect(g.__akiva.findById("a")).toBeNull();
    expect(g.__akiva.findById("b")).not.toBeNull();
  });

  it("isolates activities under their own (packageId, handle) key", () => {
    insp.apply({
      packageId: "com.a", handle: 1,
      tree: encodeTree([encodeNode({ role: 2, x: 0, y: 0, w: 1, h: 1, id: "x", label: "X" })]),
    });
    insp.apply({
      packageId: "com.b", handle: 2,
      tree: encodeTree([encodeNode({ role: 2, x: 0, y: 0, w: 1, h: 1, id: "y", label: "Y" })]),
    });
    expect(g.__akiva.list()).toHaveLength(2);
    expect(g.__akiva.findById("x").packageId).toBe("com.a");
    expect(g.__akiva.findById("y").packageId).toBe("com.b");
  });

  it("warns and drops a malformed tree", () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    insp.apply({ packageId: "p", handle: 1, tree: new Uint8Array([99, 0, 0, 0, 0]) });
    expect(warn).toHaveBeenCalled();
    expect(g.__akiva.list()).toHaveLength(0);
    warn.mockRestore();
  });

  it("destroy clears the global pointer", () => {
    expect(g.__akiva).toBeDefined();
    insp.destroy();
    expect(g.__akiva).toBeUndefined();
  });

  // --- composite transform application -------------------------------------

  it("returns surface-local rects when no transform was published", () => {
    insp.apply({
      packageId: "p", handle: 1,
      tree: encodeTree([encodeNode({ role: 2, x: 100, y: 200, w: 50, h: 30, id: "btn", label: "B" })]),
    });
    expect(g.__akiva.findById("btn").rect).toEqual({ x: 100, y: 200, w: 50, h: 30 });
  });

  it("applies translate transforms to returned rects (push-transition shape)", () => {
    insp.apply({
      packageId: "p", handle: 7,
      tree: encodeTree([encodeNode({ role: 2, x: 100, y: 200, w: 50, h: 30, id: "btn", label: "B" })]),
    });
    insp.applyTransform({
      handle: 7, opacity: 1, translate_x: 540, translate_y: 0, scale: 1,
    });
    expect(g.__akiva.findById("btn").rect).toEqual({
      x: 100 + 540, y: 200, w: 50, h: 30,
    });
  });

  it("applies scale transforms (modal sheet zoom-in shape)", () => {
    insp.apply({
      packageId: "p", handle: 7,
      tree: encodeTree([encodeNode({ role: 2, x: 100, y: 200, w: 80, h: 40, id: "btn", label: "B" })]),
    });
    insp.applyTransform({
      handle: 7, opacity: 1, translate_x: 0, translate_y: 0, scale: 0.5,
    });
    expect(g.__akiva.findById("btn").rect).toEqual({
      x: 50, y: 100, w: 40, h: 20,
    });
  });

  it("identity transforms are dropped from the map (no apply cost)", () => {
    insp.apply({
      packageId: "p", handle: 7,
      tree: encodeTree([encodeNode({ role: 2, x: 100, y: 200, w: 50, h: 30, id: "btn", label: "B" })]),
    });
    insp.applyTransform({
      handle: 7, opacity: 1, translate_x: 100, translate_y: 0, scale: 1,
    });
    expect(g.__akiva.findById("btn").rect.x).toBe(200);
    // Settle to identity: should clear the offset.
    insp.applyTransform({
      handle: 7, opacity: 1, translate_x: 0, translate_y: 0, scale: 1,
    });
    expect(g.__akiva.findById("btn").rect.x).toBe(100);
  });

  it("transforms isolate per handle", () => {
    insp.apply({
      packageId: "a", handle: 1,
      tree: encodeTree([encodeNode({ role: 2, x: 0, y: 0, w: 10, h: 10, id: "a", label: "A" })]),
    });
    insp.apply({
      packageId: "b", handle: 2,
      tree: encodeTree([encodeNode({ role: 2, x: 0, y: 0, w: 10, h: 10, id: "b", label: "B" })]),
    });
    insp.applyTransform({ handle: 1, opacity: 1, translate_x: 50, translate_y: 0, scale: 1 });
    expect(g.__akiva.findById("a").rect.x).toBe(50);
    expect(g.__akiva.findById("b").rect.x).toBe(0);
  });

  it("transform is forgotten when its activity ages out", () => {
    const fast = new Inspector({ staleMs: 5, global: {} });
    fast.apply({
      packageId: "p", handle: 9,
      tree: encodeTree([encodeNode({ role: 2, x: 0, y: 0, w: 10, h: 10, id: "x", label: "X" })]),
    });
    fast.applyTransform({ handle: 9, opacity: 1, translate_x: 99, translate_y: 0, scale: 1 });
    // Internal access — the cleanest assertion the transform actually
    // got purged with the activity.
    return new Promise((resolve) => setTimeout(() => {
      // _gcStale runs on a 1 s timer; force one manually.
      // eslint-disable-next-line no-underscore-dangle
      fast._gcStale();
      // eslint-disable-next-line no-underscore-dangle
      expect(fast._transforms.has(9)).toBe(false);
      fast.destroy();
      resolve(undefined);
    }, 20));
  });

  it("scrollOf returns the named scroll region's full state", () => {
    const fake = {};
    const i = new Inspector({ global: fake });
    const tree = encodeTreeV2([
      encodeNodeV2({
        role: 6, x: 0, y: 100, w: 200, h: 400,
        id: "feed",
        scroll: {
          scrollY: 600, contentH: 8400, viewportH: 400, viewportW: 200,
          firstVisible: 8, lastVisible: 17, itemCount: 80,
        },
      }),
    ]);
    i.apply({ packageId: "pkg", handle: 5, tree });
    const s = fake.__akiva.scrollOf("feed");
    expect(s).toEqual({
      key: "feed",
      scrollY: 600,
      contentHeight: 8400,
      viewportHeight: 400,
      viewportWidth: 200,
      firstVisibleIndex: 8,
      lastVisibleIndex: 17,
      itemCount: 80,
      // v2 fixture → axis defaults to "vertical".
      axis: "vertical",
      rect: { x: 0, y: 100, w: 200, h: 400 },
    });
    i.destroy();
  });

  it("scrollOf returns null for unknown key", () => {
    const fake = {};
    const i = new Inspector({ global: fake });
    expect(fake.__akiva.scrollOf("never-built")).toBeNull();
    i.destroy();
  });

  it("scrollOf surfaces axis='horizontal' for LazyRow (v3 wire)", () => {
    // The 2d.4 wire bump carries axis on the scroll payload; tests
    // gate carousel behaviour on this field.
    const fake = {};
    const i = new Inspector({ global: fake });
    const tree = encodeTreeV3([
      encodeNodeV3({
        role: 6,
        x: 0, y: 100, w: 400, h: 200,
        id: "carousel",
        scroll: {
          scrollY: 800, contentH: 4800, viewportH: 400, viewportW: 200,
          firstVisible: 2, lastVisible: 4, itemCount: 12,
        },
        axis: 1,
      }),
    ]);
    i.apply({ packageId: "pkg", handle: 7, tree });
    const s = fake.__akiva.scrollOf("carousel");
    expect(s.axis).toBe("horizontal");
    expect(s.itemCount).toBe(12);
    expect(s.scrollY).toBe(800);
    i.destroy();
  });
});

describe("inspectorEnabled", () => {
  function setSearch(qs) {
    history.replaceState({}, "", `${location.pathname}${qs ? "?" + qs : ""}`);
  }

  // happy-dom 20.x ships an incomplete `localStorage` implementation —
  // `setItem` / `removeItem` are missing on the property the window
  // exposes. Polyfill a Map-backed Storage stub so `inspectorEnabled`
  // exercises the real branch instead of the "no storage" fallback.
  let storageStub;
  beforeEach(() => {
    setSearch("");
    const data = new Map();
    storageStub = {
      getItem: (k) => (data.has(k) ? data.get(k) : null),
      setItem: (k, v) => data.set(k, String(v)),
      removeItem: (k) => data.delete(k),
      clear: () => data.clear(),
    };
    Object.defineProperty(globalThis, "localStorage", {
      value: storageStub, configurable: true, writable: true,
    });
  });

  it("is off by default", () => {
    expect(inspectorEnabled()).toBe(false);
  });

  it("turns on with ?inspector=1", () => {
    setSearch("inspector=1");
    expect(inspectorEnabled()).toBe(true);
  });

  it("turns on via sticky localStorage", () => {
    localStorage.setItem("akivaInspector", "1");
    expect(inspectorEnabled()).toBe(true);
  });

  it("?inspector=0 wins over sticky", () => {
    localStorage.setItem("akivaInspector", "1");
    setSearch("inspector=0");
    expect(inspectorEnabled()).toBe(false);
  });
});
