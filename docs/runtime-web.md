# runtime/web — JS HAL

This document describes the JS layer that hosts Akiva inside the
browser: what runs on the main thread, what runs in the kernel
worker, what runs in per-app workers, how leader election works,
and how an event travels from a DOM listener through the kernel and
back to the canvas.

## 1. Layout

```
runtime/web/
├── index.html              # DOM shell: canvas + boot banner + a11y overlay
├── tab.js                  # Tab HAL (main thread): canvas, input, role handling
├── leader.js               # Session — election + transport
├── worker.js               # Dedicated Worker hosting kernel.wasm + frame loop
├── app_registry.js         # WorkerPool — per-app workers, watchdog, capability bridge
├── app_worker_shell.js     # boilerplate shared by every per-app worker
├── storage.js              # OPFS-backed Storage class
├── storage_path.mjs        # shared key → OPFS path encoder
├── op_kinds.mjs            # mirrors of framework/src/wire/op_kinds.rs
├── paint_decoder.mjs       # PaintOp wire decoder for the renderer
└── renderer/
    └── canvas2d.js         # RendererBackend over OffscreenCanvas 2D
```

The kernel wasm (`akiva-kernel.wasm`) is built from the `kernel/`
crate and copied into `runtime/web/` before serving. The CLI
(`akiva run` / `akiva devserver`) automates the copy.

## 2. Roles: main thread vs. workers

| concern             | Main thread (tab.js)                                      | Kernel worker (worker.js)                       | App workers (app_worker_shell.js)                |
|---------------------|-----------------------------------------------------------|-------------------------------------------------|--------------------------------------------------|
| Canvas              | Owner until leader election, then transferred to kernel   | Receives `OffscreenCanvas` via `transferControlToOffscreen` | Read its own `OffscreenCanvas` for text measurement |
| DOM events          | Captures `pointer{down,move,up,cancel}`, `keydown/up`, `compositionend`, `visibilitychange`, `resize` | none                                            | none                                             |
| Rendering           | **Never paints** after handover                           | All paint flows through RendererBackend         | Emits paint ops; does not paint                  |
| Kernel wasm         | n/a                                                        | Hosts `akiva-kernel.wasm`                       | Hosts the app's wasm (one app per worker)        |
| BroadcastChannel    | yes — leader election + cross-tab transport               | no                                              | no                                               |
| Web Lock            | yes — holds `akiva.os.leader` once leader                 | no                                              | no                                               |

The rule: the main thread is a thin I/O layer over the canvas. The
kernel worker is the only place `akiva-kernel.wasm` runs — the
isolation has both performance and correctness reasons (only one
tab in the world should run an active kernel). App workers (Stone
11.4c) carry the per-app wasm; see §9.

## 3. Leader-election protocol

`leader.js` defines the `Session` class. Each tab creates one
`Session` at startup.

### 3.1 Session states

```
electing ──┬──► leader  (directly, when alone or when the lock is immediately free)
           │
           └──► follower ──► leader  (if the previous leader dies; this is promotion)
```

`leader` is a terminal state — there is no demotion. Once a tab
becomes leader, it owns the kernel until close.

### 3.2 The algorithm

`Session.start()` runs two streams in parallel:

```
navigator.locks.request("akiva.os.leader", exclusive, callback)
   │
   └── callback runs the moment the lock is available. It calls
       _becomeLeader() and then awaits forever (holds the lock until
       tab close).

BroadcastChannel probe
   │
   └── posts {type: "probe", probeId}
       waits LOCK_WAIT_SOFT_MS (25 ms) to see if the lock callback fired
         ─► yes: we are leader, done.
         ─► no:  wait PROBE_TIMEOUT_MS (150 ms) for a "leader_here" reply
             ─► positive reply: _becomeFollower().
             ─► timeout:        wait for the lock to arrive (race edge case).
```

The lock request stays queued FIFO behind any tab that already sent
one. When the leader dies (tab closes or crashes), the browser
releases the lock and the next callback in the queue fires
automatically — the next follower is promoted to leader **without
polling or hand-rolled timeouts**.

### 3.3 Why two channels

- The **Web Lock** is the only source of truth for leadership. It
  is atomic and managed by the browser.
- **BroadcastChannel** is transport — both for the fast probe at
  election time and for cross-tab messages during normal run.

A lock alone leaves followers without a way to talk to the leader.
A channel alone leaves us without deterministic failover.

### 3.4 Failover — sequence of facts

1. Leader-A holds the lock. Follower-B and Follower-C are queued.
2. The user closes tab A. The browser releases the lock.
3. Follower-B's lock callback fires. `_becomeLeader()` runs:
   - boots a `Worker`, loads `akiva-kernel.wasm`;
   - calls `akiva_boot()`, schedules `akiva_frame()`;
   - listens for `follower_msg` on the channel and forwards it to
     its worker.
