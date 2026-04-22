# Android Lifecycle — edge cases relevant to Akiva

> **Research output for [v0.2-review.md](v0.2-review.md) Task 3.** The point was
> not to encyclopedically document Android — it was to find state-machine edges
> we haven't handled yet and decide whether to copy them before committing to
> Phase 9.2–9.4. Five questions; answers grounded in official docs, with gaps
> filled from community references. Each ends with a **Verdict** for us.

Sources used throughout:
- [Activity lifecycle](https://developer.android.com/guide/components/activities/activity-lifecycle) — primary AOSP doc.
- [Tasks and the back stack](https://developer.android.com/guide/components/activities/tasks-and-back-stack) — launch modes + intent flags.
- [Activity API reference](https://developer.android.com/reference/android/app/Activity) — per-callback semantics.
- [Codelab — Stages of the Activity lifecycle](https://developer.android.com/codelabs/basic-android-kotlin-compose-activity-lifecycle) — onRestart flow.
- [Robolectric issue #2956](https://github.com/robolectric/robolectric/issues/2956) — community clarification on `finish()` inside `onCreate()`.

---

## 1. `finish()` called inside `onCreate()`

**Android behaviour.** `onCreate()` is allowed to call `finish()` synchronously.
When it does, the framework **skips `onStart` and `onResume`**; `onCreate`
returns, then `onDestroy` is called. `onPause` and `onStop` **may also be
skipped** — the docs are hand-wavy, community posts and Robolectric's
matching-fix confirm the shortcut. `onSaveInstanceState` is explicitly not
called when `finish()` triggers the destruction (official doc quote:
"`onSaveInstanceState()` is not called when the user explicitly closes the
activity or in other cases when `finish()` is called.").

**Our model.** Our `legal_transition` matrix has no `Created → Destroyed`
edge. Current reality is that `advance_through_create` flips us to `Created`
unconditionally; a wasm activity that tries to self-finish from `on_create`
today would have to wait until the next bring-up step since we don't surface
a shortcut. And `finish_requested` is only drained at the end of the
dispatch closure — `drive_to_resumed` doesn't check it between steps, so a
finish asserted inside `on_create` is ignored until `on_resume` finishes.

**Verdict — WANT, not critical.** The right-shape fix:
1. Add `(Created, Destroyed)` to the matrix.
2. Teach `drive_to_resumed` (or its split halves) to short-circuit if
   `finish_requested` flipped during `on_create`.
3. Skip `on_start`/`on_resume`, go direct to `on_destroy`.
Pin with a test. Deferrable to Phase 9.3 bundled with Back-stack polish — not
blocking 9.2.

---

## 2. `onRestart` as a distinct callback

**Android behaviour.** `onRestart` is a documented callback that fires
**only on the `Stopped → Started` edge** — i.e. when an activity that was
fully backgrounded returns to being visible (user hits Back from the
activity above, or returns from Recent Apps). It is **always followed by
`onStart`**, then `onResume` if the activity comes to the foreground. Its
stated purpose: "an opportunity to refresh any state that was released in
`onStop`." The docs explicitly note that most apps don't override it.

**Our model.** Already documented in `docs/activity-model.md` §2 with a
deliberate "❌ by design — Stopped → Started re-fires `on_start`." Our state
machine collapses the two: any `Stopped → Started` transition fires exactly
`on_start`, same as the first entry. An app that needs "refresh only on
restart" checks its own init flag.

**Verdict — SKIP, confirmed.** The research backs the design decision. The
callback provides no information the app can't already derive from whether
it reached `Stopped` previously. Add a note to
`docs/activity-model.md` pointing apps at the "guard with a bool in your
state" pattern if the question ever comes up.

---

## 3. `onNewIntent` trigger matrix

**Android behaviour.** `onNewIntent` is invoked on the *existing* activity
instance when the system decides to deliver a fresh `Intent` to it instead of
creating a new one. It fires for:
- `launchMode="singleTop"` — when the match is already on top of the task.
- `launchMode="singleTask"` — when an instance exists (possibly in another
  task); the route-to-existing-instance path delivers via `onNewIntent` and
  destroys everything above it.
- `FLAG_ACTIVITY_SINGLE_TOP` — per-launch equivalent of singleTop.
- `FLAG_ACTIVITY_CLEAR_TOP` — destroys intervening activities, routes to
  the now-top via `onNewIntent`.

Ordering: the existing instance gets `onNewIntent` **while still in its
current state**, then `onResume` if it wasn't resumed (after a Pause/Restart
chain if it was deeper).

**Our model.** `activity_manager::launch` already reuses `SingleTop` tops
without lifecycle churn, but **we don't fire any callback**. A wasm app with
two invocations of the same activity gets the second launch silently
swallowed. This is fine for Phase 8 — there's no way to pass parameters to a
launch yet — but will matter the instant Stones 9.4–9.5 land intent payloads.

**Verdict — CRITICAL for Phase 9.4.** When Intent payloads arrive:
1. Add `on_new_intent(&mut self, ctx, intent)` to the framework Activity trait.
2. On SingleTop reuse, fire it before handing control back to the app.
3. Make it part of the WasmActivity proxy (new ABI export).
Pin with the same mock-recording test pattern as Stone 9.1. Mentioned in
ROADMAP 9.4 as a sub-step rather than a separate stone — it's the payload
delivery mechanism, not a new concept.

---

## 4. SingleTask "clear top" — lifecycle of intervening activities

**Android behaviour.** The doc says plainly, of `singleTask` and
`FLAG_ACTIVITY_CLEAR_TOP`: "the system destroys all the other activities on
top of it." It does **not** specify the callback ordering. Community
references and the lifecycle codelab confirm the expected behaviour: each
intervening activity receives the **full teardown chain** `onPause → onStop
→ onDestroy`. No batch-destroy shortcut — the platform walks the stack
top-down and destroys each entry the normal way. Save-state is not
collected (same rule as explicit `finish()`).

**Our model.** Phase 9 hasn't implemented `SingleTask` yet — `launch`
returns `LaunchError::Unsupported`. When it does (Stone 9.2), the driver
needs to walk the task stack and call `destroy(id, ...)` on each entry above
the target. Our `destroy()` already fires the full chain, so the work is
mostly "loop + stack-fixup", not new state-machine logic.

**Verdict — NORMAL-PRIORITY for Stone 9.2.** The design already accommodates
it; we just need to implement. Test shape:
```
launch A; launch B; launch C;
relaunch A with SingleTask;
assert log == [pause_A, C: pause+stop+destroy, B: pause+stop+destroy,
               A: on_new_intent?, resume_A];
```
Where `on_new_intent` depends on Task 3's Phase 9.4 decision.

---

## 5. `allowTaskReparenting` and `clearTaskOnLaunch`

**Android behaviour.** Both are **still actively documented in 2026, not
deprecated**.
- `android:allowTaskReparenting="true"` — an activity can migrate from the
  task it launched in to a task with matching affinity when that other task
  comes foreground.
- `android:clearTaskOnLaunch="true"` — on the root activity of a task, the
  whole task is cleared back to that root whenever the user leaves and
  returns.

Both are `<activity>` manifest attributes. They're advanced; most apps
don't set them. `clearTaskOnLaunch` carries a footnote that it's ignored
unless `FLAG_ACTIVITY_RESET_TASK_IF_NEEDED` is set.

**Our model.** Single foreground task in Phase 8. Multi-task is deferred to
"Phase 11+" per `docs/v0.2-design.md`. These attributes are meaningless
without multi-task.

**Verdict — SKIP for v0.2.** Record in `docs/manifest.md` as "❌ Phase 11+"
so the attribute namespace is reserved. Don't implement. The use cases are
niche even on Android.

---

## Summary table

| Android feature                      | Our status          | Verdict               | Blocks which stone? |
| ------------------------------------ | ------------------- | --------------------- | ------------------- |
| `finish()` inside `onCreate()`       | Not handled         | WANT (soon)           | 9.3                 |
| `onRestart` distinct callback        | Merged into onStart | SKIP (deliberate)     | —                   |
| `onNewIntent` on SingleTop reuse     | Silent swallow      | CRITICAL for Intents  | 9.4                 |
| SingleTask "clear top" teardown      | Not implemented     | NORMAL priority       | 9.2                 |
| allowTaskReparenting / clearTask…    | Not implemented     | SKIP (Phase 11+)      | —                   |

## Actions landed from this task

1. `docs/activity-model.md` — add pointer to this doc, and a row in the §2
   Implementation table marking `on_new_intent` + `Created→Destroyed` as
   tracked-not-implemented.
2. `ROADMAP` — reserve a bullet inside Stone 9.4 for `on_new_intent`, and a
   bullet inside Stone 9.3 for the create→destroy shortcut.
3. `docs/manifest.md` — mark `allowTaskReparenting` / `clearTaskOnLaunch` as
   ❌ Phase 11+ so anyone skimming doesn't assume they work.

These are doc/roadmap moves only; no code changes required for this task.
