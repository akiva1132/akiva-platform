// Storage HAL (OPFS-backed).
//
// Lives inside the worker. The kernel's `hal_storage_*` imports are
// synchronous FFI calls, so the only web primitive that fits is OPFS's
// SyncAccessHandle — available exclusively inside a Worker context.
//
// Strategy for MVP:
//   - Single file at OPFS root: `akiva-store.v1.bin`.
//   - One SyncAccessHandle held open for the worker's lifetime. The
//     browser auto-releases it when the worker terminates, so the next
//     leader after a failover acquires it cleanly without coordination.
//   - In-memory `Map<string, Uint8Array>` mirrors the file. Reads answer
//     from the map; mutations rewrite the whole file synchronously.
//   - Whole-file rewrite is O(total-bytes) per write. Fine for the Phase
//     1b working set (SavedState blobs, a handful of KB). Phase 2 can
//     switch to log-structured or per-key sub-files.
//
// Init order (critical): `load()` must complete before `akiva_boot` is
// called. Otherwise the kernel's first `storage::read` would see an
// uninitialised store. See worker.js `bootKernel()`.
//
// File format (little-endian):
//
//   magic:      4 bytes, ASCII "AKSV"
//   version:    u32, currently 1
//   entry_count: u32
//   entries:    repeated {
//                 key_len: u32
//                 key:     key_len bytes (UTF-8)
//                 val_len: u32
//                 val:     val_len bytes
//               }
//
// A missing / zero-byte / malformed file is treated as empty; it will be
// overwritten on the next write.

const FILE_NAME = "akiva-store.v1.bin";
const MAGIC = 0x56534b41; // "AKSV" little-endian
const VERSION = 1;

/** Return codes shared with the kernel. */
export const SR_OK = 0;
export const SR_IO = 1;
export const SR_BAD_KEY = 2;

export class Storage {
  constructor() {
    this.map = new Map();         // key (string) → Uint8Array value
    this.handle = null;            // SyncAccessHandle | null
    this.persistent = false;       // false = in-memory fallback
    this._encoder = new TextEncoder();
    this._decoder = new TextDecoder("utf-8");
  }

  /** Open OPFS file, acquire sync handle, load existing entries.
   *  Must be awaited before any sync call.
   *  On failure, falls back to in-memory mode and logs a warning via `logFn`. */
  async load(logFn = null) {
    const log = logFn ?? ((level, text) => console[level === "warn" ? "warn" : "log"](`[storage] ${text}`));
    try {
      const root = await navigator.storage.getDirectory();
      const file = await root.getFileHandle(FILE_NAME, { create: true });
      // SyncAccessHandle is exclusive per file across the entire origin —
      // that's exactly what we want: only the leader worker holds it.
      this.handle = await file.createSyncAccessHandle();
      this.persistent = true;
      this._readAllFromHandle(log);
      log("info", `loaded ${this.map.size} entries from OPFS`);
    } catch (err) {
      this.handle = null;
      this.persistent = false;
      log("warn", `OPFS unavailable, running in-memory only: ${err?.message ?? err}`);
    }
  }

  /** Sync get. Returns Uint8Array or null. */
  get(key) {
    const v = this.map.get(key);
    return v === undefined ? null : v;
  }

  /** Sync put. Returns SR_OK / SR_IO. */
  set(key, value) {
    if (!key) return SR_BAD_KEY;
    // Copy defensively — caller's buffer may be a view into wasm memory
    // which moves on growth.
    this.map.set(key, new Uint8Array(value));
    return this._flush();
  }

  /** Sync delete. Returns SR_OK (idempotent — deleting a missing key is fine). */
  delete(key) {
    if (!key) return SR_BAD_KEY;
    this.map.delete(key);
    return this._flush();
  }

  /** Release the sync handle. Called on worker shutdown. */
  close() {
    if (this.handle) {
      try { this.handle.flush(); this.handle.close(); } catch {}
      this.handle = null;
    }
  }

  // ── Internals ──────────────────────────────────────────────────────────

  _readAllFromHandle(log) {
    const size = this.handle.getSize();
    if (size === 0) return;
    if (size < 12) {
      log("warn", `store file too small (${size} bytes), treating as empty`);
      return;
    }
    const buf = new Uint8Array(size);
    const read = this.handle.read(buf, { at: 0 });
    if (read !== size) {
      log("warn", `short read ${read}/${size}, treating as empty`);
      return;
    }
    const dv = new DataView(buf.buffer, buf.byteOffset, buf.byteLength);
    if (dv.getUint32(0, true) !== MAGIC) {
      log("warn", "store file magic mismatch, treating as empty");
      return;
    }
    const ver = dv.getUint32(4, true);
    if (ver !== VERSION) {
      log("warn", `store file version ${ver} != ${VERSION}, treating as empty`);
      return;
    }
    const count = dv.getUint32(8, true);
    let off = 12;
    for (let i = 0; i < count; i++) {
      if (off + 4 > size) { log("warn", "truncated key_len"); break; }
      const klen = dv.getUint32(off, true); off += 4;
      if (off + klen > size) { log("warn", "truncated key"); break; }
      const key = this._decoder.decode(buf.subarray(off, off + klen));
      off += klen;
      if (off + 4 > size) { log("warn", "truncated val_len"); break; }
      const vlen = dv.getUint32(off, true); off += 4;
      if (off + vlen > size) { log("warn", "truncated val"); break; }
      const val = buf.slice(off, off + vlen);
      off += vlen;
      this.map.set(key, val);
    }
  }

  _flush() {
    if (!this.persistent || !this.handle) return SR_OK; // in-memory mode
    try {
      const buf = this._serialize();
      this.handle.truncate(0);
      const written = this.handle.write(buf, { at: 0 });
      if (written !== buf.byteLength) return SR_IO;
      this.handle.flush();
      return SR_OK;
    } catch (err) {
      console.error("[storage] flush failed:", err);
      return SR_IO;
    }
  }

  _serialize() {
    let total = 12; // magic + ver + count
    const encoded = [];
    for (const [key, val] of this.map) {
      const kb = this._encoder.encode(key);
      encoded.push([kb, val]);
      total += 4 + kb.byteLength + 4 + val.byteLength;
    }
    const buf = new Uint8Array(total);
    const dv = new DataView(buf.buffer);
    dv.setUint32(0, MAGIC, true);
    dv.setUint32(4, VERSION, true);
    dv.setUint32(8, encoded.length, true);
    let off = 12;
    for (const [kb, val] of encoded) {
      dv.setUint32(off, kb.byteLength, true); off += 4;
      buf.set(kb, off); off += kb.byteLength;
      dv.setUint32(off, val.byteLength, true); off += 4;
      buf.set(val, off); off += val.byteLength;
    }
    return buf;
  }
}