4. `Session.dispatchEvent("role", { role: "leader", promoted: true })`.
5. `tab.js` sees the event and transfers its canvas to the new
   worker: `surfaceEl.transferControlToOffscreen()` →
   `session.send("attach_display", …)`.
6. The system is back in steady state with Follower-B as leader.

**What survives:** OPFS storage. That is the truth of persistence.
**What resets:** the kernel's in-memory state — activity stack,
scroll positions, the executor's queues. Live activities are
recreated by the framework rehydrating from `saved_state` blobs the
old leader had persisted.

## 4. Event pipeline

From a click to pixels on the screen:

```
Canvas 'pointerdown'                          (main thread, tab.js)
    │
    ▼
pointerEvent("down")(e)                        (tab.js)
    │  builds {type: "event", kind: "pointer", x, y, button, phase}
    ▼
session.send(msg)                              (leader.js → Session.send)
    │
    ├── role === "leader":   worker.postMessage(msg)
    └── role === "follower": bc.postMessage({type:"follower_msg", payload:msg})
                              └─► other tab's Session in leader mode
                                  receives it via _onBroadcast and posts
                                  it to its worker
    ▼
worker.addEventListener("message")             (worker.js)
    │
    ▼
forwardEvent(msg)                              (worker.js)
    │  encodes to binary wire format (10 bytes for pointer)
    │  calls akiva_alloc(len)
    │  writes the bytes into kernel memory at the returned ptr
    ▼
instance.exports.akiva_on_event(kind, ptr, len)    (boundary: JS → wasm)
    │
    ▼
Event::decode(kind, bytes)                     (kernel/event_queue.rs)
    │
    ▼
kernel().events.push(Event::Pointer{...})      (kernel/lib.rs)
    │  queued; returns to JS immediately
    │
    │   ... next frame (rAF) ...
    ▼
akiva_frame(ts)                                (kernel/lib.rs)
    │  drains the event queue:
    ▼
Kernel::handle_event(Event::Pointer{...})
    │  DisplayManager::set_pointer(x, y, pressed)
    ▼
DisplayManager::render_frame(ts)
    │  paint ops emitted via the dispatch_paint_op trait
    ▼
Canvas2DBackend (inside the kernel worker)     (runtime/web/renderer/canvas2d.js)
    │
    ▼
OffscreenCanvas paints                         (pixels appear on screen)
```

### 4.1 Wire format

Every event is encoded little-endian. The encoder is JS-side
(`worker.js::forwardEvent`); the decoder is wasm-side
(`kernel/src/event_queue.rs::Event::decode`). The fields must
agree.

| kind | type        | layout                                                                                         |
|------|-------------|------------------------------------------------------------------------------------------------|
| `1`  | pointer     | `f32 x, f32 y, u8 button, u8 phase` (`0=down`, `1=move`, `2=up`, `3=cancel`)                   |
| `2`  | key         | `u32 code, u8 modifiers, u8 phase` (`0=down`, `1=repeat`, `2=up`)                              |
| `3`  | resize      | `u32 width_px, u32 height_px, u32 density_q8_8`                                                |
| `4`  | visibility  | `u8 visible`                                                                                   |
| `5`  | text        | `u16 len_LE, utf8 bytes` — typed-character / IME-`compositionend` payload (Stone 14 follow-up) |

Density is stored as q8.8 fixed-point — `devicePixelRatio × 256`.
DPR 2.0 → 512.

### 4.2 Coalescing

The kernel `EventQueue` collapses runs of `Pointer { phase: Move }`
— the latest replaces the previous unconsumed move. This prevents
backlogs of hundreds of pointer moves accumulated in a slow second.
Clicks (`Down` / `Up`) are always preserved.

## 5. Canvas ownership

```
boot                 follower               leader                promotion
────                 ────────               ──────                ─────────
canvas.width = set   canvas.width = set     canvas transferred    canvas transferred
canvas main-thread   canvas main-thread     → worker              → worker (late)
boot banner shown    boot banner hidden     boot banner hidden    boot banner hidden
no draws             no draws               worker draws          worker draws
```

Foundational rule: `transferControlToOffscreen()` is a one-way
operation. Once called, the main thread can no longer read or
write `canvas.width` / `canvas.height` / `getContext`. Every resize
must reach the worker via a message.

`tab.js` carries `state.ownsCanvas` — `true` initially, `false`
after transfer. `applyCanvasSize` is called only while the main
thread still owns the canvas.

## 6. Run commands

