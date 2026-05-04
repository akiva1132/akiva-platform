# Activity Model — Processes and Activities

> **Implementation status (May 2026):** the lifecycle state
> machine is complete (six transitions), all four launch modes
> ship (including `SingleInstance` with a real dedicated task),
> Back navigation including back-out from an isolated task,
> `start_activity`, `on_new_intent`, `start_activity_for_result`
> + `on_activity_result`, and multi-task foreground switching
> (`Framework::switch_to_task`) are all live (Stones 9.1–9.6).
> Phase 9 is closed. See [ROADMAP — Phase 9](../ROADMAP.md) for
> the stones and [v0.2-design §4](v0.2-design.md) for the full
> historical gap map.

## 1. Core concepts

**App.** A unit of distribution — a package with a
`manifest.toml`, a `wasm` file, and `res/`. Identified by a
unique `package_id` (e.g. `com.akiva.calculator`).

**Process.** A running instance of an app's wasm. Memory is
isolated. An app may have more than one running process (governed
by launch mode).

**Activity.** A unit of UI — a screen with its own lifecycle. An
Activity belongs to an App and runs inside one of its processes.
An app may declare several Activities (e.g. `MainActivity`,
`SettingsActivity`).

**Task.** A sequence of Activities the user moved through, kept
as a stack. A Task can include Activities from different apps.

## 2. Activity lifecycle

```
        ┌─────────────┐
        │ Not Existing│
        └──────┬──────┘
               │ launch(intent)
               ▼
        ┌─────────────┐
        │   Created   │ ← on_create(saved_state?)
        └──────┬──────┘
               │ on_start()
               ▼
        ┌─────────────┐
        │   Started   │  (visible, not in focus)
        └──────┬──────┘
               │ on_resume()
               ▼
        ┌─────────────┐
        │   Resumed   │  (visible, in focus — "running")
        └──────┬──────┘
               │ on_pause()    ← another activity took focus
               ▼
        ┌─────────────┐
        │   Paused    │
        ├─────────────┤
        │ ← on_resume() │
        └──────┬──────┘
               │ on_stop()     ← no longer visible
               ▼
        ┌─────────────┐
        │   Stopped   │
        ├─────────────┤
        │ ← on_restart() → on_start()  │
        └──────┬──────┘
               │ on_destroy()
               ▼
        ┌─────────────┐
        │  Destroyed  │
        └─────────────┘
```

### Guarantees

- `on_create` fires exactly once.
- `on_destroy` fires exactly once and is **guaranteed** to fire
  even if the activity is killed (unless the process itself
  crashes).
- `on_pause` **always** runs before `on_stop`.
- `on_stop` **always** runs before `on_destroy`.
- `on_resume` runs **at least once** before any `on_pause`.

### Maximum allowed durations

- `on_create`: up to 100 ms (the user is waiting).
- `on_pause`: up to 50 ms (the transition has to be fast).
- `on_stop`, `on_destroy`: up to 200 ms (not user-visible).

If an app exceeds these, the runtime logs a warning today and
will be allowed to kill the process in a later phase.

### Implementation (Phase 2b + Phase 9.1)

