# Akiva ABI Changelog

This file tracks every change to the `ABI_VERSION` constant in
[`sdk-rust/src/lib.rs`](sdk-rust/src/lib.rs) and
[`framework/src/lib.rs`](framework/src/lib.rs). Bumps follow the
semantic-versioning policy in [docs/abi.md §1.2](docs/abi.md). PATCH
bumps are intentionally unused in the `0.X` line — every change either
adds (MINOR) or breaks (MAJOR).

The "what" column lists the public surface that changed; details and
rationale live in the linked stone or risk entry.

## [Unreleased] — Stage 2d (horizontal scroll, LazyRow, snap, carousel)

No ABI bump — Stage 2d is pure SDK-internal. Apps continue to require
`min_abi_version = "0.13.0"`. The inspector wire format
(`SemanticTree::WIRE_VERSION`) bumps 2 → 3 with a 1-byte axis enum
on the optional scroll payload; v2 decoders default to `Vertical`,
so a rolling upgrade across runtime and inspector is graceful.

- **Added — `ScrollAxis` enum (Stage 2d.1).**
  `ScrollAxis::{Vertical, Horizontal}` (Default = Vertical). Threaded
  through `ScrollCell.axis`, `Node::Scroll.axis`, `ScrollBuilder
  ::axis(...)`, `Ui::scroll_h(child)` factory. `feed_wheel`/`feed_
  pointer` project deltas + slop onto the cell's main axis; no axis
  stealing (cross-axis components drop on the floor).
- **Added — `Node::LazyRow` + shared realisation helper (Stage 2d.2).**
  Horizontal twin of `Node::LazyColumn` with `item_w` taking
  `item_h`'s role. Both builders share
  `node::realise_window(stride, viewport_main, scroll, padding_start,
  overscan, count)` for the visible-range math.
  - New: `Ui::lazy_row(count)` factory, `LazyRowBuilder` (mirror of
    `LazyColumnBuilder`).
  - Internal: `Ui::scroll_cell_viewport_h` renamed to
    `scroll_cell_viewport_main` to reflect axis-neutral use.
- **Added — `SnapMode` + cubic-out snap animation (Stage 2d.3).**
  `SnapMode::{None, Item, Page}` (Default = None). Both lazy
  builders gain `.snap(mode)`. `ScrollCell` adds `snap_mode`,
  `item_stride`, `snap_origin`, `snap_target`, `snap_elapsed_ms`,
  `snap_pending`. `tick_animations` becomes a three-phase machine
  (fling decay → snap entry → snap advance); snap targets the
  nearest stride (`Item`) or viewport-page (`Page`) and glides over
  250 ms with `cubic_out(p) = 1 - (1 - p)^3` easing. User input
  cancels in-flight snap so the touch always wins.
- **Inspector wire v2 → v3 (Stage 2d.4).**
  The optional scroll payload grows 28 → 29 bytes with a trailing
  axis byte (0 = Vertical, 1 = Horizontal). Rust + JS decoders both
  accept v2 input and default axis to Vertical.
  - JS: `__akiva.scrollOf(key)` returns `axis: "vertical" |
    "horizontal"`.
  - e2e helper: `ScrollSemantic.axis` typed accordingly.
- **Demo — `apps/carousel/` (Stage 2d.5).**
  12-card horizontal pager using `LazyRow + SnapMode::Page +
  item_w = viewport_w`. Indicator strip below the row reads
  `Ui::scroll_offset("carousel")` and divides by viewport width to
  render "n / 12". 12 distinct card palettes; centred 16-dp side
  gutters so adjacent rounded surfaces never touch screen edges.
- **Tests (Stage 2d, cumulative).**
  - sdk-rust lib: 321 → 360 (+39 in 2d.1, .2, .3, .4).
  - vitest (web): 240 → 244 (+4 inspector v3 / scrollOf axis).
  - Playwright e2e: 51 → 63 (+12 carousel).
  - Visual goldens: 5 → 6 (+ `carousel-idle-chromium-linux.png`).

## [0.13.0] — 2026-05 (Stage 2c.1 — wheel input pipeline)

