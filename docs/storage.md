# Storage

> **Implementation status (May 2026 — Stone 11.6a-c + 11.6.7 #3 +
> follow-up `list_keys` work):** the kernel KV API + the HAL over
> OPFS `SyncAccessHandle` are live. The per-app SDK layer ships
> through `akiva_sdk::storage` with automatic per-package sandboxing
> and an async-shape API (`ReadHandle::poll`-loop,
> `WriteHandle.await`, `list_keys(prefix).await`) — see §8.
> **Multi-frame deferred-handle pattern** is the current constraint
> imposed by the cross-worker boundary; see
> [`docs/sdk-rust.md §19.1`](sdk-rust.md) for the planned fix
> (embassy executor). **Per-key OPFS file layout** (Stone 11.6.7 #3,
> May 2026) — `pkgs/<pkg>/<userKey>` directory tree, app workers
> open readonly handles directly for sub-millisecond reads, the
> kernel worker is the sole owner of the write side and broadcasts
> invalidation. Auto-migration from the legacy `akiva-store.v1.bin`.
> Transactions / quota enforcement / encryption — **post-MVP**.

This document covers the storage layer of Akiva: the interface the
kernel exposes, the HAL that implements it over OPFS, what is true
today versus what is staged.

## 1. Layers

```
  App (Rust, wasm)
    │    sdk-rust::storage  — Phase 11.6 SDK API
    ▼
  Kernel (Rust, wasm)
    │    kernel::storage    — synchronous string-key / bytes-value API
    ▼ extern "C" imports    (see kernel/src/host.rs)
  HAL (JS, Worker)
    │    runtime/web/storage.js  (`Storage` class)
    ▼
  OPFS
       navigator.storage.getDirectory() → SyncAccessHandle
```

Key points:

- The kernel **does not** touch the filesystem directly. All I/O
  goes through the HAL.
- The HAL only runs in a Worker, because that is where
  `createSyncAccessHandle()` (the synchronous-access path) is
  available.
- The kernel worker holds exclusive write rights (`readwrite`
  mode); app workers open `read-only` handles to files belonging to
  themselves only, rooted at `pkgs/<package_id>/`. The two sides
  synchronise via a `storage_invalidate` `postMessage`
  (kernel → app) — before every write the kernel closes its
  `readwrite`, dispatches `invalidate`, and the app drops its
  `readonly`. See §3.2.

## 2. Kernel-level API

[`kernel/src/storage.rs`](../kernel/src/storage.rs) exposes:

```rust
pub enum StorageError {
    NotFound,
    BadKey,
    Io,
}

pub fn read(key: &str)              -> Result<Vec<u8>, StorageError>;
pub fn write(key: &str, value: &[u8]) -> Result<(),       StorageError>;
pub fn delete(key: &str)             -> Result<(),       StorageError>;
```

Keys are arbitrary UTF-8 strings. There is no namespace enforcement
at this level — the convention is that the SDK adds the `app.id/`
prefix (see §4).

## 3. HAL implementation (Stone 11.6.7 #3 — per-key files)

[`runtime/web/storage.js`](../runtime/web/storage.js) holds:

- **In-memory map** — `Map<string, Uint8Array>` of every key/value.
  Source of truth for synchronous reads.
- **Per-key OPFS files** — every key is a separate file; `/`
  characters become directory boundaries. Examples:
  - `packages/sys.shell/manifest.toml` → `OPFS:/packages/sys.shell/manifest.toml`
  - `pkgs/com.example/notes/2026.txt`  → `OPFS:/pkgs/com.example/notes/2026.txt`
- **Async write queue** — `set` / `delete` update the in-memory
  mirror immediately (sync return); the OPFS flush is enqueued and
  drained per file. The `host_op` handler runs `drain()` before
  responding so `WriteHandle.await` stays durable.
- **Lazy SyncAccessHandle** — the kernel does not retain a
  long-lived handle; every write is open–write–close so app workers
  can hold readonly handles in parallel. Open uses retry-with-
  backoff to absorb the race against the app's invalidate handler.

### 3.1 Path encoding (`storage_path.mjs` — shared)