```sh
# Build the kernel for wasm
cargo build -p akiva-kernel --release --target wasm32-unknown-unknown

# Copy to where the HAL expects it
cp target/wasm32-unknown-unknown/release/akiva_kernel.wasm runtime/web/akiva-kernel.wasm

# Spin up a static server (the HAL is static-only)
python3 -m http.server 8765 --directory runtime/web
```

Opening additional tabs to the same URL joins them automatically as
followers.

### 6.1 Manual checks

- **Leader roundtrip.** Open one tab. Console: `role=leader`,
  `kernel: boot`, `display: primary surface ready`. Move the
  pointer over the canvas — a translucent square follows it. Click
  — it turns pink.
- **Follower registration.** Open a second tab. Its console:
  `role=follower`. The first tab logs:
  `follower display registered for tab …`.
- **Failover.** Close the first tab. The second tab's console:
  `role=leader (promoted)`, then `kernel: boot`. The "Akiva Kernel"
  banner reappears and the pointer marker is back.

## 7. Known limits

- **Follower canvas is not painted.** Cross-tab composition is not
  implemented; tab 2 sees a black canvas until promoted. Frame-
  delta or ScreenSharing-API streaming is the v0.3 path.
- **State does not move between leaders in memory.** Promotion
  resets kernel memory; persistence is OPFS only — see
  [`storage.md`](storage.md).
- **`BroadcastChannel` cannot pass MessagePorts.** Follower → leader
  traffic uses the shared channel; every tab sees every message.
  Migrating to a per-follower MessagePort would need a SharedWorker
  feature-detect and is deferred.
- **All tabs must share an origin.** Web Lock and BroadcastChannel
  scope to origin. Cross-origin migration is not in the MVP.

## 8. Related docs

- [`ARCHITECTURE.md §9`](../ARCHITECTURE.md) — single-instance
  architecture; rationale for Web Locks over SharedWorker.
- [`ARCHITECTURE.md §10`](../ARCHITECTURE.md) — RendererBackend
  trait.
- [`docs/abi.md`](abi.md) — the app-side ABI (Component Model).
  Independent of the HAL described here.
- [`kernel/src/host.rs`](../kernel/src/host.rs) — the imports the
  kernel demands from the HAL.

## 9. WorkerPool + per-app workers (Stone 11.4c — May 2026)

Up to Stone 11.4 every app ran inside the single kernel worker.
From 11.4c, `AppRegistry` is a WorkerPool — every package
(calculator, hello, shell, tic-tac-toe, network, …) gets its own
Worker hosting exactly one wasm app. The kernel worker drives them
through `postMessage`.

### 9.1 The architecture

```
Main thread (tab.js)              Kernel worker (worker.js)              App-worker N (app_worker_shell.js)
─────────────────                 ──────────────────────────              ─────────────────────────────────
Canvas events       ──postMessage──► AppRegistry.dispatch  ──postMessage──► handleDispatch
                                     ┃                                       ┃
                                     ┃                                       ↓
                                     ┃                                  akiva_app_dispatch (wasm app)
                                     ┃                                       ┃
                                     ▼                                       ↓
Canvas2D paint  ◄──postMessage──   apply paint stream  ◄──postMessage──   dispatch_done {bytes}
```

Every per-app worker:

- listens for `init` / `dispatch` / `set_grants` / `set_packages` /
  `host_op_done` / `ping`;
- builds a wasm instance with its own host imports (resource
  lookups + `fire` / `io_start` / `io_poll`);
- emits `dispatch_done` / `log` / `trapped` / `pong` /
  `side_effect` / `host_op`;
- holds its own `OffscreenCanvas` for text measurement (the
  `TEXT_WIDTH` op resolves locally).

### 9.2 `postMessage` protocol (kernel ↔ app worker)

| direction | type            | payload                                            | role                                                            |
|-----------|-----------------|----------------------------------------------------|-----------------------------------------------------------------|
| K → A     | `init`          | `wasmBytes, grants, resources, packagesBlob`       | bootstrap                                                       |
| K → A     | `dispatch`      | `dispatch_id, cmd_kind, payload`                   | lifecycle / input / render call                                 |
| K → A     | `set_grants`    | `grants`                                           | upgrade-time grant push                                         |
| K → A     | `set_packages`  | `packagesBlob`                                     | install / uninstall snapshot push                                |
| K → A     | `host_op_done`  | `future_id, status, payload`                       | reply to a host_op (Stone 11.4c-3)                              |
| K → A     | `ping`          | `t`                                                | watchdog heartbeat                                              |
| A → K     | `init_ok`       | —                                                  | wasm ready, heartbeat may start                                 |
| A → K     | `init_failed`   | `reason`                                           | bootstrap failed                                                |
| A → K     | `dispatch_done` | `dispatch_id, future_id, bytes`                    | dispatch result                                                 |
| A → K     | `side_effect`   | `op_kind, args`                                    | FIRE op forwarded (`FINISH` / `START_ACTIVITY` / `SET_RESULT` / …) |
| A → K     | `host_op`       | `future_id, op_kind, args`                         | request a kernel-served IO op                                   |
| A → K     | `log`           | `level, text`                                      | log line                                                        |
| A → K     | `trapped`       | `reason`                                           | wasm trap; kernel marks `Trapped`                               |
| A → K     | `pong`          | `t`                                                | watchdog reply                                                  |

