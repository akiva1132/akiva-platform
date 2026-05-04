# Akiva ABI — the contract between Runtime and Applications

## 1. Purpose

The ABI defines how an app's wasm component talks to the
runtime. A stable, documented, version-tracked contract,
described in **WIT (WebAssembly Interface Types)** of the
Component Model.

## 1.1 Two layers: v0 "raw" ↔ v1 "component"

The documentation is split between two states:

| layer                       | state | where defined  |
|-----------------------------|-------|----------------|
| **v0 — Raw exports**        | The current state through Phase 14. Apps are plain `wasm32-unknown-unknown` modules exporting `extern "C"` functions. No Component Model, no `wit-bindgen`. | §2 below |
| **v1 — Component Model**    | The target. Apps are Components that export the `akiva:app/guest` world and import `akiva:app/host`.                              | §3 onward |

The reason for the dual layer: browser wasm still has no
built-in Component Model runtime. Adopting `wit-bindgen` early
forces toolchain and glue work for every language. v0 is the
interim contract that lets us boot the platform in Rust without
external dependencies, while v1 stays the shape of the future.

The transition from v0 to v1 will be additive, not breaking:
the WIT interfaces in §5 are designed so the raw v0 ABI maps
1:1 onto them (the call families are identical).

## 1.2 ABI versioning policy (Stone 11.6.8 #5)

`ABI_VERSION` is declared on both sides — `sdk-rust/src/lib.rs`
and `framework/src/lib.rs` — and checked at instantiation; a
mismatch fails loudly. This section pins **how and when** the
version bumps, what counts as breaking, and who approves it.

### 1.2.1 In-scope of the ABI

Things that, when changed, require a version bump:

- **Extern function signatures** on both sides — `akiva_app_*`
  exports (kernel→app) and `akiva_host_*` / `hal_app_*`
  imports (app→host).
- **`op_kind` constants** — fire kinds, io kinds
  (`STORAGE_GET`, `QUERY_PACKAGES`, …), cmd kinds (CREATE,
  RESIZE, …).
