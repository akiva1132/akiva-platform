// Shared OPFS path encoding for the per-key storage layout (Stone
// 11.6.7 #3). Both the kernel worker (runtime/web/storage.js) and
// every app worker (runtime/web/app_worker_shell.js) compute the same
// path components for a given key — they MUST agree byte-for-byte or
// the app's direct readonly handle won't find the file the kernel
// just wrote.
//
// Encoding:
//   - Split on `/` into path segments. Empty segments, `.`, and `..`
//     are rejected (`isValidKey` returns false).
//   - Each segment is percent-encoded with the strict rule: any
//     character NOT in [A-Za-z0-9._-] becomes `%XX...` of its UTF-8
//     bytes. The unencoded set is intentionally narrow so the
//     encoding is identical across platforms with different OPFS
//     filename validators.
//
// The encoder works on full Unicode strings (handles surrogate pairs
// correctly via for-of iteration). The decoder reads percent escapes
// as raw bytes and runs them through TextDecoder, so a round-trip
// through the encoded form is lossless.

const _encoder = new TextEncoder();
const _decoder = new TextDecoder("utf-8");

/** Returns true if `key` is a non-empty UTF-8 string with no leading/
 *  trailing slash, no empty path components, and no `.` / `..`. */
export function isValidKey(key) {
  if (typeof key !== "string" || key.length === 0) return false;
  if (key.startsWith("/") || key.endsWith("/")) return false;
  for (const part of key.split("/")) {
    if (part.length === 0) return false;
    if (part === "." || part === "..") return false;
  }
  return true;
}

/** Encode `key` into an array of OPFS-safe path segments. Caller is
 *  responsible for `isValidKey(key) === true` first. */
export function encodeKey(key) {
  return key.split("/").map(encodeComponent);
}

export function encodeComponent(s) {
  let out = "";
  for (const ch of s) {
    if (/^[A-Za-z0-9._-]$/.test(ch)) {
      out += ch;
    } else {
      const bytes = _encoder.encode(ch);
      for (const b of bytes) {
        out += "%" + b.toString(16).toUpperCase().padStart(2, "0");
      }
    }
  }
  return out;
}

export function decodeComponent(s) {
  const bytes = [];
  let i = 0;
  while (i < s.length) {
    if (s[i] === "%" && i + 2 < s.length) {
      const hex = s.slice(i + 1, i + 3);
      if (/^[0-9A-Fa-f]{2}$/.test(hex)) {
        bytes.push(parseInt(hex, 16));
        i += 3;
        continue;
      }
    }
    bytes.push(s.charCodeAt(i) & 0xff);
    i += 1;
  }
  return _decoder.decode(new Uint8Array(bytes));
}
