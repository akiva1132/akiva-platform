# Akiva SDK — Rust

## 1. Purpose

The SDK is a Rust crate that apps depend on. It wraps the WIT
bindings (`akiva:app@0.1.0`) in a familiar, safe, idiomatic API.
The developer never sees the Component Model directly — the whole
lifecycle / events / resources / widgets surface goes through
high-level types.

**Under the hood:** `wit-bindgen rust --world guest` produces the
raw bindings from `wit/akiva.wit`. The SDK wraps them with macros
(`#[akiva::app]`, `#[akiva::callback]`) so app authors don't have
to touch them.

## 2. Mental model

**An app = a struct + AKML + callbacks.**

- **Struct.** App state. Fields that change at runtime.
- **AKML.** UI is defined externally in `res/layout/*.akml`.
- **Callbacks.** Functions invoked when something happens (a
  click, a lifecycle event).

The SDK runs the machinery behind the scenes — macros generate
all the lifecycle hooks, the callback registry, and the binding
to the ABI.

## 3. Project setup

```toml
# apps/my-app/Cargo.toml
[package]
name    = "my-akiva-app"
version = "0.1.0"
edition = "2021"

[lib]
crate-type = ["cdylib"]

[dependencies]
akiva-sdk = { version = "0.1" }

[profile.release]
lto           = true
opt-level     = "z"
codegen-units = 1
```

**Important:** `crate-type = ["cdylib"]` so the wasm is a
complete module.

```toml
# apps/my-app/manifest.toml
[app]
id              = "com.example.my_app"
name            = "My App"
version         = "0.1.0"
version_code    = 1
min_abi_version = "0.1.0"

[runtime]
wasm = "my_akiva_app.wasm"

[[activities]]
id    = "main"
entry = true

[[activities.intent_filters]]
actions    = ["akiva.intent.action.MAIN"]
categories = ["akiva.intent.category.LAUNCHER"]
```

## 4. Hello World

### `src/lib.rs`

```rust
use akiva_sdk::prelude::*;

#[derive(Default)]
pub struct HelloApp {
    count: i32,
}

#[async_trait(?Send)]
impl Activity for HelloApp {
    async fn on_render(&mut self, ctx: &mut Context<'_>, c: &mut Canvas<'_>) {
        c.clear(0xff202030);
        c.draw_text(
            20.0, 40.0, 24.0, 0xffffffff,
            &format!("Clicks: {}", self.count),
        );
        c.fill_round_rect(20.0, 60.0, 100.0, 36.0, 4.0, 0xff3070ff);
        c.draw_text(34.0, 84.0, 16.0, 0xffffffff, "Click me");

        // Pointer state lives on `ctx`; consult it to flip behaviour.
        let _ = ctx;
    }

    async fn on_pointer(&mut self, _ctx: &mut Context<'_>, ev: PointerEvent) {
        if ev.phase == PointerPhase::Down
            && (20.0..120.0).contains(&ev.x) && (60.0..96.0).contains(&ev.y)
        {
            self.count += 1;
        }
    }

    fn on_save_state(&self, state: &mut SavedState) {
        state.put_int("count", self.count as i64);
    }
    fn on_restore_state(&mut self, state: &SavedState) {
        if let Some(v) = state.get_int("count") { self.count = v as i32; }
    }
}

akiva_sdk::akiva_app!(HelloApp);
```

This app is written imperatively — using `Canvas` directly. For
a real app you'd want the `Ui` builder (§9) or an AKML layout
(§10). The three full examples that match the `akiva new
--template` output are in §16–§18.

### Build

```bash
cd apps/my-app
akiva build       # shortcut for cargo build --target wasm32-unknown-unknown + wasm-opt
```

## 5. The `akiva_app!` macro

The SDK provides one macro that adds every wasm export App ABI
v0 needs:

```rust
akiva_sdk::akiva_app!(HelloApp);
```

What it generates:

- `#[no_mangle] extern "C" fn akiva_app_dispatch(...)`,
  `akiva_app_poll_dispatch(...)`, `akiva_app_get_result(...)` —
  three exports that replaced the per-callback export zoo from
  before Stone 11.6.5.
