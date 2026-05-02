// Storage layer unit tests (Stone 11.6.7 #3).
//
// vitest's jsdom env doesn't expose OPFS, so the persistent path is
// covered by manual smoke testing in a real browser. These tests
// cover the parts that are deterministic in pure JS: in-memory
// fallback semantics, key validation, the invalidation callback, and
// the legacy single-file parser used by the migration step.

import { describe, it, expect, vi } from "vitest";
import { Storage, SR_OK, SR_BAD_KEY } from "../storage.js";

describe("Storage — in-memory mode (no OPFS)", () => {
  it("set/get/delete round-trip without persistence", () => {
    const s = new Storage();
    expect(s.persistent).toBe(false); // never loaded
    expect(s.set("foo", new Uint8Array([1, 2, 3]))).toBe(SR_OK);
    expect(s.get("foo")).toEqual(new Uint8Array([1, 2, 3]));
    expect(s.delete("foo")).toBe(SR_OK);
    expect(s.get("foo")).toBeNull();
  });

  it("get returns null for missing keys", () => {
    const s = new Storage();
    expect(s.get("nonexistent")).toBeNull();
  });

  it("set rejects empty key", () => {
    const s = new Storage();
    expect(s.set("", new Uint8Array(0))).toBe(SR_BAD_KEY);
  });

  it("set rejects keys with leading or trailing slash", () => {
    const s = new Storage();
    expect(s.set("/leading", new Uint8Array(0))).toBe(SR_BAD_KEY);
    expect(s.set("trailing/", new Uint8Array(0))).toBe(SR_BAD_KEY);
  });

  it("set rejects keys with empty / dot / dotdot components", () => {
    const s = new Storage();
    expect(s.set("foo//bar", new Uint8Array(0))).toBe(SR_BAD_KEY);
    expect(s.set("foo/./bar", new Uint8Array(0))).toBe(SR_BAD_KEY);
    expect(s.set("foo/../bar", new Uint8Array(0))).toBe(SR_BAD_KEY);
  });

  it("delete is idempotent on missing keys", () => {
    const s = new Storage();
    expect(s.delete("never-existed")).toBe(SR_OK);
  });

  it("delete rejects empty key", () => {
    const s = new Storage();
    expect(s.delete("")).toBe(SR_BAD_KEY);
  });

  it("set copies the value buffer (no aliasing)", () => {
    const s = new Storage();
    const src = new Uint8Array([1, 2, 3]);
    s.set("foo", src);
    src[0] = 99;
    expect(s.get("foo")).toEqual(new Uint8Array([1, 2, 3]));
  });
});

describe("Storage — invalidation callback", () => {
  it("fires onInvalidate(pkg, userKey) on writes to pkgs/<pkg>/<key>", () => {
    const calls = [];
    const s = new Storage({ onInvalidate: (pkg, key) => calls.push([pkg, key]) });
    s.set("pkgs/com.example/notes", new Uint8Array(1));
    expect(calls).toEqual([["com.example", "notes"]]);
  });

  it("fires onInvalidate on delete too", () => {
    const calls = [];
    const s = new Storage({ onInvalidate: (pkg, key) => calls.push([pkg, key]) });
    s.set("pkgs/com.example/notes", new Uint8Array(1));
    calls.length = 0;
    s.delete("pkgs/com.example/notes");
    expect(calls).toEqual([["com.example", "notes"]]);
  });

  it("fires onInvalidate even when deleting an absent key", () => {
    // The app worker's cache may have a stale handle even though the
    // kernel's mirror has nothing — fire the message anyway so the
    // worker drops its handle.
    const calls = [];
    const s = new Storage({ onInvalidate: (pkg, key) => calls.push([pkg, key]) });
    s.delete("pkgs/com.example/never");
    expect(calls).toEqual([["com.example", "never"]]);
  });

  it("does NOT fire onInvalidate for non-pkgs/ keys", () => {
    const calls = [];
    const s = new Storage({ onInvalidate: (pkg, key) => calls.push([pkg, key]) });
    s.set("packages/com.example/manifest.toml", new Uint8Array(1));
    s.set("sys/packages/index", new Uint8Array(1));
    s.set("sys/permissions/com.example", new Uint8Array(1));
    expect(calls).toEqual([]);
  });

  it("nested user keys carry the full path through to the callback", () => {
    const calls = [];
    const s = new Storage({ onInvalidate: (pkg, key) => calls.push([pkg, key]) });
    s.set("pkgs/com.example/notes/2026/05.txt", new Uint8Array(1));
    expect(calls).toEqual([["com.example", "notes/2026/05.txt"]]);
  });

  it("swallows errors thrown from onInvalidate without breaking the write", () => {
    const log = vi.fn();
    const s = new Storage({ onInvalidate: () => { throw new Error("boom"); } });
    s._log = log;
    expect(s.set("pkgs/com.example/notes", new Uint8Array(1))).toBe(SR_OK);
    expect(s.get("pkgs/com.example/notes")).toEqual(new Uint8Array(1));
    expect(log).toHaveBeenCalledWith("warn", expect.stringContaining("onInvalidate threw"));
  });

  it("setter syntax: storage.onInvalidate = fn updates after construction", () => {
    const s = new Storage();
    const calls = [];
    s.onInvalidate = (pkg, key) => calls.push([pkg, key]);
    s.set("pkgs/com.example/k", new Uint8Array(1));
    expect(calls).toEqual([["com.example", "k"]]);
  });
});

