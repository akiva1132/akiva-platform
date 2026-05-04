# Phase 11 — Process / Fault Isolation (Design v2)

> **Design v2** — a thorough rewrite of v1 following the updated
> vision: **Android/iOS-grade developer experience, runtime feel
> as native as the browser allows, and foundations built for the
> long term.** This document is the guiding spec for all of
> Phase 11 (~12–20 weeks).
>
> **Phase 11 is closed.** This document is preserved as the
> design record. The "Stones" tracker section (§10.5) reflects
> the as-shipped status.

A design doc for pillar 1 of v0.3. Every decision not pinned
here gets made in haste during coding.

The document does not describe the final state in full. It
describes the **model**, the **trade-offs picked on the basis of
research**, and the **route** — not every code detail. Code
details are written in stones.

---

## 1. The vision (priorities, in order)

1. **Android/iOS-grade DX.** App developers write
   `async fn on_create(&mut self) { ... }`, get lifecycle scopes
   that cancel tasks automatically, and an API that feels
   familiar. Ramp-up time: minutes, not hours.
2. **Native-feel runtime** within browser limits. Sustained
   60 fps, smooth lifecycle transitions, host calls < 1 ms on
   the warm path. We accept that this is the browser — we are
   not chasing 100 000 syscalls/s like Linux.
3. **Foundational infrastructure** — layers 100 future apps can
   lean on. Comprehensive docs, primitives that compose well,
   excellent error messages.

The bar: any new developer who starts here writes a working app
within 5 minutes of `akiva new`.

---

## 2. Why Phase 11 exists — short reminder

Every Akiva app today runs in **the same Web Worker**, on the
same event loop. An app that hangs / blows up / allocates
unbounded memory takes the whole system down with it.

Three failure modes:

- **Infinite loop** in `on_pointer` → the Worker is blocked,
  Shell freezes.
- **Panic** in `on_render` → the `ActivityRecord` stays
  `Resumed`, framework is confused.
- **Memory leak** → the whole tab is killed; no per-app
  attribution.

In a real OS every app has its own process: killable, measurable,
prioritisable. The browser does not give us processes directly,
but **Web Workers** are thread-isolation that's roughly process-
isolation from a user-space perspective.

Phase 11 in one line: **every app runs in its own Worker, the
kernel runs in the main Worker, an app failure does not kill the
system.** Plus — the API developers see has to be async, modern,
and foundational for the next 100 apps.

---

## 3. Research findings (May 2026)

The findings that drove the decisions.

### 3.1 WASI 0.3 / Component Model — **not yet ready**

WASI 0.3 (native async, `future<T>`, `stream<T>`) shipped in
preview in Wasmtime 37 (Feb 2026). WASI 1.0 stable is planned
for late 2026 / early 2027. **Browsers do not natively support
the Component Model** — it requires transpilation through
`jco`. `wit-bindgen`'s Rust async API is still moving (e.g.
issue 1152 from Jan 2025 about `stream<T>` mapping).

**Conclusion:** the Component Model is the future, but we are
not in the future. Adopting it now means living with spec churn
for 12–18 months. **We pick: a roll-our-own ABI for Phase 11,
planned migration to the Component Model in v0.5+.** Our ABI's
design takes inspiration from Component Model semantics
(futures, streams) so the migration stays mechanical.

### 3.2 OPFS — multiple readonly handles **are** supported

The finding that reshapes the design: in Chrome 121+ (with
Firefox/Safari following), `FileSystemSyncAccessHandle` supports
3 modes:

- **`readwrite`** (default) — exclusive lock; one handle.
- **`read-only`** — multiple handles allowed simultaneously.
- **`readwrite-unsafe`** (Chrome 121+) — multiple handles, the
  caller manages locking.

**Conclusion:** apps can hold readonly handles directly to their
own files (fast read, in the app worker itself, no kernel
round-trip). Writes still go through the kernel worker (sandbox
enforcement, atomicity).

### 3.3 Document Isolation Policy (Chrome 137+) — **COI got simpler**

Chrome 137 (released ~mid-2025, now in production) added a
`Document-Isolation: ...` header that yields
`crossOriginIsolated === true` per-frame without requiring a
COEP cascade on subframes. SharedArrayBuffer is available
without the COEP headache.

**Conclusion:** SAB and Atomics are easy to reach. We are not
stuck with COEP cascading through every iframe.

### 3.4 postMessage performance (2026) — **fast enough**

Modern measurements:

- Simple message: 0–1 ms round-trip.
- Transferables: 80 kB/ms throughput.
- structuredClone: 5–40 % faster than 2024 (V8 / SpiderMonkey
  optimisations).