**MINOR** — additive: 1 new HAL event kind (`EVENT_WHEEL = 6`), 1 new
host→app dispatch kind (`cmd::WHEEL = 0x14`), 1 new `Activity` callback
(`on_wheel`). Plumbs DOM `wheel` end-to-end with `preventDefault`, so
the canvas owns scroll input and the page never moves behind it. Sets
up `Node::Scroll` / `Node::LazyColumn` in subsequent commits — see
[`docs/scroll-lazycolumn-plan.md`](docs/scroll-lazycolumn-plan.md).

- **Added (Stage 2c.1 — Wheel).**
  - `EVENT_WHEEL = 6` — HAL→kernel — `[dx:f32, dy:f32, display_id:u32]`
    (12 bytes). Wheel events are *not* coalesced — losing one means
    losing scroll distance, which the user notices.
  - `cmd::WHEEL = 0x14` — kernel→app — `[handle:u32, dx:f32, dy:f32,
    ts:f64]` (20 bytes).
  - SDK: `akiva_sdk::activity::WheelEvent { dx, dy }`,
    `Activity::on_wheel(&mut self, ctx, ev)` (async, default no-op).
  - Framework: `WheelEvent`, `Activity::on_wheel`,
    `Framework::dispatch_wheel{,_on_display}` with dialog-shield mirror
    of the pointer path.
  - Browser layer: `canvas.addEventListener("wheel", h, { passive:
    false })` + `preventDefault` — the page never scrolls behind the
    Akiva canvas.

## [0.12.0] — 2026-05 (Stones 14.4 + 14.5 + 14.6 — Phase 14 closeout)

**MINOR** — additive: 6 new op_kinds (3 io, 1 fire) plus 3 new
permission constants (`CLIPBOARD`, `VIBRATE`, `LOCATION`). All ride
the main-thread `cap_req`/`cap_resp` bridge introduced in 14.3.
This bump closes Phase 14 — every browser capability the
post-MVP roadmap targeted now has a wire path, an SDK, and a
permission gate.

- **Added (Stone 14.4 — Clipboard).**
  - `IO::CLIPBOARD_READ_TEXT = 0x0111` — `[]` → `[text:utf8]`
  - `IO::CLIPBOARD_WRITE_TEXT = 0x0112` — `[text:utf8]` → `[]`
  - SDK: `akiva_sdk::clipboard::{read_text, write_text}`,
    `ClipboardError { Denied, Host(String), Busy }`
  - Capability: `akiva.permission.CLIPBOARD`
  - Browser layer: `navigator.clipboard.{readText,writeText}`
- **Added (Stone 14.5 — Vibration).**
  - `FIRE::VIBRATE = 0x000D` — `[count:u16 LE | (ms:u32 LE){count}]`.
    Empty pattern cancels in-progress vibration.
  - SDK: `akiva_sdk::vibration::{vibrate, cancel}` (fire-and-forget)
  - Capability: `akiva.permission.VIBRATE`
  - Browser layer: `navigator.vibrate(pattern)`
- **Added (Stone 14.6 — Geolocation).**
  - `IO::GEO_GET_POSITION = 0x0113` — `[]` → `[lat:f64 | lon:f64
    | accuracy:f64 | optional alt/speed/heading | ts:f64]`
  - SDK: `akiva_sdk::geolocation::{get_position, Position, GeoError}`
  - Capability: `akiva.permission.LOCATION`
  - Browser layer: `navigator.geolocation.getCurrentPosition`
- **Permissions catalog.** `framework::permissions` MVP set went
  from 6 to 9 entries. The "exactly 6" tripwire test was renamed
  to "exactly 9" with the same intent. All three new permissions
  ship at `ProtectionLevel::Normal` (install-time auto-grant).
- **AppRegistry plumbing.** New `_pendingCaps` map keyed by
  AppRegistry-allocated `cap_id` for one-shot main-thread cap
  requests (clipboard, geolocation). `_disposeCapsForPackage`
  hooked into the same termination paths as WS / WT / notification
  cleanup. `completeClipboardRead` / `completeClipboardWrite` /
  `completeGeoGetPosition` are the symmetric `host_op_done`
  routers.
- **Demo.** `apps/network` gains four buttons: `Copy hello`, `Read
  clipboard`, `Vibrate`, `Get location`. Manifest now declares all
  five Phase 14 grants (NETWORK, NOTIFICATIONS, CLIPBOARD,
  VIBRATE, LOCATION).

## [0.11.0] — 2026-05 (Stone 14.3 — Notifications)

