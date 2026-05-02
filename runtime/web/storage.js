// Storage HAL (OPFS-backed, per-key files).
//
// Lives inside the kernel worker. The kernel's `hal_storage_*` imports
// are synchronous FFI calls, so the read/write/delete entry points
// MUST stay synchronous from the kernel's perspective. The trick:
//
//   - In-memory `Map<string, Uint8Array>` mirror is the source of
//     truth for every sync read/write. Returns immediately.
//   - Writes also enqueue an async OPFS flush (queue drained by a
//     single Promise pump). Persistence is eventually-consistent.
//   - At boot, `load()` walks the OPFS tree and pre-populates the
//     mirror, plus migrates the legacy single-file layout if present.
//
// Layout (Stone 11.6.7 #3 — per-key files):
//
//   <OPFS root>/
//     pkgs/<pkg>/<userKey...>     ← app sandbox (also opened readonly
//                                   by the package's app worker for
//                                   direct sub-millisecond reads)
//     packages/<pkg>/<...>        ← system: manifest, wasm, res
//     sys/<...>                   ← system: indices, permissions, boot
//
// Slashes inside a key become directory boundaries. Each path segment
// is percent-encoded so any UTF-8 string is a safe OPFS filename.
//
// Invalidation callback. After a write or delete to a key under
// `pkgs/<pkg>/<userKey>`, the storage layer fires
// `onInvalidate(pkg, userKey)` — AppRegistry hooks this into a
// `storage_invalidate` postMessage so the package's app worker drops
// its cached readonly SyncAccessHandle for that file. `userKey === null`
// means "all keys for this package" (uninstall).
//
// Migration. On first boot, the legacy single-file `akiva-store.v1.bin`
// at OPFS root is parsed entry-by-entry; each entry is rewritten to
// its per-key location, then the legacy file is deleted. Idempotent.

import { encodeKey, encodeComponent, decodeComponent, isValidKey } from "./storage_path.mjs";

const LEGACY_FILE = "akiva-store.v1.bin";
const LEGACY_MAGIC = 0x56534b41; // "AKSV"
const LEGACY_VERSION = 1;

/** Return codes shared with the kernel. */
export const SR_OK = 0;
export const SR_IO = 1;
export const SR_BAD_KEY = 2;

export class Storage {
  /**
   * @param {object} [opts]
   * @param {(packageId: string, userKey: string|null) => void} [opts.onInvalidate]
   *   Fired synchronously after a kernel-side write or delete to a
   *   `pkgs/<pkg>/<userKey>` path. `userKey === null` means
   *   "uninstall — drop everything for this package".
   */
  constructor(opts = {}) {
    this.map = new Map();
    this.persistent = false;
    this.onInvalidate = opts.onInvalidate ?? (() => {});
    this._encoder = new TextEncoder();
    this._decoder = new TextDecoder("utf-8");
    this._root = null;            // FileSystemDirectoryHandle
    this._writeQueue = [];        // Array<{key, value|null}>
    this._writePump = null;       // Promise | null
    this._log = (level, text) => console[level === "warn" ? "warn" : "log"](`[storage] ${text}`);
  }

  /** Open OPFS, walk the tree into the mirror, migrate legacy file.
   *  Must be awaited before `akiva_boot`. On failure, falls back to
   *  in-memory only (`persistent=false`) and logs a warning. */
  async load(logFn = null) {
    if (logFn) this._log = logFn;
    try {
      this._root = await navigator.storage.getDirectory();
      await this._migrateLegacyIfPresent();
      await this._loadAll();
      this.persistent = true;
      this._log("info", `loaded ${this.map.size} entries from OPFS (per-key)`);
    } catch (err) {
      this._root = null;
      this.persistent = false;
      this._log("warn", `OPFS unavailable, running in-memory only: ${err?.message ?? err}`);
    }
  }

  /** Sync get. Returns Uint8Array or null. */
  get(key) {
    const v = this.map.get(key);
    return v === undefined ? null : v;
  }

  /** Sync put. Returns SR_OK / SR_BAD_KEY. Persistence is async —
   *  the in-memory mirror updates immediately. */
  set(key, value) {
    if (!key) return SR_BAD_KEY;
    if (!isValidKey(key)) return SR_BAD_KEY;
    this.map.set(key, new Uint8Array(value));
    this._enqueueFlush(key, this.map.get(key));
    this._fireInvalidate(key);
    return SR_OK;
  }

