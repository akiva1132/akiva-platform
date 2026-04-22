# Activity boundary — Framework vs SDK

> **Research output for [v0.2-review.md](v0.2-review.md) Task 2.** Phase 9
> will add four context-facing APIs (`finish`, `start_activity`,
> `start_activity_for_result`, `on_new_intent`, `on_activity_result`). This
> doc inventories the current duplication between the two `Activity`
> traits, identifies a concrete gap that blocks Phase 9.3+, and proposes a
> split between **context state** and **host imports**.

## 1. What's duplicated today

Two `Activity` traits exist:

| symbol                    | framework-side ([activity.rs](../framework/src/activity.rs)) | SDK-side ([sdk-rust/activity.rs](../sdk-rust/src/activity.rs)) |
| ------------------------- | ------------------------------------------------------------ | -------------------------------------------------------------- |
| trait name                | `Activity`                                                   | `Activity`                                                     |
| context type              | `ActivityContext<'a>`                                        | `Context<'a>`                                                  |
| metrics struct            | `SurfaceMetrics`                                             | `Metrics`                                                      |
| pointer/key types         | `PointerEvent`, `KeyEvent`, `PointerPhase`, `KeyPhase`       | same names, different module                                   |
| render signature          | `render(ctx, &mut FrameCommands)`                            | `on_render(ctx, &mut Canvas)`                                  |
| save/restore              | `save_state_bytes() -> Vec<u8>` / `restore_state_bytes(&[u8])` | `on_save_state(&mut SavedState)` / `on_restore_state(&SavedState)` |
| context extras            | `finish_requested: &mut bool`, `launch_requested: &mut Option<Intent>` | `class: &'a str` (SDK-only)                             |

The split is **deliberate**, not accidental:
- The framework runs in the kernel wasm — it needs typed intent objects
  it can hand to `IntentRouter`, and it needs to never panic the whole OS.
- The SDK runs inside each app's own wasm — it speaks a friendlier API
  (`Canvas`, `SavedState`) that encapsulates the paint-op and key/value
  byte formats.

Both are reached through the `WasmActivity` proxy: framework-side
`WasmActivity` implements the framework `Activity` trait by forwarding
each method to `self.host.lifecycle(handle, transition)`. The SDK-side
`Activity` trait is invoked directly by the `akiva_activity!` macro's
generated `akiva_app_*` exports — there is no shared trait at the ABI
boundary, only a shared lifecycle *state*.

**Verdict on duplication shape:** correct. Each side's types carry the
idioms appropriate to where they execute. Collapsing into one trait would
force either the kernel to know about `Canvas` or the app to know about
`FrameCommands` — neither is desirable.

## 2. The concrete gap — apps can't finish themselves

Reading the two context types side-by-side makes one gap loud:

```rust
// framework
pub struct ActivityContext<'a> {
    pub finish_requested: &'a mut bool,             // ← drained post-callback
    pub launch_requested: &'a mut Option<Intent>,   // ← drained post-callback
    // ...
}

// SDK
pub struct Context<'a> {
    // no finish_requested
    // no launch_requested
    pub class: &'a str,
}
```

The framework-side out-params are consumed by `Framework::apply_post` to
trigger `finish_top` or `launch_app`. Built-in activities like `Shell` set
`ctx.finish_requested = true` and it Just Works.

A wasm app **has no way to set either flag**. The `WasmActivity` proxy's
`on_pointer`/`on_key`/`on_create`/… all forward through
`host.lifecycle(handle, transition)` but the return path carries no
signal. Today, a wasm calculator that wants "pressing Esc closes me" must
rely on the framework's built-in back-key handler — it cannot raise
`finish` itself.

**This blocks:**
- Stone 9.3 (Back stack + `system.back()`) — the import `system.back()`
  needs to *reach* the framework's `ActivityContext.finish_requested` (or
  an equivalent one-shot flag).
- Stone 9.4 (`start_activity` from app) — same mechanism, different flag
  (`launch_requested`).
- The `finish()`-inside-`on_create` shortcut from [Task 3 §1](research-android-lifecycle.md) —
  depends on having an in-app way to raise finish at all.

## 3. The Phase-9 API table