The key is split on `/`, and every segment is percent-encoded.
Allowed without encoding: `[A-Za-z0-9._-]`. Anything else (including
non-ASCII) is encoded as `%XX...` over UTF-8 bytes. The encoding is
defined in
[`runtime/web/storage_path.mjs`](../runtime/web/storage_path.mjs)
and used by both the kernel worker and app workers so they agree on
the OPFS path of any key.

### 3.2 Invalidation protocol (kernel → app)

Before every write / delete on a key under `pkgs/<pkg>/<userKey>`,
the `Storage` invokes `onInvalidate(packageId, userKey)`. The kernel
worker (`worker.js`) wires this to
`AppRegistry.broadcastStorageInvalidate`, which posts
`{type: "storage_invalidate", userKey}` to the package's worker (if
loaded).

The app worker shell
([`app_worker_shell.js`](../runtime/web/app_worker_shell.js))
maintains a `Map<userKey, SyncAccessHandle>` of readonly handles.
`storage_invalidate` closes the handle for the given userKey;
`userKey === null` closes all handles and resets the package
directory cache (uninstall path).

### 3.3 Direct readonly read path (Stone 11.6.7 #3)

```
app worker:
  STORAGE_GET(key) →
    cache hit (cached SyncAccessHandle)? ──yes──→ sync read, future READY (~0.1 ms)
    cache miss?                          ──→ host_op to kernel (~2 ms)
                                              + warmStorageHandle(key) async (background;
                                                handle in cache for the next read)
```

Design target: warm-cache read ≤ 0.5 ms; cold read ≤ 2 ms (matches
pre-Stone 11.6.7).

### 3.4 Lifecycle

1. `worker.js::bootKernel` calls `await storage.load()` before
   `WebAssembly.instantiate` + `akiva_boot`. `load()`:
   - `navigator.storage.getDirectory()` → root handle.
   - **Migration:** if `akiva-store.v1.bin` exists at the root,
     parses entry-by-entry → writes each key into its own file →
     `removeEntry` of the old blob.
   - **Walk:** recursive descent of the tree, populating the
     in-memory mirror via `getFile()` + `arrayBuffer()` (async,
     no handle lock).
2. Reads/writes themselves: `get` is sync from the mirror;
   `set / delete` updates the mirror sync, then enqueues a flush.
3. The flush pump asynchronously opens a `SyncAccessHandle` on the
   target file (with backoff retry if the app holds a readonly
   handle), writes the bytes, closes.
4. **Failover:** if the leader dies, the browser releases handles;
   the new leader runs `load()` again, walks the tree, populates
   the mirror.

### 3.5 Legacy format (migration source)

The legacy file `akiva-store.v1.bin` (little-endian):

```
offset  size    content
0       4       magic = "AKSV" (0x56534b41 LE)
4       4       version = 1
8       4       entry_count
12      *       entries:
                  key_len: u32
                  key:     UTF-8 bytes
                  val_len: u32
                  val:     raw bytes
```

`Storage._parseLegacyBlob` reads this format in `load()` and
migrates entry-by-entry into the new layout. After successful
migration the legacy file is removed.

### 3.6 Fallback

If `navigator.storage.getDirectory()` throws (e.g. an OPFS-less
browser), the HAL drops into **in-memory only** mode. Reads and
writes still succeed, but nothing survives a refresh. This is
flagged via `Storage.persistent = false` and a single warning is
logged. The async write queue does not run in this mode.

## 4. Per-package namespacing

At the kernel level the namespace is flat. Separation between apps
is **convention** at the SDK level:

- `sdk-rust::storage` keys are auto-prefixed with the package id by
  the host (see §8.1) — `"note"` from `com.example` becomes
  `pkgs/com.example/note` on disk.
- The framework reserves a `sys/` prefix for internal use
  (`sys/boot_count`, `sys/packages/index`).
- Installed packages live under `packages/{id}/...` — see
  `framework/src/package_manager.rs`. The PackageManager reaches
  storage through the `framework::storage::Storage` trait; at
  runtime the kernel provides a `KernelStorage` wrapper around
  `kernel::storage`, and unit tests substitute `MemoryStorage`.