  /** Sync delete. Idempotent. */
  delete(key) {
    if (!key) return SR_BAD_KEY;
    if (!isValidKey(key)) return SR_BAD_KEY;
    const had = this.map.has(key);
    this.map.delete(key);
    if (had) this._enqueueFlush(key, null);
    this._fireInvalidate(key);
    return SR_OK;
  }

  /** Drop every key matching `pkgs/<packageId>/...` from the mirror,
   *  fire onInvalidate(pkg, null), and recursively delete the OPFS
   *  directory. Used by `PackageManager::uninstall` via the
   *  `framework::storage` trait — not yet wired but the surface is
   *  here for when it lands. */
  async invalidatePackage(packageId) {
    const prefix = `pkgs/${packageId}/`;
    for (const k of [...this.map.keys()]) {
      if (k.startsWith(prefix)) this.map.delete(k);
    }
    try { this.onInvalidate(packageId, null); } catch {}
    if (!this.persistent || !this._root) return;
    try {
      const pkgsDir = await this._root.getDirectoryHandle("pkgs", { create: false });
      await pkgsDir.removeEntry(encodeComponent(packageId), { recursive: true });
    } catch (_) { /* may not exist — fine */ }
  }

  /** Block until queued writes are durable. Used by tests and
   *  graceful shutdown. */
  async drain() {
    if (this._writePump) await this._writePump;
  }

  /** Flush + release any open OPFS handles. Browsers auto-release on
   *  worker termination, but flushing first reduces the window for a
   *  partial-write loss. */
  async close() {
    await this.drain();
  }

  // ── Internals ──────────────────────────────────────────────────────────

  _fireInvalidate(key) {
    const m = key.match(/^pkgs\/([^/]+)\/(.+)$/);
    if (!m) return;
    try { this.onInvalidate(m[1], m[2]); } catch (err) {
      this._log("warn", `onInvalidate threw: ${err?.message ?? err}`);
    }
  }

  _enqueueFlush(key, value) {
    if (!this.persistent) return;
    this._writeQueue.push({ key, value });
    if (!this._writePump) {
      this._writePump = this._drainQueue().finally(() => { this._writePump = null; });
    }
  }

  async _drainQueue() {
    while (this._writeQueue.length > 0) {
      const { key, value } = this._writeQueue.shift();
      try {
        if (value === null) await this._removeFile(key);
        else await this._writeFile(key, value);
      } catch (err) {
        this._log("warn", `flush ${key}: ${err?.message ?? err}`);
      }
    }
  }

  // ── OPFS path encoding ─────────────────────────────────────────────────
  // Encoding lives in `storage_path.mjs` so the app worker (which
  // also walks `pkgs/<pkg>/...` for direct readonly reads) computes
  // the same path components.

  // ── OPFS I/O ───────────────────────────────────────────────────────────

  async _writeFile(key, value) {
    const segments = encodeKey(key);
    const fileHandle = await this._resolveFileHandle(segments, true);
    // Retry-with-backoff: an app worker may briefly hold a readonly
    // handle to this file (we sent it `storage_invalidate` but its
    // event-loop turn hasn't run yet). Up to ~30ms of patience.
    let handle = null;
    let lastErr = null;
    for (let attempt = 0; attempt < 6; attempt++) {
      try {
        handle = await fileHandle.createSyncAccessHandle();
        break;
      } catch (err) {
        lastErr = err;
        await sleep(2 * (attempt + 1));
      }
    }
    if (!handle) throw lastErr ?? new Error("createSyncAccessHandle failed");
    try {
      handle.truncate(0);
      const written = handle.write(value, { at: 0 });
      if (written !== value.byteLength) {
        throw new Error(`short write ${written}/${value.byteLength}`);
      }
      handle.flush();
    } finally {
      handle.close();
    }
  }

  async _removeFile(key) {
    const segments = encodeKey(key);
    if (segments.length === 0) return;
    const leaf = segments[segments.length - 1];
    const dirSegments = segments.slice(0, -1);
    let dir = this._root;
    for (const seg of dirSegments) {
      try { dir = await dir.getDirectoryHandle(seg, { create: false }); }
      catch { return; /* parent missing → file already gone */ }
    }
    try { await dir.removeEntry(leaf); } catch { /* missing is fine */ }
  }

  async _resolveFileHandle(segments, create) {
    if (segments.length === 0) throw new Error("empty key");
    const leaf = segments[segments.length - 1];
    const dirSegments = segments.slice(0, -1);
    let dir = this._root;
    for (const seg of dirSegments) {
      dir = await dir.getDirectoryHandle(seg, { create });
    }
    return await dir.getFileHandle(leaf, { create });
  }