| API (app-visible)                   | Stone | Our current boundary choice        |
| ----------------------------------- | ----- | ---------------------------------- |
| `ctx.finish()`                      | 9.3   | host import `akiva_host_finish()` → framework flips `finish_requested` |
| `ctx.start_activity(intent)`        | 9.4   | host import `akiva_host_start_activity(ptr, len)` carrying an encoded `Intent` blob |
| `ctx.start_activity_for_result(intent, code)` | 9.5 | host import with request code; framework stashes on the launching record |
| SDK `on_new_intent(intent)` hook    | 9.4   | new ABI export `akiva_app_new_intent(handle, ptr, len)` on the app side |
| SDK `on_activity_result(req, result)` hook | 9.5 | new ABI export `akiva_app_activity_result(handle, req, ptr, len)` |
| `ctx.back()` (imperative)           | 9.3   | same import as `finish()` — `system.back()` is really "finish top if not root" |

**Why imports, not wider context?** Two reasons:

1. **Lifetime.** `ActivityContext.finish_requested` is `&'a mut bool` for
   one callback. A wasm app's `on_create` → request finish → return is
   fine, but a paint-op render that triggers finish mid-frame would need
   the flag to outlive the context. Imports call the host synchronously
   whenever the app wants, with no borrow dance.
2. **WIT forward-compat.** v1 (Component Model, abi.md §3) represents
   every action as a method on an `akiva:app/host` interface — matching a
   raw import 1:1. A context field has no clean WIT equivalent; it would
   collapse to "set a shared resource at position X" which is exactly the
   footgun the Component Model is trying to kill.

The framework's `ActivityContext.finish_requested`/`launch_requested`
stay — but as **framework-internal bookkeeping only**. Built-in
activities (Shell) keep setting them; imports from wasm apps set the
same flags indirectly by calling `akiva_host_finish()` which flips them
on the active dispatch. Wasm apps do not see the flags at all.

## 4. SDK Context delta

What the SDK's `Context` needs to gain in Phase 9:

```rust
pub struct Context<'a> {
    pub metrics: Metrics,
    pub timestamp_ms: f64,
    pub pointer: Option<PointerEvent>,
    pub class: &'a str,
    // ── new in Phase 9 ──
}

impl<'a> Context<'a> {
    pub fn finish(&self);                                        // Stone 9.3
    pub fn start_activity(&self, intent: Intent);                // Stone 9.4
    pub fn start_activity_for_result(&self, intent: Intent,
                                     request_code: u32);          // Stone 9.5
    // …
}
```

Each method encodes its argument and calls a host import. `Intent`
needs a wire format — the SDK grows its own `Intent` type that encodes
to the same blob the framework's `Intent` decodes from. (Symmetric to
how `SavedState` is typed on the SDK and opaque on the framework.)

That is **all** the duplication we commit to: an SDK `Intent` that
encode/decode-roundtrips against framework `Intent`. No shared
`Activity` trait, no shared `Context`.

## 5. Decision — keep traits flat

Alternative considered: split SDK `Activity` into `LifecycleActivity`
(create/start/resume/pause/stop/destroy) + `InputActivity` (pointer/key/resize)
+ `StateActivity` (save/restore) + `ResultReceiver` (new: activity_result).

**Rejected.** Two reasons:
- The entire flat trait is ~10 methods with default no-ops. The ceremony
  of four traits + trait-object coercion for ~10 tiny methods is pure
  overhead, especially for app authors who may be encountering Rust
  traits for the first time.
- Android's own `Activity` is one huge class. The "split into mixins"
  refactor has been attempted inside AOSP multiple times (Jetpack
  `ComponentActivity` etc.) and keeps landing back on "one trait,
  default no-ops." There is no reason to expect we'll do better.

**Decision:** flat trait on both sides. The SDK trait grows three
context methods (`finish`, `start_activity`, `start_activity_for_result`)
and two new callbacks (`on_new_intent`, `on_activity_result`) as Phase 9
lands. No sub-traits. No proxy-generation macro — the `akiva_activity!`
macro in `sdk-rust/src/lib.rs` continues to do the one-time generation
of `akiva_app_*` exports; three new exports get added there.

## 6. Actions from this task

1. `docs/abi.md §2.3` — planned Phase-9 imports (`akiva_host_finish`,
   `akiva_host_back`, `akiva_host_start_activity`,
   `akiva_host_start_activity_for_result`, `akiva_host_set_result`) and
   planned app-side exports (`akiva_app_new_intent`,
   `akiva_app_activity_result`) documented with their stone numbers.
2. `ROADMAP` Stone 9.3 — explicit bullet to wire `Context::finish()` →
   `akiva_host_finish` import → framework's existing
   `ActivityContext.finish_requested` bookkeeping. Same bullet calls out
   `Context::back()` for the same frame of work.

No SDK code added yet — a stub `ctx.finish()` that silently no-ops would
mislead app authors. Stone 9.3 adds it end-to-end (import + SDK wrapper +
framework dispatch + test). The shape is settled.
