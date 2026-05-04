# Technical Architecture — Akiva Platform

## 1. Guiding principle

Akiva is a **real applicative operating system** — not a game
engine, not a web framework. It includes every subsystem expected
of a modern OS: a process model, activity lifecycle, a resource
system with qualifiers, a declarative layout language, intent-based
IPC, a package manager, a window manager. Each one **must** exist,
even if thin.

"Thin skeleton" does not mean "missing skeleton" — it means "every
bone is present, every bone does the minimum". You can extend
later; you cannot retroactively add a foundational subsystem
without rewriting everything.

## 2. Subsystem map

```
┌───────────────────────────────────────────────────────────────┐
│                     Akiva user space                          │
│   ┌──────────┐  ┌──────────┐  ┌──────────┐                    │
│   │ App: Calc│  │ App: Note│  │ App: Shell│  (apps = wasm,    │
│   └─────┬────┘  └─────┬────┘  └────┬─────┘   each linking the │
│         │             │            │         Akiva SDK:       │
│         │             │            │         widget toolkit + │
│         │             │            │         AKML inflater +  │
│         │             │            │         layout engine)   │
│   ══════════ ABI boundary — paint ops only ═══════════         │
│         │             │            │                           │
├─────────┼─────────────┼────────────┼──────────────────────────┤
│         ▼             ▼            ▼                           │
│                  Akiva framework                               │
│  ┌─────────────────────────────────────────────────────────┐  │
│  │  ActivityManager   │  PackageManager   │  IntentRouter  │  │
│  │  lifecycle + stack │  app registry     │  routing       │  │
│  ├─────────────────────────────────────────────────────────┤  │
│  │  WindowManager     │  ResourceManager  │  PermissionMgr │  │
│  │  shell composition │  qualifiers + look│  enforcement   │  │
│  ├─────────────────────────────────────────────────────────┤  │
│  │  Compositor — accumulates paint ops from the active     │  │
│  │  activity each frame and flushes the buffer to the HAL. │  │
│  └─────────────────────────────────────────────────────────┘  │
├───────────────────────────────────────────────────────────────┤
│                    Akiva kernel (runtime core)                │
│  ┌─────────────────────────────────────────────────────────┐  │
│  │  WasmLoader    │  MemoryManager    │  Scheduler          │  │
│  │  app instances │  per-app memory   │  frame loop + ticks │  │
│  ├─────────────────────────────────────────────────────────┤  │
│  │  EventQueue    │  StorageManager   │  IPCBus             │  │
│  │  input routing │  OPFS-backed      │  intent transport   │  │
│  └─────────────────────────────────────────────────────────┘  │
├───────────────────────────────────────────────────────────────┤
│                    JS HAL (Hardware Abstraction Layer)        │
│   canvas, input events, storage API, wasm loading             │
├───────────────────────────────────────────────────────────────┤
│                    Browser (host kernel)                       │
└───────────────────────────────────────────────────────────────┘
```

### Per-subsystem summary

**Akiva kernel** (the core):

- **WasmLoader** — loads app wasm modules, instantiates them.
- **MemoryManager** — tracks per-app memory consumption, enforces
  quotas.
- **Scheduler** — runs the frame loop, decides who gets CPU time.
- **EventQueue** — receives events from the HAL, routes by focus.
- **StorageManager** — sandboxed key-value store per app, over OPFS.
- **IPCBus** — relays messages (intents) between app instances.

**Akiva framework** (above the kernel; gives apps abstractions):

- **ActivityManager** — manages the Activity stack, lifecycle
  callbacks, task affinities.
- **PackageManager** — registry of installed apps, manifest /
  resource bookkeeping.
- **IntentRouter** — implicit-intent resolution.
- **WindowManager** — which Activity is shown when, transition
  animations.
- **ResourceManager** — loads resources with qualifier matching.
- **PermissionMgr** — checks that an app declared a permission
  and that the user approved it.
- **Compositor** — holds the buffer of `PaintOp`s for the current
  frame, accepts them from the active activity, and flushes to
  the HAL.