- App ids are reverse DNS (`com.acme.calc`); `/` separates path
  segments.

### Reserved system keys

| key                              | owner                       | content                                          |
|----------------------------------|-----------------------------|--------------------------------------------------|
| `sys/boot_count`                 | kernel                      | `u32` LE — boot counter (smoke test)             |
| `sys/packages/index`             | framework.package_manager   | LF-separated list of package ids                 |
| `sys/permissions/{id}`           | framework.permissions       | LF-separated list of granted permissions         |
| `packages/{id}/manifest.toml`    | framework.package_manager   | manifest UTF-8 bytes                             |
| `packages/{id}/app.wasm`         | framework.package_manager   | the app's wasm module                            |
| `pkgs/{app.id}/...`              | SDK (per-app sandbox)       | app-owned storage, sandboxed                     |

### Implementation (Phase 1b + 11.6.7 #3 + follow-ups)

| capability                                                                                  | status                  | location                                                                                |
|---------------------------------------------------------------------------------------------|-------------------------|-----------------------------------------------------------------------------------------|
| `Storage::read/write/delete` trait + `MemoryStorage` for tests                              | ✅                      | [`framework/src/storage.rs`](../framework/src/storage.rs)                               |
| `kernel::storage` synchronous string→bytes API                                              | ✅                      | [`kernel/src/storage.rs`](../kernel/src/storage.rs)                                     |
| HAL over OPFS — per-key files, in-memory mirror, async write queue                          | ✅ Stone 11.6.7 #3      | [`runtime/web/storage.js`](../runtime/web/storage.js)                                   |
| Migration `akiva-store.v1.bin` → per-key layout                                             | ✅ Stone 11.6.7 #3      | `Storage._migrateLegacyIfPresent`                                                       |
| Shared path encoding (kernel + apps)                                                        | ✅ Stone 11.6.7 #3      | [`runtime/web/storage_path.mjs`](../runtime/web/storage_path.mjs)                       |
| OPFS-unavailable → in-memory + `persistent=false`                                           | ✅                      | `Storage.persistent`                                                                    |
| Kernel-only writes (exclusive `readwrite` handle, brief acquire-write-close)                | ✅ Stone 11.6.7 #3      | `Storage._writeFile`                                                                    |
| App-side direct readonly handles (Direct read path)                                         | ✅ Stone 11.6.7 #3      | [`runtime/web/app_worker_shell.js`](../runtime/web/app_worker_shell.js)                 |
| `storage_invalidate` postMessage on write/delete/uninstall                                  | ✅ Stone 11.6.7 #3      | `AppRegistry.broadcastStorageInvalidate`                                                |
| SDK per-app `storage::{read, write, remove}` with automatic prefix                          | ✅ Stone 11.6c          | [`sdk-rust/src/storage.rs`](../sdk-rust/src/storage.rs)                                 |
| `list_keys(prefix)` / prefix scan                                                           | ✅ (post-Stone 11.6.7)  | `STORAGE_LIST = 0x0114`; SDK `storage::list_keys`                                        |
| Transactions (atomic multi-write)                                                           | ❌                      | not planned for v0.x                                                                    |
| Per-app quota + enforcement                                                                 | ❌ Phase 10+            | OPFS uses a browser-wide quota; per-app caps deferred                                   |
| Encryption at rest                                                                          | ❌ post-MVP             | OPFS provides origin isolation only                                                     |

## 5. Smoke test

[`kernel::bump_boot_counter`](../kernel/src/lib.rs) runs on every
`akiva_boot`:

1. Reads `sys/boot_count` (4 bytes, `u32` LE).
2. Increments by 1.
3. Writes back.
4. Logs `kernel: boot #N (prev=M)`.

To verify storage manually: open DevTools, see `kernel: boot #1
(prev=0)` on the first run; refresh and watch the number climb.
Open a second tab as a follower — the counter does **not** advance
(only the leader boots). Close the leader → the second tab
promotes → `boot #N+1`.

## 6. Limits (what is **not** working yet)

- ~~Whole-file rewrite on every write~~ — ✅ resolved in Stone
  11.6.7 #3. Each key is a separate file; writes touch only the
  file for that key.
