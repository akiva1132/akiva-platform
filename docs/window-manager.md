# Window Manager & Shell

> **Implementation status (May 2026):** the `WindowManager`
> ([`framework/src/window_manager.rs`](../framework/src/window_manager.rs))
> ships with the four-layer z-band model below
> (`Wallpaper / App / Overlay / System`), per-surface bounds and
> visibility, and focus tracking. Activities allocate real surfaces
> via `Framework::ensure_activity_surface` (Stone 13.0). Push / pop
> transitions (slide + fade, ease-out cubic) ship in
> [`framework/src/animation.rs`](../framework/src/animation.rs).
> System dialogs ship in
> [`framework/src/dialog.rs`](../framework/src/dialog.rs).
> **Multi-display, toasts, status / nav bar, and PIP are not
> implemented** — every running tab today is exactly one display
> surface; the `Overlay` z-band is reserved but has no widget
> renderer wired into it yet.

## 1. Purpose

The **WindowManager** decides **what is visible on which display at
any given moment**. It is the bridge between the `ActivityManager`
(which knows "who should be Resumed") and the Compositor (which
knows "how to paint").

**Shell** is a regular Akiva app that fills a special role — the
home screen.

## 2. WindowManager responsibilities

- **Display registry** — list of connected displays (tabs).
- **Activity placement** — which activity sits on which display.
- **Z-order** — what paints over what (system UI, dialogs, toasts).
- **Focus management** — which activity receives input events.
- **Transitions** — animating a switch (cuts in v0.1; slide / fade
  with ease-out cubic from Stone 13.3).
- **Chooser / dialogs** — system-owned modal surfaces (intent
  chooser, permission prompts).

## 3. Display model

```rust
pub struct Display {
    pub id: DisplayId,
    pub width_dp: u32,
    pub height_dp: u32,
    pub density: f32,
    pub orientation: Orientation,
    pub message_port: MessagePort,  // link to the Tab HAL
    pub state: DisplayState,
}

pub enum DisplayState {
    Active,        // tab visible, receiving events
    Idle,          // tab exists but unfocused
    Disconnected,  // tab closed
}
```

Displays come and go:

- `register_display(info)` — when a new tab connects.
- `unregister_display(id)` — when a tab closes.
- `update_display(id, info)` — resize, orientation change.

> **Today:** there is exactly one display per Akiva session — the
> first / leader tab. Follower tabs see the multi-tab failover
> banner rather than mirror the leader's content. Multi-display
> support (the API above) is the v0.3+ shape.

## 4. Activity placement

The ActivityManager asks the WindowManager to "show activity X":

```rust
pub trait WindowManager {
    fn attach_activity(&mut self, activity: ActivityId, display: DisplayId);
    fn detach_activity(&mut self, activity: ActivityId);
    fn move_activity(&mut self, activity: ActivityId, target: DisplayId);
    fn render_frame(&mut self, display: DisplayId);
}
```

In the current code,
[`framework/src/window_manager.rs`](../framework/src/window_manager.rs)
exposes a slimmer surface (`create_surface`, `destroy_surface`,
`set_bounds`, `set_visible`, `focus`, `surfaces_in_z_order`) and
the framework wires this to the activity lifecycle via
`ensure_activity_surface` / `release_activity_surface` in
[`framework/src/lib.rs`](../framework/src/lib.rs).

### Today's mapping

Single display: the foreground activity from the task stack is
attached to the `App` layer of the lone display. Background-but-
visible activities (during a transition) keep their surface around
until the transition resolves.

### Future

- **Multi-window** — two activities side-by-side.
- **Picture-in-Picture** — an activity floating in an overlay band.
- **Secondary display** — an activity scoped to tab 2 only.

## 5. Z-order layers

```
┌─────────────────────────────────────┐  ← System  (300):   pinned UI, status / nav bar, blocking dialogs
├─────────────────────────────────────┤
├─────────────────────────────────────┤  ← Overlay (200):   transient panels — toasts, snackbars, drop-downs, IME
├─────────────────────────────────────┤
├─────────────────────────────────────┤  ← App     (100):   every running activity
├─────────────────────────────────────┤
└─────────────────────────────────────┘  ← Wallpaper (0):  background fill, dynamic background
```