- `akiva_app_drive_spawned` — re-poll mechanism for spawned
  tasks in `TaskScope` (Stone 11.6.5 #11).
- Allocator hooks and instance management (`Default::default()`
  is called once on the first handle; the instance is reused for
  follow-up handles).
- The whole mini-executor that runs the `async fn` callbacks
  (`crate::executor`).

The call must happen **exactly once** in the app's `lib.rs`. Two
calls in the same crate are a linker error because of the
`#[no_mangle]` symbols.

The type passed to the macro must be `Default` (the runtime
creates an instance per handle without going through a
constructor over the ABI) and `Activity` (see §6).

A `#[akiva::activity]` macro for apps with multiple activities
does not yet exist. For now, the manifest can declare several
activity records, but they all point at the same `Activity`
impl. True separation is planned post-Phase 13.

## 6. The `Activity` trait

Current shape (post-Stone 11.2). Every callback is `async fn`
except `on_save_state` / `on_restore_state`, which are sync by
design (see the comment in `activity.rs:314-321`):

```rust
#[async_trait(?Send)]
pub trait Activity: 'static {
    // ── Lifecycle (docs/activity-model.md §2) ──────────────────────
    async fn on_create(&mut self, _ctx: &mut Context<'_>) {}
    async fn on_start(&mut self, _ctx: &mut Context<'_>) {}
    async fn on_resume(&mut self, _ctx: &mut Context<'_>) {}
    async fn on_pause(&mut self, _ctx: &mut Context<'_>) {}
    async fn on_stop(&mut self, _ctx: &mut Context<'_>) {}
    async fn on_destroy(&mut self, _ctx: &mut Context<'_>) {}

    // ── Input ───────────────────────────────────────────────────────
    async fn on_pointer(&mut self, _ctx: &mut Context<'_>, _ev: PointerEvent) {}
    async fn on_key(&mut self, _ctx: &mut Context<'_>, _ev: KeyEvent) {}
    async fn on_text_input(&mut self, _ctx: &mut Context<'_>, _text: &str) {}
    async fn on_resize(&mut self, _ctx: &mut Context<'_>, _m: Metrics) {}

    // ── Render ──────────────────────────────────────────────────────
    /// Paint one frame. Called at rAF cadence (typically 60 Hz).
    /// Must be fast — every op is serialised immediately.
    async fn on_render(&mut self, _ctx: &mut Context<'_>, _c: &mut Canvas<'_>) {}

    // ── Intent redelivery (Stone 9.4) + activity-result (9.5) ───────
    async fn on_new_intent(&mut self, _ctx: &mut Context<'_>, _intent: &Intent) {}
    async fn on_activity_result(
        &mut self,
        _ctx: &mut Context<'_>,
        _request_code: u32,
        _result: &ActivityResult,
    ) {}

    // ── Saved state (sync on purpose) ───────────────────────────────
    fn on_save_state(&self, _state: &mut SavedState) {}
    fn on_restore_state(&mut self, _saved: &SavedState) {}
}
```

**Minimum implementation.** Only `on_render` is meaningfully
required (the screen always needs to paint); everything else
defaults.

**What changed since Stone 11.2.** Before 11.2 the trait was
synchronous; `async fn` was introduced with one
`block_on_ready` poll per callback. Since Stone 11.6.5 the
runtime drives a real executor (see §19.1) that can pause on
`Pending` and resume in the next frame. The trait shape is
unchanged — apps that worked with `block_on_ready` work with
the real executor.

**`Send` bound.** `async_trait(?Send)` because the wasm worker
is single-threaded; adding `Send` / `Sync` bounds would force
`Rc` / `RefCell` interiors all over the app for no benefit.

**Saved state — why sync.** `on_save_state` is invoked from a
synchronous destroy path
(`framework::wasm_activity::dispatch_save_state_async`). Making
it async would force a future allocation per save with no
benefit; the bytes flow out of the app instance, not in from the
host.

## 7. `Context`

The handle passed to every async callback. The public field
`metrics: Metrics` exposes the surface (width / height /
density), and `timestamp_ms: f64` is the frame-stable monotonic
timestamp. Methods that issue host calls return `Pending` if
needed and are async-shaped because they may be resumed in the
next frame.

```rust
pub struct Context<'a> {
    pub metrics:      Metrics,    // width_px, height_px, density
    pub timestamp_ms: f64,        // frame timestamp (monotonic)
    // … internals (host handle, pending signal slots) …
}

impl<'a> Context<'a> {
    // Navigation (async — kernel resolves the dispatch)
    pub async fn start_activity(&mut self, intent: &Intent);
    pub async fn start_activity_for_result(
        &mut self, intent: &Intent, request_code: u32,
    );
    pub async fn finish(&mut self);
    pub async fn set_result(&mut self, result: &ActivityResult);

    // Spawned tasks (Stone 11.6.5; cancellation: see §19.5)
    pub fn activity_scope(&self) -> TaskScope; // lives until on_destroy
    pub fn resumed_scope(&self) -> TaskScope;  // cancelled at on_pause

    // Stone 13.2 — system dialog overlays. Returns immediately with
    // a `DialogFuture`; the framework paints the dialog on a
    // dedicated `WindowLayer::Overlay` surface and shields this
    // activity from pointer events while the dialog is open.
    pub fn show_dialog(
        &mut self, title: &str, body: &str, buttons: &[&str],
    ) -> DialogFuture;
}

pub enum DialogResult {
    /// User tapped the button at this index in the original `buttons`
    /// slice. Always `< buttons.len()`.
    Button(usize),
    /// User tapped outside the dialog box, or the framework dismissed
    /// the dialog (e.g. the activity was destroyed mid-prompt).
    Dismissed,
}
```

**Dialog usage.** Empty `buttons` resolves immediately to
`Dismissed`. The framework reserves the dialog box geometry per
frame from the current `metrics`, so the dialog re-flows on
resize without an extra hook. Single-dialog invariant per
activity: opening a second dialog on the same handle dismisses
the first.

```rust
let scope = ctx.activity_scope();
let fut = ctx.show_dialog(
    "Clear?", "This will reset the calculator.", &["Cancel", "OK"],
);
let confirmed = self.confirmed.clone();   // Rc<Cell<bool>>
scope.spawn(async move {
    if matches!(fut.await, DialogResult::Button(1)) {
        confirmed.set(true);
    }
});
// On the next render: read-and-reset `confirmed`, then act.
```

**Changes since the Stone 11.0 baseline.** `set_content_view`,
`find_view`, `inflate`, and `show_toast` were removed — the
direct `Canvas` (§8) and the `Ui` builder (§9) replaced the
AKML view-handle model. AKML is still supported (§10) but
through `inflate_str_with` per frame, not via persistent
handles.

**`set_content_view` previously.** It was needed to bind an
activity to an AKML root. Today `on_render` is the entry point
for paint; every frame the app builds its tree on `Canvas` or
through `Ui` / AKML inflate. There is no separate "set the
layout" step.

## 8. `Canvas`

Immediate-mode painter. The API is meant to be invoked inside
`on_render`; calling it outside a render context produces
ill-defined ops with no compile-time protection. All colours are
`u32` in `0xAARRGGBB` (alpha first).

```rust
impl<'a> Canvas<'a> {
    pub fn clear(&mut self, rgba: u32);
    pub fn fill_rect(&mut self, x: f32, y: f32, w: f32, h: f32, rgba: u32);
    pub fn fill_round_rect(
        &mut self, x: f32, y: f32, w: f32, h: f32, radius: f32, rgba: u32,
    );
    pub fn draw_text(
        &mut self, x: f32, y: f32, size: f32, rgba: u32, text: &str,
    );

    // Stone 13.0prep B — outline ops. `width` is the stroke in dp;
    // strokes are centre-aligned (Canvas 2D default).
    pub fn stroke_rect(&mut self, x: f32, y: f32, w: f32, h: f32, width: f32, rgba: u32);
    pub fn stroke_round_rect(
        &mut self, x: f32, y: f32, w: f32, h: f32, radius: f32, width: f32, rgba: u32,
    );
    pub fn draw_line(
        &mut self, x0: f32, y0: f32, x1: f32, y1: f32, width: f32, rgba: u32,
    );

    // Stone 13.0prep C — bitmap blit. `drawable_id` from
    // `res::drawable("name")`; `0` paints a magenta placeholder.
    pub fn draw_image(&mut self, drawable_id: u32, x: f32, y: f32, w: f32, h: f32);

    // Stone 13.0 follow-up — cross-package drawable. Resolves
    // through the foreign-drawable cache; used by Shell to render
    // launcher icons of apps it doesn't own.
    pub fn draw_foreign_image(&mut self, package_id: &str, name: &str,
                              x: f32, y: f32, w: f32, h: f32);

    // Clipping + transforms (push/pop pairs)
    pub fn push_clip(&mut self, x: f32, y: f32, w: f32, h: f32);
    pub fn pop_clip(&mut self);
    pub fn push_transform(&mut self, dx: f32, dy: f32, scale: f32);
    pub fn pop_transform(&mut self);
}
```

`Canvas` itself doesn't display anything — it is a serialiser.
Ops flow to the host through wire encoding and reach Canvas2D /
WebGL2 there. This means paint code must be quick (≤ 16 ms per
frame, including logic) but isn't doing GPU acrobatics.

## 9. The `Ui` builder

A builder layer above `Canvas` that handles layout (VStack /
HStack / grid), pointer hit-testing, and widget state.
Cross-frame state lives in a `UiState` the app holds as a field.

```rust
impl<'s, 'c, 'b> Ui<'s, 'c, 'b> {
    pub fn new(state: &'s mut UiState, ctx: &'c mut Context<'_>, c: &'b mut Canvas<'_>) -> Self;

    // Leaf widgets
    pub fn text(&mut self, text: &str) -> TextBuilder;     // .size(), .color(), .build()
    pub fn button(&mut self, action: &str, label: &str) -> NodeId;

    // Containers
    pub fn v_stack(&mut self, children: &[NodeId]) -> StackBuilder;
    pub fn h_stack(&mut self, children: &[NodeId]) -> StackBuilder;

    // Commit + dispatch
    pub fn present(&mut self, root: NodeId);
    pub fn was_clicked(&self, action: &str) -> bool;
    pub fn was_triggered(&self, action: &str) -> bool;     // alias, used in compose-style
}
```

`feed_pointer` on `UiState` is called from `on_pointer` to
inject the latest event coords into the builder. The pattern:

```rust
async fn on_pointer(&mut self, _ctx: &mut Context<'_>, ev: PointerEvent) {
    self.ui.feed_pointer(ev);
}
```

See §16 for a full example (the `default` template).

## 10. `Intent` construction

```rust
use akiva_sdk::intent::*;

let intent = Intent::new(actions::MAIN)
    .with_category(categories::LAUNCHER)
    .with_package("com.example.app");

let intent = Intent::new(actions::VIEW)
    .with_data("https://example.com");

let intent = Intent::new(actions::SEND)
    .with_mime_type("text/plain")
    .with_extra("TEXT", IntentValue::Str("Hello!".into()));
```

Constants for actions / categories follow the catalogue in
[`docs/intents.md`](intents.md).

## 11. Resources

```rust
use akiva_sdk::res;

// Get resources manually (rare — most uses go through AKML)
let welcome: String = res::string("@string/welcome");
let primary: u32    = res::color("@color/primary");

// Stone 13.0prep C — `@drawable/` resolution. `0` is the missing
// sentinel; the renderer paints a magenta placeholder for it
// (parallel to the magenta returned by missing `@color/`).
let icon: u32 = res::drawable("icon");
canvas.draw_image(icon, 16.0, 16.0, 48.0, 48.0);

// Format args
let greeting = res::string_fmt("@string/hello_name", &[("name", "Akiva")]);
```

## 12. Storage

The API is async (Stone 11.6.7 #1). `read` returns
`Option<ReadFuture>` — `None` if the app didn't declare
`STORAGE_SANDBOX` in its manifest; otherwise a future resolving
to `Result<Option<Vec<u8>>, StorageError>`. `write` returns a
`WriteHandle`; dropping it without `await` keeps fire-and-forget
behaviour (the host_op was already sent), but `#[must_use]`
warns on `storage::write(k, v);` without a `let _ =`.

```rust
use akiva_sdk::storage;

async fn on_create(&mut self, _ctx: &mut Context<'_>) {
    if let Some(fut) = storage::read("notes") {
        if let Ok(Some(bytes)) = fut.await {
            self.text = String::from_utf8_lossy(&bytes).into_owned();
        }
    }
}

async fn save(&self) {
    // Awaiting the WriteHandle gets durability confirmation:
    let _ = storage::write("notes", self.text.as_bytes()).await;
}

fn fire_and_forget_write(&self, value: &[u8]) {
    // `let _ =` silences must_use; the host_op is still sent.
    let _ = storage::write("ephemeral", value);
}
```

`storage::remove(key)` exists synchronously (no durability ack).
`storage::list_keys(prefix)` resolves a prefix scan to
`Vec<String>` (post-11.6.7 follow-up).

### `time::*` (Stone 13.0prep A)

```rust
use akiva_sdk::time;

// Park for ≥ ms of wall-clock; resolves with ().
time::sleep(200).await;

// Race a future against a deadline.
match time::timeout(storage::read("k").unwrap(), 500).await {
    Ok(Ok(Some(bytes))) => self.value = bytes,
    Ok(Ok(None)) | Ok(Err(_)) => self.value = Vec::new(),
    Err(time::Elapsed) => log_warn!("read timed out"),
}

// Periodic ticker. First tick fires after `period_ms`, not immediately.
let mut tick = time::interval(16);
loop { tick.tick().await; /* one frame */ }
```

`sleep` is best-effort — subject to JS event-loop scheduling
and tab throttling on background tabs (the host caps each delay
at 60 s as a safety bound). Cancel-on-drop isn't honest yet: a
`Sleep` dropped before its timer fires leaves a stale futures
entry until the worker dies; bounded by `MAX_PENDING_FUTURES =
256`. A documented limitation.

## 13. Logging

Macros from `prelude` (not a module path):

```rust
use akiva_sdk::prelude::*;

log_debug!("Counter is {}", app.count);
log_info!("App started");
log_warn!("Unusual value: {:?}", value);
log_error!("Failed to parse: {:?}", err);
log_trace!("very fine-grained: x={x:.4}, y={y:.4}");
```

The macros encode the message + level via
`host_fire(FIRE::LOG, ...)`. The host filters by `package_id` +
level (debug audible on debug builds; info+ on release). Stone
11.6.7 added a batching pass that emits log lines once per frame
to amortise `postMessage` overhead — transparent to the app.

## 14. Build & Run

```bash
# Create a new app — defaults to the `default` template (§16).
# Pass --template compose-style (§17) for AKML+Bindings, or
# --template game (§18) for a per-frame physics scaffold.
akiva new my-app
akiva new mynotes --template compose-style
akiva new pong    --template game

# Build wasm
cd my-app
akiva build

# Run (dev server + auto-install)
akiva run

# Open the SDK docs in a browser
akiva docs
```

## 15. Three full examples — one per template

`akiva new` ships three templates. Each demonstrates a different
working style:

| template          | UI shape                        | when                                       | §  |
|-------------------|---------------------------------|--------------------------------------------|----|
| `default`         | imperative `Ui` builder         | simple starts, hot-loop state              | §16 |
| `compose-style`   | declarative AKML + Bindings     | tabular UI, theming, RTL                   | §17 |
| `game`            | direct `Canvas` in `on_render`, dt-driven | games, animations, simulations | §18 |

All examples run on the same `Activity` trait — choosing a
style is up to the app author. You can also mix (game-style
canvas + Ui overlay).

## 16. Example — the `default` template (counter)

```rust
use akiva_sdk::prelude::*;

#[derive(Default)]
pub struct Counter {
    ui:    UiState,
    count: i32,
}

#[async_trait(?Send)]
impl Activity for Counter {
    async fn on_create(&mut self, ctx: &mut Context<'_>) {
        log_info!("Counter created ({}x{} @ {:.1}x)",
            ctx.metrics.width_px, ctx.metrics.height_px, ctx.metrics.density);
    }

    async fn on_pointer(&mut self, _ctx: &mut Context<'_>, ev: PointerEvent) {
        self.ui.feed_pointer(ev);
    }

    async fn on_render(&mut self, ctx: &mut Context<'_>, c: &mut Canvas<'_>) {
        c.clear(0xff0f1420);
        let mut ui = Ui::new(&mut self.ui, ctx, c);

        let title   = ui.text("Counter").size(28.0).color(0xfff0f4fa).build();
        let counter = ui.text(&format!("count = {}", self.count))
            .size(20.0).color(0xffe0e6f0).build();
        let inc     = ui.button("inc", "+1");

        let root = ui.v_stack(&[title, counter, inc])
            .gap(16.0).padding(24.0).build();
        ui.present(root);

        if ui.was_clicked("inc") { self.count += 1; }
    }

    fn on_save_state(&self, state: &mut SavedState) {
        state.put_int("count", self.count as i64);
    }
    fn on_restore_state(&mut self, state: &SavedState) {
        if let Some(v) = state.get_int("count") { self.count = v as i32; }
    }
}

akiva_sdk::akiva_app!(Counter);
```

**Notes.** `UiState` keeps widget state across frames (focused
button, click-in-progress); `feed_pointer` injects the latest
event coords. `was_clicked("inc")` is checked only after
`present()` — the builder knows who was clicked once layout
closes. There's no ID-stability requirement: the string `"inc"`
is the action key and is hashed.

## 17. Example — the `compose-style` template (notes-like)

The UI is defined in `res/layout/main.akml`:

```xml
<?akml version="1.0"?>
<VStack padding="24" gap="16" cross="stretch">
    <Text text="@string/app_name" size="28" color="@color/title_fg" />
    <Text text="{greeting}, world!" size="36" color="@color/greeting_fg" />
    <Text text="Clicks: {count}" size="18" color="@color/body_fg" />
    <HStack gap="12">
        <Button id="greet" label="@string/cta_greet" onClick="greet" flex="1" />
        <Button id="reset" label="@string/cta_reset" onClick="reset" />
    </HStack>
</VStack>
```

```rust
use akiva_sdk::prelude::*;
use akiva_sdk::akml::{self, Bindings};
use akiva_sdk::res;

const LAYOUT:    &str       = include_str!("../res/layout/main.akml");
const GREETINGS: &[&str]    = &["Hello", "Hola", "Bonjour", "Shalom"];

#[derive(Default)]
pub struct Greeter {
    ui:           UiState,
    greeting_idx: i32,
    count:        i32,
}

#[async_trait(?Send)]
impl Activity for Greeter {
    async fn on_pointer(&mut self, _ctx: &mut Context<'_>, ev: PointerEvent) {
        self.ui.feed_pointer(ev);
    }

    async fn on_render(&mut self, ctx: &mut Context<'_>, c: &mut Canvas<'_>) {
        c.clear(res::color("bg_window"));

        let greeting = GREETINGS[(self.greeting_idx as usize) % GREETINGS.len()];
        let bindings = Bindings::new()
            .text("greeting", greeting.to_string())
            .text("count",    format!("{}", self.count));

        let mut ui = Ui::new(&mut self.ui, ctx, c);
        let root = akml::inflate_str_with(&mut ui, LAYOUT, &bindings)
            .expect("layout is statically valid — inflate failure is a build bug");
        ui.present(root);

        if ui.was_triggered("greet") {
            self.greeting_idx = (self.greeting_idx + 1) % (GREETINGS.len() as i32);
            self.count       += 1;
        }
        if ui.was_triggered("reset") {
            self.greeting_idx = 0;
            self.count        = 0;
        }
    }

    fn on_save_state(&self, state: &mut SavedState) {
        state.put_int("greeting_idx", self.greeting_idx as i64);
        state.put_int("count",        self.count        as i64);
    }
    fn on_restore_state(&mut self, state: &SavedState) {
        if let Some(v) = state.get_int("greeting_idx") { self.greeting_idx = v as i32; }
        if let Some(v) = state.get_int("count")        { self.count        = v as i32; }
    }
}

akiva_sdk::akiva_app!(Greeter);
```

**Notes.** `Bindings` fills `{placeholder}` tokens in the AKML
with runtime strings. `@string/...` and `@color/...` resolve
through `res/values/`; `res/values-night/` is selected
automatically when the OS reports dark mode. The
`onClick="greet"` in AKML is observed via
`ui.was_triggered("greet")` — the string is the stable contract.

The AKML inflate runs **every frame**. The cost is known
(~5 µs parse — see `bench_akml_parse`); the trade-off beats
keeping a pretty-printed tree alive across frames.

## 18. Example — the `game` template (ball physics)

`on_render` runs at rAF cadence; use `ctx.timestamp_ms` as a
monotonic time source to compute `dt`. The example: a ball
bouncing off the walls; tap to kick.

```rust
use akiva_sdk::prelude::*;

const BALL_R:     f32 = 24.0;
const INIT_SPEED: f32 = 280.0;
const MAX_DT_MS:  f32 = 100.0;
const BG_COLOR:   u32 = 0xff0a0e1a;
const BALL_COLOR: u32 = 0xfff97316;

pub struct Ball {
    cx: f32, cy: f32,
    vx: f32, vy: f32,
    last_ts: Option<f64>,
    bounds:  (f32, f32),
}

impl Default for Ball {
    fn default() -> Self {
        Self {
            cx: 0.0, cy: 0.0, vx: INIT_SPEED, vy: INIT_SPEED * 0.85,
            last_ts: None, bounds: (1.0, 1.0),
        }
    }
}

#[async_trait(?Send)]
impl Activity for Ball {
    async fn on_create(&mut self, ctx: &mut Context<'_>) {
        let w = ctx.metrics.width_px  as f32;
        let h = ctx.metrics.height_px as f32;
        if self.cx == 0.0 && self.cy == 0.0 {
            self.cx = w * 0.5;
            self.cy = h * 0.5;
        }
        self.bounds = (w, h);
    }

    async fn on_pointer(&mut self, _ctx: &mut Context<'_>, ev: PointerEvent) {
        if ev.phase == PointerPhase::Down {
            self.cx = ev.x; self.cy = ev.y;
            let (w, h) = self.bounds;
            let dx = ev.x - w * 0.5;
            let dy = ev.y - h * 0.5;
            let len = (dx*dx + dy*dy).sqrt().max(1.0);
            self.vx = INIT_SPEED * (dx / len);
            self.vy = INIT_SPEED * (dy / len);
        }
    }

    async fn on_render(&mut self, ctx: &mut Context<'_>, c: &mut Canvas<'_>) {
        let w = ctx.metrics.width_px  as f32;
        let h = ctx.metrics.height_px as f32;
        self.bounds = (w, h);

        // dt in seconds, clamped — first frame OR resume after suspend
        // both have a stale `last_ts`; the clamp prevents tunneling.
        let dt = match self.last_ts {
            Some(prev) => ((ctx.timestamp_ms - prev) as f32 / 1000.0)
                .clamp(0.0, MAX_DT_MS / 1000.0),
            None => 0.0,
        };
        self.last_ts = Some(ctx.timestamp_ms);

        self.cx += self.vx * dt;
        self.cy += self.vy * dt;

        if self.cx - BALL_R < 0.0 { self.cx = BALL_R;     self.vx = self.vx.abs(); }
        if self.cx + BALL_R > w   { self.cx = w - BALL_R; self.vx = -self.vx.abs(); }
        if self.cy - BALL_R < 0.0 { self.cy = BALL_R;     self.vy = self.vy.abs(); }
        if self.cy + BALL_R > h   { self.cy = h - BALL_R; self.vy = -self.vy.abs(); }

        c.clear(BG_COLOR);
        c.fill_round_rect(
            self.cx - BALL_R, self.cy - BALL_R,
            BALL_R * 2.0, BALL_R * 2.0,
            BALL_R, BALL_COLOR,
        );
    }

    fn on_save_state(&self, state: &mut SavedState) {
        state.put_int("cx", self.cx.to_bits() as i64);
        state.put_int("cy", self.cy.to_bits() as i64);
        state.put_int("vx", self.vx.to_bits() as i64);
        state.put_int("vy", self.vy.to_bits() as i64);
    }
    fn on_restore_state(&mut self, state: &SavedState) {
        if let Some(v) = state.get_int("cx") { self.cx = f32::from_bits(v as u32); }
        if let Some(v) = state.get_int("cy") { self.cy = f32::from_bits(v as u32); }
        if let Some(v) = state.get_int("vx") { self.vx = f32::from_bits(v as u32); }
        if let Some(v) = state.get_int("vy") { self.vy = f32::from_bits(v as u32); }
    }
}

akiva_sdk::akiva_app!(Ball);
```

**Notes.** The `f32::to_bits ↔ from_bits` round-trip preserves
position and velocity through `SavedState` (whose tags are
integer-only on the wire). `MAX_DT_MS` clamping is the safety
net for resume after a long tab switch — without it, the first
frame after resume would slingshot the ball across the screen.

## 19. Known limitations (Stone 11.6+ backlog)

This document records real SDK gaps observed after 11.4c
(worker-per-app) and 11.5 (fault containment). See
[`docs/isolation-design.md` §10.5 Phase 11 Foundation Gaps](isolation-design.md)
for the full work plan.

### 19.1 ✅ The async executor — fixed (P0 #4, May 2026)

**Was.** The `Activity` trait is `async fn`, but
`block_on_ready` polled exactly once and panicked on `Pending`.
Every cross-worker future (`storage::read`) had to be wrapped in
a deferred handle (`ReadHandle::poll()` in `on_render`) —
a leaky abstraction that forced apps to manage state by hand
across frames.

**Fix (~700 lines, breaking ABI bump).** New ABI (bumped to
`0.2.0`) + a hand-rolled mini-executor in
`sdk-rust/src/executor.rs` (~120 lines, not embassy). Three
exports replace the previous pair:

```rust
akiva_app_dispatch(dispatch_id: u64, cmd: u32, ptr, len) -> u32   // 0=accept, !=0=refuse
akiva_app_poll_dispatch(dispatch_id: u64) -> u64                   // packed (status, len)
akiva_app_get_result(dispatch_id: u64, out_len_ptr) -> *const u8   // single-shot
```

The wasm runtime now holds a `Pin<Box<dyn Future + 'static>>`
per handle, captured by raw pointer into `slot.activity` and
`slot.ctx`. Self-referential but contained: the `Slot` lives in
`HashMap<u32, Box<Slot>>` (heap-pinned); the framework
guarantees one dispatch per handle at any time
(`PendingTransition` discipline). See `crate::executor` and
`crate::runtime` for all SAFETY invariants.

**Render and `save_state` stay synchronous** — paint code
shouldn't wait on host I/O, and `save_state` is `&self` in the
trait. `executor::run_to_ready` still panics on `Pending` —
accidental awaits in `on_render` surface immediately.

**`storage::read` becomes a real `Future`:**

```rust
async fn on_create(&mut self, _ctx: &mut Context<'_>) {
    if let Some(fut) = storage::read("notes") {
        if let Ok(Some(bytes)) = fut.await {
            self.text = String::from_utf8_lossy(&bytes).into_owned();
        }
    }
}
```

The `.await` legitimately suspends the dispatch; the worker
shell registers `pendingDispatches`, waits for `host_op_done`,
re-polls. `ReadHandle` was deleted.

**Why not embassy?** `embassy-executor` adds ~10 KB per app and
is built around interrupt-driven wakers + timer drivers
irrelevant in a wasm worker. The 120-line custom executor fits
the one-future-per-handle pattern exactly, with re-poll
triggered externally by `host_op_done`. Zero bundle bloat over
the legacy stub.

### 19.2 ✅ `save_state` degraded under cross-worker — fixed (P0 #1, May 2026)

**Was.** In Stone 11.4c, `WasmActivity::save_state_bytes`
called `host.save_state()` + `poll_future()` in the same frame;
post-cross-worker the first poll always returned `Pending`;
release fell through to `Vec::new()` → Phase 6 saved-state was
broken.

**Fix.** Added `Activity::dispatch_save_state_async(&self) ->
u64`. `WasmActivity` overrides it to return the `future_id`
from `host.save_state()`; the synchronous `save_state_bytes`
returns `Vec::new()` for wasm activities. The framework keeps a
`pending_saves: Vec<PendingSave>`; in `destroy_with_persist` it
calls `dispatch_save_state_async` before `pause → stop →
destroy`, and the worker FIFO handles them in order. A new
`Framework::drive_pending_saves` runs once per frame from
`akiva_frame`, polls each future, and persists bytes when the
worker replies. Empty bytes → delete; `Err` → drop without
persist.

In-process activities (tests, future built-ins) keep using the
sync `save_state_bytes` — the framework consults it only when
`dispatch_save_state_async` returned `0`.

### 19.3 ✅ `storage::write` returns `WriteHandle` (Stone 11.6.7 #1, May 2026)

**Was.** `pub fn write(key, value)` had no return value. The
host did the write and posted `host_op_done` with a status, but
the SDK abandoned the `future_id`. Apps that needed durability
(settings, notes, drafts) couldn't tell whether the write
failed.

**Fix.** `pub fn write(key: &str, value: &[u8]) -> WriteHandle`
where `WriteHandle: Future<Output = Result<(), StorageError>>`.
`storage::write(k, v).await?` gets confirmation;
`let _ = storage::write(k, v);` (drop without await) preserves
the pre-Stone fire-and-forget shape — the `STORAGE_SET` host_op
is sent inside the function. `WriteHandle` is `#[must_use]`, so
`storage::write(k, v);` (no `let _`) emits a compiler warning
for apps writing important data.

### 19.4 ✅ `pkgs_wire` decoder dedup — fixed (Stone 11.6.7 #2, May 2026)

**Was.** Three duplicates of the decoder:
[`framework/src/wire/pkgs.rs`](../framework/src/wire/pkgs.rs),
[`sdk-rust/src/pkgs.rs`](../sdk-rust/src/pkgs.rs), and
`runtime/web/app_registry.js::_warmupWorkers`. Drift risk every
time a field was added.

**Fix.** New workspace crate `akiva-wire` (at
`wire/src/pkgs.rs`) is the canonical source. Both Rust crates
that touch the wire format depend on it via
`workspace.dependencies.akiva-wire`; their existing files are
now thin re-exports. The JS side can't import a Rust crate, but
`_warmupWorkers` carries a `// SCHEMA:` comment pointing at
`wire/src/pkgs.rs` — drift surfaces in code review when the
layout changes.

Side effect: `PackageEntry.entry_class` was renamed to
`PackageEntry.class` for consistency with the wire crate (apps
only used `id` and `name`, no migration needed).

### 19.4a 🟢 Other minor SDK gaps

- **No public `ctx.is_trapped(other_handle)` query.** An
  activity can't tell whether another activity it launched
  crashed. Relevant to launcher UI (the Stone 11.5d UI was
  deferred until after the foundation lands).

### 19.5 ✅ TaskScope cancellation — fixed (Stone 11.6.5 #11, May 2026)

**Was.** `Context::activity_scope()` /
`Context::resumed_scope()` returned `&'static TaskScope` whose
`spawn(fut)` was literally `block_on_ready(fut)` — no
cross-callback retention, no cancellation, no lifecycle
awareness. The doc-comment said the quiet part out loud:
*"renamed `block_on_ready`."* Locking that into Stone 11.7's
docs would have been bedrock-level damage.

**Fix.** `TaskScope` carries `(handle, kind)` by value;
`spawn<F: Future + 'static>` queues into `SPAWN_QUEUE`
(re-borrow-safe), drained into `Slot.spawned: Vec<SpawnedTask>`
after each dispatch poll. New SDK export
`akiva_app_drive_spawned()` polls every spawned task across
every slot; the worker shell calls it after each
`host_op_done` and dispatch resolution. Cancellation:
`PendingDispatch.cmd_kind` lets `drive_pending` drop
`Resumed`-scoped spawns when a `PAUSE` resolves; `DESTROY`
evicts the slot which drops `spawned` by Drop.

### 19.6 ✅ Concurrent host calls per activity — fixed (Stone 11.6.5 #12, May 2026)

**Was.** `Slot.pending: Option<PendingDispatch>` allocated a
single slot per handle. Two `storage::read("a")` and
`storage::read("b")` from the same activity → the second
panicked or trampled. The invariant was correct for the
framework's callback FIFO (PendingTransition discipline) but
wrong for user-spawned tasks that don't go through
PendingTransition.

**Fix.** `Slot.pending` is now
`HashMap<u64, PendingDispatch>` keyed by `dispatch_id`. Every
`install_*` / `run_*_sync` helper takes a `dispatch_id`
parameter and inserts there; `drive_pending(id)` and
`get_result(id)` operate on a specific entry; the
`debug_assert` "second dispatch arrived" is gone. Soundness for
callback dispatches still holds — the framework still
serialises them. User-spawned tasks live in `Slot.spawned`
(§19.5), are `'static` futures with no `&mut Activity` borrow,
and run concurrently with whatever callback is in flight.

### 19.7 🟡 No streams primitive (Phase-13 prep, doesn't block 12)

**The gap.** The ABI knows one-shot futures only (`io_start`
→ eventual `io_poll` → result, single-shot). There is no
`stream<T>` primitive: sensor readings, audio samples, file
watch, animation tickers.

**What this breaks.** Every naturally-stream capability has to
be wrapped as polling-as-future, which loses backpressure and
ordering guarantees. A drag/scroll gesture with sub-frame
ordering would complicate the app event loop instead of being
a clean `stream.next().await`.

**What it doesn't break right now.** Phase 12 doesn't
introduce a capability that needs a stream.

**Blocker for Phase 13+.** WindowManager event routing,
animation tickers, real-time data — all streams. WASI 0.3
(v0.5+ migration) is built around `stream<T>`. Every day of
delay raises the migration cost.

**Plan.** A new op_kind `STREAM_OPEN` returning `stream_id`;
`stream_poll(stream_id)` returning `Vec<Item>` of values
batched since the prior poll; `stream_close(stream_id)`. Wire
shape mirroring Component Model `stream<T>`.

**Recommendation.** Push to Phase 13 prep — the problem
domain will be clearer when the WindowManager needs event
streams. **Not in Stone 11.6.5.** Documented in
[`docs/isolation-design.md §10.5 #13`](isolation-design.md).

### Status & ordering

See
[`docs/isolation-design.md §10.5 Phase 11 Foundation Gaps`](isolation-design.md)
for the full work plan. In short:

- **P0** (closed): ✅ §19.1 (async executor — cardinal),
  ✅ §19.2 (save_state pipeline), graceful shutdown, Trapped
  recycle.
- **P1** (closed): ✅ magic-numbers config, ✅ drain phase
  separation, ✅ §19.3 (WriteHandle), ✅ §19.4 (pkgs_wire
  dedup).
- **🔴 Stone 11.6.5 (closed)** — ✅ §19.5 (real TaskScope),
  ✅ §19.6 (concurrent host calls per activity).
- **🔴 Stone 11.6.7 (closed)** — WriteHandle (§19.3) +
  pkgs_wire dedup (§19.4) + direct readonly storage handles +
  lazy-spawn workers + watchdog tunability. See
  [ROADMAP.md Stone 11.6.7](../ROADMAP.md) for the full DoD.
- **P2** (Phase 11.8): e2e tests, worker-shell unit tests.
- **Phase 13 prep:** §19.7 (streams).