**MINOR** — additive: 3 new op_kinds (2 io, 1 fire), no kernel
export changes. First capability that bridges through the main
thread (the browser `Notification` API and especially
`Notification.requestPermission()` require a Window context). Lays
the cap_req/cap_resp infrastructure that 14.4 Clipboard / 14.5
Vibration / 14.6 Geolocation will reuse.

- **Added (Stone 14.3 — IO ops).**
  - `IO::NOTIFY_REQUEST_PERMISSION = 0x010F` — request browser
    permission. Args: `[]`. Result: `[status: u8]` (0=Default,
    1=Granted, 2=Denied). Should be called from a user-gesture
    handler; browsers auto-deny non-gesture prompts.
  - `IO::NOTIFY_POST = 0x0110` — post a system notification.
    Args: `[title_len:u16 LE | title:utf8 | body_len:u16 LE | body:utf8 | tag_len:u16 LE | tag:utf8]`.
    Result: `[notify_id:u32 LE]` once the browser accepts;
    ERROR with reason on permission/denial. The `tag` collapses
    duplicates per browser convention.
- **Added (Stone 14.3 — fire ops).**
  - `FIRE::NOTIFY_CLOSE = 0x000C` — dismiss a posted notification.
    Args: `[notify_id:u32 LE]`. Idempotent.
- **Added (SDK).** `akiva_sdk::notification` module:
  - `Notification` handle with `close` + `Drop` auto-close
  - `notification::post(title, body)` / `post_with_tag(title, body, tag)`
  - `notification::request_permission() -> RequestPermissionFuture`
  - `PermissionStatus { Default, Granted, Denied }`
  - `NotifyError { Denied, PermissionNotGranted, BadTitle, Host(String), Busy }`
  - 7 unit tests (encode round-trips, status decode, pre-resolved
    error paths)
- **Capability machinery.** Reuses the existing
  `akiva.permission.NOTIFICATIONS` declaration from Stone 10.3
  (declared at `ProtectionLevel::Normal`, install-time auto-grant).
  Two-layer model: the system grant gates the wire op; the
  browser's own `Notification.permission` gates the actual
  display. Apps must obtain both — `request_permission` surfaces
  the browser side.
- **Main-thread bridge.** New `sendToTab` dependency on
  AppRegistry's constructor. The kernel-worker's `worker.js`
  passes `(msg) => self.postMessage(msg)`; tab.js routes
  `notify_request_permission` / `notify_post` / `notify_close`
  to the browser API, replies via the existing kernel-bound
  `session.send` channel; worker.js forwards `notify_done` /
  `notify_perm_done` back into AppRegistry. This pattern is the
  first instance of the cap_req/cap_resp shape that 14.4-14.6
  ride.
- **Demo.** `apps/network` gains a "Send notification" button that
  walks the full flow: `request_permission` (surfaces the
  browser prompt on first use), then `post` with a title and
  body. Manifest declares both NETWORK and NOTIFICATIONS grants.

## [0.10.0] — 2026-05 (Stone 14.2 — WebTransport)

**MINOR** — additive: 8 new op_kinds (4 io, 4 fire), no kernel
export changes, no new HAL imports. Ride entirely on the unified
ABI dispatcher established in 11.3 / 14.1. Reuses the existing
`akiva.permission.NETWORK` grant — no new permission type. Apps
with `min_abi_version` ≤ 0.10.0 load unchanged.

- **Added (Stone 14.2 — IO ops).**
  - `IO::WT_OPEN = 0x010B` — open a WebTransport (HTTP/3) session.
    Args: `[url_len:u16 LE | url:utf8]`. Result: `[wt_handle:u32 LE]`
    once `wt.ready` resolves; ERROR on handshake failure.
  - `IO::WT_RECV_DATAGRAM = 0x010C` — pull next datagram or session
    closed event. Args: `[wt_handle:u32 LE]`. Result: `[kind:u8 |
    data]` where kind 0=datagram bytes, 1=closed (data=`[code:u32
    LE | reason]`).
  - `IO::WT_OPEN_BIDI = 0x010D` — open a bidirectional stream under
    an open session. Args: `[wt_handle:u32 LE]`. Result:
    `[stream_handle:u32 LE]`.
  - `IO::WT_RECV_STREAM = 0x010E` — pull next chunk off a bidi
    stream. Args: `[stream_handle:u32 LE]`. Result: `[kind:u8 |
    data]` where kind 0=bytes, 1=fin (peer closed write side, data
    empty), 2=error (data=utf8 reason).