- **No transactions.** Two consecutive `write()`s may show an
  intermediate state if the worker crashes mid-sequence. Strict
  consistency against crashes waits on a real engine.
- **No iteration except `list_keys(prefix)`.** A full-scan / cursor
  API is not in scope.
- **No quota enforcement.** The kernel `MemoryManager` does not
  block writes by package. OPFS itself respects the browser quota,
  but per-app caps are deferred.
- **No encryption.** OPFS is protected by origin isolation only.
  Sensitive material (tokens, credentials) is expected to use a
  dedicated credential store in a later phase.
- **Async-flush durability gap.** A write whose app worker crashes
  immediately after seeing `READY` could lose its bytes if the tab
  closes before `drain()` finishes. The `host_op` channel
  compensates: `_handleHostOp` awaits `this._storage.drain()`
  before sending `host_op_done`, so `WriteHandle.await` only
  resolves after the OPFS flush has completed.

## 7. Implementation notes

- The HAL copies the value twice: once in
  `new Uint8Array(memory.buffer, vptr, vlen).slice()` (because a
  view into wasm memory is invalidated by `memory.grow`) and once
  more inside `Storage.set` (to defend the caller against external
  mutation). Not optimal but clean. A single-copy refactor is
  available if `Storage.set` shows up in profiles.
- `hal_storage_read` returns a buffer the kernel must free — the
  HAL allocates via `akiva_alloc` and the kernel (in
  `storage::read`) calls `host_dealloc` after copying into a
  `Vec<u8>`.
- `storage.close()` is invoked on worker `unload`. This is best
  effort; the browser releases handles either way, but an explicit
  flush narrows the window in which a final write could be lost.

## 8. SDK per-app storage (Stone 11.6a-c — May 2026)

From Stone 11.6 a/b/c, apps interact with storage through an SDK:

```rust
use akiva_sdk::storage;

storage::write("note", b"hello");                  // fire-and-forget
let handle = storage::read("note");                // -> Option<ReadHandle>
storage::remove("note");                           // fire-and-forget
let ids = storage::list_keys("notes/").await?;     // prefix scan
```

### 8.1 Per-package sandbox (host side)

The host (`AppRegistry._dispatchStorage` in
`runtime/web/app_registry.js`) prepends a prefix to every user key:

```
pkgs/{package_id}/{user_key}
```

Example: `com.example.notes` writing `"note"` ends up under
`pkgs/com.example.notes/note` in OPFS. A different app writing to
the same `user_key` does not see the first app's data. The sandbox
is enforced by `AppRegistry`; the SDK itself does not add the prefix.

### 8.2 Wire format (`host_op` channel)

See [`framework/src/wire/op_kinds.rs`](../framework/src/wire/op_kinds.rs)
in `pub mod io`:

| op                | code     | args                                         | result                                     |
|-------------------|----------|----------------------------------------------|--------------------------------------------|
| `STORAGE_GET`     | `0x0103` | `[key utf8...]`                              | `[present:u8, value...]`                   |
| `STORAGE_SET`     | `0x0104` | `[key_len:u32 LE, key, value]`               | `[]`                                       |
| `STORAGE_DELETE`  | `0x0105` | `[key utf8...]`                              | `[]`                                       |
| `STORAGE_LIST`    | `0x0114` | `[prefix utf8...]`                           | `[count:u32 LE, (key_len:u16 LE, key){count}]` |

The `present` byte in `STORAGE_GET` distinguishes "key absent"
(`=0`, no value bytes) from "key exists with empty value" (`=1`,
zero value bytes). This lets `Option<Vec<u8>>` on the SDK side stay
precise.

### 8.3 Capability gate

`STORAGE_*` ops require `akiva.permission.STORAGE_SANDBOX`. The
grant is **automatic** for any manifest declaring it (no UI
confirmation; the app declares, the PackageManager grants).
Enforcement: `AppRegistry._handleHostOp` checks before dispatching
to `_dispatchStorage`. Apps without the grant get a `STATUS::ERROR`
with a UTF-8 reason.

