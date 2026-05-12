# Native UX Primitives — Stone 2c: `ScrollView` + `LazyColumn` — Implementation Plan

> **Status:** plan only. Implementation has not started.
> **Track:** Native UX primitives (informal "Stage 2" track, post-MVP widget polish — sits in the same arc as the rounded-clip primitive and multi-line text already shipped).
> **Pre-reqs (closed):**
> - `3a6cf27` test-infra: visual regression baselines for the calculator (Stage 1)
> - `6dc603b` sdk: rounded-clip primitive — `Canvas::push_round_clip` + `Frame::clip_radius` (Stage 2a)
> - `b35a15f` sdk: multi-line text — wrap, `max_lines` + ellipsis, `Frame::size` lock (Stage 2b)
> - `72c6379` test-infra: articles e2e + visual + calc visual stable-layout gate (Stage 2b closeout)
> **Follow-ups:** Stage 2d (gestures: tap-vs-drag arbitration, swipe-to-dismiss), Stage 2e (spring animations), Stage 3 (Instagram-like feed combining all primitives).
> **ROADMAP entry:** `🟡 Image (✅) / Input (❌) / ScrollView (❌) widgets.` (deferred-to-v0.3+ widgets list).

This document is written so a fresh Claude Code session — with no
memory of the prior conversation — can pick it up and execute. It
states the architectural decisions, the sub-stones, the file-by-file
changes, the wire format additions, the test strategy, and the
user-verification checkpoints.

The author's bar for "professional / Android-grade": the primitive
must look and feel like `RecyclerView` + `OverScroller` — own the
scroll position, own the fling physics, own the viewport
virtualization. **No `overflow: scroll` on the canvas, no browser
scrollbars, no DOM mirror.**

---

## 0. State at the start of 2c

### 0.1 What's already in place

**Per-Activity surfaces (Phase 15.1–15.4):** each activity paints into
its own `OffscreenCanvas`; the compositor samples surfaces in z-order
with `SurfaceDrawOpts { opacity, translate, scale, blur, shadow, tint, clip }`.
Live in [`framework/src/compositor.rs`](../framework/src/compositor.rs)
and [`runtime/web/renderer/canvas2d.js`](../runtime/web/renderer/canvas2d.js).

**Immediate-mode UI builder (`sdk-rust`):** `Ui::new(state, ctx, canvas)`
builds a `Vec<Node>` per frame. `Node` variants today: `Text`, `Rect`,
`Image`, `Spacer`, `Frame`, `Grid`, `VStack`, `HStack`. Layout in
[`sdk-rust/src/ui/layout.rs`](../sdk-rust/src/ui/layout.rs) (`measure_node`
+ `place`); paint in [`sdk-rust/src/ui/paint.rs`](../sdk-rust/src/ui/paint.rs).

**Pointer pipeline (Phase 13.4):**
- DOM: `tab.js` attaches `pointerdown/move/up` to the canvas element,
  forwards `{ x, y, button, phase, tabId }` to the worker.
- Worker (`worker.js:861`): packs as 14-byte payload `[x:f32 | y:f32 |
  button:u8 | phase:u8 | display_id:u32]`, calls `postEvent(EVENT_POINTER, …)`.
- Kernel (`event_queue.rs:78`): decodes to `Event::Pointer { x, y,
  button, phase, display_id }`; routes to the activity owning that display.
- Activity: `Activity::on_pointer(&mut self, ctx, ev: PointerEvent)`.
  SDK's `Ui::feed_pointer(ev)` correlates clicks to widget rects for the
  next frame's `Ui::was_clicked`.

**Inspector + visual regression (Stage 1):** SDK emits
`PUBLISH_SEMANTICS = 0x000F` once per frame with a flat semantic tree
(see [`sdk-rust/src/ui/semantics.rs`](../sdk-rust/src/ui/semantics.rs));
runtime exposes `window.__akiva.activities()`,
`window.__akiva.findText(...)`, etc. Playwright e2e suite
([`tests/e2e/`](../tests/e2e/)) covers calculator + clip-shapes + articles
with semantic + pixel-level geometric checks; visual goldens in
[`tests/e2e/visual/`](../tests/e2e/visual/) gated by `waitForStableLayout`
([`tests/e2e/helpers/akiva.ts`](../tests/e2e/helpers/akiva.ts)).