- **Added (Stone 14.2 — fire ops).**
  - `FIRE::WT_SEND_DATAGRAM = 0x0008` — best-effort datagram send.
    Args: `[wt_handle:u32 | data...]`.
  - `FIRE::WT_SEND_STREAM = 0x0009` — write bytes on a bidi stream.
    Args: `[stream_handle:u32 | data...]`.
  - `FIRE::WT_CLOSE_STREAM = 0x000A` — close stream's writer side
    (idempotent). Args: `[stream_handle:u32 | code:u32]`.
  - `FIRE::WT_CLOSE = 0x000B` — close session and all streams under
    it (idempotent). Args: `[wt_handle:u32 | code:u32 | reason_len:
    u16 | reason:utf8]`.
- **Added (SDK).** `akiva_sdk::net::WebTransport` and
  `akiva_sdk::net::BidiStream` with `connect/send/recv/close`,
  `WtDatagram { Bytes(Vec<u8>) | Closed }`, `StreamEvent
  { Bytes | Fin | Error(String) }`. Drop on both types fires the
  appropriate close so handles never leak past activity teardown.
- **AppRegistry-side.** Two new maps: `_wtHandles` (sessions) and
  `_wtStreams` (streams across all sessions). Datagram pump reads
  from `wt.datagrams.readable` into the per-session queue (cap 64
  events; overflow drops oldest, matching UDP semantics). Stream
  pump reads from each stream's `readable.getReader()` into a
  per-stream queue; overflow surfaces as a one-shot `Error`
  ("stream queue overflow") so reliable-stream consumers see the
  truth instead of silent drops. Provenance check on every op.
  Cleanup hooked into `_disposeWsForPackage` (renamed conceptually
  to "all-net cleanup" but the function name is preserved).
- **Demo.** `apps/network` gained a "WebTransport echo" button.
  Stable public WT echo servers are scarce so the demo treats
  endpoint failure as expected and surfaces the reason in the
  status line. Apps shipping their own endpoint can override via
  `localStorage.setItem('akiva.wt_endpoint', '…')`.

## [0.9.0] — 2026-05 (Stone 14.1 — Net capability)

**MINOR** — additive: 5 new op_kinds, no kernel export changes, no new
HAL imports (net ops travel entirely through the existing
`akiva_host_fire` / `akiva_host_io_start` / `akiva_host_io_poll`
unified-ABI multiplexers, dispatched inside AppRegistry to browser
`fetch` / `WebSocket`). Apps with `min_abi_version` ≤ 0.9.0 load
unchanged.

- **Added (Stone 14.1 — IO ops).**
  - `IO::HTTP_FETCH = 0x0108` — gated by `akiva.permission.NETWORK`.
    Args: `[method:u8 | url_len:u16 LE | url:utf8 | hdr_count:u16 LE
    | (name_len:u16 | name:utf8 | val_len:u16 | val:utf8)* | body...]`.
    Result: `[status:u16 LE | hdr_count:u16 LE | (headers as above)
    | body...]`. CORS rules apply transparently; servers without
    CORS headers are unreachable from the web runtime by design.
  - `IO::WS_OPEN = 0x0109` — open a WebSocket. Args: `[url_len:u16
    | url:utf8 | proto_count:u8 | (proto_len:u16 | proto:utf8)*]`.
    Result: `[ws_handle:u32 LE]`. Resolves on the underlying
    socket's `open` event; ERROR with a UTF-8 reason on handshake
    failure.
  - `IO::WS_RECV = 0x010A` — pull next event off a handle.
    Args: `[ws_handle:u32 LE]`. Result: `[kind:u8 | data...]` where
    kind 0=text, 1=binary, 2=closed (data = `[code:u16 LE | reason]`).
- **Added (Stone 14.1 — fire ops).**
  - `FIRE::WS_SEND = 0x0006` — send a frame. Args:
    `[ws_handle:u32 | kind:u8 | data...]` (kind 0=text, 1=binary).
  - `FIRE::WS_CLOSE = 0x0007` — close idempotent.
    Args: `[ws_handle:u32 | code:u16 | reason_len:u16 | reason:utf8]`.
