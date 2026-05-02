// Pure unit tests for the OPFS path encoding helpers (Stone 11.6.7 #3).
//
// Both the kernel storage layer (`runtime/web/storage.js`) and the
// app worker shell (`runtime/web/app_worker_shell.js`) walk the same
// per-key OPFS layout. They MUST agree byte-for-byte on the encoding
// or the app's direct read won't find the file the kernel just wrote.

import { describe, it, expect } from "vitest";
import { encodeKey, encodeComponent, decodeComponent, isValidKey } from "../storage_path.mjs";

describe("isValidKey", () => {
  it("accepts simple keys", () => {
    expect(isValidKey("notes")).toBe(true);
    expect(isValidKey("a")).toBe(true);
    expect(isValidKey("packages/sys.shell/manifest.toml")).toBe(true);
  });

  it("rejects empty and non-strings", () => {
    expect(isValidKey("")).toBe(false);
    expect(isValidKey(null)).toBe(false);
    expect(isValidKey(undefined)).toBe(false);
    expect(isValidKey(42)).toBe(false);
  });

  it("rejects leading or trailing slashes", () => {
    expect(isValidKey("/foo")).toBe(false);
    expect(isValidKey("foo/")).toBe(false);
    expect(isValidKey("/")).toBe(false);
  });

  it("rejects empty path components", () => {
    expect(isValidKey("foo//bar")).toBe(false);
  });

  it("rejects . and .. components", () => {
    expect(isValidKey(".")).toBe(false);
    expect(isValidKey("..")).toBe(false);
    expect(isValidKey("foo/./bar")).toBe(false);
    expect(isValidKey("foo/../bar")).toBe(false);
  });
});

describe("encodeComponent", () => {
  it("passes safe ASCII through unchanged", () => {
    expect(encodeComponent("hello")).toBe("hello");
    expect(encodeComponent("file.txt")).toBe("file.txt");
    expect(encodeComponent("a-b_c")).toBe("a-b_c");
    expect(encodeComponent("sys.shell")).toBe("sys.shell");
    expect(encodeComponent("ABC123")).toBe("ABC123");
  });

  it("percent-encodes spaces and other ASCII non-safe chars", () => {
    expect(encodeComponent("hello world")).toBe("hello%20world");
    expect(encodeComponent("a/b")).toBe("a%2Fb");
    expect(encodeComponent("a%b")).toBe("a%25b");
    expect(encodeComponent("a:b")).toBe("a%3Ab");
  });

  it("percent-encodes non-ASCII as UTF-8 bytes", () => {
    // עברית = D7 A2 D7 91 D7 A8 D7 99 D7 AA in UTF-8 (5 chars, 10 bytes)
    expect(encodeComponent("עברית"))
      .toBe("%D7%A2%D7%91%D7%A8%D7%99%D7%AA");
    // 你好 = E4 BD A0 E5 A5 BD
    expect(encodeComponent("你好")).toBe("%E4%BD%A0%E5%A5%BD");
  });

  it("handles emoji (surrogate pairs) cleanly", () => {
    // 🚀 = F0 9F 9A 80 in UTF-8 (one rocket emoji)
    expect(encodeComponent("🚀")).toBe("%F0%9F%9A%80");
  });
});

describe("decodeComponent", () => {
  it("inverts encodeComponent for ASCII", () => {
    expect(decodeComponent("hello")).toBe("hello");
    expect(decodeComponent("hello%20world")).toBe("hello world");
    expect(decodeComponent("a%2Fb")).toBe("a/b");
    expect(decodeComponent("a%25b")).toBe("a%b");
  });

  it("inverts encodeComponent for non-ASCII UTF-8", () => {
    expect(decodeComponent("%D7%A2%D7%91%D7%A8%D7%99%D7%AA")).toBe("עברית");
    expect(decodeComponent("%E4%BD%A0%E5%A5%BD")).toBe("你好");
    expect(decodeComponent("%F0%9F%9A%80")).toBe("🚀");
  });

  it("accepts both upper and lower case hex", () => {
    expect(decodeComponent("%2f")).toBe("/");
    expect(decodeComponent("%2F")).toBe("/");
    expect(decodeComponent("%aA")).toBe(decodeComponent("%AA"));
  });

  it("leaves stray % alone if not followed by valid hex", () => {
    // Not strictly mandated, but the implementation should be robust.
    // We do best-effort decoding and treat invalid escapes as literal.
    expect(decodeComponent("%ZZ")).toBe("%ZZ");
  });
});

describe("encodeKey", () => {
  it("splits on slash, encodes each segment", () => {
    expect(encodeKey("packages/sys.shell/manifest.toml"))
      .toEqual(["packages", "sys.shell", "manifest.toml"]);
    expect(encodeKey("pkgs/com.example/notes/2026 Lunch.txt"))
      .toEqual(["pkgs", "com.example", "notes", "2026%20Lunch.txt"]);
  });

  it("returns single segment for slash-free keys", () => {
    expect(encodeKey("foo")).toEqual(["foo"]);
  });
});

describe("round-trip", () => {
  const cases = [
    "simple",
    "with-dashes_and.dots",
    "spaces are encoded",
    "עברית",
    "你好世界",
    "🚀 launch",
    "edge%case",
    "weird:colons:everywhere",
    "tab\there",
    "newline\nhere",
  ];
  for (const original of cases) {
    it(`survives round-trip: '${original}'`, () => {
      expect(decodeComponent(encodeComponent(original))).toBe(original);
    });
  }
});