**Akiva SDK** (linked into each app — code that runs *inside* the
app's wasm, not in the kernel):

- **Widget toolkit** (`sdk-rust/src/ui`) —
  `VStack`/`HStack`/`Frame`/`Text`/`Button`/`Rect`/`Spacer`,
  alignment, growth, click detection.
- **Layout engine** (`sdk-rust/src/ui/layout.rs`) — measure pass +
  place pass; the functionality the original ROADMAP earmarked
  for the framework.
- **AKML inflater** (`sdk-rust/src/akml`) — parser + inflater +
  bindings (`{name}` substitution); AKML becomes a widget tree
  inside the app, not in the kernel.

See [§2.1](#21-abi-boundary--why-widgets-and-akml-live-in-the-sdk-not-the-framework)
for why widgets / layout / AKML live in the SDK and not the
framework.

**JS HAL** (the only non-Rust part):

- Thin. ~500 lines total. No OS logic.
- Physically split:
  - **Tab HAL** (`tab.js`, in every tab): canvas + input events
    + leader-election client. Sends to / receives from the
    Worker through a MessagePort.
  - **Kernel-Worker HAL** (`worker.js`, only on the leader tab):
    storage (OPFS), wasm loading, runs the runtime and the
    renderer.
  - **Per-app workers** (Stone 11.4c, `app_worker_shell.js`):
    one worker per package — see [§9](#9-single-instance-architecture-tabs--displays-not-devices).

## 2.1 ABI boundary — why widgets and AKML live in the SDK, not the framework

The original ROADMAP put `LayoutInflater`, `LayoutEngine`, and
the widget tree in the framework. In practice (Phases 3a + 3b)
they were moved into the SDK. The decision is architectural, not
arbitrary.

**What actually crosses the ABI:** `PaintOp` only —
`FillRect`, `DrawText`, `PushClip`, etc. A simple, stable binary
format defined in [`docs/abi.md §2`](docs/abi.md).

**If widgets had stayed in the framework:** the ABI would have
had to carry widget declarations (hierarchy + attributes) and
return events back to the app with widget ids as handles. A
complex bidirectional protocol where any widget change is a
breaking change for every app.

**What the SDK-side approach gives:**

- A narrow, stable ABI — paint ops only. Changing the widget
  toolkit doesn't touch the framework.
- Apps evolve at their own pace; `cargo update` on the SDK
  preserves binary compatibility.
- The framework becomes a "paint-op decoder" — small, isolated,
  easy to test.
- Layout logic lives next to the widgets that use it — no
  measure/place split between two crates.

**The cost:** every app carries the widget code in its own wasm
(~50 KB compressed). If that ever becomes a bottleneck, or if
we want centralised a11y / RTL enforcement, a widget ABI can be
introduced — but it is not required today.

See [`docs/abi.md`](docs/abi.md) for what `akiva_app_*` and
`akiva_host_*` exports/imports actually carry.

## 3. Process model

### Conceptual model

Each app runs in a **separate wasm instance** — our shape of a
"process":

- isolated memory;
- a defined quota;
- killable (instance destroyed) and recoverable (new instance);
- identified by a unique `ProcessId`.

### App states

The same shape as Android:

```
┌──────────┐
│ NotExists│ ─────────┐
└──────────┘          │
     │                │ (low-memory killer)
     │ launch         │
     ▼                │
┌──────────┐          │
│ Created  │          │
└─────┬────┘          │
      │ on_start      │
      ▼               │
┌──────────┐          │
│ Started  │          │
└─────┬────┘          │
      │ on_resume     │
      ▼               │
┌──────────┐          │
│ Resumed  │ ◄────────┼─── (user focuses)
└─────┬────┘          │
      │ on_pause      │
      ▼               │
┌──────────┐          │
│ Paused   │          │
└─────┬────┘          │
      │ on_stop       │
      ▼               │
┌──────────┐          │
│ Stopped  │          │
└─────┬────┘          │
      │ on_destroy    │
      ▼               │
┌──────────┐          │
│ Destroyed│ ─────────┘
└──────────┘
```

### Task stack

A global stack of activity instances (Android-style):

```
Stack top (what the user sees)
┌──────────────────────────┐
│  Notes.EditScreen        │  ← Resumed
├──────────────────────────┤
│  Calculator.MainScreen   │  ← Stopped
├──────────────────────────┤
│  Shell.Home              │  ← Stopped
└──────────────────────────┘
Stack bottom
```

Back button → pop stack top → previous activity resumes.

Launch modes: `standard`, `singleTop`, `singleTask`,
`singleInstance` — same as Android. See
[`docs/activity-model.md §4`](docs/activity-model.md).

## 4. Resource system

### Resources directory

Every app contains a `res/` directory:

```
apps/calculator/
├── manifest.toml
├── calculator.wasm
└── res/
    ├── layout/
    │   └── main.akml
    ├── layout-land/        ← planned
    │   └── main.akml
    ├── values/
    │   ├── strings.toml
    │   ├── colors.toml
    │   └── dimens.toml
    ├── values-he/          ← planned
    │   └── strings.toml
    ├── values-night/       ← live
    │   └── colors.toml
    └── drawable/
        └── icon.png
```

### Qualifiers (target list)

- `-he`, `-en` — language (ISO 639-1) — *planned*
- `-ldrtl`, `-ldltr` — text direction — *planned*
- `-land`, `-port` — orientation — *planned*
- `-night` — dark mode — **live (Stone 11.6.9 #3)**
- `-sw600dp` — future, screen width

### Resolution

The ResourceManager at boot:

1. reads the device config — e.g.
   `{ locale: "he-IL", orientation: "portrait", night: false }`;
2. for every resource reference (`@string/welcome`) finds the
   best match;
3. fallback: always `values/` with no qualifier.

### Units

- `dp` (density-independent pixels): multiplied by the device
  density factor.
- `sp` (scale-independent pixels): like `dp` plus the user's font
  scale.
- `px`: raw pixels (discouraged).
- `%`: percent of the parent.

Notation: `"16dp"`, `"1.5sp"`, `"50%"`.

## 5. AKML — the declarative description language

AKML = **Akiva Markup Language**. XML-based, separate from the
app.

> **Physical location (Phase 3b).** The parser + inflater live
> in the SDK (`sdk-rust/src/akml`), not in the framework. Every
> app that calls `akml::inflate_str(&mut ui, LAYOUT)` runs the
> parser inside its own wasm, produces a widget tree in the SDK,
> and emits paint ops the framework collects. The
> `set_content_view` + auto-callback API shown in the example
> below is the **long-term** API; the MVP today uses a builder
> API (`akml::inflate_str_with(&mut ui, LAYOUT, &bindings)`)
> with pull-based dispatch (`ui.was_triggered("action")`). See
> [`docs/akml.md`](docs/akml.md) and
> [`sdk-rust/src/akml/mod.rs`](sdk-rust/src/akml/mod.rs).

### Example

```xml
<!-- res/layout/main.akml -->
<VStack
    id="@+id/root"
    padding="16dp"
    background="@color/surface"
    spacing="8dp">

    <Text
        id="@+id/display"
        text="@string/zero"
        textSize="48sp"
        textColor="@color/on_surface"
        textAlign="end"
        padding="12dp" />

    <Grid cols="4" rows="5" spacing="8dp" flex="1">
        <Button text="@string/clear" onClick="on_clear"  />
        <Button text="/"             onClick="on_op" tag="div" />
        <Button text="×"             onClick="on_op" tag="mul" />
        <!-- … -->
    </Grid>
</VStack>
```

### What the app receives

The app (wasm) at `on_create`:

```rust
async fn on_create(&mut self, ctx: &mut Context<'_>) {
    ctx.set_content_view("@layout/main");
    // Handlers fire automatically because AKML declared them.
}

#[akiva::callback]
fn on_clear(app: &mut Calculator, ctx: &CallbackContext) {
    app.display = "0".to_string();
    ctx.invalidate("@+id/display");  // runtime updates the Text widget
}
```

### Inflation flow — long-term (with ResourceManager + callback registry)

```
1. App:        ctx.set_content_view("@layout/main")
2. Framework:  ResourceManager.get_layout("main") → main.akml (after qualifier matching)
3. Framework:  LayoutInflater.inflate(akml_bytes)
4. Framework:  XML parsed → internal widget tree
5. Framework:  every attribute is translated into a runtime API call (set_text, set_padding, …)
6. Framework:  @string / @color / @dimen refs resolved through ResourceManager
7. Framework:  onClick="on_clear" registered through the app's callback registry
8. LayoutEngine: measure pass → layout pass
9. Compositor: draw commands → canvas
```

### Inflation flow — MVP (Phase 3b; no ResourceManager, no callback registry)

```
1. App:    Bindings::new().text("display", …).color("display_color", …)
2. SDK:    akml::inflate_str_with(&mut ui, LAYOUT, &bindings)
3. SDK:    parser → AST → Ui node ids (id + onClick action registered)
4. SDK:    measure pass → place pass (bottom-up / top-down)
5. SDK:    paint pass emits PaintOps via the Canvas
6. Framework: collected into FrameCommands, sent to the HAL
7. Next frame: app calls `ui.was_triggered("equals")` and routes to itself

What's missing relative to the long-term plan: resource lookup
(AKML literals only), qualifier matching (no layout-land/),
callback auto-wiring (the app calls was_triggered manually),
set_content_view (the app holds the layout as a &'static str).
```

## 6. Intent system

### Intent shape

```rust
pub struct Intent {
    pub action:    String,            // "akiva.intent.action.VIEW"
    pub data_uri:  Option<String>,    // "content://..."
    pub mime_type: Option<String>,
    pub extras:    HashMap<String, Value>,
    pub target:    IntentTarget,      // Explicit | Implicit
}
```

### Intent filters (in the manifest)

```toml
[[activities]]
id = "main"
[[activities.intent_filters]]
actions    = ["akiva.intent.action.MAIN"]
categories = ["akiva.intent.category.LAUNCHER"]
```

### Resolution

1. App sends an intent → `IntentRouter::resolve(intent)`.
2. Implicit: scans every registered filter for a match.
3. If multiple matches → chooser UI *(planned)*.
4. ActivityManager launches the chosen activity, pushes onto the
   stack.
5. Target activity sees the intent in `on_create` /
   `on_new_intent`.

### Example

```rust
// Calculator wants to open Settings
let intent = Intent::new("akiva.intent.action.VIEW")
    .with_data("akiva://settings/theme");
ctx.start_activity(&intent).await;
```

## 7. Window Manager

### Role

Decides **what is shown on screen** at any given moment.

### Basic composition (today)

- One activity at a time (fullscreen).
- Push / pop transition animations (slide + fade with ease-out
  cubic — Stone 13.3).
- `Overlay` z-band reserved for system UI (status bar, navigation
  bar, toasts) — the layer exists but no widget renders into it
  yet.

### Future complexity

- Multi-window (split-screen).
- Floating windows.
- Picture-in-picture.
- Drag and drop between windows.

### Shell

**Shell is a regular Akiva app** with extra privileges:

- Launches automatically at boot.
- Handles the `akiva.intent.category.HOME` intent.
- Renders the list of installed apps via PackageManager.
- Hosts a task switcher *(planned).*

**Back button:** WindowManager owns it. A press →
`ActivityManager::pop()`.

## 8. Permission model (Capability-via-Imports)

> **Implementation status (May 2026):** the model is **fully
> live**. `framework/src/permissions.rs` declares the catalog at
> boot via `permissions::declare_mvp_permissions`;
> `package_manager::install` rejects any required permission not
> declared (Stone 10.2); the wasm loader builds the import map
> per app from grants; an app calling an ungranted import fails
> at instantiation. Phase 14 added five capability gates
> (NETWORK, NOTIFICATIONS, CLIPBOARD, VIBRATE, LOCATION).

**Principle.** An app can only call functions the runtime
imported into its wasm instance. Permission denied = import
absent. No runtime check. This is the natural wasm / WASI /
Component-Model model.

### Permission tiers

- `auto` — always granted (e.g. the app's own storage sandbox,
  log, system config).
- `install-time` — user approves at install time; bound for the
  app's lifetime (e.g. network, cross-app storage).
- `runtime` — user approves at runtime; the app receives
  temporary imports through re-link *(deferred).*
- `signature` — only apps signed by the same publisher
  *(post-MVP).*

### Enforcement at instantiation time

```
PackageManager.install(app)
  │
  ▼
Validate manifest.permissions.required
  │
  ▼
User confirms (today: auto-approve since there is no app store)
  │
  ▼
Store the granted capability set in the registry
  │
  ▼
ActivityManager.launch(app)
  │
  ▼
WasmLoader builds the import map per capability:
  │
  ├─ "akiva.permission.STORAGE_SANDBOX" → include the storage interface
  ├─ "akiva.permission.NETWORK"         → include the network interface
  ├─ "akiva.permission.QUERY_PACKAGES"  → include extra methods on activity-api
  │
  ▼
Instantiate the component with the computed imports
  │
  └─ The app tries to call an absent import → link error at
     instantiate, NOT a runtime error. The code path can't be
     reached.
```

### Manifest declaration

```toml
[permissions]
required = ["akiva.permission.NETWORK"]
optional = ["akiva.permission.CAMERA"]
```

At build time, `akiva build` warns if the code imports
interfaces not listed in `permissions` (the build can't even
succeed in the first place).

### Why it works

| advantage              | how                                                                        |
|------------------------|----------------------------------------------------------------------------|
| **Fast**               | No branch on every API call.                                               |
| **Unforgeable**        | An app cannot reach a function that wasn't imported — confused-deputy is impossible. |
| **Auditable**          | `akiva build` prints the capability set; the WIT file documents the exact surface. |
| **Component-Model native** | Exactly how WASI works.                                                  |

### Full mapping (today)

| capability                              | imports                                                                                                                |
|-----------------------------------------|------------------------------------------------------------------------------------------------------------------------|
| (automatic)                             | `log`, monotonic / wall clocks, random bytes, widget / resource / activity / intent core APIs                          |
| `akiva.permission.STORAGE_SANDBOX`      | `storage::*` (per-app sandbox)                                                                                          |
| `akiva.permission.QUERY_PACKAGES`       | `query_packages` (Shell-only)                                                                                           |
| `akiva.permission.LAUNCH_ANY_APP`       | `start_activity` for any package (Shell-only)                                                                          |
| `akiva.permission.NETWORK`              | `net::*` — WebSocket / WebTransport / fetch (Phase 14.1, 14.2)                                                          |
| `akiva.permission.NOTIFICATIONS`        | `notify_*` (Stone 14.3)                                                                                                |
| `akiva.permission.CLIPBOARD`            | `clipboard::*` (Stone 14.4)                                                                                            |
| `akiva.permission.VIBRATE`              | `vibrate` (Stone 14.5)                                                                                                  |
| `akiva.permission.LOCATION`             | `geolocation::get_position` (Stone 14.6)                                                                               |

## 9. Single-Instance Architecture (Tabs = Displays, not Devices)

**Principle.** There is **one OS instance** per user per origin.
Two tabs of Akiva in the same browser — both see the same OS,
same running apps, same state. Just like one Android instance can
serve two external displays.

**Conceptual model:**

- **Akiva OS** runs in a **single Worker** held by the "leader"
  tab — one central place that holds all the state.
- Each **tab = a display + input client** — does not run the OS,
  only paints a canvas and forwards events.
- Opening a new tab = "plug in another display" to the same Akiva.
- Closing the leader → another tab is selected automatically and
  takes over the worker.
- Closing every tab → the OS shuts down (state persisted to OPFS).

### Architectural decision: Web Locks leader election (primary)

**Chosen approach:** use the
[Web Locks API](https://developer.mozilla.org/en-US/docs/Web/API/Web_Locks_API)
for leader election, run the OS inside a **Dedicated Worker**
the leader holds.

**Why not SharedWorker as primary:**

- Safari support is partial; mobile coverage is sparse.
- Browsers kill idle SharedWorkers even with open tabs.
- OPFS `SyncAccessHandle` is exclusive to a single context —
  SharedWorker conflicts with tabs over access.
- Dedicated Worker is more reliable, dies only with its owning
  tab.

**What Web Locks provide:**

- A single named lock `akiva.os.leader` — only one tab holds it
  at a time.
- `navigator.locks.request(name)` is held for the tab's
  lifetime; the browser releases automatically on tab close.
- Another waiting tab gets the lock immediately — the transition
  is transparent to the user.
- Available in every modern browser (Chrome / Firefox / Safari /
  Edge) since 2022.

### Technical implementation

```
┌────────────────┐  ┌────────────────┐  ┌────────────────┐
│   Leader tab   │  │   Follower tab │  │   Follower tab │
│  HAL + canvas  │  │  HAL + canvas  │  │  HAL + canvas  │
│  (display #1)  │  │  (display #2)  │  │  (display #3)  │
│                │  │                │  │                │
│  holds         │  │                │  │                │
│  akiva.os.     │  │                │  │                │
│  leader lock   │  │                │  │                │
│        │       │  │                │  │                │
│        ▼       │  │                │  │                │
│  ┌──────────┐  │  │                │  │                │
│  │ Dedicated│  │  │                │  │                │
│  │  Worker  │  │  │                │  │                │
│  │          │  │  │                │  │                │
│  │ Akiva    │◄─┼──┼── BroadcastChannel + MessageChannel│
│  │ Runtime  │  │  │   ("akiva.os")                      │
│  │  (wasm)  │  │  │                │  │                │
│  └──────────┘  │  │                │  │                │
└────────────────┘  └────────────────┘  └────────────────┘
        │
        ▼
┌───────────────────────────────────────────────────────────┐
│  OPFS (Origin Private File System) — persistent state     │
│  Accessed via SyncAccessHandle from the Worker only        │
└───────────────────────────────────────────────────────────┘
```

**Tab bootstrap (`tab.js` in every tab):**

```js
async function boot() {
  const channel = new BroadcastChannel("akiva.os");

  // First, announce presence and ask: is there a leader?
  const leaderPort = await probeForLeader(channel, { timeoutMs: 150 });
  if (leaderPort) {
    connectAsFollower(leaderPort);
    return;
  }

  // No leader answered — race for the lock.
  navigator.locks.request("akiva.os.leader", async (lock) => {
    // We won. Start the Worker and serve others until we close.
    const worker = new Worker("worker.js", { type: "module" });
    registerAsLeader(channel, worker);
    connectAsFollower(worker.port);          // the leader is also display #1
    await new Promise(() => {});             // hold lock until tab closes
  });
}
```

**How it works:**

- A new tab posts `"who-is-leader?"` on the BroadcastChannel.
  If someone answers within 150 ms — it joins as a follower.
- Otherwise it races for the lock. The winner spins up a Worker,
  boots the OS, and when its BroadcastChannel sees a
  `"who-is-leader?"` it opens a fresh MessagePort and sends it
  to the asker.
- All coordination flows through BroadcastChannel. The actual
  draw-commands / events transport per tab is a MessagePort
  (point-to-point, faster).

### Division of responsibility

**HAL in every tab** (`tab.js`, lightweight, ~200 lines):

- Builds the canvas + transfers `OffscreenCanvas` to the Worker
  (see [§10](#10-renderer-backend-abstract)).
- Opens the BroadcastChannel + MessagePort.
- Translates pointer / keyboard / IME events into messages.
- **No OS logic in the tab.**

**Dedicated Worker on the leader** (`worker.js`, ~300 lines):

- Holds the wasm runtime instance.
- Runs every app worker via `AppRegistry`.
- Holds the OPFS `SyncAccessHandle` for the storage sandbox.
- Manages the displays list — one MessagePort per follower.
- Sends draw commands to the display where the activity "lives".
- Receives input events from displays.

**Service Worker** (optional, post-MVP):

- Cache for the runtime wasm + installed apps.
- Offline boot.
- **Does not** run the OS — its lifetime is too short.

### Leader failover

When the leader closes:

1. The browser releases `akiva.os.leader` automatically.
2. The next follower waiting on
   `navigator.locks.request` receives it.
3. The winner spins up a fresh Worker. The wasm runtime boots,
   reads state from OPFS (ActivityManager task stack + saved
   states), and continues from there.
4. Other tabs (followers) open a new MessagePort to the new
   leader.
5. The user sees a brief flash (depending on how long the wasm
   takes to load — typically ~100–300 ms from a Service Worker
   cache).

**Result:** a logical single OS instance that retains state
continuously, regardless of which tab "is at the wheel".

### Multi-tab scenarios

**User opens the first tab:**

1. The tab races for the lock — wins (no others).
2. Spins up a Worker; the runtime boots.
3. The tab registers as display #1 inside the Worker.
4. Shell appears.

**User opens a second tab:**

1. The tab posts `"who-is-leader?"` → tab one replies with a
   MessagePort.
2. The tab connects as display #2.
3. ActivityManager decides what to show on display #2 (today:
   Shell).

**User closes the leader while other tabs remain:**

1. Leader closes → lock released → follower receives.
2. A fresh Worker boots in the new leader.
3. State is loaded from OPFS.
4. Other tabs reconnect to the new leader.

**User closes every tab:**

- Every Worker dies.
- State is preserved in OPFS via activity `save_state` and
  ActivityManager persistence.
- Opening a new tab brings everything back.

### Known limits

- **Cross-origin:** not shared. `akiva.app` with `akiva.app`
  works; not with a different domain.
- **Incognito:** no persistence (acceptable, expected).
- **iOS Safari:** Web Locks + BroadcastChannel are supported
  from 16.4+. Older versions are not supported (we display a
  "Akiva requires a modern browser" notice).
- **Tab freeze (mobile):** mobile browsers may freeze
  background tabs. A frozen leader doesn't immediately impact
  the Worker, but the throttle can drop to a 60 s tick. On
  mobile we'll prefer the active tab as leader where possible.

### Fallback for older versions

If the Web Locks API is missing (shouldn't happen in supported
browsers):

- Fallback to `localStorage` leader election (mutex via
  timestamp + random nonce). Less reliable, but works.
  Documented only — we don't ship it.

**Decision (today):** Web Locks + Dedicated Worker is the
primary. SharedWorker is not on the roadmap — we may revisit
it in the future as an optimisation if a scenario justifies it.

## 10. Renderer Backend (abstract)

**Principle.** The Compositor produces an abstract layer of
draw commands. Behind it sits the `RendererBackend` trait —
with multiple implementations. Choosing a backend is a host-time
decision, not framework-level.

**Why abstract from the start.** Canvas 2D's performance
degrades around ~2 500 objects. The moment we draw a Shell with
an app grid + toolbars + overlays + dialogs, we leave that
budget. We don't want to discover that on the fifth app.

### Trait

```rust
pub trait RendererBackend: Send {
    /// Initialise with an OffscreenCanvas + device metrics.
    fn init(&mut self, canvas: RenderSurface, metrics: SurfaceMetrics) -> Result<()>;

    /// Apply one frame's draw commands. Commands reference glyph/image handles
    /// the backend caches internally.
    fn submit(&mut self, frame: &FrameCommands) -> Result<()>;

    /// Resize the target surface.
    fn resize(&mut self, width_px: u32, height_px: u32);

    /// Upload / evict image + glyph caches (used by ResourceManager + text pipeline).
    fn upload_image(&mut self, id: ImageId, rgba: &[u8], w: u32, h: u32);
    fn evict_image(&mut self, id: ImageId);

    fn destroy(self);
}

pub enum FrameCommand {
    FillRect        { rect: Rect, rgba: u32 },
    StrokeRect      { rect: Rect, rgba: u32, width_px: f32 },
    FillRoundedRect { rect: Rect, rgba: u32, radius_px: f32 },
    FillText        { origin: Point, text: GlyphRun, rgba: u32 },
    DrawImage       { dst: Rect, image: ImageId, src: Option<Rect> },
    PushClip        { rect: Rect },
    PopClip,
    PushOpacity     { alpha: f32 },
    PopOpacity,
}
```

### Implementations

| backend            | when used                                         | notes                                              |
|--------------------|---------------------------------------------------|----------------------------------------------------|
| `Canvas2DBackend`  | MVP default; compatibility floor                   | `OffscreenCanvas.getContext("2d")` from the Worker |
| `WebGPUBackend`    | Primary post-MVP for performance                   | requires `navigator.gpu` — detect at boot          |
| `WebGL2Backend`    | Optional fallback for older browsers               | only if a target ships without WebGPU              |

**Boot-time selection (in the Worker):**

```rust
let backend: Box<dyn RendererBackend> = if gpu_available() {
    Box::new(WebGPUBackend::new())
} else {
    Box::new(Canvas2DBackend::new())
};
```

**What ships today:** `Canvas2DBackend` only. The trait exists
and the Compositor writes against it — replacing the backend is
a pure implementation addition.

### OffscreenCanvas mandatory

All rendering happens in the **Worker** through an
`OffscreenCanvas` the tab transferred via
`transferControlToOffscreen`. The tab's main thread only
displays — it never paints. This eliminates UI jank when the OS
is busy.

---

## 11. Graphics primitives (coordinate system + colour + density)

Without this section pinned, every widget would invent its own
spec. These are the ground rules:

### Coordinate system

- **Origin:** top-left of every surface (canvas, widget, frame).
- **Axes:** X right, Y **down**. Same as every browser API.
- **Internal units:** the Compositor receives `dp` from layout,
  converts to physical pixels (`px = dp × density`) before
  submitting to the backend.
- **Sub-pixel:** coordinates are `f32`. The backend rounds if
  necessary.

### Density

- **Baseline:** 160 dpi = density `1.0` (`mdpi`).
- **Density buckets** (for picking a drawable variant):
  - `mdpi`    — density ≤ 1.25 (≤ 200 dpi)
  - `hdpi`    — 1.25 < density ≤ 1.75 (≤ 280 dpi)
  - `xhdpi`   — 1.75 < density ≤ 2.5  (≤ 400 dpi)
  - `xxhdpi`  — 2.5  < density ≤ 3.5  (≤ 560 dpi)
  - `xxxhdpi` — density > 3.5         (>  560 dpi)
- **Source:** `window.devicePixelRatio` inside the tab,
  forwarded to the Worker on display registration.

### Colour space

- **MVP:** sRGB only. Every colour API (`0xRRGGBBAA`, TOML
  `#RRGGBB[AA]`) is sRGB.
- **Gamut assumption:** no display-P3 or HDR support in v0.1.
  Add via a colour profile in the manifest in v0.3+.
- **Blending:** "source-over" default. Alpha is pre-multiplied
  inside the renderer (the API exposes straight alpha).

### Font metrics

- **Baseline font stack (MVP):** system-ui, sans-serif.
- **`sp → px`:** `px = sp × density × font_scale`, where
  `font_scale = 1.0` in MVP (future: user preference).
- **Text rendering:** Canvas `fillText` in MVP (Latin + basic
  unicode). Proper shaping (HarfBuzz) + BiDi post-MVP.

### Clocks

- `system.monotonic-nanos`: `performance.now() * 1_000_000`
  inside the Worker.
- `system.wall-millis`: `Date.now()`.

### RNG

- `system.random-bytes(n)` is mapped to the Worker's
  `crypto.getRandomValues`. Always secure; no fallback.

---

## 12. Crash recovery

### App level

- App crash → ActivityManager flips the state to `Trapped`.
- The task stack keeps a snapshot of the activity's saved state.
- The user can choose to relaunch or kill.
- Relaunch: a new instance is created, `on_create` runs with the
  saved state.
- A 30 s grace window applies before the trapped record is
  reaped — see [`docs/risks.md §7.4`](docs/risks.md).

### Runtime level

- Runtime crash → browser reload of the page.
- StorageManager keeps everything in OPFS, state survives
  reload.
- ActivityManager rehydrates the task stack (only the top
  activity reloads).

## 13. Full boot flow

```
1.  Browser loads index.html (in a tab).
2.  index.html loads tab.js (Tab HAL).
3.  Tab HAL creates <canvas>, registers event listeners.
4.  Tab HAL connects to / spawns the kernel Worker (worker.js).
5.  If the worker is fresh: worker loads akiva-kernel.wasm.
6.  Worker: WebAssembly.instantiate(kernel, imports).
7.  Tab registers with the worker as display #N.
8.  Runtime boot (once, in the Worker only):
    a. MemoryManager.init()
    b. StorageManager.init(OPFS)
    c. PackageManager.scan_installed()  → finds apps
    d. PermissionController.load_grants()
    e. ActivityManager.init()
    f. ResourceManager.load_device_config()
    g. IntentRouter.init()
    h. DisplayManager.init() — manages tabs as displays
9.  Framework dispatches an intent: action=MAIN, category=HOME.
10. PackageManager returns: com.akiva.shell.
11. ActivityManager.launch("com.akiva.shell/.HomeActivity", display=1):
    a. WasmLoader loads shell.wasm → new instance.
    b. Calls akiva_app_create.
    c. The app calls `akml::inflate_str_with(&mut ui, LAYOUT, &bindings)` (SDK-side).
    d. SDK: parser → AST → measure → place → paint ops on the Canvas.
    e. Canvas writes the ops into a buffer the framework collects as `FrameCommands`.
    f. Compositor sends draw commands to display #1 via MessagePort.
    g. Tab HAL paints to the canvas.
12. User taps the Calculator icon:
    a. Tab HAL forwards a pointer event to the Worker.
    b. EventQueue routes to Shell.
13. Shell sends an intent: action=MAIN, target=com.akiva.calculator.
14. ActivityManager.launch(calculator) — push onto the stack.
15. WindowManager: transition animation; draw commands flow to
    the same display.
```

## 14. What we keep from AOSP as inspiration

| AOSP                                | Akiva mirrors the idea of                              |
|-------------------------------------|--------------------------------------------------------|
| `ActivityManagerService`            | ActivityManager                                        |
| `PackageManagerService`             | PackageManager                                         |
| `WindowManagerService`              | WindowManager                                          |
| `Resources` / `AssetManager`        | ResourceManager                                        |
| `LayoutInflater`                    | AKML inflater (in the SDK, not the framework — see §2.1) |
| `Intent` + `IntentFilter`           | Intent + IntentRouter                                  |
| XML layouts                         | AKML                                                   |
| XML themes / styles                 | AKML styles                                            |
| `android:id="@+id/..."`             | `id="@+id/..."` (identical)                            |
| dp / sp units                       | dp / sp (identical)                                    |
| `onCreate` / `onPause` lifecycle    | identical                                              |
| `onSaveInstanceState`               | `save_state`                                           |

**We are not copying code. We are taking ideas that were proven
across 20 years in production and reimplementing them in Rust
with adaptations for wasm/browser.**

## 15. What we do **not** take from AOSP

- Java / Kotlin (we are Rust).
- Binder IPC (we have in-process IPC through the runtime).
- ART (a VM — we are wasm).
- SELinux (we have the wasm sandbox).
- Hardware services (the browser handles them).
- SystemServer as a separate process (the framework lives in
  the runtime instance with us).

## 16. Introspection surface

**Principle.** When the runtime is not the DOM, the browser's
regular debug tools (Elements, Inspect) lose access to the UI
structure. The system has to provide a **separate introspection
layer** as a first-class concern, not as an afterthought —
otherwise non-DOM development quickly becomes "blind".

### Two layers

- **Runtime UI** — the actual renderer (Canvas2D / WebGPU; §10),
  the widget tree, the frame commands. What the user sees.
- **Introspection** — overlays showing internal state: a
  widget-tree inspector, an event trace, an activity-stack view,
  a resource-resolution log, frame timing. Separate from the
  renderer and detached from the app's widget tree.

### Implementation principles

- Lives in the **framework**, not the kernel. Reads state via
  existing internal APIs of ActivityManager / ResourceManager /
  Compositor — without extending the app's ABI.
- Active only in dev mode (a session-level flag, not per-app).
- Runs as a separate overlay on top of the Compositor's
  `FrameCommands`, not mixed into them.
- Surfaces out of the worker via `postMessage` to the tab so a
  future DevTools extension can render the trace
  interactively.

### MVP vs. future

Today we **do not** implement introspection. This section is an
architectural anchor that prevents us from adding ad-hoc debug
APIs that pollute the ABI. The actual implementation will land
when the Shell starts hosting more than "kernel: boot" — likely
around the introspection-overlay phase, when the widget tree is
complex enough to justify an inspector.