| layer                                                                 | status            | location                                                                                                                                                                                                                                                                                                  |
|-----------------------------------------------------------------------|-------------------|-----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `LifecycleState` enum — `Initial / Created / Started / Resumed / Paused / Stopped / Destroyed` | ✅                | [`framework/src/activity_manager.rs`](../framework/src/activity_manager.rs)                                                                                                                                                                                                                              |
| Legal-transition matrix (including `Stopped → Started` for `on_restart`) | ✅                | `legal_transition` + 2 unit tests                                                                                                                                                                                                                                                                          |
| Drivers: `resume` / `pause` / `destroy` / `advance`                    | ✅ Phase 9.1       | 13 unit tests on a mock `RecordingActivity` covering each transition, idempotency, and the cycle edges (`Stopped → destroy`, `Paused → destroy`)                                                                                                                                                          |
| Framework bring-up / tear-down (`drive_to_resumed`, `launch_app`, `finish_top`) | ✅                | [`framework/src/lib.rs`](../framework/src/lib.rs)                                                                                                                                                                                                                                                         |
| Kernel ABI: `hal_app_lifecycle(handle, transition)`                    | ✅                | [`kernel/src/host.rs`](../kernel/src/host.rs), `KernelWasmHost`                                                                                                                                                                                                                                            |
| Worker dispatch: `LIFECYCLE_EXPORTS[0..5]` → `akiva_app_{create,start,resume,pause,stop,destroy}` | ✅                | [`runtime/web/app_registry.js`](../runtime/web/app_registry.js)                                                                                                                                                                                                                                            |
| SDK exports (all 6 `akiva_app_*` `extern "C"`)                         | ✅                | [`sdk-rust/src/lib.rs`](../sdk-rust/src/lib.rs) `akiva_activity!` macro                                                                                                                                                                                                                                    |
| SDK trait — `on_create / start / resume / pause / stop / destroy` (no-op defaults) | ✅                | [`sdk-rust/src/activity.rs`](../sdk-rust/src/activity.rs)                                                                                                                                                                                                                                                  |
| `save_state_bytes` / `restore_state_bytes` wired through `Create → restore → Start` | ✅                | framework `launch_app`, SDK runtime                                                                                                                                                                                                                                                                        |
| `on_restart` as a distinct callback                                    | ❌ by design      | `Stopped → Started` re-fires `on_start`; apps that need "first start vs. restart" gate on internal flags                                                                                                                                                                                                  |
| `finish()` inside `on_create` short-circuit (`Created → Destroyed`)    | ✅ Stone 9.3      | `legal_transition` adds the edge; `advance_through_create` reports whether finish was raised; `launch_app` calls `destroy_shortcut` (fires only `on_destroy`) and pops the stack entry instead of continuing to Start / Resume. Android parity                                                            |
| Wasm apps can call `ctx.finish()`                                      | ✅ Stone 9.3      | SDK `Context::finish()` → `akiva_host_finish(handle)` import → worker sets a per-handle flag → kernel drains via `hal_app_drain_finish` at the end of every ctx-bearing callback → framework flips `ActivityContext.finish_requested`                                                                     |
| `on_new_intent` on SingleTop reuse                                     | ✅ Stone 9.4      | `Framework::launch_app` detects `new_id == current top` → `deliver_new_intent` runs the callback, no bring-up chain, no stack churn. Test: `singletop_reuse_fires_on_new_intent_instead_of_bring_up`                                                                                                       |
| Wasm apps can call `ctx.start_activity(&intent)`                        | ✅ Stone 9.4      | SDK `Context::start_activity` → `akiva_host_start_activity(handle, ptr, len)` import with [`Intent::encode`](../sdk-rust/src/intent.rs) → kernel drains via `hal_app_drain_intent` → `IntentRouter::resolve` → `launch_app`                                                                                |
| SDK `Activity::on_new_intent(ctx, &intent)` + `akiva_app_new_intent`    | ✅ Stone 9.4      | Default no-op; dispatched from `runtime::on_new_intent(handle, ptr, len)`. `WasmActivity::on_new_intent` re-encodes the decoded `Intent` and forwards via `WasmHost::new_intent` → `hal_app_new_intent`                                                                                                    |
| `on_new_intent` on `SingleTask` / `SingleInstance` reuse                | ✅ Stone 9.2      | `ActivityManager::launch` returns [`LaunchOutcome`](../framework/src/activity_manager.rs); reuse scans the foreground task for a matching `(pkg, class)`, pops everything above, resumes the target, then delivers the new intent                                                                          |
| `SingleInstance` dedicated-task isolation                              | ✅ Stone 9.6      | `ActivityManager::launch` allocates a new task per `SingleInstance` fresh-create. Reuse scans **all** tasks. Test: `launch_singleinstance_lives_in_its_own_task` + `singleinstance_fresh_create_allocates_dedicated_task_and_foregrounds_it`                                                                |
| Multi-task foreground switching                                        | ✅ Stone 9.6      | [`ActivityManager::bring_task_to_front`](../framework/src/activity_manager.rs); `Framework::switch_to_task` (Shell-facing). Internal `task_history: Vec<TaskId>` so `finish_top` reverts to the prior task when the foreground task empties                                                                |
| `start_activity_for_result` + `set_result` + `on_activity_result`       | ✅ Stone 9.5      | SDK `Context::start_activity_for_result(&intent, request_code)` / `Context::set_result(&result)`; framework tracks `(caller_id, request_code)` against the callee in a `pending_result_for` map; `finish_top` drains the result (or synthesises `RESULT_CANCELED`) before the caller's `Paused → Resumed`. Wire pinned in [docs/abi.md §2.5.2](abi.md). Tests: `start_for_result_delivers_picked_value_to_caller` + `callee_finishing_without_set_result_synthesises_canceled` |
| `start_activity_for_result` against a reuse target                     | ⏭️ by design      | The reused activity never "finishes" in the delivery sense — the framework silently drops the `request_code`. Android parity                                                                                                                                                                              |