describe("Storage — legacy blob parser", () => {
  it("parses an empty blob as no entries", () => {
    const s = new Storage();
    expect(s._parseLegacyBlob(new Uint8Array(0))).toEqual([]);
  });

  it("rejects too-small blobs", () => {
    const s = new Storage();
    expect(s._parseLegacyBlob(new Uint8Array([1, 2, 3]))).toBeNull();
  });

  it("rejects bad magic", () => {
    const s = new Storage();
    const buf = new Uint8Array(12);
    new DataView(buf.buffer).setUint32(0, 0xdeadbeef, true);
    expect(s._parseLegacyBlob(buf)).toBeNull();
  });

  it("rejects bad version", () => {
    const s = new Storage();
    const buf = new Uint8Array(12);
    new DataView(buf.buffer).setUint32(0, 0x56534b41, true); // "AKSV"
    new DataView(buf.buffer).setUint32(4, 99, true);          // wrong version
    expect(s._parseLegacyBlob(buf)).toBeNull();
  });

  it("round-trips: serialize entries → parseLegacyBlob → same entries", () => {
    const s = new Storage();
    const entries = [
      ["foo", new Uint8Array([1, 2, 3])],
      ["packages/com.test/manifest.toml", new TextEncoder().encode("[app]\nid='com.test'\n")],
      ["pkgs/com.test/notes", new Uint8Array(0)],
    ];
    const blob = serializeLegacy(entries);
    const parsed = s._parseLegacyBlob(blob);
    expect(parsed).not.toBeNull();
    expect(parsed.length).toBe(entries.length);
    for (let i = 0; i < entries.length; i++) {
      expect(parsed[i][0]).toBe(entries[i][0]);
      expect(parsed[i][1]).toEqual(entries[i][1]);
    }
  });

  it("rejects truncated key length prefix", () => {
    const s = new Storage();
    // Header announces 1 entry but body is empty.
    const buf = new Uint8Array(12);
    new DataView(buf.buffer).setUint32(0, 0x56534b41, true);
    new DataView(buf.buffer).setUint32(4, 1, true);  // version
    new DataView(buf.buffer).setUint32(8, 1, true);  // count
    expect(s._parseLegacyBlob(buf)).toBeNull();
  });

  it("rejects truncated value", () => {
    const s = new Storage();
    // Entry says key_len=3, key="foo", val_len=10, but only 2 bytes of value.
    const enc = new TextEncoder();
    const buf = new Uint8Array(12 + 4 + 3 + 4 + 2);
    const dv = new DataView(buf.buffer);
    dv.setUint32(0, 0x56534b41, true);
    dv.setUint32(4, 1, true);
    dv.setUint32(8, 1, true);
    dv.setUint32(12, 3, true);
    buf.set(enc.encode("foo"), 16);
    dv.setUint32(19, 10, true);  // claims 10 bytes
    buf.set(enc.encode("AB"), 23); // only 2 bytes provided
    expect(s._parseLegacyBlob(buf)).toBeNull();
  });
});

// ── Helpers ──────────────────────────────────────────────────────────────

function serializeLegacy(entries) {
  const enc = new TextEncoder();
  let total = 12;
  const encoded = [];
  for (const [k, v] of entries) {
    const kb = enc.encode(k);
    encoded.push([kb, v]);
    total += 4 + kb.byteLength + 4 + v.byteLength;
  }
  const buf = new Uint8Array(total);
  const dv = new DataView(buf.buffer);
  dv.setUint32(0, 0x56534b41, true);
  dv.setUint32(4, 1, true);
  dv.setUint32(8, encoded.length, true);
  let off = 12;
  for (const [kb, v] of encoded) {
    dv.setUint32(off, kb.byteLength, true); off += 4;
    buf.set(kb, off); off += kb.byteLength;
    dv.setUint32(off, v.byteLength, true); off += 4;
    buf.set(v, off); off += v.byteLength;
  }
  return buf;
}