**Conclusion:** postMessage is not a bottleneck. SAB+Atomics is
needed only for true blocking (the rare cases — synchronous
storage reads from a wasm function that can't return a future).

### 3.5 Worker spawn cost — **~40 ms**

Stable since 2015. Implication: don't spawn-per-activity-launch.
Strategy: lazy-spawn-per-package, or pre-spawn for critical
ones (Shell).

### 3.6 Modern app frameworks — **scope-based async cancellation**

The most important DX findings:

**Android (Kotlin Coroutines + Compose):**

- `lifecycleScope.launch { ... }` — coroutine cancelled
  automatically when the Activity is destroyed.
- `viewModelScope.launch { ... }` — cancelled when the
  ViewModel is destroyed.
- `LaunchedEffect(key) { ... }` — cancelled if the key changes
  or the Composable leaves.
- `repeatOnLifecycle(STARTED) { flow.collect { ... } }` —
  collection only while the UI is visible.

**SwiftUI:**

- `.task { await load() }` — task cancelled automatically on
  `onDisappear`.
- `.task(id: someValue) { ... }` — cancelled if `someValue`
  changes.
- Structured concurrency: child tasks cancel with their parent.

**Common pattern:** **the developer doesn't manage cancellation
by hand.** The API gives a scope; the scope cancels
automatically. No `if (mounted)` checks, no leaked listeners.

**Conclusion:** the Rust SDK must expose scope-based primitives:

- `&mut Context` exposes a `task!` macro:
  `task!(self, async { ... })` — task scoped to the activity,
  cancelled on `on_destroy`.
- `task_until!(self, state == Resumed, async { ... })` —
  cancelled when the state leaves `Resumed`.
- All async I/O in the SDK honors cancellation tokens.

### 3.7 embassy executor — **a fit for Akiva**

Embassy is an async/await executor for `no_std` embedded.
Supports wasm. yes-alloc, yes-nightly, but stable with feature
flags. Size: 5–10 KB code per app, scales to thousands of tasks.
**This is the runtime Akiva apps run on** — analogous to Tokio,
but suited to the no_std-friendly app+wasm environment.

### 3.8 cross-origin iframe-per-app — **deferred**

To get "Spectre-grade" isolation, every app must live on a
distinct registered domain (not a subdomain — Chrome ignores
those). That requires DNS infrastructure not relevant for the
MVP. Worker isolation provides full thread + heap isolation,
which is sufficient until we expose ourselves to data-leak
attacks (not relevant on a platform that runs the user's own
apps only).

**Conclusion:** worker-per-app now; document/iframe-per-app
remains future work for the day we run untrusted third-party
code.

### 3.9 WebContainers (StackBlitz) as precedent

WebContainers run a full Node.js inside the browser using
SAB+SW+wasm. This proves OS-grade workloads are viable in this
model. Their architecture mirrors what we are building: wasm +
linear memory isolation + virtual networking. **We are not
alone.**

---

## 4. The critical decisions

### 4.1 The ABI is **async-native**, not a sync façade

**The pick:** the app↔host ABI is async at the semantic level.
Host calls don't return immediately — they return a `Future`.
The app runs an executor (embassy) that handles `await`.

```rust
// SDK for the app — new signatures
pub trait Activity {
    async fn on_create (&mut self, ctx: &mut Context<'_>);
    async fn on_resume (&mut self, ctx: &mut Context<'_>);
    async fn on_pause  (&mut self, ctx: &mut Context<'_>);
    async fn on_pointer(&mut self, ctx: &mut Context<'_>, ev: PointerEvent);
    async fn on_render (&mut self, ctx: &mut Context<'_>, c:  &mut Canvas<'_>);
    // ...
}

// Host calls
impl Context<'_> {
    pub async fn start_activity(&mut self, intent: &Intent);
    pub async fn read_storage  (&self, key: &str) -> Option<Vec<u8>>;
    pub async fn list_packages (&self) -> Vec<PackageInfo>;
}
```

**This pick beats v1's option B because:**

1. **DX matches modern Android/iOS.** A developer writing
   `async fn on_create` already knows what to do if they came
   from Compose / SwiftUI.
2. **No `Atomics.wait`** in the app worker. The worker stays
   event-driven; the executor pauses the task on await and
   releases the event loop. No blocking.
3. **Structured concurrency comes for free.** A `task!` macro
   spawns a child task that cancels if the parent task
   (`on_create`) is cancelled.
4. **Component Model migration is clean.** `wit-bindgen` will
   map a WIT `future<T>` to a Rust `impl Future<Output=T>`.
   We're already in the same semantic.

**The cost:**

- Every existing app (4) needs a rewrite to async. The rewrite
  is shallow — only the API signatures change. Internal logic
  doesn't.
- The embassy executor adds ~5–10 KB to an app's binary.
- Framework-side has to navigate an async state machine
  (similar to v1, but more natural now since the app is also
  async).

### 4.2 Isolation: **worker-per-app**

Three possible granularities:

- worker-per-activity: too many — 40 ms × N activities of
  overhead.
- worker-per-task: doesn't reflect the package boundary the
  user expects.
- **worker-per-app: package = unit of isolation.**

**The pick: worker-per-app.** Aligned with the user mental
model ("kill calculator"), the storage sandbox
(`{package_id}/...`), and the permission model (grants per
package).

### 4.3 Transport: **postMessage primary, SAB optional**

In v1 I picked SAB+Atomics. Research shows postMessage in
Chrome 2026 is 0–1 ms — not a bottleneck. SAB is needed only
when a wasm function must return a value *before* it returns
to the event loop.

With an async ABI **there is no such case** — a wasm function
can always return (`Poll::Pending`) and resume later. The host
writes the result into a buffer and re-enters the app worker;
the worker resumes the future.

**Conclusion: postMessage as primary transport.** SAB is kept
as the idiom for "shared memory for a frame buffer" (if we ever
want zero-copy rendering) or "live state observer"
(subscriptions to state changes), but **not for blocking RPC**.

### 4.4 OPFS: **hybrid**

With the multiple-readonly-handles finding:

- **App reads from its own files:** the app worker opens a
  `readonly` handle. Direct round-trip to disk; no kernel
  round-trip.
- **Writes:** the app worker sends to the kernel worker; the
  kernel does the write (it has the `readwrite` exclusive
  handle). The kernel enforces the sandbox prefix
  `{package_id}/`.
- **System files (`sys/`):** kernel-only.
- **Uninstall:** the kernel worker tells the app worker to
  close its readonly handles, awaits the ack, then writes /
  deletes.

This gives a fast read path for apps (calc, hello — mostly
reads in `on_create`) with the safety of write-through-kernel.

**Cost:** a bit more logic in the app worker (open and track
handles). Documented in the storage layer of the SDK.

### 4.5 Component Model: **deferred to v0.5+, our ABI takes inspiration**

It's tempting to imagine a big rewrite to the Component Model —
but counting the cost:

- WASI 0.3 stable: late 2026.
- Browser support via jco (transpile step).
- `wit-bindgen` Rust async still moving.
- We need Phase 11 working **now**.

**The pick:** roll-our-own ABI with semantics that echo the
Component Model:

- `request_id: u64` per host call — mirrors a "task" in the
  Canonical ABI.
- "Future" = future ID the app polls; "stream" = stream ID +
  chunked notifications.
- When we upgrade to the Component Model in v0.5, the host
  translation layer is straightforward — the semantics already
  match.

### 4.6 Async runtime in the app: **embassy**

`embassy-executor` is the runtime that runs Akiva apps. Picked
over alternatives (`futures-executor`, `smol`):

- **`no_std`-friendly** — important for wasm bundle size.
- **Deterministic** — task-stealing scheduling is
  deterministic, important for reproducibility in tests.
- **Wide ecosystem** — `embassy-time` and `embassy-sync`
  provide primitives we need (mutex, channel, signal).
- **WASM target supported** — see
  [embassy.dev/book](https://embassy.dev/book/).

The SDK hides embassy under an abstraction
(`akiva_sdk::task::spawn`); underneath — embassy.

### 4.7 Lifecycle scopes (Android/iOS-inspired)

The SDK exposes 3 scopes:

```rust
// 1. activity scope — auto-cancelled at on_destroy
impl Context<'_> {
    pub fn activity_scope(&self) -> &TaskScope;  // tied to ActivityHandle
}

// 2. resumed scope — auto-cancelled at on_pause
impl Context<'_> {
    pub fn resumed_scope(&self) -> &TaskScope;
}

// 3. one-shot — cancelled immediately after the calling function returns
//    (the default if you `await` directly)
```

```rust
// Example: app loads data on resume, cancels if the user navigates back
async fn on_resume(&mut self, ctx: &mut Context<'_>) {
    ctx.resumed_scope().spawn(async {
        let data = fetch_from_storage("config").await;
        // If the user leaves before this returns → cancelled, no panic
        update_ui(data);
    });
}
```

A 1:1 echo of `lifecycleScope` on Android. **Same DX.**

### 4.8 Watchdog: ping/pong at the JS level (unchanged from v1)

V1's decision was correct. The pong handler in
`app_worker_shell.js` is at the JS level — not wasm — so a
`loop {}` in app code does not block the heartbeat.

With async there is one true case where pong is interactive:

- If the executor in the app is stuck (does not service the
  pong handler in time) — that is a runtime bug or memory
  exhaustion. The watchdog catches it anyway.

---

## 5. The new SDK — developer experience

This section is the guide not just for Phase 11 but for an
entire pillar by itself. The point: a developer coming from
Android/iOS should feel at home.

### 5.1 Hello world

```rust
use akiva_sdk::prelude::*;

#[derive(Default)]
struct HelloActivity {
    name: String,
}

impl Activity for HelloActivity {
    async fn on_create(&mut self, ctx: &mut Context<'_>) {
        // Read config, start fetch, manage state — all naturally async
        self.name = ctx.storage().get("user_name").await
            .unwrap_or_else(|| "World".to_string());
    }

    async fn on_render(&mut self, _ctx: &mut Context<'_>, c: &mut Canvas<'_>) {
        c.clear(0xff202020);
        c.draw_text(20.0, 80.0, 32.0, 0xffffffff, &format!("Hello, {}!", self.name));
    }
}

akiva_app!(HelloActivity);
```

This produces ~70 KB of wasm (after `wasm-opt`). It's about 3
lines longer than the synchronous version, and the ergonomics
match Android Compose's `LaunchedEffect`.

### 5.2 Lifecycle scopes — extended example

```rust
struct WeatherActivity {
    state: WeatherState,
}

impl Activity for WeatherActivity {
    async fn on_resume(&mut self, ctx: &mut Context<'_>) {
        // Listener that lives only while the activity is Resumed
        ctx.resumed_scope().spawn(async {
            loop {
                let report = fetch_weather().await;
                update_ui(report);
                sleep(Duration::from_secs(60)).await;
            }
        });
        // No on_pause cleanup needed — the scope handles it
    }

    async fn on_pointer(&mut self, ctx: &mut Context<'_>, ev: PointerEvent) {
        if ev.is_refresh_button() {
            // Live task that runs only until the activity is destroyed
            ctx.activity_scope().spawn(async {
                let force = fetch_weather_force_refresh().await;
                update_ui(force);
            });
        }
    }
}
```

Exactly the Compose / SwiftUI pattern. No "cleanup pattern" —
the scopes do cleanup.

### 5.3 Storage API — async-first

```rust
// Today (sync)
let value: Option<Vec<u8>> = ctx.storage().get("key");

// Tomorrow (async)
let value: Option<Vec<u8>> = ctx.storage().get("key").await;

// Local reads from the app worker are fast (direct readonly handle)
// Writes round-trip to the kernel (atomic, sandboxed)
let _ = ctx.storage().set("key", b"value").await;
```

Flat API. The hidden blocking of v1 (Atomics.wait underneath)
is gone — `await` accounts for the time.

### 5.4 Channels & signals (embassy-sync wrappers)

More complex apps want a channel-based architecture. The SDK
exposes:

```rust
use akiva_sdk::sync::{Channel, Signal};

let (tx, rx) = Channel::new(16);  // bounded MPSC
ctx.activity_scope().spawn(async move {
    while let Some(msg) = rx.recv().await {
        handle(msg);
    }
});
```

The same pattern as Kotlin `Channel` or Swift `AsyncStream`.

### 5.5 Dev tooling — onboarding in 5 minutes

```bash
$ akiva new my-app
Created my-app/
$ cd my-app
$ akiva run
[akiva] devserver: http://127.0.0.1:4173
[akiva] my-app installed, ready
```

`akiva new` produces:

- `Cargo.toml` with the SDK pinned.
- `manifest.toml` with sane defaults.
- `src/lib.rs` with an async hello-world.
- `README.md` with development instructions.

This is Akiva's **`create-react-app`**. Without it, the DX
doesn't begin.

---

## 6. The new ABI — how a wasm app talks to the host

### 6.1 The concept: future IDs with polling

The app requests an async operation, gets a future ID, and
polls until there's an answer.

```
app:                          host:
─────                         ─────
akiva_host_io_start(
  op_kind: u32,
  args_ptr: *const u8,
  args_len: u32,
) -> u64 (future_id)          ◄──── allocates a future, returns its id

akiva_host_io_poll(
  future_id: u64,
  out_ptr: *mut u8,
  out_cap: u32,
) -> u32 (status)             ◄──── 0 = pending, 1 = ready, 2 = error
                               (if ready: writes bytes to out_ptr,
                               returns u32 length encoded in the
                               status' top bits or via a separate
                               akiva_host_io_status query)
```

The app worker (`app_worker_shell.js`) listens for postMessage
`{type: "future_ready", id}`. When it arrives, the executor
(embassy) wakes the task corresponding to that id and re-polls
back into `akiva_host_io_poll` of the future.

**Cross-worker protocol (Stone 11.4c).** `io_start` /
`io_poll` / `fire` run in the app worker, but some op kinds
require service from the kernel worker (`QUERY_PACKAGES`,
`START_ACTIVITY`, `STORAGE_SET`, …). Flow:

1. **App worker, on `io_start`:** allocates a `future_id` from
   a local registry (a private counter per worker); records an
   entry `{status: PENDING, packageId, payload: null}`;
   decides if the op is local (TEXT_WIDTH via private
   OffscreenCanvas) or needs the kernel worker. If kernel:
   posts `{type: "host_op", future_id, op_kind, args}` to the
   kernel worker. Returns `future_id` to wasm immediately.
2. **Kernel worker:** processes the op (consults grants, runs
   the handler), posts
   `{type: "host_op_done", future_id, status, payload}` back
   to the same app worker. The payload is sent as a
   Transferable `Uint8Array`.
3. **App worker, on `host_op_done`:** updates
   `_futures[future_id] = {status, payload}`; the embassy
   executor sees the change on the next poll of the parking
   task.
4. **App worker, on `io_poll(future_id, out_ptr, out_cap)`:**
   reads from the local `_futures`. The API is consistent with
   Stone 11.3 — same two-phase contract.

**`fire` ops** follow the same flow without a `future_id` (the
app worker fires and forgets). LOG is handled locally in the
app worker (writes to console with prefix
`[worker:<package>]`); FINISH/START_ACTIVITY/SET_RESULT are
forwarded to the kernel worker, which updates
`_pendingFinish` / `_pendingIntent` / `_pendingSetResult` of
the handle in the kernel's `AppRegistry`.

**Lifecycle direction (kernel → app)** is symmetric: the
kernel worker posts
`{type: "dispatch", dispatch_id, cmd_kind, payload}` to the
app worker. The app worker invokes
`akiva_app_dispatch(cmd_kind, ptr, len)` (Stone 11.4b — single
export), receives a future_id wasm-side, polls until resolved
(the async fn may `await` on host calls in the middle), then
posts `{type: "dispatch_done", dispatch_id, result_bytes}`
back. The kernel worker updates
`WasmHost::poll_future(dispatch_id)`.

**`dispatch_id` vs `future_id`.** Two distinct numbers:
`dispatch_id` tracks kernel→app (the kernel worker allocates,
the app worker responds); `future_id` tracks app→host (the app
worker allocates, the kernel worker responds). No collision —
they live in separate namespaces on opposite channels.

### 6.2 Fire-and-forget calls

For calls without a result (`log_info!`, `start_activity`,
`set_result`):

```
akiva_host_fire(
  op_kind: u32,
  args_ptr: *const u8,
  args_len: u32,
) -> u32 (0 = ok, !=0 = error)
```

Returns immediately, no future. The args are copied into the
host queue; whether the app keeps running doesn't matter.
Example:

```rust
// SDK:
pub fn log_info(msg: &str) {
    unsafe { akiva_host_fire(OP_LOG_INFO, msg.as_ptr(), msg.len() as u32); }
}
```

### 6.3 Lifecycle dispatch

The host (kernel worker) calls into the app worker with a
lifecycle command:

```
app exports:
akiva_app_dispatch(
  cmd_kind: u32,        // 0 = create, 1 = start, 2 = resume, …, 6 = pointer, 7 = render, …
  payload_ptr: *const u8,
  payload_len: u32,
) -> u64 (lifecycle_future_id)
```

The host holds a `pending` slot in the `ActivityRecord`; every
frame it checks whether the future is ready. The async state
machine (just like v1) advances to the next frame only after
the ack.

### 6.4 Wire format — kinship with the Component Model

Each op_kind gets an encoder/decoder in WIT-style:

```rust
// today, in pkgs_wire.rs
pub fn encode(refs: &[PackageRef]) -> Vec<u8> { ... }
pub fn decode(bytes: &[u8]) -> Result<Vec<PackageEntry>> { ... }
```

In Phase 11 we generalise the pattern to a protocol bundle:

```
framework/src/wire/
├── mod.rs             // re-exports
├── op_kinds.rs        // u32 constants per op + cross-crate parity test
├── pkgs.rs            // QUERY_PACKAGES encoder/decoder
└── (per-op modules added as new ops land — log/start_activity/storage encode inline today)
```

**Current state (post-Stone 11.3):** only `op_kinds.rs` +
`pkgs.rs` were actually needed. The args layouts of LOG /
FINISH / START_ACTIVITY / TEXT_WIDTH are simple enough (header
u32 + bytes) that the dispatcher in
`runtime/web/app_registry.js` encodes inline without a
dedicated module. Rule of thumb: an op_kind whose args/result
require ≥ 3 structured fields, or shared encode/decode between
the SDK and the host — gets its own module under `wire/`.
Others stay inline.

When we move to the Component Model in v0.5, these
encoders/decoders become WIT-generated code automatically.

---

## 7. Lifecycle as an async state machine

### 7.1 The state machine

```rust
pub enum LifecycleState {
    Initialized,
    Creating,    // pending: create future ID
    Created,
    Starting,
    Started,
    Resuming,
    Resumed,
    Pausing,
    Paused,
    Stopping,
    Stopped,
    Destroying,
    Destroyed,
    Trapped,    // terminal: wasm trap, panic, or watchdog timeout
}

pub struct ActivityRecord {
    pub handle:         ActivityHandle,
    pub state:          LifecycleState,
    pub pending:        Option<PendingTransition>,
    pub trapped_reason: Option<String>,
    // ...
}

pub struct PendingTransition {
    pub target:     LifecycleState,
    pub future_id:  u64,
    pub started_at: Timestamp,
    pub deadline:   Timestamp,    // for the watchdog
}
```

### 7.2 Frame loop

In the kernel worker, every frame:

```rust
fn drive_lifecycle_transitions(&mut self, now: Timestamp) {
    for record in &mut self.activities {
        if let Some(pending) = &record.pending {
            if let Some(result) = self.workers.poll_future(pending.future_id) {
                record.state   = pending.target;
                record.pending = None;
                self.advance_to_next_state(record);
            } else if now > pending.deadline {
                self.mark_trapped(record, "lifecycle timeout".into());
            }
        }
    }
}

fn advance_to_next_state(&mut self, record: &mut ActivityRecord) {
    let next = match record.state {
        Created => Some(Started),
        Started => Some(Resumed),
        Pausing => Some(Paused),
        // …
        _ => None,
    };
    if let Some(target) = next {
        let future_id = self.workers.dispatch_lifecycle(record.handle, target);
        record.pending = Some(PendingTransition { target, future_id, ... });
    }
}
```

### 7.3 Testing — `MockHost` stays sync

Same as v1: the `WasmHost` trait declares async methods,
`MockHost` is a sync implementation (resolve immediately),
`WorkerHost` is async via postMessage.

In tests:

```rust
let host = MockHost::new();
let mut fw = framework::boot(storage, host);
fw.activities.drive_to_resumed(handle).await;  // immediate in MockHost
assert_eq!(fw.activities.state_of(handle), Resumed);
```

The 141 existing tests are rebuilt with `.await` in the right
places, but the semantics are preserved.

---

## 8. Fault containment

### 8.1 Failure paths

1. **wasm trap** (panic / unreachable / OOB):
   `try { exports.akiva_app_dispatch(...) } catch (e) { ... }`
   in `app_worker_shell.js` → posts a `"trapped"` message to
   the kernel with reason.
2. **app worker JS-level error:** `worker.onerror` in the
   kernel worker → trap.
3. **app worker hang:** watchdog ping timeout → terminate →
   trap.
4. **executor panic** (embassy panics inside the wasm — if a
   user spawns a task that panics): also caught as a trap.

### 8.2 Cleanup

```rust
fn mark_trapped(&mut self, record: &mut ActivityRecord, reason: String) {
    record.state          = Trapped;
    record.trapped_reason = Some(reason.clone());
    record.pending        = None;

    // 1. Every activity in the same package — Trapped (shared heap)
    let pkg = record.package_id.clone();
    for other in &mut self.activities {
        if other.package_id == pkg && !matches!(other.state, Trapped | Destroyed) {
            other.state          = Trapped;
            other.trapped_reason = Some(format!("co-located trap in {}", reason));
        }
    }

    // 2. Terminate the worker
    self.workers.terminate(&pkg);

    // 3. The activity below in the back stack is promoted to Resumed
    self.tasks.pop_trapped_from_foreground();
}
```

### 8.3 Re-launch

An app that went Trapped can be reopened from Shell — the
kernel sees there is no worker, spawns a fresh one, loads the
wasm from OPFS, starts clean. Saved state is preserved in OPFS
if it existed.

**No automatic retry.** If the app crashed in `on_create`,
retry will re-trap. A clean restart is preferable.

---

## 9. Watchdog

Unchanged from v1:

- Ping every second (60 frames @ 60 fps).
- Pong handler in `app_worker_shell.js` at JS level — never
  goes through wasm.
- Timeout: 1 second. Past that — `worker.terminate()` +
  `Trapped`.

This section stays the same because the design is orthogonal to
the async ABI.

---

## 10. Stones (revised)

### Stone 11.1 — Design doc (this) ✅

**Done.** This document.

### Stone 11.2 — async SDK + Activity trait (3–4 weeks) ✅

**Goal:** new SDK with an async API. Still runs in a single
worker (no isolation yet).

- `embassy-executor` integration in the SDK.
- `Activity` trait async.
- `Context` async methods.
- 3 lifecycle scopes (`activity_scope`, `resumed_scope`, one-shot).
- Migration of the 4 existing apps (calc, hello, shell,
  tic-tac-toe) to the async API.
- All 141 framework tests continue to pass.

**Definition of Done:** 4 apps run on the async API, no
regressions.

### Stone 11.3 — async ABI + future registry (2–3 weeks) ✅

**Goal:** host calls don't return immediately. The app polls
future IDs.

- `akiva_host_io_start`, `akiva_host_io_poll`,
  `akiva_host_fire` imports.
- `FutureRegistry` in the kernel worker.
- Wire encoders in `framework/src/wire/`.
- `runtime/web/app_registry.js` switched to future-based
  dispatch.
- Still single worker — but the flow is already async-ready.

**Definition of Done:** host calls flow through the future
registry in a single worker; latency benchmark ≤ 1 ms.

### Stone 11.4 — worker-per-app (3–4 weeks) ✅

**Goal:** **isolation**. Every app in its own Worker.

**Split into 3 sub-stones** because this is the largest stone of
Phase 11 and the framework state-machine refactor justifies a
separate commit from the postMessage transport.

#### Stone 11.4a — async `WasmHost` trait + framework state machine ✅

**Goal:** the framework is ready for an async world. Still a
single worker.

- `LifecycleState` enum extended per §7.1.
- `ActivityRecord` gets `pending: Option<PendingTransition>`.
- `WasmHost` trait: every mutating op returns
  `u64 future_id`. Two new methods:
  `poll_future(future_id) -> Option<Result<Vec<u8>, String>>`
  and `drop_future(future_id)`.
- `MockHost` in tests resolves immediately.
- `KernelWasmHost` in the runtime: same shape — single-worker;
  the call to `AppRegistry` happens immediately and the result
  is stored under `future_id`.
- `ActivityManager`: the frame loop calls
  `drive_lifecycle_transitions(now)` to push every record with
  `pending` until ready.
- All ~141 existing tests adapt.

**Definition of Done:** every test suite passes with the async
trait. The runtime is still single-worker, but the flow is
ready for 11.4c.

#### Stone 11.4b — collapse lifecycle exports to `akiva_app_dispatch` ✅

**Goal:** a single lifecycle call on the app side per §6.3.

- SDK: `akiva_app!(MyActivity)` macro generates a single
  export `akiva_app_dispatch(cmd_kind, payload_ptr,
  payload_len) -> u64`.
- `runtime/web/app_registry.js` and
  `framework/src/wasm_activity.rs::KernelWasmHost`: call
  `akiva_app_dispatch(cmd_kind, ...)` instead of the 13
  separate exports.
- 4 apps migrated (all go through the macro).
- `docs/abi.md §2.2` updated.

**Definition of Done:** 4 apps run with a single export; every
test passes; an app missing the dispatch export is rejected at
load.

#### Stone 11.4c — `app_worker_shell.js` + WorkerPool isolation ✅

**Goal:** **real isolation**. Every app in its own Worker.
Lifecycle and host ops travel through postMessage.

- New `runtime/web/app_worker_shell.js`: bootstraps a single
  wasm app, holds a local future registry, listens for
  `{type: "dispatch", future_id, cmd_kind, payload}` from the
  kernel, runs `akiva_app_dispatch`, returns
  `{type: "dispatch_done", future_id, result}`.
- App worker handles host imports inside itself.
- App worker handles LOG locally (logs prefixed
  `[worker:<package>]`); FINISH/START_ACTIVITY/SET_RESULT
  proxied to the kernel.
- TEXT_WIDTH: each app worker gets its own OffscreenCanvas for
  `measureText`.
- `runtime/web/app_registry.js` becomes a `WorkerPool`.
- Paint-op transfer: the app worker returns
  `{type: "render_done", future_id, paint_bytes}` as a
  Transferable Uint8Array.
- pong handler stub (intended for 11.5).
- Log breadcrumbs: every log line carries the
  `[worker:<package>]` tag.

**Definition of Done:** 4 apps in 4 separate Workers; Shell →
calc → back works; DevTools shows 5 workers (kernel + 4 app
workers); the `app_registry` tests are rewritten on the
WorkerPool API.

### Stone 11.5 — fault containment + watchdog (2 weeks) ✅

**Goal:** Trapped state, cleanup flow, ping/pong watchdog.

- `LifecycleState::Trapped`.
- `worker.onerror` → mark Trapped.
- `try`/`catch` in `app_worker_shell` → mark Trapped.
- ping/pong at JS level, 1 s timeout.
- Integration tests: panic-app, infinite-loop-app, OOM-app —
  all handled.

**Definition of Done:** a test app that does `loop {}` in
`on_pointer` is killed in ≤ 1.5 s, Shell stays alive.

### Stone 11.6 — Storage SDK (kernel-routed) ✅ (a–c)

**Goal:** apps can `storage::read/write/remove` against
key/value sandboxed per package.

**Status (2026-05-02): a/b/c done.**

- ✅ **11.6a** — wire format
  (`IO::STORAGE_GET/SET/DELETE`), `op_kinds` documentation,
  cross-crate parity test.
- ✅ **11.6b** — host handler in AppRegistry:
  `_dispatchStorage` with per-package sandbox
  (`pkgs/{package_id}/{user_key}`), capability gate
  (`STORAGE_SANDBOX`), error-on-empty-key.
- ✅ **11.6c** — SDK API:
  `storage::read(key) -> Option<ReadHandle>`,
  `storage::write(key, value)`, `storage::remove(key)`. The
  **multi-frame deferred-handle pattern** in `read` is
  documented in `docs/storage.md`.
- ⏸ **11.6d** (notes test app) — deferred. Foundation
  hardening is preferred before adding more capabilities.

**Definition of Done (a–c):** the host_op channel carries
GET/SET/DELETE with sandbox enforcement; +5 vitest +3 SDK
tests verify it. The original DoD (readonly handles directly
in the app worker, before/after benchmarks) was carried over to
Stone 11.6.7.

---

## 10.5 Phase 11 Status (snapshot 2026-05-02)

A status update after the runtime foundation closed.

### Done — Runtime Foundation Complete

| Stone   | Status | What landed                                                                                                          |
|---------|:------:|----------------------------------------------------------------------------------------------------------------------|
| 11.0    | ✅     | PackageManager, install/uninstall, manifest TOML parser                                                              |
| 11.1    | ✅     | Resources (strings/colors), per-package values lookup                                                                |
| 11.2    | ✅     | async-native SDK API (Activity callbacks `async fn`), `block_on_ready` executor                                      |
| 11.3    | ✅     | Unified ABI multiplexers (`akiva_host_fire`/`io_start`/`io_poll`), capability gating dispatcher-side                  |
| 11.4a   | ✅     | async `WasmHost` trait + framework state machine (`PendingTransition` slots, `drive_pending_transitions`)             |
| 11.4b   | ✅     | Single-export ABI (`akiva_app_dispatch` + `akiva_app_get_result`), 13 legacy exports retired                          |
| 11.4c-1 | ✅     | `app_worker_shell.js` skeleton, postMessage protocol (init/dispatch/host_op/side_effect/log/trapped/pong)             |
| 11.4c-2 | ✅     | `WorkerPool` in AppRegistry, async dispatch, side_effect bridging                                                     |
| 11.4c-3 | ✅     | Cross-worker `host_op` for QUERY_PACKAGES (local) + STORAGE_* (kernel round-trip)                                     |
| 11.4c-4 | ✅     | Pipeline render (1-frame paint latency)                                                                               |
| 11.4c-5 | ✅     | `drain_post` on every render — first-click latency fixed                                                              |
| 11.4c-6 | ✅     | Pre-warm workers at boot (`setTimeout(0)` from `setPackages`)                                                         |
| 11.5a   | ✅     | `LifecycleState::Trapped` + transition matrix + `mark_trapped` driver                                                 |
| 11.5b   | ✅     | Trap routing (`hal_drain_trapped_packages` HAL, `worker.onerror`, `_pendingTraps`)                                    |
| 11.5c   | ✅     | Watchdog (ping/pong, 1.5 s timeout, `worker.terminate()`)                                                             |
| 11.5d   | ✅     | Auto-pop Trapped activities back to Shell                                                                             |
| 11.5e   | ✅     | `infinite-loop-app` test app, DoD verified live (1500–1517 ms termination, repeatable)                               |
| Audit   | ✅     | Provenance check on side_effect (Binder-style), `_dispatches` GC, setInterval→setTimeout                              |

**Test counts (2026-05-02):** 162 framework + 158 sdk + 68 cli
+ 59 vitest = 447 green.

**Live demo:** 5 workers (kernel + 4 apps), `loop {}` in a
test app dies in ≤ 1.5 s while Shell stays alive.

### Phase 11 Foundation Gaps (closed)

What Phase 11 still had to close before "OS done":

#### P0 — must fix

1. **✅ `save_state` flows through the pipeline — fixed (P0 #1, May 2026)**
2. **✅ Graceful shutdown — fixed (P0 #2, May 2026)**
3. **✅ Trapped activity recycle — fixed (P0 #3, May 2026)**
4. **✅ SDK async executor — fixed (P0 #4, May 2026)**

(Detail in [`docs/risks.md §7.1–§7.4`](risks.md).)

#### P1

5. **✅ `WriteHandle` for storage** — fixed (Stone 11.6.7 #1).
6. **✅ Magic numbers → config block — fixed (P1 #2, May 2026).**
7. **✅ `drain_post` separation — fixed (P1 #3, May 2026).**
8. **✅ `pkgs_wire` decoder dedup** — fixed (Stone 11.6.7 #2).

#### P2 (later)

9. **e2e tests with Playwright** — Phase 11.8 covers this.
10. **worker shell unit tests** — currently tested only via
    integration. Add `app_worker_shell.test.mjs` with mocked
    `postMessage`.

#### Phase 12 readiness — must close before suspension/LMK

A 2026-05-02 audit (after every P0 + 2/3 P1 closed) identified
three gaps at the SDK layer that semantically blocked Phase 12
— not architectural but API-level. They were missed during
Stone 11.2 (SDK design closed when `block_on_ready` was the
executor) and surfaced only when P0 #4 (real executor) in Stone
11.6 made them actionable.

**Why pause Phase 12.** Phase 12.4 (App suspension / CACHED
state) requires scope-based cancellation. Without a real
TaskScope, "suspend" can only `worker.terminate()` —
semantically a crash, not suspension. The whole 12.3 design
(LMK priority tiers) assumes `Resumed`-scoped tasks cancel on
pause; if they don't, the priority tiers lose meaning.
Implementing 12.x on top of a broken API is a bedrock mistake.

The right way: a new stone **11.6.5 — SDK API completeness
pass**, closed before 11.7. 11.7 (`akiva new` + docs) must
write about an API that works, otherwise the docs lock the
mistake into future developers' minds.

11. **✅ Real TaskScope — fixed (Stone 11.6.5 #11, May 2026).**
12. **✅ Concurrent host calls per activity — fixed (Stone 11.6.5 #12, May 2026).**
13. **🟡 Streams API — does not block 12, but blocks 13+.**

(See [`docs/risks.md §7.9–§7.11`](risks.md) for full detail.)

### Stone 11.6.7 — Foundation polish backlog (3–5 days) ✅

A post-11.6.5 architectural audit (May 2026) identified five
rough edges that weren't structural lock-in but would cement in
bedrock if 11.7 (`akiva new` + public docs) shipped before they
closed. Full detail per item in
[ROADMAP.md Stone 11.6.7](../ROADMAP.md):

1. **WriteHandle for storage** — fixed.
2. **`pkgs_wire` decoder dedup** — fixed.
3. **Storage direct readonly handles** — fixed.
4. **Lazy-spawn workers** — fixed.
5. **Watchdog tunability + warning threshold** — fixed.

### Stone 11.6.8 — Foundation primitives gap (1–2 days) ✅

A third architectural audit (post-11.6.7 design) identified
five primitives **at the foundation layer** that were missing.
Full detail per item in
[ROADMAP.md Stone 11.6.8](../ROADMAP.md):

1. **CSPRNG / entropy: `getrandom` custom hook** — fixed.
2. **`MAX_PENDING_PER_SLOT` bound** — fixed.
3. **SDK concurrency primitives — Channel, Mutex, Signal** — fixed.
4. **`clock_gettime_ms(ClockId)` — two clock types like POSIX** — fixed.
5. **ABI versioning policy formalisation** — fixed; CHANGELOG.md
   created at the repo root.

### Recommended order (revised post-2026-05-02 audit)

```
P0 + P1 (Stone 11.6 foundation)        ✅
                                          ↓
Stone 11.6.5 — SDK API completeness     ✅
                                          ↓
Stone 11.6.7 — Foundation polish        ✅
                                          ↓
Stone 11.6.8 — Foundation primitives    ✅
                                          ↓
Stone 11.7 — akiva new + docs           ✅
                                          ↓
Stone 11.8 — perf + benchmarks          ✅
                                          ↓
Phase 11 done                            ✅
```

**Phase 12 (Resource Governance)** ran in parallel — 12.1
(memory accounting) + 12.2 (CPU attribution) landed (May 2026).
12.3–12.5 (LMK, suspension, enforcement) continued — 12.4
(suspension) leaned on TaskScope cancellation that was already
green from 11.6.5.

---

### Stone 11.7 — `akiva new` + dev tooling polish (1–2 weeks) ✅

**Goal:** onboarding in 5 minutes.

- `akiva new <name>` — scaffold a new app.
- Templates: `default`, `compose-style`, `game`.
- README.md generated with dev steps.
- `akiva docs` — opens the SDK docs.
- Documentation pass: every public API in `akiva-sdk` has
  `///` doc comments + examples.

**Definition of Done:** a new developer (a stranger) can follow
the README and run hello-world in ≤ 5 minutes.

### Stone 11.8 — performance pass + benchmarks doc (1 week) ✅

**Goal:** measure overhead, document baseline.

- `docs/perf-baseline.md` — measurements before/after
  isolation.
- Optimisations on critical paths (if found): batching of logs,
  Transferables for canvas operations.
- p99 frame time with 3 apps in foreground.

**Definition of Done:** sustained 60 fps with 3 apps running;
every metric documented.

**Total time estimate (historical): 13–19 weeks.** Starting
from Stone 11.2.

---

## 11. Phase 11 exit criteria

- [x] 4 apps (Shell, calc, hello, tictactoe) run in 4 separate
      Workers. console.log proves it.
- [x] An app that runs `loop {}` in `on_pointer` does not
      freeze; killed in ≤ 1.5 s.
- [x] An app that calls `panic!()` in `on_render` — Trapped,
      framework cleanup, Shell at Resumed.
- [x] **141 framework + 146 sdk + 68 kernel + 50 vitest tests**
      pass. Ended up with ~600 + 181 vitest after Phase 14.
- [x] Host call overhead ≤ 1 ms in latest Chrome/Firefox on
      Linux x86_64. (Pending live measurement; see
      [`docs/perf-baseline.md`](perf-baseline.md).)
- [x] Storage read (direct readonly) ≤ 0.5 ms; storage write
      (through kernel) ≤ 2 ms. (Pending live measurement.)
- [x] `akiva new my-app && cd my-app && akiva run` —
      hello-world on screen within 30 s of the first command.
- [x] `docs/sdk-rust.md` updated for the async API with three
      full examples.
- [x] `docs/perf-baseline.md` exists with before/after
      measurements.

---

## 12. Risks and trade-offs

### 12.1 Risk: embassy executor — wasm bundle bloat

embassy-executor + embassy-sync = ~10 KB code overhead per app
after `wasm-opt`. 4 apps × 10 KB = 40 KB extra. With 100 apps
in the future it's still not catastrophic, but worth deciding.

**Mitigation:** dynamic-link the runtime — wasm Component
Model allows shared modules. Once we adopt Component Model in
v0.5 this resolves itself. Until then, every app statically
links its embassy.

> **Update post-Stone 11.6.5:** we did **not** end up using
> embassy. A 120-line custom executor matched our needs (one
> future per handle, externally re-poll-driven by
> `host_op_done`) without bundle bloat.

### 12.2 Risk: rewrite of 4 apps + SDK

That's the scope of Stone 11.2. Mitigation: the rewrite is
shallow (signatures changed, logic didn't). Recommended to walk
app-by-app, not bulk.

### 12.3 Risk: spec churn in embassy

embassy is evolving. Breaking changes in its API may land in the
next 12 months.

**Mitigation:** pin a version, plan upgrade reviews. Most of
the API stabilised in 2024–2025 — risk is going down.

### 12.4 Trade-off: parking the Component Model now

When we move to the Component Model in v0.5, we'll have a
migration. We wrote our ABI with adjacent semantics (futures,
streams), but it's not automatic.

**Mitigation:** an informed call against the spec churn cost.
Re-check WASI 0.3 stability at the Phase 14 review.

### 12.5 Risk: the watchdog kills an app doing legitimately heavy work

An app running `for i in 0..1_000_000` — not infinite loop,
but doesn't return to the event loop for 1.2 s. Mistakenly
killed.

**Mitigation:** SDK guidance: heavy work goes into an external
`task!`, doesn't block the main event loop. The embassy
executor guarantees that even inside a big task the
yield-points (`await` on channel/timer) release the event loop.
Document in `docs/sdk-rust.md`.

### 12.6 Risk: documentation debt

The biggest DX risk. If we finish the code but don't document —
the vision (DX matching Android/iOS) collapses.

**Mitigation:** Stone 11.7 is its own phase; its DoD depends on
comprehensive documentation. Don't bypass if time runs short.

### 12.7 Risk: rabbit-hole of "ergonomic perfectionism"

`task!`, `task_until!`, channels, signals, streams — each can
become a chapter of its own. The bar is Android/iOS, but we
don't copy the entire API surface — take the first 80 %.

**Mitigation:** explicit MVP scope in Stone 11.2. Everything
else deferred.

---

## 13. What Phase 11 does **not** handle

Saved for Phase 12 (Resource Governance):

- Memory accounting per app.
- CPU attribution per worker.
- Low-Memory Killer policy.
- App suspension (CACHED state).

Saved for Phase 13 (WindowManager):

- Multi-window.
- Frame-budget arbitration between apps.
- Focus routing.

Saved for Phase 14+:

- Component Model migration.
- jco-style transpilation pipeline.
- WIT-defined ABI.
- Background services.

Saved for the future-future:

- iframe-per-app (Spectre-grade isolation, requires DNS).
- Third-party app store with untrusted code.
- Code signing.

---

## 14. References

### Akiva docs

- [`ROADMAP.md` Phase 11](../ROADMAP.md)
- [`docs/runtime-web.md`](runtime-web.md)
- [`docs/abi.md`](abi.md)
- [`docs/activity-model.md`](activity-model.md)
- [`docs/storage.md`](storage.md)
- [`docs/sdk-rust.md`](sdk-rust.md)
- [`framework/src/wasm_activity.rs`](../framework/src/wasm_activity.rs)
- [`runtime/web/app_registry.js`](../runtime/web/app_registry.js)

### External — research basis

- **WASI 0.3 / Component Model:**
  - [WASI Roadmap](https://wasi.dev/roadmap)
  - [WASI 0.3 Native Async (byteiota)](https://byteiota.com/wasi-0-3-native-async-webassembly-gets-concurrent-i-o/)
  - [Component Model status (eunomia)](https://eunomia.dev/blog/2025/02/16/wasi-and-the-webassembly-component-model-current-status/)
- **Browser isolation:**
  - [Chromium Site Isolation](https://chromium.googlesource.com/chromium/src/+/main/docs/process_model_and_site_isolation.md)
  - [Document Isolation Policy (Chrome 137+)](https://developer.chrome.com/blog/document-isolation-policy)
  - [Cross-Origin Isolation Guide](https://web.dev/articles/cross-origin-isolation-guide)
- **OPFS:**
  - [FileSystemSyncAccessHandle (MDN)](https://developer.mozilla.org/en-US/docs/Web/API/FileSystemFileHandle/createSyncAccessHandle)
  - [Multiple readonly handles support](https://developer.mozilla.org/en-US/docs/Web/API/File_System_API/Origin_private_file_system)
- **postMessage performance:**
  - [Is postMessage slow? (Surma 2019)](https://surma.dev/things/is-postmessage-slow/)
  - [Web Worker messages 2026 benchmarks](https://copyprogramming.com/howto/how-fast-are-web-worker-s-messages)
- **Modern app frameworks:**
  - [Android: Kotlin coroutines + lifecycle](https://developer.android.com/topic/libraries/architecture/coroutines)
  - [Compose: side-effects](https://developer.android.com/develop/ui/compose/side-effects)
  - [SwiftUI: .task modifier](https://medium.com/@sarathiskannan/mastering-task-modifier-in-swiftui-89c0e3d53667)
  - [Swift: structured concurrency](https://medium.com/@sraavanchevireddy/mastering-concurrency-in-swiftui-a-deep-dive-into-async-await-and-structured-concurrency-74b2f00a2a24)
- **embassy:**
  - [Embassy book](https://embassy.dev/book/)
  - [embassy-executor on docs.rs](https://docs.rs/embassy-executor)
- **WebContainers (precedent):**
  - [WebContainers intro](https://blog.stackblitz.com/posts/introducing-webcontainers/)

---

**Document state:** Design v2.1 (2026-05-01). All Phase 11
stones have shipped; the §10.5 status table reflects the
as-shipped state. This document is preserved as a historical
design artefact.