### 8.4 Direct-readonly path (Stone 11.6.7 #3)

`STORAGE_GET` on the app side can be satisfied directly from OPFS
without a kernel round-trip. The app worker shell opens a
`read-only` `SyncAccessHandle` to the file for the key (under
`pkgs/<package_id>/<userKey>`) into a lazy cache:

- **Cache hit** — sync read from the handle; the future becomes
  `READY` immediately (~0.1 ms).
- **Cache miss** — falls through to `host_op` (the kernel is the
  in-memory mirror); in parallel an async open populates the cache
  for the next read.
- **Invalidation** — every kernel write/delete dispatches
  `storage_invalidate {userKey}`, which closes the cached handle.
  The next read re-opens asynchronously from a fresh file.

Design tradeoff: warm reads are sub-millisecond; the first read for
each key (cold) pays the `host_op` cost (~2 ms) once, then stays
fast.

### 8.5 Multi-frame deferred reads (limitation)

`storage::read` cannot be sync across the cross-worker boundary —
the app worker has to return to the JS event loop so the
`host_op_done` can arrive. Hence the API:

```rust
let handle = storage::read("k");        // returns Option<ReadHandle> — fast
// In on_render, polled until ready:
if let Some(h) = &self.loading {
    if let Some(result) = h.poll() { ... }
}
```

See [`docs/sdk-rust.md §19.1`](sdk-rust.md) for the full background
and the planned fix (embassy executor — once it lands, the handle
pattern goes away in favour of `let bytes = storage::read(k).await`).

### 8.6 Fire-and-forget writes — closed (Stone 11.6.7 #1)

From Stone 11.6.7 #1, `storage::write` returns
`WriteHandle: Future<Output = Result<(), StorageError>>`. Apps that
require durability `await`; the kernel's `host_op` only responds
after `storage.drain()` confirms the OPFS flush. Dropping the
handle without `await` preserves the pre-11.6.7 fire-and-forget
behaviour. See
[`sdk-rust/src/storage.rs::WriteHandle`](../sdk-rust/src/storage.rs).

### 8.7 Implementation status (Stone 11.6a-c + 11.6.7 #1, #3)

| capability                                                              | status                | location                                                                       |
|-------------------------------------------------------------------------|-----------------------|--------------------------------------------------------------------------------|
| SDK per-app `storage::{read, write, remove}` with auto sandbox           | ✅ Stone 11.6c        | [`sdk-rust/src/storage.rs`](../sdk-rust/src/storage.rs)                        |
| `storage::list_keys(prefix)` (`STORAGE_LIST`)                            | ✅ post-11.6.7         | same                                                                            |
| Host-side sandbox enforcement (`pkgs/{id}/...` prefix)                   | ✅ Stone 11.6b        | [`runtime/web/app_registry.js::_dispatchStorage`](../runtime/web/app_registry.js) |
| Wire ops `STORAGE_GET / SET / DELETE / LIST`                             | ✅ Stone 11.6a + post  | [`framework/src/wire/op_kinds.rs`](../framework/src/wire/op_kinds.rs)          |
| Cross-worker `host_op` round-trip                                        | ✅ Stone 11.4c-3       | `AppRegistry._handleHostOp`                                                    |
| `STORAGE_SANDBOX` capability gate                                        | ✅                    | per-call check in `_handleHostOp`                                              |
| Multi-frame `ReadHandle` pattern                                         | ⚠ workaround           | `sdk-rust/src/storage.rs`                                                      |
| `WriteHandle` (durability)                                               | ✅ Stone 11.6.7 #1    | same                                                                            |
| Direct readonly OPFS handles in app worker                               | ✅ Stone 11.6.7 #3    | [`runtime/web/app_worker_shell.js`](../runtime/web/app_worker_shell.js)        |
| Per-key OPFS file layout + migration                                     | ✅ Stone 11.6.7 #3    | [`runtime/web/storage.js`](../runtime/web/storage.js)                          |
| Fluent `let bytes = storage::read(k).await` (no handle pattern)         | ❌ blocked on §19.1   | TODO                                                                            |
| Per-app quota                                                            | ❌ Phase 10+           |                                                                                 |