The compositor paints layers in increasing-z order. Input events
hit-test top-down — the highest layer covering the pointer claims
the event.

The values (0/100/200/300) deliberately leave gaps so a future
band (e.g. `Drag = 250` for a drag-and-drop ghost) can slot in
without renumbering.

## 6. Input routing

```
Tab HAL receives pointer_down(x, y)
  │
  ▼
MessagePort → Worker
  │
  ▼
EventQueue (kernel) receives the event
  │
  ▼
Dispatcher reads the display id from the event
  │
  ▼
WindowManager.hit_test(display, x, y) → widget id
  │
  ├─ check the System layer first (dialogs, toasts)
  ├─ then Overlay
  ├─ then App (the running activities)
  └─ finally Wallpaper
  │
  ▼
Activity receives akiva_on_event(widget_id, event_type, payload)
  │
  ▼
Callback fires, state updates
  │
  ▼
Next frame: re-layout, re-compose, fresh paint ops
```

## 7. Focus management

At any given moment, on any given display, **exactly one activity**
is in the `Resumed` state — the only state that receives input.

- A new activity opens → the previous goes `Paused`, the new one
  reaches `Resumed`.
- A click in empty space leaves focus where it was (no
  focus-on-click, contrary to web defaults).
- Within an activity, an input widget that has focus consumes
  text-input events first.

### Keyboard / text events

- Only the `Resumed` activity receives them.
- If an input widget has focus, the text goes to it.
- Otherwise the activity gets the raw event in `on_key_event`.
- IME composition resolves through the `compositionend` listener in
  `tab.js`, surfacing as `Event::TextInput { text }` to the
  activity.

## 8. Shell app

### Definition

Shell is **a regular Akiva app**, with a few privileges:

- Pre-installed; cannot be uninstalled.
- Holds `akiva.permission.LAUNCH_ANY_APP` and
  `akiva.permission.QUERY_PACKAGES`.
- Handles the `category=HOME` intent.
- Loads automatically at boot.

### Responsibilities

- **Home screen** — list of installed apps; the launcher.
- **Task switcher** — list of running activities, switch between
  them. *(Not implemented yet.)*
- **System UI host** — render the status bar / nav bar (future).
- **Default fallback** — when an activity finishes and no other
  remains on the stack, control returns to Shell.

### Shell at present

```xml
<!-- apps/shell/res/layout/home.akml -->
<VStack padding="16dp" background="@color/shell_bg" spacing="16dp">
    <Text text="@string/akiva_os" textSize="32sp" textColor="@color/on_bg" />

    <Grid columns="4" spacing="8dp" flex="1">
        <!-- icons inserted dynamically by Shell code -->
    </Grid>
</VStack>
```

```rust
// apps/shell/src/lib.rs
#[akiva::app]
struct Shell {
    installed: Vec<PackageInfo>,
}

impl App for Shell {
    fn on_create(&mut self, ctx: &ActivityContext) {
        self.installed = ctx.package_manager().installed_apps();
        ctx.set_content_view("@layout/home");
        self.render_app_grid(ctx);
    }

    fn on_resume(&mut self, ctx: &ActivityContext) {
        let current = ctx.package_manager().installed_apps();
        if current.len() != self.installed.len() {
            self.installed = current;
            self.render_app_grid(ctx);
        }
    }
}
```

Icons are rendered via `c.draw_foreign_image(&app.id, &app.icon,
…)` — the cross-package drawable cache pulls each package's
`@drawable/icon` resource. Tap dispatches an explicit intent
launching that package's main activity.

## 9. Back navigation

Each display owns its own task stack (today there is one display,
so one stack).

A **Back** event (hardware key, software key, or browser back):

1. Tab HAL emits `back_pressed`.
2. WindowManager → `ActivityManager::on_back()`.
3. ActivityManager:
   - if a dialog is open → close the dialog;
   - else if the current activity has an `on_back_pressed`
     callback → call it, the app may consume / cancel;
   - else → pop the stack;
   - if the stack is empty → return to Shell.

### Browser Back button

The browser maintains its own history. The WindowManager can sync:

- every `pushActivity` → `history.pushState()`;
- a browser-side back triggers a popstate event;
- the event flows into `ActivityManager::on_back()`.