- **Added (SDK).** `akiva_sdk::net` module: `Request` builder + free
  `get`/`post` helpers + `WebSocket::connect/send_text/send_binary/
  recv/close`, `NetError` enum (Denied/BadUrl/Network/Closed/Busy).
  `WebSocket` impls `Drop` to send normal-closure 1000 so handles
  never leak past activity teardown.
- **Capability machinery.** No new wire surface — reuses the existing
  `akiva.permission.NETWORK` declaration from Stone 10.3 (declared at
  `ProtectionLevel::Normal`, auto-granted on install). Gating in two
  layers: app worker shell (early reject before host_op round-trip)
  and AppRegistry dispatcher (real enforcement). Upgrading to
  `Dangerous` (runtime prompt) is queued as Stone 14.1.x.

## [0.8.0] — 2026-05 (Stone 13.2)

**MINOR** — additive: one new io op_kind + one new kernel export +
one new HAL import. Stone 13.3 (transitions) ships entirely inside
framework + SDK behaviour and adds no ABI surface — the spec's "no
opcode change" constraint was held.

- **Added (Stone 13.2).** `IO::SHOW_DIALOG = 0x0107` host op. Args:
  `[handle: u32 LE | title_len: u16 LE | title: utf8... |
   body_len: u16 LE | body: utf8... | btn_count: u8 |
   (btn_len: u16 LE | btn: utf8...)*]`. Result: `[kind: u8,
   btn_idx: u8]` — `kind == 0` → `Button(btn_idx)`, `kind == 1` →
   `Dismissed`. SDK exposes `Context::show_dialog(title, body,
   buttons) -> DialogFuture`. Framework owns the dialog surface (a
   fresh `WindowLayer::Overlay`), input shielding (pointer events
   intercepted while a dialog is open), and the dim layer (alpha
   ~0.4 over the calling activity's surface).
- **Added (Stone 13.2 — kernel surface).** `akiva_open_dialog(handle:
  u32, future_id: u64, args_ptr: *const u8, args_len: usize) -> u32`
  kernel export. Host (`runtime/web/app_registry.js`) decodes the
  SHOW_DIALOG payload, copies it into kernel memory via
  `akiva_alloc`, and calls this. Returns 0 on success, 1 on bad
  payload, 2 on no-foreground-activity. The host is responsible for
  pairing one `akiva_dealloc` per call.
- **Added (Stone 13.2 — kernel imports).** `hal_dialog_complete(handle:
  u32, future_id: u64, kind: u32, button_idx: u32)` HAL import.
  Called by the kernel for every dialog the user resolves; the JS
  host looks up the (handle, future_id) → (packageId,
  workerFutureId) routing entry and posts `host_op_done` back to the
  originating app worker.

## [0.7.0] — 2026-05 (Stone 13.0prep)

**MINOR** — three additive bumps in flight: timer host op (A), three
outline paint opcodes (B), and `@drawable/` resources (C). Apps with
manifest `min_abi_version <= "0.7.0"` load against this runtime; apps
that target older runtimes keep working since every addition is
purely-additive.

- **Added (A — Stone 13.0prep A).** `IO::TIMER_SLEEP = 0x0106` host op.
  Args `[delay_ms: u32 LE]`, result empty. SDK exposes the new
  `akiva_sdk::time` module: `sleep(ms)`, `timeout(fut, ms)`,
  `interval(ms).tick()`. Required by Stone 13.3 animation transitions
  (waker-on-tick) and useful immediately for debounce / cooldown
  patterns. Cancel-on-drop is a known limitation: a Sleep dropped
  before its timer fires leaves a stale futures entry until the worker
  dies (bounded by `MAX_PENDING_FUTURES = 256`).
- **Added (B — Stone 13.0prep B).** Three outline paint opcodes:
  `OP_STROKE_RECT = 8` (25 bytes), `OP_STROKE_ROUND_RECT = 9` (29
  bytes), `OP_DRAW_LINE = 10` (25 bytes). Each carries the same coords
  as its fill counterpart plus a `width: f32` LE before the rgba u32.
  Backed by `hal_draw_stroke_rect` / `hal_draw_stroke_round_rect` /
  `hal_draw_line` HAL imports (kernel-side). Center-stroked per Canvas
  2D default; `draw_line` uses round caps. SDK adds matching
  `Canvas::stroke_rect` / `stroke_round_rect` / `draw_line` methods.
- **Added (C — Stone 13.0prep C).** `OP_DRAW_IMAGE = 11` paint op (21
  bytes: opcode + drawable_id u32 + 4 × f32) backed by `hal_draw_image`
  HAL import. New `akiva_host_resource_drawable(name_ptr, name_len)
  -> u32` host import — resolves a `@drawable/<name>` ref to a stable
  per-package `drawable_id` (`0` is the missing sentinel; renderer
  paints magenta). SDK exposes `Canvas::draw_image` and
  `akiva_sdk::res::drawable(name)`. AKML inflater handles `<Image
  src="@drawable/...">`; manifest `app.icon = "@drawable/<name>"` is
  validated at install.
- **Added (host — no ABI surface).** PNG decode pipeline:
  `AppRegistry._loadDrawableBitmaps` async-decodes per-package PNGs
  via `createImageBitmap` and registers each with the renderer
  (`Canvas2DBackend.registerDrawable(id, bitmap)`); decodes that land
  before the renderer is wired queue in `_pendingDrawableRegistrations`
  and flush on `setBackend`. Dev-apps installer fetches the fixed list
  `{icon,launcher,banner,avatar}.png` (404-tolerant); the proper
  manifest-driven discovery lives in a follow-up. CLI build copies
  `apps/<n>/res/drawable/*.png` to the dev-apps stage.

## [0.4.0] — 2026-05 (Stone 11.6.9)

**MINOR** — additive: one new HAL paint op + one new device-config
export. The qualifier system that backs `values-night/` ships entirely
inside the framework + SDK + JS runtime; no new ABI surface for that
piece.

- **Added.** `hal_draw_round_rect(x, y, w, h, radius, rgba)` HAL
  import (kernel-side) — backs the new `Canvas::fill_round_rect` SDK
  method. New paint opcode `OP_FILL_ROUND_RECT = 7` joins the v0
  paint stream; encode/decode is byte-for-byte symmetric (1 opcode
  byte + 5 × f32 + u32 = 25 bytes total). Maps to `roundRect()` on
  the Canvas2D backend (Baseline 2023). `radius=0.0` falls back to
  `fillRect`. Promoted from Stone 11.6.9 #1 — every native OS uses
  rounded corners as its default UI shape; sharp-cornered Akiva apps
  looked like late-90s web pages by comparison.
- **Added.** `akiva_config_change(night: i32)` kernel export — host
  calls it on boot and on every `prefers-color-scheme` change
  observed via `matchMedia`. Flips
  `ResourceManager::set_night_mode(bool)`, which selects the
  `values-night/` overlay for subsequent `@string/` / `@color/`
  lookups. Idempotent on the current state. Promoted from Stone
  11.6.9 #3.
- **Added (framework — no ABI surface).** Qualifier-aware
  `ResourceTable` with default + `Option<night>` overlays;
  `ResourceManager` owns a `DeviceConfig` and resolves through it on
  every lookup. `PackageManager::read_resource_tables` loads both
  `res/values/*.toml` and `res/values-night/*.toml` at boot.
  Uninstall sweeps both qualifier directories. CLI `akiva build` /
  `akiva package` / dev-apps stage all carry `values-night/`
  alongside `values/`.
- **Added (JS runtime — no ABI surface).** `AppRegistry.setNightMode`
  re-resolves resources for every running app and posts
  `set_resources` to each app worker; the worker hot-swaps its
  in-memory strings/colors maps without restarting.
  `tab.js` installs a `matchMedia('(prefers-color-scheme: dark)')`
  listener and forwards `config-change` to the kernel-worker.
- **Changed (JS runtime — no ABI surface).** `tab.js` resize handler
  switched from `window.addEventListener("resize")` to
  `ResizeObserver` reading `devicePixelContentBoxSize` (Baseline
  2022). Catches zoom-level DPR changes (Ctrl+/-) that the plain
  resize event misses; reports physical-pixel boxes directly so the
  CSS-px×DPR rounding mismatch on fractional zoom levels is gone.
  Promoted from Stone 11.6.9 #2.
- **Changed (apps — no ABI surface).** Calculator and Shell palettes
  split: `res/values/colors.toml` is now the light-theme default and
  the previous-default dark palette moved under
  `res/values-night/colors.toml`. Shell src/lib.rs converted from
  hardcoded `const u32` constants to per-frame
  `akiva_sdk::res::color()` lookups so a runtime night-mode flip is
  visible on the next paint without a respawn.

## [0.3.0] — 2026-05 (Stone 11.6.8)

**MINOR** — additive: two new host imports, a new status code, and
an SDK-only sync surface.

- **Added.** `akiva_host_random_bytes(out_ptr, len) -> u32` import —
  CSPRNG primitive, ungated. Maps to `crypto.getRandomValues` in the
  worker shell with internal chunking around the 64 KiB cap. Wired
  via `getrandom`'s `register_custom_getrandom!` so the entire Rust
  crypto ecosystem (`rand`, `uuid`, `ring`, `rustls`, …) works in
  Akiva apps without app-side glue. Promoted from `risks.md §7.15`.
- **Added.** `akiva_host_clock_ms(clock_id) -> u64` import — single
  extern that mirrors POSIX `clock_gettime(clockid_t)`. `clock_id`
  values: `0 = Monotonic` (`performance.now()`), `1 = RealTime`
  (`Date.now()`). Single extern with id keeps the ABI extensible —
  future clock types can land without a new import or another bump.
  SDK side: `clock_gettime_ms(ClockId)` in `akiva_sdk::clock`.
  Promoted from `risks.md §7.18`.
- **Added.** `STATUS_BUSY = 3` — io_poll status returned when a slot
  hits `MAX_PENDING_PER_SLOT = 256` concurrent in-flight dispatches.
  Apps backoff or treat as `Err(_::Busy)`. Promoted from
  `risks.md §7.16`.
- **Added (SDK only — no ABI surface).** `akiva_sdk::sync` module —
  `Channel<T, N>`, `Mutex<T>`, `Signal<T>`. Single-threaded futurized
  primitives for spawned-task coordination. Backed by the executor's
  wake-flag mechanism (also new in this release). Promoted from
  `risks.md §7.17`.
- **Added (docs only).** Versioning policy in [`docs/abi.md
  §1.2`](docs/abi.md) + this CHANGELOG. Promoted from
  `risks.md §3`.
- **Internal.** `executor::poll_once` switched from a no-op waker to
  one that sets a thread-local flag; `runtime::drive_spawned` loops
  until quiescence. Affects no public API but enables the sync
  primitives to coordinate via real `Waker::wake()` calls.

## [0.2.0] — 2026-05 (P0 #4 + Stone 11.6.5)

**MAJOR** — pre-1.0 breaking change to the dispatch protocol.

- **Changed.** `akiva_app_dispatch` is now async-resumable: takes a
  `dispatch_id: u64`, returns acceptance status (0 = accepted), and
  the result is fetched via the new `akiva_app_poll_dispatch` and
  `akiva_app_get_result` exports. The old "single-shot synchronous
  dispatch returning packed (status, len)" shape is gone. See
  [`docs/risks.md §7.1`](docs/risks.md).
- **Added.** `akiva_app_drive_spawned()` export — pumps tasks queued
  by `TaskScope::spawn`. Stone 11.6.5 #11.
- **Changed.** `Slot.pending` from `Option<PendingDispatch>` to
  `HashMap<u64, PendingDispatch>` so multiple concurrent host calls
  per activity work. Stone 11.6.5 #12. SDK-internal but visible
  through `futures::join!(read("a"), read("b"))` patterns now
  succeeding where they used to assertion-fail.

## [0.1.0] — 2026-05 (initial)

First tagged ABI. Includes:
- `akiva_app_register / unregister`, `akiva_app_dispatch` (single-shot),
  `akiva_app_alloc / dealloc`, `akiva_app_abi_version`.
- `akiva_host_fire / io_start / io_poll` multiplexers — fire kinds
  `LOG / FINISH / START_ACTIVITY / START_ACTIVITY_FOR_RESULT /
  SET_RESULT`; io kinds `TEXT_WIDTH / QUERY_PACKAGES / STORAGE_GET /
  STORAGE_SET / STORAGE_DELETE`.
- Resource imports: `akiva_host_resource_color / string / has_color /
  has_string`.
- `hal_app_*` HAL imports for the host-side dispatcher.
- Wire codecs: paint, saved-state, intent, pkgs blob, result blob.
- Capabilities: `akiva.permission.{LAUNCH_ANY_APP, QUERY_PACKAGES,
  STORAGE_SANDBOX}`.

[0.4.0]: docs/abi.md
[0.3.0]: docs/abi.md
[0.2.0]: docs/abi.md
[0.1.0]: docs/abi.md