**Frame size lock (Stage 2b):** `Node::Frame { size: Option<(f32, f32)> }`
fixes a Frame's measured size regardless of children's intrinsic. Used by
`apps/articles` to keep a card 360 dp wide even when the body Text would
intrinsically be ~1500 dp. Pattern reused below for `ScrollView`.

### 0.2 Files to know

```
sdk-rust/src/ui/node.rs                — Node enum, builder structs
sdk-rust/src/ui/layout.rs              — measure_node, place
sdk-rust/src/ui/paint.rs               — paint_node, text_width callback
sdk-rust/src/ui/mod.rs                 — Ui builder, UiState, feed_pointer
sdk-rust/src/ui/semantics.rs           — semantic-tree projection + PUBLISH op
sdk-rust/src/activity.rs               — Activity trait, Context, PointerEvent
sdk-rust/src/runtime.rs                — Activity dispatcher

framework/src/activity.rs              — runtime-side Activity, on_pointer dispatch
framework/src/compositor.rs            — PaintOp inventory, dispatch_paint_op

kernel/src/event_queue.rs              — Event decode + routing
kernel/src/lib.rs                      — event → activity dispatch
kernel/src/host.rs                     — HAL extern declarations

runtime/web/tab.js                     — DOM listeners → worker messages
runtime/web/worker.js                  — forwardEvent → postEvent → kernel
runtime/web/inspector.js               — semantic-tree consumer, window.__akiva

tests/e2e/helpers/akiva.ts             — launch helpers, waitForStableLayout
tests/e2e/                             — Playwright e2e suite
tests/e2e/visual/                      — visual regression
```

### 0.3 Test counts to keep green

- **Rust:** sdk-rust 270 lib tests; framework 294; kernel/wire suites.
- **JS (vitest):** ~194 cases across 4 files.
- **Playwright e2e:** 34 tests across calculator, clip-shapes, articles
  (+ 3 visual goldens). All must stay green at every sub-stone.

---

## 1. Architectural decisions

### 1.1 Two primitives, not one: `Scroll` *and* `LazyColumn`

`Scroll` and `LazyColumn` are deliberately separate. They look similar
to a beginner — both produce a clipped, draggable viewport — but they
solve different problems:

| | `Node::Scroll` | `Node::LazyColumn` |
|---|---|---|
| Children | one (any subtree) | item-builder closure invoked by index |
| Content height | measured from the single child | `count * (item_h + gap) - gap + padding` |
| Memory | proportional to the child's full tree | bounded to viewport + overscan |
| Use case | a long card; a settings page; a tall image | a feed of 100s of items |
| Android analogue | `ScrollView` (or modern `Modifier.verticalScroll`) | `RecyclerView` / Compose's `LazyColumn` |

Both share the **same scroll-state cell** keyed by a stable identifier
the caller passes (`ScrollKey`). Both share the same fling/drag/wheel
input path. Differentiation lives only in measure/paint: `LazyColumn`
asks the caller's closure for items in the visible range; `Scroll`
just renders its single child with a translate.

### 1.2 Akiva owns the scroll, not the browser

The DOM canvas never gets `overflow: scroll`. Wheel events on the
canvas are captured with `preventDefault()` and forwarded through the
existing HAL bridge. Scroll position lives in `UiState` (SDK-side, per
scroll key); fling lives in the SDK's render-loop tick.

**Why:** every gesture composition (pull-to-refresh, swipe-to-dismiss,
nested scroll, fling-then-grab) requires us to own the position. Once
the browser owns it, every later gesture in Stage 2d gets uglier.
Also: the only thing the browser scroll engine gives us "for free" is
the scrollbar — and the scrollbar belongs to the OS chrome, not to the
content, so we'd be painting it ourselves anyway.

### 1.3 Fling physics: `OverScroller`-style exponential decay

Android's `OverScroller` integrates a friction-based decel:
`v(t) = v₀ · exp(-t / τ)`, with `τ ≈ 0.35 s` for a "natural" feel.
Pragmatic discrete form, evaluated each frame:

```
const FLING_DECAY_PER_FRAME: f32 = 0.95;          // 60fps; ≈ 0.05 ratio after 0.5s
const FLING_STOP_VELOCITY: f32 = 0.5;             // px/frame; below → snap to rest

velocity *= FLING_DECAY_PER_FRAME;
scroll_y += velocity * (dt_ms / 16.667);          // dt-normalized so 30fps frames don't drift
if velocity.abs() < FLING_STOP_VELOCITY { velocity = 0.0; }
```

The dt-normalization keeps fling distance invariant to frame rate — a
slow tab still flings the same distance, just over more frames.

**Bounds clamp:** `scroll_y` is clamped to `[0, content_h - viewport_h]`.
If a fling would carry past either edge, the velocity is zeroed at the
clamp (no rubber-band overscroll in 2c — that's a Stage 2e nice-to-have).

### 1.4 Hit-testing & event dispatch

The Activity's `Ui` keeps the last frame's **placed rects per scroll
key** so a wheel/pointer event arriving in frame `N+1` can be routed
to the topmost `Scroll`/`LazyColumn` whose viewport rect contains
`(x, y)`. The dispatcher walks scroll regions in paint order; the last
matching key wins (innermost wins for nesting).

For pointer-drag-as-scroll: the SDK only counts a pointer-down as a
drag start once movement crosses a `DRAG_SLOP` threshold (~6 dp, matches
Android's `ViewConfiguration.getScaledTouchSlop()`). Below slop, the
event still propagates as a tap, so `was_clicked` keeps working on
buttons that happen to sit inside a scrollable region.

### 1.5 Inspector contract for tests

`PUBLISH_SEMANTICS` gains a new node kind `scroll`:

```jsonc
{
  "kind": "scroll",
  "key": "feed",                   // caller-supplied stable id
  "scroll_y": 312.5,
  "content_height": 8400.0,
  "viewport_height": 712.0,
  "first_visible_index": 8,        // LazyColumn only; null for plain Scroll
  "last_visible_index": 17,
  "item_count": 80,                // LazyColumn only
  "rect": { "x": 0, "y": 88, "w": 412, "h": 712 },
}
```

`window.__akiva.scrollOf(key)` returns this object (null if no scroll
region by that key has published this frame). Tests use it to assert
"after wheel(0, 400), scroll_y is in [380, 420]", "fling velocity decays
monotonically over N frames", "first_visible_index advances by ≥ 5 after
a 500 dp scroll".

---

## 2. Sub-stones

Each sub-stone is one commit. The order is chosen so every intermediate
state has tests green and the demo app at least partially exercises the
new piece. Estimated ~6–8 commits total.

### 2c.1 — Wire `EVENT_WHEEL` end-to-end

Plumb wheel events from the canvas to the activity's `on_wheel`.
**No SDK widgets yet** — the activity just receives the event.

**Files**
- `runtime/web/tab.js`: add `canvas.addEventListener("wheel", …)`,
  `preventDefault()`, post `{ kind: "wheel", dx, dy, tabId }`.
- `runtime/web/worker.js`: extend `forwardEvent` switch with
  `case "wheel":`. New payload: 12 bytes `[dx:f32 | dy:f32 |
  display_id:u32]`. Define `EVENT_WHEEL = 6` (alongside the existing
  `EVENT_POINTER = 1`, …, `EVENT_TEXT = 5`).
- `kernel/src/event_queue.rs`: add `Event::Wheel { dx, dy, display_id }`
  variant + `EVENT_WHEEL` decode arm.
- `kernel/src/lib.rs`: dispatch `Event::Wheel` to the activity that owns
  `display_id` (mirror the `Event::Pointer` path).
- `framework/src/activity.rs` + `sdk-rust/src/activity.rs`:
  add `fn on_wheel(&mut self, ctx, ev: WheelEvent)` with a no-op default.
  `WheelEvent { dx: f32, dy: f32 }` — pixel-space delta, positive `dy`
  = scroll down (matches DOM convention).
- `sdk-rust/src/runtime.rs`: wire the `on_wheel` dispatch case.

**Tests**
- `kernel/src/event_queue.rs` unit: round-trip 12-byte encode/decode.
- vitest: `worker.js` `forwardEvent({kind:"wheel"})` posts correct bytes.
- Playwright sanity: a manual `await page.mouse.wheel(0, 100)` over the
  calculator does **not** cause the test page itself to scroll
  (confirms `preventDefault`).

**DoD:** new `on_wheel` callback fires in a calculator console log after
mouse wheel over the canvas; existing tests stay green.

### 2c.2 — `Node::Scroll` (single-child) + scroll-state cell + clip

Introduce the primitive without any interactive scrolling. Scroll
position is settable programmatically via `Ui::scroll_to(key, y)`.

**Files**
- `sdk-rust/src/ui/node.rs`: add
  ```rust
  Node::Scroll {
      child: NodeId,
      key: ScrollKey,           // u64 hash of caller string
      viewport_w: Option<f32>,  // None = inherit from slot
      viewport_h: Option<f32>,
  }
  ```
  + `ScrollBuilder { key, viewport_w, viewport_h }` with
  `ui.scroll(child).key("name").size(w, h).build()`.
- `sdk-rust/src/ui/mod.rs` (`UiState`):
  ```rust
  pub struct ScrollCell {
      pub scroll_y: f32,
      pub velocity: f32,
      pub content_h: f32,        // last measured; needed for clamp
      pub viewport_h: f32,       // ditto
      pub last_rect: Rect,       // for hit-test in next frame
  }
  scroll_states: HashMap<ScrollKey, ScrollCell>,
  ```
  + `Ui::scroll_to(key, y)` setter, `Ui::scroll_offset(key) -> f32` getter.
- `sdk-rust/src/ui/layout.rs`: `measure_node` for `Scroll` returns
  the viewport size (locked, like `Frame::size`); separately
  measures the child against `(viewport_w, f32::MAX)` to compute
  `content_h`. Stores `content_h` and `last_rect` back to `UiState`.
- `sdk-rust/src/ui/paint.rs`: `paint_node` for `Scroll` emits
  `PushClip(viewport)` → `PushTransform(0, -scroll_y, 1.0)` → child
  paint → `PopTransform` → `PopClip`. Child is painted in its own
  unclipped coord space; transform shifts it.
- `sdk-rust/src/ui/semantics.rs`: emit `"scroll"` kind node with
  `key`, `scroll_y`, `content_height`, `viewport_height`, `rect`.

**Tests (unit, sdk-rust)**
- `scroll_measure_locks_viewport`: child taller than viewport → outer
  measure returns viewport size.
- `scroll_paint_emits_clip_and_transform`: paint trace matches
  `PushClip · PushTransform · …child… · PopTransform · PopClip`.
- `scroll_to_clamps_to_content`: `scroll_to(key, 1e9)` clamps to
  `content_h - viewport_h`; `scroll_to(key, -1.0)` clamps to 0.
- `scroll_state_persists_across_frames`: scroll_y survives a rebuild
  with the same key; new key starts at 0.

**Playwright**
- A throw-away test: build a `Scroll` in the clip-shapes app with a tall
  Rect inside; programmatic `scroll_to(key, 200)` shifts the Rect's
  inspector rect by 200 px upward.

**DoD:** `Scroll` is testable end-to-end without user input.

### 2c.3 — Hit-test + wheel + pointer-drag scroll

Wire input to the scroll cell. After this commit, mouse-wheel over a
scroll region scrolls it; click-and-drag also scrolls.

**Files**
- `sdk-rust/src/ui/mod.rs`:
  - `Ui::feed_wheel(WheelEvent)`: find the scroll region whose
    `last_rect` contains the last pointer position (or the wheel's
    own coords if a coordless wheel arrives); update `scroll_y += dy`,
    clamp.
  - Extend `Ui::feed_pointer(PointerEvent)`:
    - On `Down` inside a scroll region, capture initial `(x, y)` and
      seed a candidate drag.
    - On `Move`, if moved past `DRAG_SLOP` (6.0 dp), switch to
      "dragging" mode; subsequent moves apply `delta_y` to `scroll_y`.
      Track velocity (EMA over last ~5 frames).
    - On `Up`, if dragging, hand velocity to the fling state (2c.4).
      If never crossed slop, propagate as a normal click.
    - On `Cancel`, drop drag state.
- `sdk-rust/src/runtime.rs`: dispatcher calls `Ui::feed_wheel(ev)` from
  the wheel handler the same way `Ui::feed_pointer` is plumbed.

**Tests (unit)**
- `feed_wheel_in_region_updates_scroll`: wheel dy=100 inside last_rect
  → scroll_y +=100; outside → unchanged.
- `feed_wheel_clamps_at_top` / `…_at_bottom`.
- `pointer_below_slop_is_click_not_drag`: down + move 3 px + up →
  `was_clicked` fires; scroll_y unchanged.
- `pointer_above_slop_is_drag`: down + move 20 px + up → scroll_y
  advanced ≈ 20 px; `was_clicked` does *not* fire.
- `nested_scroll_innermost_wins`: outer scroll wraps inner scroll
  → event inside inner only updates inner's scroll_y.

**Playwright**
- Wheel over `Scroll` advances inspector rects; wheel past
  `content_h - viewport_h` clamps.
- `mouse.down → move(50px) → up` over the scroll region advances
  scroll_y by ≈50 px and does *not* fire a button click that happens
  to sit inside.

**DoD:** scroll feels responsive to wheel and drag; no fling yet.

### 2c.4 — Fling animation (`OverScroller` decay)

Drag-release with non-zero velocity continues the scroll under
exponential decay.

**Files**
- `sdk-rust/src/ui/mod.rs`:
  - On pointer-up after a drag, seed `cell.velocity` from the EMA.
  - `Ui::tick_animations(dt_ms)` runs each frame: for every scroll
    cell with `|velocity| > 0`, advance `scroll_y` and decay velocity
    per §1.3.
  - Mark "frame dirty" for the activity while any fling is active so
    the framework keeps re-rendering even without input.
- `framework/src/lib.rs`: in `finalize_frame`, query the SDK for any
  animations-in-flight and request another rAF. (Probably an existing
  hook; otherwise piggy-back on the same mechanism that drives the
  Phase 15.3 transitions.)

**Tests (unit)**
- `fling_decays_monotonically`: seed velocity=10, tick 60×; sequence
  of scroll_y deltas strictly decreasing.
- `fling_stops_below_threshold`: after enough ticks, velocity == 0
  and scroll_y stable.
- `fling_clamps_at_edge_and_zeros_velocity`: fling past bottom → at
  clamp, velocity = 0, scroll_y = content_h - viewport_h.
- `dt_normalization`: ticking 30× at 33ms ≈ ticking 60× at 16.67ms in
  total scroll distance (within ~5% tolerance).

**Playwright**
- Drag down 200 px in 50 ms, release. Sample scroll_y at +100 ms, +300 ms,
  +600 ms: strictly increasing, then plateauing. After 1000 ms the value
  is stable.

**DoD:** scroll-release feels like a real OS, not a teleport.

### 2c.5 — `Node::LazyColumn` (uniform item-height virtualizer)

Adapter pattern: caller supplies `count` + `item_h` + builder closure.
SDK invokes the closure only for indices in `[first_visible - overscan,
last_visible + overscan]`. Same scroll-state cell as `Scroll`.

**Files**
- `sdk-rust/src/ui/node.rs`:
  ```rust
  Node::LazyColumn {
      key: ScrollKey,
      count: u32,
      item_h: f32,
      gap: f32,
      padding: (f32, f32, f32, f32),
      viewport_w: Option<f32>,
      viewport_h: Option<f32>,
      realized: Vec<(u32, NodeId)>,   // (index, node) for items the builder produced
  }
  ```
  Builder API:
  ```rust
  ui.lazy_column(count)
      .key("feed")
      .item_h(96.0).gap(8.0).padding(...)
      .size(w, h)
      .items(|ui, i| /* returns NodeId */)
      .build();
  ```
  Inside `.items()`, the builder iterates only the visible range
  (queried via `Ui::scroll_offset(key)` + `viewport_h`).
- `sdk-rust/src/ui/layout.rs`:
  - `measure_node` for `LazyColumn`: content_h is
    `count * item_h + (count - 1) * gap + padding_v`. Viewport is
    `Some(viewport_h)` or slot height.
  - `place` for `LazyColumn`: each realized `(i, node)` is placed at
    `y = padding_top + i * (item_h + gap) - scroll_y`, width =
    inner_w, height = item_h.
- `sdk-rust/src/ui/paint.rs`: clip + translate are *not* needed at the
  Node level because `place` already accounts for `scroll_y`; just clip
  the viewport rect (`PushClip(viewport)` → paint realized children →
  `PopClip`). (Alternative: keep the same clip+transform shape as
  `Scroll` for consistency. Pick whichever yields the cleaner
  paint-op trace in inspection.)
- `sdk-rust/src/ui/semantics.rs`: same `"scroll"` kind plus
  `first_visible_index`, `last_visible_index`, `item_count`,
  `realized_count`.

**Caveat: builder closure semantics.** The immediate-mode `Ui` API is
synchronous — the caller's `.items(|ui, i| …)` closure is invoked
during `on_render`. The SDK must know `scroll_y` *before* calling the
closure so it iterates the right range. That's fine because
`scroll_y` is cached in `UiState` from the previous frame.

**First-frame edge case.** On the very first frame the scroll cell has
`viewport_h = 0` (never measured). Approach: build *all* items up to a
configurable cap (default 30) on first frame; subsequent frames use
the cached `viewport_h` for proper virtualization. This is what
Compose does too — first-frame measure is the slow path.

**Tests (unit)**
- `lazycolumn_content_h_is_count_times_item_h`: count=100, item_h=80,
  gap=8 → content_h = 100*80 + 99*8.
- `lazycolumn_first_visible_index_at_zero_scroll`: 0.
- `lazycolumn_visible_range_after_scroll`: scroll_y=400, item_h=80
  → first_visible = 5.
- `lazycolumn_realizes_only_visible_plus_overscan`: builder closure
  invocation-count check; should be ≤ visible + 2 * overscan.
- `lazycolumn_index_out_of_range_is_skipped`: count=10, scroll_y past
  last → realized list ends at index 9.

**Playwright**
- Build a `feed` LazyColumn with count=80, item_h=96, items each a
  rect with text "row {i}". Inspector reports item_count=80; first
  frame's `realized_count` ≤ ~10 + overscan.
- After `wheel(0, 800)`, `first_visible_index` advances by ≈ 8;
  `findText("row 9")` now returns a rect; `findText("row 0")` does
  not (off-screen).

**DoD:** memory scales with viewport, not item count.

### 2c.6 — Inspector polish: `scrollOf(key)`, smoke through e2e helper

Helper API + waitForStable extension.

**Files**
- `runtime/web/inspector.js`: index scroll nodes by key in the per-frame
  digest; expose `window.__akiva.scrollOf(key)` returning the JSON shape
  in §1.5.
- `tests/e2e/helpers/akiva.ts`:
  - `scrollOf(page, key)` wrapper.
  - `waitForStableLayout` already gates on identity transform; add an
    option `until: { scrollKey, predicate }` so tests can wait for
    "fling decayed to rest" without sleeping arbitrary times.

**Tests**
- vitest: `inspector` digest with a fake scroll PUBLISH carries the
  fields through to `scrollOf("feed")`.

### 2c.7 — `feed` demo app + e2e suite + visual goldens

The first real-world use of the primitives.

**Files**
- `apps/feed/` — new SDK demo. Cargo crate, manifest, `lib.rs`:
  - 80 mock items (post header + body text + meta line + thin
    divider). Item height 112 dp.
  - `LazyColumn` keyed `"feed"`, item builder takes `i` and produces a
    `Frame { size(w, 112) padding(16) v_stack(title, body, meta) }`.
  - Static header card above the LazyColumn ("Akiva Feed") to make sure
    LazyColumn composes inside a `VStack` parent without breaking
    measure.
- `runtime/web/dev-apps/feed/` — manifest, drawable, dev install.
- `runtime/web/app_registry.js` — list "feed" alongside articles &
  calculator (alphabetic order — `articles` < `calculator` < `feed`).
- `tests/e2e/helpers/akiva.ts` — `launchFeed(page)` (row 2).
- `tests/e2e/feed.spec.ts` — semantic + pixel-level geometric tests:
  1. activity launches; inspector shows scroll key="feed";
  2. `item_count == 80` published;
  3. `first_visible_index == 0` on launch; `realized_count` in [8, 14];
  4. `findText("row 0")` returns a rect inside the viewport;
  5. `findText("row 79")` is null pre-scroll;
  6. `mouse.wheel(0, 400)` advances `scroll_y` by ≈ 400 (±10);
  7. `first_visible_index` advances by ≥ 3;
  8. row 0 inspector rect is now above viewport top;
  9. `mouse.wheel(0, 10000)` clamps `scroll_y` to `content_h - viewport_h`;
  10. clamped state: `findText("row 79")` returns a rect inside viewport;
  11. drag (down 200 px, fast release) seeds non-zero velocity;
  12. velocity decays monotonically over next 6 frames;
  13. fling settles by 1000 ms (predicate wait, not sleep);
  14. pointer-down + move 3 px + up over an item is treated as a *tap*
      (semantic test: no scroll_y delta; click registers);
  15. wheel-up at scroll_y=0 clamps at 0 (no rubber-band);
  16. pixel sanity: after `scroll_y` = 600, the row at the expected
      y-band is non-backdrop (item visibly painted there).
- `tests/e2e/visual/feed.visual.spec.ts` — two goldens:
  - `feed-idle.png`: initial frame.
  - `feed-scrolled.png`: after `Ui.scroll_to("feed", 600)` (programmatic
    so the golden is deterministic — no drag-physics drift).

**DoD:** `feed` is the most polished demo in the repo and the e2e suite
proves every primitive of Stage 2c.

---

## 3. Test strategy summary

Three layers, like every earlier stage:

1. **Semantic (`tests/e2e/feed.spec.ts` + others):** the inspector
   reports what the SDK *believes* it built. Tests assert
   `item_count`, `first_visible_index`, `scroll_y`, `findText(...)`.
   Catches "drops a scroll node from PUBLISH_SEMANTICS" regressions
   even when pixels still look fine.
2. **Pixel-level geometric (same files):** sample the canvas at
   expected x/y, count glyph pixels in horizontal bands, scan whether
   a row is painted in a viewport band. Catches "paint doesn't apply
   scroll translate" / "lazy_column realizes off-screen items
   visibly" regressions.
3. **Visual regression (`tests/e2e/visual/feed.visual.spec.ts`):**
   `waitForStableLayout` gates two pixel-perfect goldens. Catches
   regressions the prior two layers miss (subpixel paint changes,
   accidental color shifts).

Unit tests (sdk-rust lib) cover every measure/place/paint case so we
catch issues *before* paying the cost of an e2e run.

---

## 4. User-verification checkpoints

After each sub-stone the user should be able to verify it locally:

- **2c.1:** `cargo run -p akiva -- run`; open devtools, scroll the
  wheel over the canvas — a console log from `on_wheel` fires.
- **2c.2:** new dev knob in the clip-shapes app shifts an inner Rect by
  programmatic `scroll_to` (or run a Playwright test).
- **2c.3:** scroll the wheel over the new `feed` placeholder → content
  shifts. Drag and release → content shifts but does *not* continue.
- **2c.4:** drag and release → content continues with decaying glide.
- **2c.5:** open devtools memory profile → `feed` with 80 items
  retains a bounded count of realized nodes regardless of scroll
  position.
- **2c.6:** in console, `__akiva.scrollOf("feed").first_visible_index`
  returns a sane number.
- **2c.7:** `npm run test:e2e -- feed` is green; goldens match.

---

## 5. Risks

| # | Risk | Mitigation |
|---|---|---|
| 1 | LazyColumn first-frame "cold start" lag — builder closure runs for *all* items before viewport is known. | Cap first-frame realized count at 30 (configurable). Once `viewport_h` is cached, second frame uses real range. |
| 2 | Wheel events bypass `preventDefault` if registered passively, causing the page itself to scroll. | `addEventListener("wheel", h, { passive: false })`. Sanity covered in 2c.1 Playwright test. |
| 3 | Drag-slop too small → accidental scrolls on intended taps; too large → drag feels laggy. | Start at 6.0 dp (Android's median). Make it tweakable via a SDK const, not per-call. |
| 4 | Fling velocity carried from drag is noisy on slow trackpads. | EMA over last 5 sample frames (Android uses a windowed velocity tracker; close enough). |
| 5 | Nested scroll (LazyColumn inside Scroll) routes events to the wrong region. | Innermost-wins dispatch (§1.4). Explicit unit test `nested_scroll_innermost_wins`. |
| 6 | Inspector publishing balloons when LazyColumn has 1000s of items. | Publish item indices only for *realized* items (not all). `item_count` is a single number; the heavy field is per-item `text`. |
| 7 | Programmatic `scroll_to` mid-fling fights with active velocity. | `scroll_to` zeroes velocity. Documented in `Ui::scroll_to` rustdoc. |
| 8 | Variable item heights (non-uniform) — needed for any real feed. | **Explicitly out of scope for 2c.** Tracked as a follow-up; the uniform path is the load-bearing 80% and unblocks Stage 2d. |

---

## 6. Out of scope (deferred to 2c-follow / Stage 2d / 2e)

- **Horizontal scroll.** Vertical-only in 2c. Horizontal is an easy
  pivot once the vertical path lands; deferred to keep test surface
  small.
- **Variable item heights in LazyColumn.** Compose's
  variable-height path uses a "measured items cache" — well-trodden
  but not necessary for the demo. Plan: 2c-follow.
- **Pull-to-refresh / overscroll bounce.** Stage 2e (spring
  animations) is the better home for these.
- **Snap-to-item (carousel-style).** Stage 2d (gestures).
- **Scrollbar rendering.** None in 2c. The OS-chrome question is
  unsolved at this stage of Akiva.
- **Keyboard scrolling** (page up/down, arrow keys). When IME /
  keyboard lands (post-MVP roadmap item).
- **Accessibility scroll announcements.** Tied to the screen-reader
  bridge already noted in the roadmap.

---

## 7. Estimated work and commit shape

- **6–8 commits**, one per sub-stone, plus a docs commit at the end:
  - `runtime+kernel+sdk: EVENT_WHEEL end-to-end + Activity::on_wheel`
  - `sdk: Node::Scroll — single-child scroll + viewport clip + scroll_to`
  - `sdk: feed_wheel + pointer-drag with slop → Node::Scroll`
  - `sdk: fling animation — OverScroller-style exponential decay`
  - `sdk: Node::LazyColumn — uniform-height virtualizer + item builder`
  - `runtime+test-infra: inspector.scrollOf + waitForStableLayout 'until' hook`
  - `demo: feed app — 80-row LazyColumn`
  - `test-infra: feed e2e suite + visual goldens`
  - `docs: CHANGELOG entry + ROADMAP tick for scroll widgets`

- **Expected new tests:** ~25 sdk-rust unit, ~16 Playwright e2e, 2 visual
  goldens. Brings totals to ~295 sdk-rust lib, ~50 e2e, 5 goldens.

---

## 8. Open questions to revisit during implementation

These are *not* blockers — defaults are listed — but worth a
deliberate decision once the code is in front of us:

1. **`ScrollKey`** — string-hash to u64, or interned `&'static str`?
   Default: hash. Same approach as widget keys for `was_clicked`.
2. **Fling decay** — 0.95 per frame (≈ 0.5 s settle) or 0.92 (≈ 0.3 s)?
   Default: 0.95; user-tunable later if the demo feels sluggish.
3. **Overscan** — 2 items or `ceil(viewport_h / item_h * 0.5)`?
   Default: 2; bump to dynamic if 60fps scroll shows item-pop-in.
4. **Where does the wheel/touch listener live for follower tabs?** —
   Phase 13.4 already routes per-display events; 2c just rides that.
   But worth a sanity check.