This makes the browser Back button feel native within Akiva.

## 10. Transition animations

**Today (Stone 13.3 + ease-out cubic).** Push transitions
(`SlideFromRight` + `FadeIn`) and pop transitions
(`SlideToLeft` + `FadeOut`) are wired through the
`AnimationController` in
[`framework/src/animation.rs`](../framework/src/animation.rs). The
controller uses cubic ease-out so motion decelerates into place;
linear motion read as stuttery on dense activities (see the
animation source for the rationale comment).

**Available transitions** today: `SlideFromRight`, `SlideFromLeft`,
`SlideToLeft`, `FadeIn`, `FadeOut`. Composition is supported (push
typically uses slide + fade in parallel).

**Future:**

- Shared element (a widget "continues" from one activity to
  another).
- Custom per-activity transitions (declared in the manifest).

The WindowManager owns the animation; the ActivityManager only
declares "transition from X to Y" and the rest is bookkeeping.

## 11. Dialogs and toasts

### Dialogs

Modal surfaces in the `System` band — input below them is blocked.
Examples: intent chooser, permission prompt, custom app dialogs.

The implementation lives in
[`framework/src/dialog.rs`](../framework/src/dialog.rs). The
DialogManager queues pending dialogs and resolutions; the kernel
drains resolutions per frame (see `kernel/src/lib.rs`) and routes
them back to the requesting app via `host_op_done`.

### Toasts

Designed as `Overlay`-band ephemeral surfaces (2–3 second
persistence, no input blocking). The intended SDK shape:

```rust
ctx.show_toast("Saved!");
ctx.show_toast_long("This will take a moment...");
```

**Status:** the layer slot exists; no widget / op renders into it
yet. See [`docs/notifications.md §5`](notifications.md#5-not-yet-implemented).

## 12. System UI (future)

Status bar (top), navigation bar (bottom) will eventually live as
overlays painted by Shell into the `System` band. Today every
activity fills the screen.

## 12.5 Compositor architecture

The WindowManager's surface model is the input to the compositor;
the **compositor** is what turns surfaces into pixels. Today
those two roles are collapsed: the kernel walks
`surfaces_in_z_order` and replays each surface's PaintOps
directly into the one screen `OffscreenCanvas`. There is no
separate "surface texture" — the surface is purely a logical
record of bounds, z-band, and paint ops.

**Target shape (planned, ROADMAP Phase 15):** every surface owns
a backing `OffscreenCanvas` + `Canvas2DBackend`. Activity
PaintOps paint into *that* surface's backend. A separate
compositor pass — running on its own rAF cadence, decoupled
from per-Activity paint — samples the surface canvases into the
screen via `drawImage` in z-order.

This unlocks the OS-grade properties today's model doesn't have:

- A slow Activity stalls **only its own surface refresh**, not
  the system bar or other apps.
- A crashed Activity leaves a frozen pane (its surface canvas
  retains its last pixels), not a black region.
- The compositor pass becomes the natural home for effects
  (live backdrop blur, drop shadows, dim overlays, color
  grading) — Canvas2D fallback for v1, WebGPU for the polished
  version.

The full design — surface lifecycle, compositor scheduling,
effects pipeline (`SurfaceDrawOpts`), memory budget, migration
plan — lives in
[`docs/compositor-architecture.md`](compositor-architecture.md).
Note in particular that this work is **prerequisite** to a
useful WebGPU swap: a WebGPU compositor on top of today's
single-canvas model gains nothing of substance; the per-surface
architecture is the load-bearing change.

## 13. Today's limits

- A single display in practice (the leader tab); follower tabs see
  the multi-tab banner rather than a mirror or independent display.
- No toasts — the layer is reserved, the renderer is not.
- No PIP, no split-screen.
- Dialogs render in a fixed centred shape; size and theming pass
  through whatever the requesting code produces.
- Shell is intentionally minimal — an app grid and not much else.
  No task switcher, no search, no widgets-on-home.
- **Single-canvas direct paint.** Surfaces are logical records;
  there is no per-surface backing texture. A slow or crashed
  Activity affects the whole frame. The fix is the per-Activity
  surface + decoupled compositor described in §12.5 (ROADMAP
  Phase 15).