Bug caught by the Phase 9.1 test sweep: `destroy()` used to
`break` on the first illegal edge, so destroying a `Paused` or
`Stopped` activity silently skipped `on_stop` / `on_destroy`.
Fixed to ignore-and-continue, matching `resume`.

Bug caught by the post-9.1 state-machine audit: `destroy()`
called from `Initial` silently dropped the instance and rewrote
the stack even though no lifecycle callback fired. Guard added so
cleanup only runs once the record actually reached `Destroyed`.

## 3. Saved state

### `on_save_state` / `on_restore_state`

An activity may save a small piece of state that survives even if
the process is killed:

```rust
impl Activity for MyApp {
    fn save_state(&self) -> SavedState {
        let mut s = SavedState::new();
        s.put_i32("counter", self.counter);
        s.put_str("input", &self.input);
        s
    }

    async fn on_restore_state(&mut self, ctx: &mut Context<'_>, state: &SavedState) {
        self.counter = state.get_i32("counter").unwrap_or(0);
        self.input   = state.get_str("input").unwrap_or_default();
    }
}
```

**Timing:**

- `save_state` — before `on_stop` (not before `on_pause`, since
  pause stays in the same process).
- `on_restore_state` — after `on_create`, when a saved state
  exists.

**Limits:**

- 64 KB per activity.
- Serialisable types only.
- **Not** a long-term persistence channel — recovery only.

> **✅ Stone 11.4c regression — `on_save_state` degraded —
> fixed (P0 #1, May 2026).**
>
> The fix: a new method `Activity::dispatch_save_state_async(&self) -> u64`.
> `WasmActivity` overrides it to return a `future_id` from
> `host.save_state()`; the framework holds a `pending_saves:
> Vec<PendingSave>` and `destroy_with_persist` records an entry
> there before `pause → stop → destroy`. A new `drive_pending_saves`
> runs once per frame from `akiva_frame` and persists bytes when
> the worker replies. In-process activities continue to use the
> sync `save_state_bytes` path. See
> [`docs/sdk-rust.md §19.2`](sdk-rust.md) and
> [`docs/risks.md §7.2`](risks.md).

### Persistent state

For long-lived data, use the StorageManager API
([`docs/storage.md`](storage.md)).

### State pattern — three layers

`save_state` with a 64 KB cap is intentionally small, like
Android's `onSaveInstanceState` (the Bundle limit is 1 MB but in
practice < 50 KB is used). In real code there are three layers
of state, each with a separate purpose:

| layer            | purpose                                                                                    | when persisted                       | where                              | size              |
|------------------|--------------------------------------------------------------------------------------------|--------------------------------------|-----------------------------------|-------------------|
| **UI state**     | What is on screen right now (scroll position, selected tab, unsaved form input).            | `save_state`                         | Kernel in-memory + OPFS at kill   | ≤ 64 KB           |
| **ViewModel**    | Computed activity state irrelevant to persistence (cached queries, currently-displayed list).| Not persisted across processes.      | In memory in the app wasm.        | bound by quota    |
| **Persistent**   | App data (user files, settings, DB).                                                        | Explicit through `storage::*`.       | StorageManager (OPFS).            | bound by quota    |