  // ── Boot-time tree walk ────────────────────────────────────────────────

  async _loadAll() {
    await this._walkDir(this._root, []);
  }

  async _walkDir(dirHandle, pathSoFar) {
    // OPFS dir handles are async-iterable. `.entries()` yields [name, handle].
    // Some implementations expose `.values()` instead; be flexible.
    let iter;
    if (typeof dirHandle.entries === "function") {
      iter = dirHandle.entries();
    } else if (typeof dirHandle.values === "function") {
      iter = (async function* () {
        for await (const h of dirHandle.values()) yield [h.name, h];
      })();
    } else {
      this._log("warn", "OPFS dir handle has no entries() or values()");
      return;
    }
    for await (const pair of iter) {
      const [name, entry] = pair;
      const newPath = [...pathSoFar, name];
      if (entry.kind === "directory") {
        await this._walkDir(entry, newPath);
      } else if (entry.kind === "file") {
        const decoded = newPath.map((p) => decodeComponent(p));
        const key = decoded.join("/");
        try {
          const file = await entry.getFile();
          const buf = new Uint8Array(await file.arrayBuffer());
          this.map.set(key, buf);
        } catch (err) {
          this._log("warn", `read ${key}: ${err?.message ?? err}`);
        }
      }
    }
  }

  // ── Legacy migration (single-file → per-key) ───────────────────────────

  async _migrateLegacyIfPresent() {
    let legacyHandle = null;
    try {
      legacyHandle = await this._root.getFileHandle(LEGACY_FILE, { create: false });
    } catch (_) {
      return; // No legacy file — first boot on the new layout.
    }
    let bytes;
    try {
      const file = await legacyHandle.getFile();
      bytes = new Uint8Array(await file.arrayBuffer());
    } catch (err) {
      this._log("warn", `legacy file unreadable: ${err?.message ?? err}`);
      // Try to remove anyway so we don't loop forever.
      try { await this._root.removeEntry(LEGACY_FILE); } catch {}
      return;
    }
    const entries = this._parseLegacyBlob(bytes);
    if (entries === null) {
      this._log("warn", "legacy file malformed; discarding");
      try { await this._root.removeEntry(LEGACY_FILE); } catch {}
      return;
    }
    this._log("info", `migrating ${entries.length} legacy entries to per-key layout`);
    let migrated = 0;
    for (const [key, value] of entries) {
      if (!isValidKey(key)) {
        this._log("warn", `migrate: skip invalid key '${key}'`);
        continue;
      }
      try {
        const segments = encodeKey(key);
        const fileHandle = await this._resolveFileHandle(segments, true);
        const sync = await fileHandle.createSyncAccessHandle();
        try {
          sync.truncate(0);
          sync.write(value, { at: 0 });
          sync.flush();
        } finally {
          sync.close();
        }
        migrated += 1;
      } catch (err) {
        this._log("warn", `migrate ${key}: ${err?.message ?? err}`);
      }
    }
    this._log("info", `migrated ${migrated}/${entries.length}; removing legacy file`);
    try { await this._root.removeEntry(LEGACY_FILE); }
    catch (err) { this._log("warn", `legacy removeEntry: ${err?.message ?? err}`); }
  }

  /** Parse the AKSV v1 blob format. Returns array of [key, value]
   *  pairs, or null if the bytes are malformed. Empty / zero-byte
   *  file is treated as "no entries". */
  _parseLegacyBlob(buf) {
    if (buf.byteLength === 0) return [];
    if (buf.byteLength < 12) return null;
    const dv = new DataView(buf.buffer, buf.byteOffset, buf.byteLength);
    if (dv.getUint32(0, true) !== LEGACY_MAGIC) return null;
    if (dv.getUint32(4, true) !== LEGACY_VERSION) return null;
    const count = dv.getUint32(8, true);
    let off = 12;
    const out = [];
    for (let i = 0; i < count; i++) {
      if (off + 4 > buf.byteLength) return null;
      const klen = dv.getUint32(off, true); off += 4;
      if (off + klen > buf.byteLength) return null;
      const key = this._decoder.decode(buf.subarray(off, off + klen));
      off += klen;
      if (off + 4 > buf.byteLength) return null;
      const vlen = dv.getUint32(off, true); off += 4;
      if (off + vlen > buf.byteLength) return null;
      const val = buf.slice(off, off + vlen);
      off += vlen;
      out.push([key, val]);
    }
    return out;
  }
}

function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}