- **Status code constants** — `POLL_STATUS::PENDING/READY/ERROR`,
  future status values, `STATUS_BUSY` (post-11.6.8 #2).
- **Wire-format byte layouts** — intent encoding (§2.5), paint
  encoding (§2.7), saved-state, pkgs blob, result blob,
  capability list. Changing field order, alignment, or prefix
  length is MAJOR.
- **Capability constants** — `akiva.permission.*` strings in
  the manifest.
- **Public `MAX_*_BYTES` constants** — `Intent::MAX_WIRE_BYTES`,
  etc. Lowering a limit is breaking; raising one is additive.

### 1.2.2 NOT in-scope of the ABI

Changes that don't require a bump:

- **SDK Rust ergonomics** — function names, error types, doc
  comments, helper signatures. `WriteHandle` got `must_use` in
  Stone 11.6.7 #1 without a bump because the binary signature
  did not change.
- **Internal kernel implementation** — kernel internals,
  framework internal logic, `runtime/web` JS internals,
  scheduler tweaks, watchdog thresholds.
- **Test harness** — test fixtures, mock implementations,
  vitest helpers.
- **Documentation** — text in `docs/`, README, code comments,
  log messages.

### 1.2.3 Bump rules

**PATCH** (`0.X.Y` → `0.X.(Y+1)`): bug fix in the
implementation, contract unchanged. Frozen during the 0.X
era — we won't use it; reserved for 1.0+.

**MINOR** (`0.X.Y` → `0.(X+1).0`): additive change. Examples:

- A new op_kind added to the table.
- A new extern import (apps that don't call it keep working).
- A new capability string (apps that don't request it are
  unaffected).
- A field appended to a wire format documented as "may be
  extended" (an old decoder ignores the trailing bytes).
- A new status code (apps that don't check it specifically see
  it as a fallback ERROR).

Apps built on MINOR-N work on MINOR-(N+k) without rebuild.
Apps built on MINOR-(N+k) are **not** guaranteed to work on
MINOR-N (they may rely on a primitive that didn't exist).

**MAJOR** (pre-1.0: `0.X` → `0.(X+1).0`; post-1.0: `X.Y` →
`(X+1).0.0`): breaking change. Examples:

- Removal / renaming of a primitive.
- Changing the type of an arg.
- Reordering fields in a wire format.
- Changing the semantics of an existing op_kind.
- Adding a field to a wire format that wasn't documented as
  extensible.

Apps require rebuild + retest.

### 1.2.4 Promotion to 1.0

Criteria, all green before `1.0.0`:

- **Phase 11 + 12 + 13 exit criteria** all met (workers,
  governance, WindowManager).
- **Three third-party apps** in the public ecosystem used the
  ABI and survived at least one MINOR bump without breakage.
- **WIT migration plan** written down (not executed —
  planning required).
- **CHANGELOG.md** comprehensive with rationale for every
  MAJOR / MINOR since `0.1.0`.
- **Public docs** in `docs/abi.md` marked "frozen surface" (no
  longer draft).

Until 1.0 lands, MAJOR bumps in 0.X are relatively free — but
each requires a `CHANGELOG.md` entry and explicit PR review.

### 1.2.5 Review process

- **MINOR / PATCH:** one PR; one reviewer; checklist of "I
  added a CHANGELOG entry, I bumped `ABI_VERSION` on both
  sides".
- **MAJOR (pre-1.0):** PR + a short design memo in
  `docs/research-*.md` explaining why the old shape is wrong.
  The CHANGELOG entry must specify the break-compat path.
- **MAJOR (post-1.0):** requires 2 reviewers and a 30-day
  announcement window before merging to `main`. The window
  lets third-party apps respond and adjust planning.

### 1.2.6 Symbolic source of truth

The source-of-truth chain for `ABI_VERSION`:

```
sdk-rust/src/runtime.rs::ABI_VERSION  ← canonical
framework/src/lib.rs::ABI_VERSION     ← must match; mismatch = panic at load
CHANGELOG.md                           ← history of every bump with rationale
docs/abi.md (this section)             ← policy of how / when to bump
```

A change to `ABI_VERSION` not documented in CHANGELOG must be
caught in CI (TODO: tooling — Stone 11.8 or 12).

---

## 1.3 Principle: ABI narrow-by-design

ABI v0 is intentionally small. That isn't an accident; it is
policy. An ABI that goes wide too early becomes an anchor that
slows every future change, dictating decisions for the framework
layer for years.

Every extension to the ABI — whether a new app export, a new
host import, or a new wire-format field — has to pass three
gates:

1. **Proven use case.** At least one reference app is
   meaningfully impaired without the extension. "Might be
   useful later" doesn't qualify.
2. **Cross-app justification.** An extension that serves a
   single app or a niche use case belongs in the framework or
   the SDK, not the ABI. The ABI serves only what every app, or
   the runtime core, requires.
3. **Forward compat to v1.** A clear mapping exists to a WIT
   interface in §5. If the extension doesn't translate cleanly
   to the Component Model, that's a sign its shape is wrong.

The practical rule: better one missing function today than
seven unnecessary ones we'd have to drag forever. New
extensions to `hal_app_*` or paint-op opcodes get explicit PR
review and are documented in CHANGELOG (§1.2.6) and the
versions section (§10).

---

## 2. App ABI v0 — raw exports (Phase 2b)

### 2.1 Runtime model

Every app is a stand-alone wasm module the host (`worker.js`)
loads alongside the kernel wasm. The kernel **does not**
identify the app inside its own address space — it sends
requests to the host through imports named `hal_app_*`, and
the host routes them to the right app instance. Immediate
benefits: isolation between apps, independent memory growth,
crash containment.

```
  Kernel wasm           Worker JS host             App wasm (one per package)
  ───────────           ──────────────             ─────────────────────────
  WasmActivity   ──►    hal_app_dispatch    ──►    akiva_app_dispatch(cmd, payload)
  (framework)           (routes by handle)          (app-side)
                  ◄──   bytes via               ◄── akiva_app_get_result → *u8
                        hal_app_get_result
```

The `handle` is a non-zero `u32` the host allocates in
`hal_app_register` and uses as the key for the tuple
`(package_id, class, wasm_instance)`. The kernel has no
visibility into what the handle stands for.

### 2.2 Exports the app must provide

Every app must export the wasm `memory` (rustc does this
automatically for `wasm32-unknown-unknown`) and the following
functions (C ABI). `runtime/web/app_registry.js::REQUIRED_EXPORTS`
checks the list at load time; apps missing anything are rejected
before the first lifecycle call.

#### 2.2.1 Memory ABI (always required)

```rust
// The host uses this to inject data into the app (intents, saved state, dispatch payloads).
extern "C" fn akiva_app_alloc(size: u32) -> *mut u8;
extern "C" fn akiva_app_dealloc(ptr: *mut u8, size: u32);

// ABI version — null-terminated UTF-8 ("0.12.0").
extern "C" fn akiva_app_abi_version() -> *const u8;
```

#### 2.2.2 Unified dispatch entry (P0 #4 async-resumable)

The exports introduced in Stone 11.4b became async-resumable in
P0 #4 (May 2026). Three exports replace the previous pair:

```rust
// Hand-off into the SDK runtime. dispatch_id is allocated by
// the host (the AppRegistry's per-package monotonic counter)
// and is the stable handle the host uses for every follow-up
// call related to this dispatch (poll_dispatch + get_result).
// Every payload begins with [handle: u32 LE, ...] — the
// cmd_kind table at §2.2 lists the per-cmd remainder.
//
// Returns a status code:
//   0   — accepted; host should drive to terminal status via
//         poll_dispatch.
//   !=0 — hard refusal (bad payload, unknown cmd_kind, OOM
//         during slot construction). The host treats it as
//         ack-only and does NOT poll.
extern "C" fn akiva_app_dispatch(
    dispatch_id: u64,
    cmd_kind:    u32,
    payload_ptr: *const u8,
    payload_len: u32,
) -> u32;

// Drive a dispatch one poll forward. Returns packed (status, len):
//   * status (low 32 bits):
//       0 PENDING — wasm is parked on a host_op; the host
//                   should call again after `host_op_done`
//                   arrives at the shell.
//       1 READY   — bytes (if any) available via get_result.
//       2 ERROR   — the future trapped, wire bug, or unknown
//                   dispatch_id.
//   * len (high 32 bits): byte count for READY/ERROR; 0 for
//     PENDING and ack-only dispatches.
extern "C" fn akiva_app_poll_dispatch(dispatch_id: u64) -> u64;

// Two-phase fetch of the result bytes for a READY dispatch.
// `*out_len_ptr` receives the length; the returned pointer +
// length are valid until this single call evicts the
// dispatch_id (single-shot). For ack-only dispatches
// (lifecycle, input, intent, …) the pointer may be null and
// the length 0.
//
// DESTROY is special: get_result also tears down the slot once
// the bytes are drained (always 0 for destroy). This delays
// slot teardown until AFTER any pending future has dropped its
// borrows into the slot.
extern "C" fn akiva_app_get_result(
    dispatch_id: u64,
    out_len_ptr: *mut u32,
) -> *const u8;
```

`cmd_kind` is a stable integer from
`framework::wire::op_kinds::cmd::*` (mirrored in
`akiva_sdk::op_kinds::cmd::*` and
`runtime/web/op_kinds.mjs::CMD`). The ranges are pinned for
all time:

| range          | category   | ops                                                       |
|----------------|------------|-----------------------------------------------------------|
| `0x01..=0x0F`  | Lifecycle  | CREATE / START / RESUME / PAUSE / STOP / DESTROY          |
| `0x10..=0x1F`  | Input      | POINTER / KEY / RESIZE / TEXT_INPUT (`0x13`)              |
| `0x20..=0x2F`  | Render     | RENDER                                                    |
| `0x30..=0x3F`  | State      | SAVE_STATE / RESTORE_STATE                                |
| `0x40..=0x4F`  | Intent     | NEW_INTENT / ACTIVITY_RESULT                              |
| `0x50..=`      | reserved   | streams / pushed events                                   |

Per-cmd payload layout (each starts with `[handle: u32 LE, ...]`):

| cmd_kind                           | payload after handle                                                     |
|------------------------------------|--------------------------------------------------------------------------|
| CREATE                             | `class: utf8` (host-augmented — kernel passes only `[handle]`)           |
| START / RESUME / PAUSE / STOP / DESTROY | none                                                                |
| POINTER                            | `x: f32, y: f32, button: u32, phase: u32, ts: f64` (24 bytes)            |
| KEY                                | `code: u32, modifiers: u32, phase: u32, ts: f64` (20 bytes)              |
| TEXT_INPUT                         | `len: u16 LE, text: utf8`                                                |
| RESIZE                             | `width: u32, height: u32, density: f32` (12 bytes)                       |
| RENDER                             | `ts: f64` (8 bytes); result = paint-op stream                            |
| SAVE_STATE                         | none; result = `SavedState::encode` bytes                                |
| RESTORE_STATE                      | `state: bytes`                                                           |
| NEW_INTENT                         | `intent: encoded Intent`                                                 |
| ACTIVITY_RESULT                    | `request_code: u32, result: encoded ActivityResult`                      |

`dispatch_id` is a monotonic counter the AppRegistry allocates
each time `hal_app_dispatch` is called. The id keys both the
host-side `_dispatches` map and the app-side per-handle pending
registry — both sides agree on the same value, which makes
debugging trivial (the same int across the two pools).

The buffer returned by `akiva_app_get_result` is app-owned and
valid until the very call that consumes it (single-shot). For
RENDER, the paint buffer is reused per handle; the pointer
returned now is valid until the next RENDER on the same handle.
SAVE_STATE / ack-only buffers have the same lifetime.

#### 2.2.3 Async-resumable dispatch — pending → host_op_done → re-poll

P0 #4 (May 2026) changed the protocol from "sync-or-handle"
to async-resumable so `storage::read().await` can work
transparently:

1. The app worker shell receives
   `{type: "dispatch", dispatch_id, cmd_kind, payload}` from
   the AppRegistry.
2. The shell copies the payload into wasm memory and calls
   `akiva_app_dispatch(dispatch_id, cmd, ptr, len)`. The
   runtime in wasm builds a future and polls once.
3. The shell calls `akiva_app_poll_dispatch(dispatch_id)`:
   - **READY** — fetches the bytes (if any) via
     `get_result`; sends
     `{type: "dispatch_done", dispatch_id, bytes}` to the
     AppRegistry.
   - **PENDING** — stores in `pendingDispatches` Map; **does
     not** send dispatch_done.
   - **ERROR** — sends dispatch_done with null bytes; an
     optional trapped notice may follow.
4. For pending dispatches: when `host_op_done` arrives at the
   shell (storage read resolved, network reply, etc.), the
   shell re-polls **every** pending dispatch. Any of them
   could have been parked on this future. This is O(n) iter
   — but n ≤ 1 in practice (the framework allows only one
   dispatch per handle at a time).
5. When a poll returns READY, the §3 flow continues normally.

The AppRegistry on the kernel side is unchanged — it keeps
seeing a `dispatch_id` go out and a `dispatch_done` come back;
it just waited longer. Kernel polling through
`hal_app_poll_future` keeps seeing PENDING until the shell
sends dispatch_done.

### 2.3 Imports (host → app)

The host exposes imports under module name `env`. As of Stone
11.3, the import surface is the **unified ABI**: three
multiplexer functions + four lightweight resource lookups.
Every host-mediated operation routes through `akiva_host_fire`
(fire-and-forget), `akiva_host_io_start` +
`akiva_host_io_poll` (async with a future ID), or one of the
lightweight resource lookups. The mapping to fixed `op_kind`
constants is documented in §2.3.2.

#### 2.3.1 Resource lookups (legacy, kept)

Four readonly per-package functions that don't fit the fire/io
shape (no side effect, no handle context, no async). Always
mounted, no gating:

```rust
// Resolve @color/name for the calling package. Returns 0xAARRGGBB.
// Undefined names return 0xFFFF00FF (magenta) and log a host-side
// warning — missing refs surface visually, not silently.
extern "C" fn akiva_host_resource_color(name_ptr: *const u8, name_len: u32) -> u32;

// Resolve @string/name. Writes UTF-8 bytes into [out_ptr, out_ptr+out_cap)
// and returns the *actual* length. If the returned length exceeds out_cap,
// nothing is written — the caller reallocates and tries again. Undefined
// names write "⟨missing:NAME⟩".
extern "C" fn akiva_host_resource_string(
    name_ptr: *const u8, name_len: u32,
    out_ptr: *mut u8,    out_cap:  u32,
) -> u32;

// Probes: 1 if the ref is registered, 0 otherwise.
extern "C" fn akiva_host_resource_has_color(name_ptr: *const u8, name_len: u32) -> u32;
extern "C" fn akiva_host_resource_has_string(name_ptr: *const u8, name_len: u32) -> u32;

// Stone 13.0prep C — resolve @drawable/name to a stable per-package
// drawable_id. The host built the index at install / boot by scanning
// res/drawable/*.png and decoding each PNG to an ImageBitmap; this
// lookup is an O(1) Map fetch. Id 0 is the missing-drawable sentinel
// (the renderer paints a magenta placeholder).
extern "C" fn akiva_host_resource_drawable(name_ptr: *const u8, name_len: u32) -> u32;
```

#### 2.3.2 Unified ABI multiplexers (Stone 11.3)

Three imports route every host-mediated operation (log,
finish, start_activity, set_result, text_width, query_packages,
storage, …) through an `op_kind` integer. The args layout per
op_kind is known on both the SDK side and the host side; the
encoder/decoder is matched 1:1 between
`framework/src/wire/op_kinds.rs` (kernel side) and
`sdk-rust/src/op_kinds.rs` (app side, mirror) — a unit test
across crates verifies equality.

```rust
// Fire-and-forget — the host performs the operation without
// returning data. Return value: 0 = ok, !=0 = error (unknown
// op_kind, malformed args, capability not granted). Used by
// LOG, FINISH, START_ACTIVITY, START_ACTIVITY_FOR_RESULT,
// SET_RESULT, etc.
extern "C" fn akiva_host_fire(
    op_kind:  u32,
    args_ptr: *const u8,
    args_len: u32,
) -> u32;

// Async start — the host allocates a future_id and starts
// processing. Return: non-zero future_id on success, 0n on
// failure (unknown op_kind, capability not granted, malformed
// args). Used by TEXT_WIDTH, QUERY_PACKAGES, STORAGE_GET/SET,
// HTTP_FETCH, WS_*, …
extern "C" fn akiva_host_io_start(
    op_kind:  u32,
    args_ptr: *const u8,
    args_len: u32,
) -> u64;

// Poll until the future resolves. Return packed (status: u32
// low, length: u32 high). status: 0=PENDING, 1=READY (length
// bytes written to out_ptr), 2=ERROR. If length > out_cap —
// nothing is written; the caller allocates a buffer of the
// required size and calls again (two-phase contract — the
// future is retained until the call succeeds). After a
// successful call the future is evicted from the registry.
extern "C" fn akiva_host_io_poll(
    future_id: u64,
    out_ptr:   *mut u8,
    out_cap:   u32,
) -> u64;
```

**op_kind layout** (full detail in
`framework/src/wire/op_kinds.rs`):

| range            | transport       | constants                                                                                          |
|------------------|-----------------|----------------------------------------------------------------------------------------------------|
| `0x0001..0x00FF` | `fire`          | `LOG=0x01`, `FINISH=0x02`, `START_ACTIVITY=0x03`, `START_ACTIVITY_FOR_RESULT=0x04`, `SET_RESULT=0x05`, `WS_SEND=0x06`, `WS_CLOSE=0x07`, `WT_SEND_DATAGRAM=0x08`, `WT_SEND_STREAM=0x09`, `WT_CLOSE_STREAM=0x0A`, `WT_CLOSE=0x0B`, `NOTIFY_CLOSE=0x0C`, `VIBRATE=0x0D` |
| `0x0101..0x01FF` | `io_start`/poll | `TEXT_WIDTH=0x101`, `QUERY_PACKAGES=0x102`, `STORAGE_GET=0x103`, `STORAGE_SET=0x104`, `STORAGE_DELETE=0x105`, `TIMER_SLEEP=0x106`, `SHOW_DIALOG=0x107`, `HTTP_FETCH=0x108`, `WS_OPEN=0x109`, `WS_RECV=0x10A`, `WT_OPEN=0x10B`, `WT_RECV_DATAGRAM=0x10C`, `WT_OPEN_BIDI=0x10D`, `WT_RECV_STREAM=0x10E`, `NOTIFY_REQUEST_PERMISSION=0x10F`, `NOTIFY_POST=0x110`, `CLIPBOARD_READ_TEXT=0x111`, `CLIPBOARD_WRITE_TEXT=0x112`, `GEO_GET_POSITION=0x113`, `STORAGE_LIST=0x114` |
| `0x0200..`       | reserved        | streams / pushed events                                                                            |

Args layouts per op_kind (LE byte stream — encoded inline in
the dispatcher) are tabulated here. Wire shapes for every
Phase 14 capability (Notifications / Clipboard / Vibration /
Geolocation / WebSocket / WebTransport) are pinned in this
table:

| op_kind                                    | args                                                                                                                                                                                                                                                                                            | result                                                                                                                                       |
|--------------------------------------------|-------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|----------------------------------------------------------------------------------------------------------------------------------------------|
| `FIRE::LOG`                                | `[level: u32 LE \| msg_bytes…]`                                                                                                                                                                                                                                                                  | —                                                                                                                                            |
| `FIRE::FINISH`                             | `[handle: u32 LE]`                                                                                                                                                                                                                                                                              | —                                                                                                                                            |
| `FIRE::START_ACTIVITY`                     | `[handle: u32 LE \| intent_bytes…]`                                                                                                                                                                                                                                                             | —                                                                                                                                            |
| `FIRE::START_ACTIVITY_FOR_RESULT`          | `[handle: u32 LE \| request_code: u32 LE \| intent_bytes…]`                                                                                                                                                                                                                                     | —                                                                                                                                            |
| `FIRE::SET_RESULT`                         | `[handle: u32 LE \| result_bytes…]`                                                                                                                                                                                                                                                             | —                                                                                                                                            |
| `IO::TEXT_WIDTH`                           | `[size: f32 LE \| text_bytes…]`                                                                                                                                                                                                                                                                 | `[width: f32 LE]`                                                                                                                            |
| `IO::QUERY_PACKAGES`                       | `[]`                                                                                                                                                                                                                                                                                            | `pkgs_wire blob` (see `wire/pkgs.rs`)                                                                                                        |
| `IO::STORAGE_GET`                          | `[user_key: utf8…]`                                                                                                                                                                                                                                                                             | `[present: u8, value…]`                                                                                                                      |
| `IO::STORAGE_SET`                          | `[key_len: u32 LE, key, value]`                                                                                                                                                                                                                                                                 | `[]`                                                                                                                                         |
| `IO::STORAGE_DELETE`                       | `[user_key: utf8…]`                                                                                                                                                                                                                                                                             | `[]`                                                                                                                                         |
| `IO::STORAGE_LIST`                         | `[prefix: utf8…]`                                                                                                                                                                                                                                                                               | `[count: u32 LE, (key_len: u16 LE, key){count}]`                                                                                              |
| `IO::TIMER_SLEEP`                          | `[delay_ms: u32 LE]`                                                                                                                                                                                                                                                                            | `[]` (READY after delay)                                                                                                                     |
| `IO::SHOW_DIALOG`                          | `[handle: u32 LE \| title_len: u16 LE \| title: utf8 \| body_len: u16 LE \| body: utf8 \| btn_count: u8 \| (btn_len: u16 LE \| btn: utf8)*]`                                                                                                                                                    | `[kind: u8, btn_idx: u8]` — `kind == 0` → `Button(btn_idx)`, `kind == 1` → `Dismissed`                                                       |
| `IO::HTTP_FETCH` (14.1)                    | `[method: u8 \| url_len: u16 LE \| url: utf8 \| hdr_count: u16 LE \| (name_len: u16 LE \| name: utf8 \| val_len: u16 LE \| val: utf8)* \| body…]`. Method codes: 0=GET, 1=POST, 2=PUT, 3=DELETE, 4=PATCH, 5=HEAD. Gated by `akiva.permission.NETWORK`. CORS rules apply.                          | `[status: u16 LE \| hdr_count: u16 LE \| (headers as above) \| body…]`; ERROR = utf8 reason                                                  |
| `IO::WS_OPEN` (14.1)                       | `[url_len: u16 LE \| url: utf8 \| proto_count: u8 \| (proto_len: u16 LE \| proto: utf8)*]`. Gated by `akiva.permission.NETWORK`.                                                                                                                                                                  | `[ws_handle: u32 LE]` once `open` fires; ERROR on handshake failure                                                                          |
| `IO::WS_RECV` (14.1)                       | `[ws_handle: u32 LE]`                                                                                                                                                                                                                                                                            | `[event_kind: u8 \| data…]` — kind 0=text (data utf8), 1=binary, 2=closed (data = `[code: u16 LE \| reason: utf8…]`)                       |
| `FIRE::WS_SEND` (14.1)                     | `[ws_handle: u32 LE \| kind: u8 \| data…]` (kind 0=text, 1=binary). Silent no-op on a closed handle.                                                                                                                                                                                            | —                                                                                                                                            |
| `FIRE::WS_CLOSE` (14.1)                    | `[ws_handle: u32 LE \| code: u16 LE \| reason_len: u16 LE \| reason: utf8…]`. Idempotent.                                                                                                                                                                                                       | —                                                                                                                                            |
| `IO::WT_OPEN` (14.2)                       | `[url_len: u16 LE \| url: utf8…]`. Gated by `akiva.permission.NETWORK`.                                                                                                                                                                                                                          | `[wt_handle: u32 LE]` once `wt.ready` resolves; ERROR on handshake failure                                                                   |
| `IO::WT_RECV_DATAGRAM` (14.2)              | `[wt_handle: u32 LE]`                                                                                                                                                                                                                                                                            | `[event_kind: u8 \| data…]` — kind 0=datagram, 1=closed (data = `[code: u32 LE \| reason: utf8…]`). Datagrams unreliable + unordered.       |
| `IO::WT_OPEN_BIDI` (14.2)                  | `[wt_handle: u32 LE]`                                                                                                                                                                                                                                                                            | `[stream_handle: u32 LE]` — opens a reliable, ordered bidi stream under the session                                                          |
| `IO::WT_RECV_STREAM` (14.2)                | `[stream_handle: u32 LE]`                                                                                                                                                                                                                                                                        | `[event_kind: u8 \| data…]` — kind 0=bytes, 1=fin (peer closed write side; data empty), 2=error (data=utf8 reason)                          |
| `FIRE::WT_SEND_DATAGRAM` (14.2)            | `[wt_handle: u32 LE \| data: bytes…]`. Best-effort.                                                                                                                                                                                                                                              | —                                                                                                                                            |
| `FIRE::WT_SEND_STREAM` (14.2)              | `[stream_handle: u32 LE \| data: bytes…]`                                                                                                                                                                                                                                                       | —                                                                                                                                            |
| `FIRE::WT_CLOSE_STREAM` (14.2)             | `[stream_handle: u32 LE \| code: u32 LE]`. Idempotent.                                                                                                                                                                                                                                          | —                                                                                                                                            |
| `FIRE::WT_CLOSE` (14.2)                    | `[wt_handle: u32 LE \| code: u32 LE \| reason_len: u16 LE \| reason: utf8…]`. Idempotent; closes every stream under the session.                                                                                                                                                                | —                                                                                                                                            |
| `IO::NOTIFY_REQUEST_PERMISSION` (14.3)     | `[]`. Gated by `akiva.permission.NOTIFICATIONS`.                                                                                                                                                                                                                                                 | `[status: u8]` — 0=Default, 1=Granted, 2=Denied. Browsers auto-deny outside user-gesture handlers.                                          |
| `IO::NOTIFY_POST` (14.3)                   | `[title_len: u16 LE \| title: utf8 \| body_len: u16 LE \| body: utf8 \| tag_len: u16 LE \| tag: utf8…]`. Empty `tag` lets the browser allocate a fresh slot per call; non-empty tag collapses duplicates.                                                                                       | `[notify_id: u32 LE]` once the browser accepts; ERROR with utf8 reason on permission denial / browser refusal                                |
| `FIRE::NOTIFY_CLOSE` (14.3)                | `[notify_id: u32 LE]`. Idempotent.                                                                                                                                                                                                                                                              | —                                                                                                                                            |
| `IO::CLIPBOARD_READ_TEXT` (14.4)           | `[]`. Gated by `akiva.permission.CLIPBOARD`.                                                                                                                                                                                                                                                     | `[text: utf8…]`; ERROR with reason on browser refusal                                                                                        |
| `IO::CLIPBOARD_WRITE_TEXT` (14.4)          | `[text: utf8…]`                                                                                                                                                                                                                                                                                  | empty (READY = success); ERROR on refusal                                                                                                    |
| `FIRE::VIBRATE` (14.5)                     | `[count: u16 LE \| (duration_ms: u32 LE){count}]`. Empty pattern cancels in-progress vibration. Gated by `akiva.permission.VIBRATE`.                                                                                                                                                            | —                                                                                                                                            |
| `IO::GEO_GET_POSITION` (14.6)              | `[]`. Gated by `akiva.permission.LOCATION`.                                                                                                                                                                                                                                                      | `[lat:f64 LE \| lon:f64 LE \| accuracy_m:f64 LE \| has_alt:u8 \| (alt:f64 LE)? \| has_speed:u8 \| (speed:f64 LE)? \| has_heading:u8 \| (heading:f64 LE)? \| ts_ms:f64 LE]` |

**Capability gating moved into the dispatcher.** Through Stone
10.3b gating happened at instantiate time (LinkError on a
missing import). Since Stone 11.3 gating happens per-call: the
import itself is always mounted, but the dispatcher checks
grants before serving an op_kind that requires a capability.
An app without permission → an error return code (1 or 2,
depending on the path), and a warning in the log. The reason
for the choice: one import serves ~250 op_kinds, some auto and
some gated; filter at call site, not at link.

**Exports the app exposes for intent / result delivery** (these
are not multiplexed — they stay as separate exports):

```rust
// Stone 9.4 — SingleTop / SingleTask / SingleInstance reuse
// fires this with the fresh intent. handle identifies the
// existing activity; ptr/len — a blob from `Intent::encode`
// the host copies into app memory before the call (symmetric
// with `akiva_app_restore_state`). The activity stays
// `Resumed` and does not pass through on_create / on_start /
// on_resume.
extern "C" fn akiva_app_new_intent(handle: u32, ptr: *const u8, len: u32);

// Stone 9.5 — caller-side hand-off from a for-result launch.
// Bytes encode an ActivityResult (see §2.5.2). Fires AFTER
// the callee's on_destroy, BEFORE the caller's
// Paused → Resumed edge (Android parity).
extern "C" fn akiva_app_activity_result(
    handle: u32, request_code: u32, ptr: *const u8, len: u32,
);
```

**In v1 (Component Model)** every op_kind maps to a WIT
interface: `LOG` → `log.write`, `TEXT_WIDTH` →
`text.measure-width`, `START_ACTIVITY` →
`activity-api.start-activity`, etc. The multiplexers go away —
but the semantics are unchanged (futures, fire-and-forget,
status codes), so the translation layer is clean.

#### 2.3.3 Capability gating (Stone 10.3 → 11.3)

The `PermissionController` allocates per-package permissions at
install time and the framework pushes them to the host through
`hal_host_set_grants`. The Rust-side source of truth is
[`framework/src/permissions.rs`](../framework/src/permissions.rs)
(`AUTO_IMPORTS` + `imports_for_capability`) with its mirror in
`runtime/web/app_registry.js` (`AUTO_IMPORTS` +
`CAPABILITY_IMPORTS`); vitest + Rust tests verify the
symmetry.

| capability                          | imports mounted at link time          | op_kinds gated at dispatch time                                                        |
|-------------------------------------|---------------------------------------|----------------------------------------------------------------------------------------|
| `auto` (no declaration)             | 4 resource lookups + 3 multiplexers   | `FIRE::LOG`, `FIRE::FINISH`, `FIRE::SET_RESULT`, `IO::TEXT_WIDTH`                      |
| `akiva.permission.LAUNCH_ANY_APP`   | —                                     | `FIRE::START_ACTIVITY`, `FIRE::START_ACTIVITY_FOR_RESULT`                              |
| `akiva.permission.QUERY_PACKAGES`   | —                                     | `IO::QUERY_PACKAGES`                                                                   |
| `akiva.permission.STORAGE_SANDBOX`  | —                                     | `IO::STORAGE_GET / SET / DELETE / LIST`                                                |
| `akiva.permission.NETWORK`          | —                                     | `IO::HTTP_FETCH`, `IO::WS_*`, `IO::WT_*` + `FIRE::WS_*` + `FIRE::WT_*`                  |
| `akiva.permission.NOTIFICATIONS`    | —                                     | `IO::NOTIFY_REQUEST_PERMISSION`, `IO::NOTIFY_POST`, `FIRE::NOTIFY_CLOSE`               |
| `akiva.permission.CLIPBOARD`        | —                                     | `IO::CLIPBOARD_READ_TEXT`, `IO::CLIPBOARD_WRITE_TEXT`                                  |
| `akiva.permission.VIBRATE`          | —                                     | `FIRE::VIBRATE`                                                                        |
| `akiva.permission.LOCATION`         | —                                     | `IO::GEO_GET_POSITION`                                                                 |

**Two enforcement layers:**

1. **Link time** — the import set is bounded at 7 functions
   (4 resource + 3 multiplexers). `AUTO_IMPORTS` is the set
   every app receives; no capability today adds more imports.
   If a wasm app tries to import a name that isn't there →
   `LinkError` at instantiation. This catches typos and
   imports left over from refactors.
2. **Dispatch time** — inside `akiva_host_fire` /
   `akiva_host_io_start`, the dispatcher checks grants before
   running an op_kind that needs a capability. An app without
   permission → `fire` returns `2` (denied), `io_start`
   returns `0n` (couldn't start). The warning in the log lets
   the developer see the deny.

**Why the split.** Up to 11.3 gating was link-time only (each
capability mapped to a set of imports); after 11.3, one import
serves ~250 op_kinds, mostly auto, some gated. Link-time
gating would have required a separate multiplexer per
capability — complicating the ABI without buying anything.
Dispatch-time gating keeps the import surface tiny (7 entries)
and concentrates the logic in one place that's easy to test.

### 2.4 Imports (kernel → host) — `hal_app_*`

These are imports the kernel wasm requests; the host
(`worker.js`) provides. Defined in
[`kernel/src/host.rs`](../kernel/src/host.rs):

```rust
fn hal_app_register(
    package_id_ptr: *const u8, package_id_len: usize,
    class_ptr:      *const u8, class_len:      usize,
) -> u32;  // 0 = unknown package/class
fn hal_app_unregister(handle: u32);

// ── Unified host→app dispatch (Stone 11.4b) ───────────────────────
//
// `hal_app_dispatch` is what the framework's `KernelWasmHost` calls
// for every per-callback transition (lifecycle, input, render, state,
// intent). The JS host reads `[handle: u32 LE, ...]` from the payload,
// looks up the wasm process that owns the handle, copies the payload
// into app memory, and forwards to the app's `akiva_app_dispatch`.
// CREATE special case: kernel passes only `[handle]`; the JS host
// augments with the slot's class string before forwarding.
//
// Returns the app's u64 future_id:
//   0           — sync, no result bytes (every ack-only cmd_kind).
//   non-zero    — sync, bytes available; the kernel calls
//                 `hal_app_get_result(future_id, ...)` next.
fn hal_app_dispatch(
    cmd_kind: u32, payload_ptr: *const u8, payload_len: u32,
) -> u64;

// Companion to `hal_app_dispatch`: fetch the bytes for a non-zero
// future_id (RENDER paint stream, SAVE_STATE encoded state).
// Two-phase buffer contract: returns the *needed* length; truncated
// copies trigger a retry with a larger buffer.
fn hal_app_get_result(
    future_id: u64, out_ptr: *mut u8, out_cap: u32,
) -> u32;

// Stone 9.3 — read + clear the per-handle "finish requested" flag the
// app raised via `akiva_host_fire(FIRE::FINISH, …)`. Called by
// `WasmActivity` at the end of every ctx-bearing callback; return 1
// flips `ActivityContext.finish_requested = true` in the active
// dispatch.
fn hal_app_drain_finish(handle: u32) -> u32;

// Stone 9.4 — read + clear the per-handle pending intent blob staged
// via `akiva_host_fire(FIRE::START_ACTIVITY, …)`. Two-phase buffer:
// the return value is the total encoded size; if it exceeds `out_cap`
// nothing is copied and the blob stays armed so the kernel can retry
// with a larger buffer. Zero means "nothing pending" — distinct from
// "empty intent" (which can't happen; the encoder's minimum is 17
// bytes).
fn hal_app_drain_intent(handle: u32, out_ptr: *mut u8, out_cap: u32) -> u32;

// Stone 9.5 — two drain primitives matching the Stone 9.4 shapes.
// (Host→app delivery for ACTIVITY_RESULT goes through
// `hal_app_dispatch` with cmd_kind = ACTIVITY_RESULT; no dedicated
// import.)
//
// Drain a pending start_activity_for_result request. Two-phase buffer:
// returns the total encoded size of the Intent. `rc_out` is always
// written with the request_code on a successful probe — so a retry
// (when `needed > out_cap`) finds the same value. Zero return =
// nothing pending.
fn hal_app_drain_start_for_result(
    handle: u32, out_ptr: *mut u8, out_cap: u32, rc_out: *mut u32,
) -> u32;

// Drain whatever bytes the app stashed via
// `akiva_host_fire(FIRE::SET_RESULT, …)`. Same two-phase contract;
// payload is an encoded ActivityResult (see §2.5.2). Zero return =
// nothing stashed.
fn hal_app_drain_set_result(handle: u32, out_ptr: *mut u8, out_cap: u32) -> u32;
```

### 2.5 Paint-op wire format

The app serialises its paint commands into a flat byte stream
that the kernel parses back into
[`FrameCommands`](../framework/src/compositor.rs). Every op
starts with a `u8` opcode:

| Opcode | Op                | Payload (little-endian)                                                                       |
|--------|-------------------|-----------------------------------------------------------------------------------------------|
| `0x00` | `Clear`           | `u32 rgba`                                                                                    |
| `0x01` | `FillRect`        | `f32 x, f32 y, f32 w, f32 h, u32 rgba`                                                        |
| `0x02` | `DrawText`        | `f32 x, f32 y, f32 size, u32 rgba, u32 len, <len> UTF-8 bytes`                                |
| `0x03` | `PushClip`        | `f32 x, f32 y, f32 w, f32 h`                                                                  |
| `0x04` | `PopClip`         | —                                                                                             |
| `0x05` | `PushTransform`   | `f32 dx, f32 dy, f32 scale`                                                                   |
| `0x06` | `PopTransform`    | —                                                                                             |
| `0x07` | `FillRoundRect`   | `f32 x, f32 y, f32 w, f32 h, f32 radius, u32 rgba` *(Stone 11.6.9 #1)*                        |
| `0x08` | `StrokeRect`      | `f32 x, f32 y, f32 w, f32 h, f32 width, u32 rgba` *(Stone 13.0prep B)*                        |
| `0x09` | `StrokeRoundRect` | `f32 x, f32 y, f32 w, f32 h, f32 radius, f32 width, u32 rgba` *(Stone 13.0prep B)*            |
| `0x0A` | `DrawLine`        | `f32 x0, f32 y0, f32 x1, f32 y1, f32 width, u32 rgba` *(Stone 13.0prep B)*                    |
| `0x0B` | `DrawImage`       | `u32 drawable_id, f32 x, f32 y, f32 w, f32 h` *(Stone 13.0prep C)* — id `0` paints a magenta placeholder |
| `0x0C` | `DrawForeignImage`| `u16 pkg_len, pkg utf8, u16 name_len, name utf8, f32 x, f32 y, f32 w, f32 h` *(post-13)* — cross-package drawable cache |
| `0x0D` | `SetGlobalAlpha`  | `f32 alpha` *(post-13 transition)* — multiplies the GPU-side alpha for a transform window     |

The full codec is in
[`framework/src/wasm_activity.rs`](../framework/src/wasm_activity.rs)
— `encode_op` / `decode_frame`. Additive entries get new
opcodes that continue the sequence; the decoder rejects unknown
opcodes as an error (it can't know the payload length to skip).

### 2.5.1 Intent wire format (Stone 9.4)

A flat format the SDK encodes with `Intent::encode` and the
framework decodes with `Intent::decode`. The blob travels both
directions — app→kernel through
`akiva_host_fire(FIRE::START_ACTIVITY, …)` +
`hal_app_drain_intent`, and kernel→app through
`hal_app_new_intent` + `akiva_app_new_intent`.

```
u32  action_len + <action_len> UTF-8 bytes
u32  category_count
  for each:  u32 len + <len> UTF-8 bytes
u8   has_data_uri       (0/1) + if 1: u32 len + bytes
u8   has_mime_type      (0/1) + if 1: u32 len + bytes
u8   has_target_package (0/1) + if 1: u32 len + bytes
u8   has_target_class   (0/1) + if 1: u32 len + bytes
u32  extras_count       (v0.2: must be 0 — reserved for Stone 9.5)
```

- **Max size:** 16 KiB (`Intent::MAX_WIRE_BYTES`) — checked on
  both sides; a larger blob → `IntentDecodeError::TooLarge`.
- **Min size:** 17 bytes (action of 0 bytes + 4 × flag=0 +
  extras_count=0). So `drain_intent` returning `0` ⇒
  unambiguously "nothing pending" — every real intent is ≥ 17
  bytes.
- **Extensibility:** `extras_count = 0` is a checkpoint. A
  future extras layout adds a payload of (`u32 key_len` +
  bytes + `u8 tag` + payload-by-tag). An older reader meeting
  `extras_count > 0` rejects with `ExtrasNotSupported` rather
  than getting confused on offsets. Stone 9.5 didn't need
  extras — it uses a separate `ActivityResult` (§2.5.2).
- **Identical encode on both sides:**
  `framework::intent::Intent::encode` and
  `sdk-rust::intent::Intent::encode` must remain byte-for-byte
  symmetric — like the paint codec, every change is a
  breaking event covered by §1.2.

The full codec is in
[`framework/src/intent.rs`](../framework/src/intent.rs) and its
mirror in [`sdk-rust/src/intent.rs`](../sdk-rust/src/intent.rs).

### 2.5.2 ActivityResult wire format (Stone 9.5)

The smaller of the two stone-9.5 wire formats: a `i32` code +
an opaque variable-length blob. Travels both directions —
app→kernel through `akiva_host_fire(FIRE::SET_RESULT, …)` +
`hal_app_drain_set_result`, and kernel→app through
`hal_app_activity_result` + `akiva_app_activity_result`.

```
i32  code           (little-endian — RESULT_OK=0, RESULT_CANCELED=1, others app-defined)
u32  data_len + <data_len> bytes
```

- **Max size:** 16 KiB (`ActivityResult::MAX_WIRE_BYTES`) —
  the same cap as Intent; an average result of "picked=42" is
  tens of bytes; anything beyond should travel through shared
  storage.
- **Min size:** 8 bytes (code + data_len=0).
  `drain_set_result` returning `0` ⇒ "no result stashed";
  synthesising `RESULT_CANCELED` with empty data happens on
  the framework side in `deliver_activity_result_for`.
- **Opaque data:** the framework does not inspect the bytes.
  The caller+callee pair agrees on a format (JSON, bincode, a
  number as LE u32) — there is no schema lineage.
- **Identical encode on both sides:**
  `framework::result::ActivityResult::encode` and
  `sdk-rust::result::ActivityResult::encode` byte-for-byte
  symmetric. Breaking-change discipline like Intent.

The codec is in
[`framework/src/result.rs`](../framework/src/result.rs) and its
mirror in [`sdk-rust/src/result.rs`](../sdk-rust/src/result.rs).

### 2.6 The intermediate framework layer

In the framework itself:

- `akiva_framework::wasm_activity::WasmHost` — trait the
  kernel implements (`KernelWasmHost`) through the externs
  above; in unit tests we implement `MockHost` to capture
  calls.
- `WasmActivity` — struct that implements the internal
  `Activity` trait and routes every callback to `WasmHost`
  with the relevant handle. This is the only bridge between
  the framework and wasm apps.
- `LifecycleTransition`, `opcode::*`, `encode_op`,
  `decode_frame` — the entire ABI gathered in one place.

### 2.7 What is **not** included in v0

- **Network / Camera ABI v0** — Phase 14 added Network
  (HTTP_FETCH + WebSocket + WebTransport). Camera, microphone,
  and MIDI remain post-MVP.
- **Streams / pushed events** — range `0x0200..` is reserved;
  the shape will follow once a real-time capability needs it
  (audio, gestures). See
  [`docs/risks.md §7.11`](risks.md).

---

> **The rest of this document describes the v1 layer (Component
> Model).** The numbered sections that follow (principles, WIT
> definitions, capability model, etc.) remain the target spec.
> They are not implemented in Phase 2b, but the API strength
> in §2 is shaped consciously to align with them.

## 3. Principles (v1)

1. **Component Model + WIT** — the ABI is defined through
   [WIT](https://component-model.bytecodealliance.org/design/wit.html).
   No `extern "C"`, no manual raw pointers, no manual
   alloc/free on the host side. `wit-bindgen` produces glue
   automatically for every language (Rust, C, Go, Python, JS).
2. **Typed resources** — widgets, activities, intents are WIT
   `resource`s, not `u32` handles. This gives type safety and
   lifetime management the runtime enforces.
3. **Strings, records, lists, options, results** — at the
   native WIT level. No manual marshalling.
4. **Version-tracked** — every world gets SemVer
   (`akiva:app/activity@0.1.0`). Breaking changes → MAJOR bump.
5. **Capability via imports** — an app receives only imports
   the runtime decided to grant. Permission denied = import
   missing = link error, not a runtime check. See §6.
6. **Additive only within MAJOR** — backward-compatible
   additions inside a MAJOR.

## 4. Module shape

An Akiva app is a **Component** that implements the
`akiva:app/guest` world and imports `akiva:app/host`.

```
akiva:app package:
├── world guest      ← what the app must export (lifecycle + event dispatch)
├── world host       ← what the runtime exposes (widgets, resources, intents, storage, ...)
└── interfaces:
    ├── lifecycle
    ├── events
    ├── widget
    ├── resources
    ├── intent
    ├── storage
    ├── system
    ├── log
    └── types (shared)
```

---

## 5. WIT Definitions

### 4.1 Shared types

```wit
package akiva:app@0.1.0;

interface types {
    /// Widget identity. Stable for the lifetime of the widget in the tree.
    resource widget;

    /// An active Activity instance (distinct from the declared manifest entry).
    resource activity;

    /// An Intent — a composable message for IPC.
    resource intent;

    /// Opaque handle returned when registering an event listener; used for unregister.
    type listener-id = u32;

    /// Logical pixel value (dp). Runtime applies density.
    type dp = f32;

    /// Scale-independent pixel value (sp). Runtime applies font scale + density.
    type sp = f32;

    /// RGBA in 0xRRGGBBAA layout.
    type rgba = u32;

    /// Monotonic elapsed nanoseconds since boot. Clock source is the host.
    type mono-nanos = u64;

    /// Wall-clock milliseconds since Unix epoch.
    type wall-millis = u64;

    record dimension {
        /// Explicit value; if absent, the widget uses intrinsic sizing.
        value: option<size-value>,
    }

    variant size-value {
        dp(dp),
        percent(f32),
        match-parent,
        wrap-content,
    }

    record insets {
        top: dp,
        right: dp,
        bottom: dp,
        left: dp,
    }

    enum visibility {
        visible,
        invisible,  // takes space, not drawn
        gone,       // no space, not drawn
    }

    enum text-align {
        start,
        center,
        end,
    }

    enum event-kind {
        click,
        long-click,
        text-change,
        focus,
        blur,
        key-down,
        key-up,
        pointer-enter,
        pointer-leave,
    }

    variant event-payload {
        point(tuple<f32, f32>),       // click, pointer
        text(string),                  // text-change
        key(key-event),                // key-down/up
        none,                          // focus/blur/enter/leave
    }

    record key-event {
        code: u32,
        modifiers: u32,  // bitmask: 1=shift 2=ctrl 4=alt 8=meta
    }

    variant widget-error {
        invalid-parent,
        widget-destroyed,
        invalid-value(string),
    }

    variant resource-error {
        not-found,
        wrong-type,
    }

    variant storage-error {
        not-found,
        quota-exceeded,
        io-error(string),
    }

    variant intent-error {
        no-match,
        permission-denied,
        invalid,
    }
}
```

### 4.2 Lifecycle (exports — guest must implement)

```wit
interface lifecycle {
    use types.{activity};

    /// ABI semver the guest targets. Runtime rejects if major mismatch.
    abi-version: func() -> string;  // e.g. "0.1.0"

    on-create: func(act: borrow<activity>, saved-state: option<list<u8>>);
    on-start: func(act: borrow<activity>);
    on-resume: func(act: borrow<activity>);
    on-pause: func(act: borrow<activity>);
    on-stop: func(act: borrow<activity>);
    on-destroy: func(act: borrow<activity>);

    /// Called before on-stop. Returned bytes persist across process kills (<= 64 KiB).
    on-save-state: func(act: borrow<activity>) -> list<u8>;

    /// Config change with bitmask (see types.config-changes). Only called if the
    /// activity declared it handles these changes in manifest; otherwise recreate.
    on-config-change: func(act: borrow<activity>, changes: u32);

    /// Delivered when a new intent arrives for an already-existing instance
    /// (launch modes singleTop / singleTask).
    on-new-intent: func(act: borrow<activity>, in: intent);

    /// Result from start-activity-for-result. `data` is none if canceled.
    on-activity-result: func(
        act: borrow<activity>,
        request-code: u32,
        result-code: s32,
        data: option<intent>,
    );
}

interface events {
    use types.{activity, widget, event-kind, event-payload};

    /// Dispatched for every widget event wired via widget.set-listener.
    on-event: func(
        act: borrow<activity>,
        w: borrow<widget>,
        kind: event-kind,
        payload: event-payload,
    );
}
```

### 4.3 Widget (imports — runtime provides)

```wit
interface widget {
    use types.{widget, activity, insets, size-value, visibility,
              text-align, rgba, sp, dp, event-kind, listener-id, widget-error};

    // ---- Creation ----
    // parent: none => root of the activity's content view.
    create-vstack:  func(parent: option<borrow<widget>>) -> result<widget, widget-error>;
    create-hstack:  func(parent: option<borrow<widget>>) -> result<widget, widget-error>;
    create-grid:    func(parent: option<borrow<widget>>, cols: u32, rows: u32)
                      -> result<widget, widget-error>;
    create-frame:   func(parent: option<borrow<widget>>) -> result<widget, widget-error>;
    create-text:    func(parent: option<borrow<widget>>) -> result<widget, widget-error>;
    create-button:  func(parent: option<borrow<widget>>) -> result<widget, widget-error>;
    create-image:   func(parent: option<borrow<widget>>) -> result<widget, widget-error>;
    create-spacer:  func(parent: option<borrow<widget>>) -> result<widget, widget-error>;

    // ---- Lookup ----
    find-by-id: func(act: borrow<activity>, id: string) -> option<widget>;

    // ---- Size & spacing ----
    set-width:   func(w: borrow<widget>, v: size-value);
    set-height:  func(w: borrow<widget>, v: size-value);
    set-padding: func(w: borrow<widget>, p: insets);
    set-margin:  func(w: borrow<widget>, m: insets);
    set-flex:    func(w: borrow<widget>, v: f32);

    // ---- Color ----
    set-background-color: func(w: borrow<widget>, c: rgba);
    set-foreground-color: func(w: borrow<widget>, c: rgba);

    // ---- Visibility & state ----
    set-visibility: func(w: borrow<widget>, v: visibility);
    set-enabled:    func(w: borrow<widget>, on: bool);

    // ---- Text widget ----
    set-text:        func(w: borrow<widget>, text: string);
    set-text-size:   func(w: borrow<widget>, size: sp);
    set-text-align:  func(w: borrow<widget>, a: text-align);
    set-font-weight: func(w: borrow<widget>, weight: u32);   // 400/500/700

    // ---- Arbitrary tag (useful in click handlers) ----
    set-tag: func(w: borrow<widget>, tag: string);
    get-tag: func(w: borrow<widget>) -> option<string>;

    // ---- Tree ----
    clear-children: func(w: borrow<widget>);
    remove:         func(w: widget);                         // own handle → drop
    parent:         func(w: borrow<widget>) -> option<widget>;
    child-count:    func(w: borrow<widget>) -> u32;
    child-at:       func(w: borrow<widget>, index: u32) -> option<widget>;

    // ---- Events ----
    set-listener:    func(w: borrow<widget>, kind: event-kind) -> listener-id;
    clear-listener:  func(w: borrow<widget>, kind: event-kind);
}
```

### 4.4 Resources (imports)

```wit
interface resources {
    use types.{activity, widget, rgba, dp, resource-error};

    get-string: func(ref: string) -> result<string, resource-error>;
    get-string-fmt: func(ref: string, args: list<tuple<string, string>>)
                     -> result<string, resource-error>;
    get-color:  func(ref: string) -> result<rgba, resource-error>;
    get-dimen:  func(ref: string) -> result<dp, resource-error>;

    /// Inflate an AKML layout as a child of `parent` (root of activity if none).
    /// All @+id children become findable via widget.find-by-id.
    inflate-layout: func(
        act: borrow<activity>,
        layout-ref: string,
        parent: option<borrow<widget>>,
    ) -> result<widget, resource-error>;

    /// Drawable returned as opaque handle the Image widget understands.
    resource drawable;
    get-drawable: func(ref: string) -> result<drawable, resource-error>;
    set-image-drawable: func(w: borrow<widget>, d: borrow<drawable>);
}
```

### 4.5 Activity & Intent (imports)

```wit
interface intent {
    use types.{activity, intent, intent-error};

    // ---- Construction ----
    new: func(action: string) -> intent;
    set-data:     func(i: borrow<intent>, uri: string);
    set-mime:     func(i: borrow<intent>, mime: string);
    add-category: func(i: borrow<intent>, category: string);
    set-target:   func(i: borrow<intent>, package: string, activity: string);

    put-string: func(i: borrow<intent>, key: string, value: string);
    put-int:    func(i: borrow<intent>, key: string, value: s64);
    put-bool:   func(i: borrow<intent>, key: string, value: bool);
    put-bytes:  func(i: borrow<intent>, key: string, value: list<u8>);

    // ---- Inspection ----
    get-action:     func(i: borrow<intent>) -> string;
    get-data:       func(i: borrow<intent>) -> option<string>;
    get-mime:       func(i: borrow<intent>) -> option<string>;
    get-categories: func(i: borrow<intent>) -> list<string>;

    get-string:  func(i: borrow<intent>, key: string) -> option<string>;
    get-int:     func(i: borrow<intent>, key: string) -> option<s64>;
    get-bool:    func(i: borrow<intent>, key: string) -> option<bool>;
    get-bytes:   func(i: borrow<intent>, key: string) -> option<list<u8>>;
}

interface activity-api {
    use types.{activity, intent, intent-error};

    set-content-view: func(act: borrow<activity>, layout-ref: string);

    /// Returns an opaque request-id the runtime maps back to the caller on result.
    start-activity: func(act: borrow<activity>, i: intent) -> result<_, intent-error>;

    start-activity-for-result: func(
        act: borrow<activity>,
        i: intent,
        request-code: u32,
    ) -> result<_, intent-error>;

    finish: func(act: borrow<activity>);

    set-result: func(
        act: borrow<activity>,
        code: s32,
        data: option<intent>,
    );

    /// Current intent that started this activity (or the one last delivered
    /// via on-new-intent).
    current-intent: func(act: borrow<activity>) -> option<intent>;
}
```

### 4.6 Storage (imports) — capability gated

The storage interface is **only linked** if the app's manifest
declared `akiva.permission.STORAGE_SANDBOX` (which is
auto-granted — it's the app's own sandbox). Cross-app storage
is a separate, user-granted capability.

```wit
interface storage {
    use types.{storage-error};

    /// Per-app sandboxed key-value store backed by OPFS.
    set:    func(key: string, value: list<u8>) -> result<_, storage-error>;
    get:    func(key: string) -> result<list<u8>, storage-error>;
    remove: func(key: string) -> result<_, storage-error>;
    keys:   func(prefix: string) -> list<string>;
    size-of: func(key: string) -> option<u64>;
}
```

### 4.7 System & logging (imports)

```wit
interface log {
    enum level { debug, info, warn, error }
    write: func(level: level, message: string);

    /// Tracing spans — handle is returned as an opaque id that must be passed
    /// to span-end. Runtime emits structured traces to the host observability sink.
    span-begin: func(name: string) -> u32;
    span-end:   func(id: u32);
}

interface system {
    use types.{mono-nanos, wall-millis};

    // ---- Device config snapshot ----
    record device-config {
        locale: string,              // BCP-47, e.g. "en-US"
        layout-direction: layout-dir,
        orientation: orientation,
        ui-mode: ui-mode,
        density: f32,                // 1.0 = 160 dpi baseline
        screen-width-dp: u32,
        screen-height-dp: u32,
    }
    enum layout-dir   { ltr, rtl }
    enum orientation  { portrait, landscape }
    enum ui-mode      { light, dark }

    current-config: func() -> device-config;

    // ---- Clock ----
    monotonic-nanos: func() -> mono-nanos;
    wall-millis:     func() -> wall-millis;

    // ---- Toasts (lightweight notifications, non-interactive) ----
    enum toast-duration { short, long }
    show-toast: func(message: string, duration: toast-duration);

    // ---- Secure RNG ----
    // Backed by the host's CSPRNG (crypto.getRandomValues). Always available.
    random-bytes: func(len: u32) -> list<u8>;
}
```

### 4.8 World composition

```wit
world guest {
    export lifecycle;
    export events;
}

world host {
    import widget;
    import resources;
    import intent;
    import activity-api;
    import storage;      // gated by capability (see section 6)
    import log;
    import system;
}
```

An Akiva app is a component that imports `host` and exports
`guest`. The runtime inspects the component's imports,
validates each against the granted capability set, and
instantiates only if every import is permitted.

---

## 6. Capability-via-imports model

Permissions in Akiva are enforced at **instantiation time**,
not at call time.

- The `manifest.toml` declares which capabilities the app
  requires / may use.
- The PackageManager maps each capability to a concrete set of
  WIT imports.
- On instantiate, the runtime builds an import map containing
  **only** the imports the app is authorised to use. Calls to
  missing imports fail at link time, not with a runtime
  permission error.

Example mapping:

| capability                         | imports gated                        |
|------------------------------------|--------------------------------------|
| `akiva.permission.STORAGE_SANDBOX` | `storage` (always granted)           |
| `akiva.permission.NETWORK`         | `network` (Phase 14.1+)              |
| `akiva.permission.NOTIFICATIONS`   | `notifications` (Phase 14.3)         |
| `akiva.permission.CLIPBOARD`       | `clipboard` (Phase 14.4)             |
| `akiva.permission.VIBRATE`         | `vibration` (Phase 14.5)             |
| `akiva.permission.LOCATION`        | `geolocation` (Phase 14.6)           |
| `akiva.permission.QUERY_PACKAGES`  | extra methods on `activity-api`      |
| `akiva.permission.LAUNCH_ANY_APP`  | `start_activity` for any package     |

Benefits over a runtime-check model:

- **Fast:** no branch on every API call.
- **Unforgeable:** an app with no NETWORK capability cannot
  synthesise a network call by any trick — the function does
  not exist in its import namespace.
- **Auditable:** capability set == linked imports, visible in
  `akiva build` output.

---

## 7. Observability hooks

The `log` interface is always imported. Output is sunk in the
host as follows:

| sink             | audience        | routing                                            |
|------------------|-----------------|----------------------------------------------------|
| Browser console  | Developer       | `log.write` at info+ ; all debug if devtools flag  |
| In-app log view  | End user / QA   | Shell's "Developer → Logs" (post-MVP)              |
| Structured trace | Host tooling    | `span-begin` / `span-end` → OT-compatible events   |

See [`docs/observability.md`](observability.md) for how the
host wires this up.

---

## 8. Example — Calculator boot (conceptual)

```
Runtime:       instantiate(calc.wasm, imports={widget, resources, intent,
                                                activity-api, storage, log,
                                                system})
Runtime → App: lifecycle.on-create(act, saved-state=none)
App:           activity-api.set-content-view(act, "@layout/main")
Runtime:       (layout inflater walks AKML, calls widget.create-* internally,
                registers listeners declared via onClick="on_digit")
Runtime → App: lifecycle.on-start(act)
Runtime → App: lifecycle.on-resume(act)

User taps "7":
Tab HAL:       pointer-down(100, 300) → Worker
Runtime:       hit-test → widget w12 (button "7")
Runtime → App: events.on-event(act, w12, click, point(100.0, 300.0))
App:           widget.get-tag(w12) → "7"
App:           widget.set-text(display_widget, "7")
Runtime:       next frame re-layout + re-composite
```

---

## 9. Not in v1 ABI

- Camera / microphone / sensors.
- Custom font loading.
- Animation driver (post-MVP; fade/slide handled by runtime
  transitions).
- WASI-Threads (when it stabilises on the browser).
- Broadcast receivers (post-MVP).
- PendingIntent (post-MVP).

## 10. Versioning

- Each interface has its own SemVer. Guest declares via
  `abi-version()`.
- Runtime keeps a compatibility matrix: "runtime X can host
  guests with ABI 0.1.0–0.1.z".
- Breaking changes require a major bump; guests pin to a range
  in the manifest (`min_abi_version = "0.1.0"`).