**Rule.** If it doesn't fit in 64 KB serialised, it doesn't
belong in `save_state`. Anything bigger that may be lost →
ViewModel (re-compute). Anything that must not be lost → explicit
storage.

**Example — task-list app:**

```rust
struct TasksApp {
    // Persistent: written explicitly to storage; source of truth.
    storage_key: &'static str,

    // ViewModel: hydrated from storage on on_create.
    tasks: Vec<Task>,
    filter: Filter,

    // UI state: persisted via save_state.
    scroll_position: f32,
    edit_in_progress: Option<String>,
}

impl Activity for TasksApp {
    async fn on_create(&mut self, ctx: &mut Context<'_>) {
        if let Some(fut) = storage::read("tasks") {
            if let Ok(Some(bytes)) = fut.await {
                self.tasks = serde_json::from_slice(&bytes).unwrap_or_default();
            }
        }
    }

    async fn on_restore_state(&mut self, _ctx: &mut Context<'_>, state: &SavedState) {
        self.scroll_position    = state.get_f32("scroll").unwrap_or(0.0);
        self.edit_in_progress   = state.get_str("edit").map(String::from);
    }

    fn save_state(&self) -> SavedState {
        let mut s = SavedState::new();
        s.put_f32("scroll", self.scroll_position);
        if let Some(ref edit) = self.edit_in_progress {
            s.put_str("edit", edit);
        }
        s
    }
}
```

**Why not save `tasks` in `saved_state`:** with 5 000 tasks it
exceeds 64 KB; and the source of truth is storage anyway —
they're rehydrated from there every time. `saved_state` is for
state that is small and worth preserving across a process death.

## 4. Launch modes

Same as Android, four modes:

### `standard` (default)

Every `start_activity` creates a new instance. You can see five
instances of the same activity in the stack.

### `singleTop`

If the activity is already at the top of the stack, no new
instance is created — the intent is delivered to the existing one
through `on_new_intent`. Otherwise a new instance is created.

Useful for a Search activity: when the user searches from a
search screen, prefer the same screen with a new intent.

### `singleTask`

There is **exactly one instance** in a given task. If it already
exists, every activity above it is cleared and it is resumed.

Useful for the main activity of an app — Browser, Calculator.

### `singleInstance`

There is **exactly one instance system-wide**, and it lives in a
**dedicated task**. No other activity may join its task.

Useful for dialer, phone-call screens.

> **Implementation (Stone 9.6).** `ActivityManager::launch`
> allocates a fresh dedicated task on a `SingleInstance`
> fresh-create, and reuse scans **every** task in the system
> (not just the foreground). The framework calls
> `bring_task_to_front` after the launch to surface the dedicated
> task and pauses the previous foreground top. Exit (Back or
> `finish()`) returns the foreground to the previous task via
> `task_history`.

### Manifest declaration

```toml
[[activities]]
id = "main"
launch_mode = "singleTask"
```

## 5. Task stack

### Structure

```rust
pub struct Task {
    pub id: TaskId,
    pub affinity: String,                  // default = package_id
    pub activities: Vec<ActivityInstance>, // bottom → top
}
```

### Task affinity

Same affinity → activities collect in the same task. Different
affinity → separate tasks.

Default: `affinity = package_id`. Two different apps → two
tasks. All activities of the same app → one task.

### Back order

A Back press:

1. If there's an activity above the bottom of the stack → pop it.
   The previous activity becomes resumed.
2. If it's the last activity in the task → the task ends; the
   user returns to the previous view (typically Shell).
3. If it's the only task → the user returns to Shell.

### Walk-through

```
Stack (top → bottom):
  Calculator.SettingsActivity  ← Resumed
  Calculator.MainActivity      ← Stopped
  Shell.HomeActivity           ← Stopped

User presses Back:
  Calculator.SettingsActivity  → on_pause → on_stop → on_destroy
  Calculator.MainActivity      → on_restart → on_start → on_resume

Stack after:
  Calculator.MainActivity      ← Resumed
  Shell.HomeActivity           ← Stopped
```