### 9.3 Watchdog (Stone 11.5c)

The kernel worker pings each app worker every 500 ms (a
self-scheduling `setTimeout`, not `setInterval` — overlap-safe). If
no pong arrives within 1 500 ms, the worker is considered hung:

1. `worker.terminate()` — the browser kills the thread.
2. The package is added to `_pendingTraps`.
3. Every dispatch in flight for this package is dropped (Audit
   fix #2).
4. The kernel calls `hal_drain_trapped_packages` in `akiva_frame`
   → framework `mark_package_trapped` → activities are flipped to
   `Trapped` and popped from the stack.

JS-level pong is the right check because a wasm `loop {}` blocks
the worker's JS event loop too (single-threaded), so pings queue
but pongs never arrive — exactly the trigger we want.

### 9.4 Pre-warm (Stone 11.4c-6)

When `setPackages` is called (at boot and on every install /
uninstall), a `setTimeout(0)` schedules the spawn of a Worker for
every package not already in `_processes`. Each worker compiles its
own wasm in parallel, on its own thread. By the time the user taps,
`register()` finds the worker ready and only allocates a handle.
Launch latency: ~50 ms instead of ~2 s cold.

### 9.5 Debugging

- **DevTools → Application → Service Workers** lists only Service
  Workers — Akiva does not register any.
- **DevTools → Sources → Threads** lists every Worker. Expect to
  see one `worker.js` (the kernel) plus one `app_worker_shell.js`
  per package.
- **Console** lines are tagged `[kernel N]` for kernel logs and
  `[com.example.app] …` for app logs (the shell prepends the
  package id).
- **Network** shows the first fetch of `app_worker_shell.js`
  per-worker spawn — cached after the first.

For the full `postMessage` protocol see
[`runtime/web/app_worker_shell.js`](../runtime/web/app_worker_shell.js)
and [`runtime/web/app_registry.js`](../runtime/web/app_registry.js).

### 9.6 Foundation gaps relevant to runtime-web

See [`docs/risks.md §7`](risks.md) and
[`docs/isolation-design.md §10.5`](isolation-design.md). The
sub-sections that affected `runtime-web` (§7.1 SDK async executor,
§7.5 drain_post phase) closed under P0 #4 and P1 #3 respectively.

### 9.7 Graceful shutdown checkpoint (P0 #2, May 2026)

`tab.js` posts `{type: "checkpoint"}` to the Session on two
triggers:

1. **`visibilitychange` → `hidden`** — the page goes to background
   (tab switch, mobile swipe-close, OS-level backgrounding). This
   is the recommended hook — the classical `beforeunload` is not
   reliable on mobile or in Safari (bfcache). The page lives for
   tens of seconds in this state, so the cross-worker save-bytes
   round-trip finishes and the data is persisted.
2. **`pagehide` with `persisted=false`** — last resort on a hard
   tab close while the page is foreground. The bytes may not make
   it (the page dies before the round-trip completes); best
   effort.

The kernel worker receives the message and calls
`instance.exports.akiva_checkpoint(performance.now())`. The kernel
calls `Framework::checkpoint(ts)` which:

- finds the foreground activity (top of stack);
- verifies its state is live (`Created` / `Started` / `Resumed` /
  `Paused` — not `Trapped` / `Destroyed`);
- ensures no `PendingSave` for `(pkg, class)` is already in flight
  (de-dup);
- calls `Activity::dispatch_save_state_async()` — gets a
  `future_id` from `host.save_state(handle)`;
- records an entry in `pending_saves`.

The regular `drive_pending_saves` (running every frame) polls the
future and persists the bytes when the worker responds. The
activity does not change lifecycle state — checkpoint is read-only
from the state machine's point of view.

The MVP coverage is foreground-only: paused activities deeper in
the stack do not get a checkpoint. To make their state survive a
tab close we'd need either (a) `save_state` on `pause` instead of
only on `destroy` (Android-style) or (b) a checkpoint that walks
the entire stack. Either is reachable without an architectural
change once the trigger is observed.

**Limitation, explicit.** Any state created after the latest
`visibilitychange` checkpoint and before close is lost. Apps with
critical data (notes, drafts) should issue an explicit
`storage::write` on edit and not rely on `on_save_state` alone.
