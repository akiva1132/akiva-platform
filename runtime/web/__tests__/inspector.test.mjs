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
      id: "ok", label: "OK", value: null,
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