## 6. Process states (kernel view)

In addition to Activity states, a process has its own state:

- **FOREGROUND** — has an activity in `Resumed`. Killed only in
  emergencies.
- **VISIBLE** — has an activity in `Started` (paused, not in
  focus). Killed only when forced.
- **BACKGROUND** — every activity is `Stopped`. Eligible for
  killing.
- **CACHED** — no activities, but the process is kept around for
  fast restart.
- **EMPTY** — empty process; immediate kill candidate.

### Low-Memory Killer

When the MemoryManager detects pressure
([`framework/src/lmk.rs`](../framework/src/lmk.rs)):

1. Kill CACHED processes.
2. If still tight → EMPTY, then BACKGROUND.
3. VISIBLE only as a last resort.
4. FOREGROUND is never killed automatically.

Before kill: `save_state` runs, the bytes are persisted.

## 7. Recovery

### App crash

If the wasm instance traps / panics:

1. ActivityManager flips its state to `Trapped`.
2. WindowManager *will* show a "Calculator crashed" toast (the
   toast widget is not yet implemented — see
   [`docs/notifications.md §5`](notifications.md)).
3. The user can:
   - press Back → return to the previous activity;
   - tap Restart → a new instance is created with the saved
     state.

The `Trapped` record is reaped after a 30 s grace window — see
[`docs/risks.md §7.4`](risks.md) for the recycle policy.

### Process killed by the OS

1. Activity state ends up `Killed`.
2. If the user navigates back to it via the back stack →
   relaunch with the saved state.
3. The user should not perceive a difference (unless state was
   not saved correctly).

## 8. Multi-display behaviour

`ActivityInstance` carries a `display_id`. WindowManager decides
which display shows what.

Today there is one effective display per session (the leader
tab); follower tabs see the multi-tab banner. The model anticipates
multi-display:

- Single display (one tab): every activity is on the same display.
- Multi-display (multiple tabs, future): Shell on display 1, an
  app the user opened in tab 2 → display 2.
- Moving an activity between displays (future):
  `WindowManager::move_activity(id, target_display)`.

## 9. Manifest declaration

```toml
[[activities]]
id            = "main"                                  # unique within the app
class         = "CalculatorMainActivity"                # log/debug label
launch_mode   = "standard"                              # standard | singleTop | singleTask | singleInstance
task_affinity = "com.akiva.calculator"                  # default = package_id
exported      = true                                    # may other apps launch it?
entry         = true                                    # default-launched activity?

[[activities.intent_filters]]
actions    = ["akiva.intent.action.MAIN"]
categories = ["akiva.intent.category.LAUNCHER"]
```

## 10. End-to-end lifecycle example

### Scenario: user opens Calculator, switches to Shell, returns to the calculator, the OS kills the process, the user returns again

```
T=0    Shell.Home in Resumed state
T=1    User taps the Calculator icon
T=1    Shell.Home → on_pause
T=1    Calc.Main → on_create(saved_state = None)
T=1    Calc.Main → on_start → on_resume
T=1    Shell.Home → on_stop
T=10   User presses system Back to Shell
T=10   Calc.Main → on_pause → on_stop (but NOT on_destroy)
T=10   Shell.Home → on_restart → on_start → on_resume
T=10   Calc.Main save_state called → state saved to storage

(later, low memory)
T=60   OS kills the Calculator process. Calc.Main state = Killed.

T=100  User taps the Calculator icon again
T=100  Shell.Home → on_pause
T=100  ActivityManager sees that saved state exists
T=100  New Calculator process spawned
T=100  Calc.Main → on_create(saved_state = restored)
T=100  Calc.Main → on_restore_state
T=100  Calc.Main → on_start → on_resume
T=100  Shell.Home → on_stop

(The user sees the same state as when they left — display value, etc.)
```

This is the standard Android shape. Akiva implements it in Rust
with wasm.
