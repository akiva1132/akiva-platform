# Stage 2d — horizontal scroll, LazyRow, snap, carousel demo

> **Status:** Done (2026-05-13). All seven sub-stones landed; carousel
> demo + 12 e2e + visual golden green. Builds on Stage 2c
> (scroll-lazycolumn-plan.md).
> **Owner:** Claude × akiva1132. **Target:** 5-7 commits, ~25 new tests.
> **Why a follow-up:** Stage 2c shipped vertical-only by deliberate
> design (smaller test surface, one moving piece at a time). Stage 2d
> generalises the scroll cell along an axis and lands the carousel
> primitive Android calls `HorizontalPager` / iOS calls `UIPageView`.
> A page-snap carousel is the single most "look-and-feel" widget that
> divides a real app from a tech demo — every modern OS UI uses it
> (lock-screen widgets, app-drawer pages, photo-galleries, story
> rings).
> **Follow-ups (out of scope here):** swipe-to-dismiss (different
> gesture lifecycle, separate stage), pull-to-refresh / overscroll
> bounce (Stage 2e with springs), nested-scroll arbitration (touch
> nesting / nested-scroll chains — its own large stage), accessibility
> (whole bridge).

---

## 1. State at start (the receipts)

Behaviour already in place from Stage 2c:

- `Node::Scroll { child, key, viewport_w, viewport_h }` — vertical-only
  single-child scroll. Content measured against `(slot_w, f32::MAX)`;
  painted with `-scroll_y` y-offset inside a viewport clip.
- `Node::LazyColumn { key, count, item_h, gap, padding, viewport_*,
  realized }` — uniform-height virtualizer. Realises `[first-overscan,
  last+overscan]` window each frame.
- `UiState::scroll_states: HashMap<String, ScrollCell>` —
  `ScrollCell { scroll_y, velocity, content_h, viewport_w,
  viewport_h, last_rect }`. Persists across rebuilds.
- `feed_pointer` — slop-thresholded drag (DRAG_SLOP=6 dp). Vertical
  delta → `scroll_y` delta. EMA velocity for fling.
- `feed_wheel` — hit-tests against last_pointer. `ev.dy` →
  `scroll_y` delta. Kills any in-flight fling.
- `tick_animations(dt_ms)` — exponential-decay fling
  (`DECAY^frames_dt`, dt-normalised against 60fps).
  `FLING_DECAY_PER_FRAME=0.92`, `FLING_STOP_VELOCITY=0.5 px/frame`,
  4-frame clamp guards against tab-resume teleport.
- Wire format **v2** publishes `ScrollSemantic { scroll_y, content_h,
  viewport_h, viewport_w, first_visible, last_visible, item_count }`.
- `window.__akiva.scrollOf(key)` exposes the above.
- `apps/feed/` — 80-row vertical feed; 16 e2e tests + 1 visual golden.

Test totals at start of 2d:
- sdk-rust lib: **321**
- framework lib: **294**
- wire crate: **9**
- kernel: **9**
- vitest (web runtime): **240**
- Playwright e2e: **51**
- visual goldens: **5**

---

## 2. Goal — what "done" looks like

After Stage 2d:

1. **`Node::Scroll`** accepts a `ScrollAxis` (Vertical default,
   Horizontal new). Content measures against the appropriate
   unlimited-axis dimension; paints with offset along main axis.
2. **`Node::LazyRow`** exists (twin of LazyColumn, horizontal main
   axis, `item_w` instead of `item_h`). Realises by x-band rather
   than y-band.
3. **Horizontal-aware input**:
   - `feed_wheel`: routes `dx` to horizontal cells, `dy` to vertical
     ones. (Trackpad two-finger swipe naturally produces both axes; the
     scroll cell consumes the axis matching its orientation.)
   - `feed_pointer`: drag delta is the main-axis projection; slop is
     measured along the main axis only (so a near-horizontal drag
     inside a vertical scroll stays a tap, not an axis steal).
4. **Snap modes**:
   - `SnapMode::None` (default — feed behaves as today).
   - `SnapMode::Item` — when fling settles, animate to nearest item
     edge over ~250 ms with cubic-out easing.
   - `SnapMode::Page` — page size = viewport along main axis. Snap
     to nearest page. Single-tap fling = page-flip (Compose
     `HorizontalPager` parity).
5. **`apps/carousel/`** — demo app with 12 colourful cards in a
   `LazyRow + SnapMode::Page`. Title + page indicator below. Drag
   left/right to flip cards; release seeds the snap.
6. **Inspector v3**: `axis` byte added to `ScrollSemantic`; everything
   else unchanged (forward-compat: v2 decoders ignore the byte).
7. **Tests**:
   - sdk-rust unit: axis routing in feed_wheel + feed_pointer, snap
     target math, snap animation tick, page index from offset, edge
     cases (count=0, item_w=0). Target **~20 new**.
   - e2e (Playwright): carousel page flips, snap precision, drag-
     and-release seeds correct page, wheel-x advances, vertical
     wheel inside horizontal carousel does nothing. Target **~10 new**.
   - Visual golden for `carousel-idle.png`.

---

## 3. Architectural decisions (the load-bearing ones)

### 3.1 Axis as data, not generics

`ScrollAxis::{Vertical, Horizontal}` lives in `sdk-rust/src/ui/mod.rs`.
The scroll cell stores it; `feed_wheel`/`feed_pointer` branch on it.
Layout/paint pass it down. No generics over axis — the field count is
tiny (1 enum) and a generic would double instantiation for every
internal place. Compose does it the same way (Orientation enum).

### 3.2 Naming: `scroll_y` stays, semantically "main-axis offset"

Renaming `ScrollCell.scroll_y` → `offset`, `content_h` → `content_main`,
etc., touches ~80 sites across sdk-rust, framework, inspector, tests.
The diff is mechanically simple but voluminous, and the public wire
contract already says `scroll_y` / `scrollY`. **Pragmatic choice:**
keep field names; document them as "main-axis-projected" for
horizontal cells (vertical: scroll_y = y-offset, content_h = total
content height; horizontal: scroll_y = x-offset masquerading as
scroll_y, content_h = total content width). The inspector adds an
`axis: "vertical"|"horizontal"` field — callers that care can read
it; backward-compat callers don't have to.

**Cost of this choice:** the names lie for horizontal lists. We add a
docstring on `ScrollCell` calling this out explicitly, and the
internal axis-routing code reads `cell.scroll_y` regardless. Anyone
reading the code path encounters the docstring before the naming.

If this proves too confusing in practice, the rename is a follow-up
mechanical pass — `cargo fmt` + grep handles it cleanly.

### 3.3 LazyColumn vs LazyList — keep separate, share internal

`Node::LazyColumn` stays in place. We add a sibling `Node::LazyRow`
with `item_w` instead of `item_h`. **Why two nodes:** the field
naming is the user-facing API; `LazyRow { item_w }` reads as Compose
does, where `LazyList { item_main_size }` reads as Flutter's
`SliverFixedExtentList`. We pick Compose's vocabulary because it's
what the rest of the SDK already imitates.

The realisation logic (compute first/last from offset, stride, viewport)
is shared via a private helper `realise_window(axis, count, stride,
viewport_main, scroll, overscan)` returning `(start, end)`. Both
builders call it; both `place_*` arms call the same window math.

### 3.4 Snap as a second animation, not "fling with target"

Fling is exponential decay (already wired). Snap is a fixed-duration
ease toward a target. They don't compose well as a single animator
(snap needs to *override* fling near the end, not extend it). Model:

```
enum ScrollAnim {
  None,                                      // velocity == 0
  Fling { velocity: f32 },                   // existing path
  Snap  { from: f32, to: f32, t_ms: f32 },   // new
}
```

State transitions:
- Drag release → seed `Fling { velocity }`.
- Tick decays Fling.velocity; when `|velocity| < FLING_STOP_VELOCITY`,
  read `cell.snap_mode`:
  - `None` → transition to `ScrollAnim::None` (current behaviour).
  - `Item` / `Page` → compute `to = nearest_snap(scroll_y)`;
    transition to `Snap { from: scroll_y, to, t_ms: 0 }`.
- Tick advances `Snap.t_ms`; positions via cubic-out:
  `progress = 1 - (1 - clamp(t/250, 0, 1))^3`; `scroll_y = lerp(from, to,
  progress)`; at progress == 1, transition to `None`.
- User input (wheel, drag-start) cancels Snap (sets to None) — user
  is taking control back.
- `Ui::scroll_to(key, y)` zeroes any animation (matches 2c behaviour).

`ScrollCell.velocity: f32` becomes `ScrollCell.anim: ScrollAnim` (the
moral upgrade). For wire/inspector continuity, the published
`velocity` field is `if let Fling { v } = anim { v } else { 0 }` —
callers that watched velocity for "is fling active" still work.

### 3.5 Page snap: `page_size = viewport_main`, no other knobs

Compose `HorizontalPager` exposes `pageSize` (Fill, Fixed(N.dp),
custom). We start with Fill only (page = full viewport). It covers
the carousel demo and ~80% of real uses. Fixed-page and per-item-snap
align well as future builder methods.

### 3.6 Cross-axis: ignored, not stolen

When the user wheels diagonally inside a vertical scroll, the
horizontal `dx` is dropped on the floor. Same for a vertical wheel
inside a horizontal scroll. **No axis-stealing** (some browsers'
horizontal-trackpad fallback to vertical-wheel). Two reasons:
1. The wheel event already carries a clean `(dx, dy)`; we trust the
   browser to have done axis detection in its own pipeline.
2. Axis-stealing breaks nested-scroll (vertical feed of horizontal
   carousels would steal y-wheel into the carousel and refuse to
   scroll the parent feed). Better to be predictable.

For diagonal drags, the slop check uses the main-axis component
only — so a drag that's mostly cross-axis stays under the slop and
resolves as a tap. This matches Android `RecyclerView`'s axis-locking
behaviour.

---

## 4. Sub-stones (commits)

### 2d.1 — `ScrollAxis` enum + axis-aware `Node::Scroll` and layout

- `sdk-rust/src/ui/mod.rs`:
  - `pub enum ScrollAxis { Vertical, Horizontal }`, `Default` =
    Vertical.
  - `ScrollCell` gains `axis: ScrollAxis` field.
- `sdk-rust/src/ui/node.rs`:
  - `Node::Scroll` gains `axis: ScrollAxis`.
  - `ScrollBuilder::axis(axis)`. New `Ui::scroll_h(child)` factory =
    horizontal `ScrollBuilder`. Existing `Ui::scroll(child)` stays
    vertical.
- `sdk-rust/src/ui/layout.rs`:
  - `Node::Scroll` measure: if horizontal, child measured against
    `(f32::MAX, slot_h)`; child's intrinsic width = content_h
    (still the "content along main axis" — name lie noted in plan).
  - Place: child rect = `(rect.x - scroll_y, rect.y, content, rect.h)`
    for horizontal.
- `sdk-rust/src/ui/paint.rs`: clip unchanged (viewport rect already
  axis-agnostic).
- `sdk-rust/src/ui/mod.rs` — `feed_wheel`:
  - Look up cell's `axis`; pick `ev.dx` for horizontal, `ev.dy` for
    vertical.
- `sdk-rust/src/ui/mod.rs` — `feed_pointer`:
  - Replace `total_dy` with `total_main = |ev.[xy] - started.[xy]|`
    chosen by cell's axis.
  - Replace `delta_y = g.last_y - ev.y` with axis-projected delta.
  - `DragGesture.last_y` → `last_main`.
- **Tests:** axis enum default = vertical; horizontal scroll updates
  scroll_y from dx; vertical scroll ignores dx; pointer-drag-x on
  horizontal scroll moves it; pointer-drag-y inside horizontal
  scroll stays under slop. ~6 unit tests.

### 2d.2 — `Node::LazyRow` factory + shared realisation helper

- `sdk-rust/src/ui/node.rs`:
  - `pub enum Node::LazyRow { key, count, item_w, gap, padding,
    viewport_*, realized }`.
  - `LazyRowBuilder` mirrors `LazyColumnBuilder`. Same `.items(|ui,
    i| -> NodeId)` shape.
  - Private `realise_window(stride, viewport_main, scroll, overscan,
    count) -> (u32, u32)` helper. Both column + row builders call it.
  - `Ui::lazy_row(count)` factory.
- `sdk-rust/src/ui/layout.rs`: `LazyRow` arm — measure realised
  items against `(item_w, inner_h)`; place at
  `x = padding_left + idx * (item_w + gap) - scroll_y`,
  `y = padding_top`.
- `sdk-rust/src/ui/paint.rs`: LazyRow same as LazyColumn (push_clip,
  paint realised, pop_clip).
- `sdk-rust/src/ui/mod.rs`: `build_semantic_tree` emits
  `ScrollSemantic` for LazyRow (with `axis = Horizontal`).
- **Tests:** lazy_row realises bounded items, first_visible math,
  cold-start cap, viewport inheritance, scroll-y advances ordinal
  range. ~6 unit tests.

### 2d.3 — `SnapMode` + `ScrollAnim::Snap`

- `sdk-rust/src/ui/mod.rs`:
  - `pub enum SnapMode { None, Item, Page }`, `Default` = None.
  - `pub(crate) enum ScrollAnim { None, Fling { velocity }, Snap {
    from, to, t_ms } }`. Replaces `ScrollCell.velocity: f32`.
  - `ScrollCell.snap_mode: SnapMode`, `item_main_size: f32` (set by
    LazyColumn/LazyRow each present pass; needed for snap target
    math even after the builder closure has dropped).
  - `ScrollCell.velocity()` returns `match anim { Fling{v}=>v,
    _=>0 }` — preserves the existing public read pattern.
  - `tick_animations`: state machine per 3.4. Snap target picked
    from `snap_mode`:
    - `Item`: `to = round(scroll_y / stride) * stride` (stride =
      item_main_size + gap).
    - `Page`: `to = round(scroll_y / viewport_main) * viewport_main`.
  - Cubic-out lerp; 250 ms duration constant `SNAP_DURATION_MS`.
- `sdk-rust/src/ui/node.rs`:
  - `LazyColumnBuilder::snap(mode)`, `LazyRowBuilder::snap(mode)`.
  - Both threaded into the `ScrollCell.snap_mode` (set by `Ui::present`
    while refreshing the cell, alongside `viewport_main`/`content_main`).
- `framework/src/wire/op_kinds.rs`: no changes; snap is purely
  client-side state.
- **Tests:** Item snap rounds to nearest stride; Page snap rounds to
  nearest viewport; mid-fling+snap transition; user wheel mid-snap
  cancels it; programmatic scroll_to mid-snap cancels it;
  cubic-out monotonic; settle within tolerance after 250 ms ticks.
  ~8 unit tests.

### 2d.4 — Inspector v3: `axis` field on `ScrollSemantic`

- `sdk-rust/src/ui/semantics.rs`:
  - `WIRE_VERSION` 2 → 3.
  - `ScrollSemantic` gains `axis: ScrollAxis`. Encode 1 byte after
    the existing 28-byte payload (29 bytes total). 0 = Vertical,
    1 = Horizontal.
  - Round-trip tests + truncated-payload guard.
- `runtime/web/inspector.js`:
  - Bump WIRE_VERSION constant; accept v2 (default axis to
    "vertical") and v3.
  - `scrollOf(key)` adds `axis: "vertical" | "horizontal"`.
- `tests/e2e/helpers/akiva.ts`:
  - `ScrollSemantic` type adds `axis: "vertical" | "horizontal"`.
- `runtime/web/__tests__/inspector.test.mjs`:
  - `encodeNodeV3`, `encodeTreeV3` helpers. Round-trip with axis
    field. v2 fallback decodes (defaults axis to vertical). ~3 new
    vitest tests.

### 2d.5 — `apps/carousel/` demo app

- `apps/carousel/Cargo.toml`: `akiva-app-carousel`.
- `apps/carousel/manifest.toml`: `com.akiva.carousel`,
  `min_abi_version = "0.13.0"` (no ABI bump needed — Stage 2d is
  pure SDK-internal). One activity "CarouselMain".
- `apps/carousel/src/lib.rs`:
  - 12 mock cards, each `(title, subtitle, bg_color)`.
  - `LazyRow::key("carousel").item_w(viewport_w - 32.0).gap(16.0)
    .padding_lr(16.0).snap(SnapMode::Page)`.
  - Card = `Frame { Rect.color(bg).radius(20.0) + v_stack(title,
    subtitle) }`. Height inherits.
  - Below the carousel: a centered "x / 12" page indicator. Read
    from `ui.scroll_offset("carousel")` rounded by viewport_w.
  - `on_pointer`/`on_wheel` forward as in feed.
- `runtime/web/app_registry.js`: add `carousel` at the correct
  alphabetical slot.
- **No tests in this commit** — paired test commit follows.

### 2d.6 — e2e + visual goldens for the carousel

- `tests/e2e/helpers/akiva.ts`: `launchCarousel(page)` at the new
  alphabetical row.
- `tests/e2e/carousel.spec.ts`:
  - scroll region published with horizontal axis
  - item_count = 12, content along main axis ≈ 12 * (vw - 32) +
    11 * 16 + 32
  - first_visible starts at 0
  - drag left ~200 px → release → after snap, scroll_y ≈ viewport_w
    (one page).
  - drag right at page 0 stays at 0 (no rubber-band).
  - wheel-x advances scroll_y; wheel-y does not.
  - flick across (high velocity) → settles after snap.
  - "1 / 12" indicator on first paint; flips to "2 / 12" after
    one page snap.
  - Row 11 not findable at scroll=0; findable after scrolling past
    page 10. ~10 tests.
- `tests/e2e/visual/carousel.visual.spec.ts`:
  - `carousel-idle.png` golden. Re-run with `--update-snapshots` on
    first commit.

### 2d.7 — `docs:` CHANGELOG + ROADMAP entries

- `CHANGELOG.md`:
  - 0.14.0 entry under Unreleased / Stage 2d: ScrollAxis,
    Node::LazyRow, SnapMode { None, Item, Page }, Inspector wire v3
    (axis byte), carousel demo.
- `docs/stage-2d-carousel-plan.md` — mark Done in front-matter.
- `docs/ROADMAP.md` (if it exists): tick the relevant box.

---

## 5. Test strategy in three layers

1. **Unit (sdk-rust)** — pure logic, no canvas.
   - Snap target math: `Item` rounds to stride; `Page` rounds to vw.
   - Cubic-out monotonic 0→1.
   - Fling → Snap state transition fires at velocity threshold.
   - User input cancels Snap.
   - Axis routing in feed_wheel/feed_pointer.
   - LazyRow realisation window.

2. **Wire / inspector (vitest)** — encode/decode.
   - v3 round-trip with axis.
   - v2 decode defaults axis = vertical (compat path).
   - Truncated payload returns null.

3. **e2e (Playwright)** — full pipeline with the carousel app.
   - Page snaps land at integer * viewport_w within ±2 px.
   - Wheel-x advances; wheel-y inside the carousel is a no-op.
   - Visual golden for first paint.

---

## 6. Risks and mitigations

| # | Risk | Mitigation |
|---|------|------------|
| 1 | Snap "fights" with active fling at the edge (settle threshold flickers across velocity boundary). | Hysteresis: only enter Snap if `|velocity| < FLING_STOP_VELOCITY` for ≥ 1 frame. Tests cover the edge. |
| 2 | Cubic-out at 60fps + 250 ms = 15 frames; slow devices skip frames. | dt-normalised lerp (`progress = clamp(t_ms/250, 0, 1)` with cubic-out applied to progress, not to discrete frames). 4-frame dt clamp already in place from 2c. |
| 3 | Page snap with `page_size = viewport_main` rounds to 0 if `item_w + gap` > viewport_w (one-and-a-half-card layouts). | Document `SnapMode::Page` as "page = viewport"; one-and-a-half-card layouts should use `SnapMode::Item`. Hint in rustdoc. |
| 4 | Wire v2 decoders crash on v3 payload (29 bytes vs 28). | The flag-bit encoding already prefixes the byte count; v2 inspector reads exactly 28 and skips the trailing byte if more is present. Test the compat path. |
| 5 | Naming lie (`scroll_y` for horizontal). | Plan §3.2 calls it out. Follow-up rename pass tracked separately. |
| 6 | Carousel item closures allocate per realisation. | Same shape as LazyColumn — only realised items pay the cost. No new risk. |
| 7 | Snap target mid-drag (user drags after snap started). | `feed_pointer.Down` inside a snapping cell cancels Snap (set anim → None). |

---

## 7. Out of scope (Stage 2d-follow or later)

- **Swipe-to-dismiss** (different gesture lifecycle, animation kind).
- **Pull-to-refresh / overscroll bounce** (Stage 2e, springs).
- **Nested-scroll arbitration** (touch nesting / chained scroll).
- **Per-item custom snap offsets** (Compose `SnapPosition`).
- **Variable-width items in LazyRow** (same deferral as LazyColumn).
- **Keyboard / arrow-key navigation between pages.**
- **Accessibility (`scrolled to page X`)** — bridged together with
  the rest of A11y post-MVP.

---

## 8. Estimated work and commit shape

- **5-7 commits**, one per sub-stone, plus docs at the end:
  - `sdk: ScrollAxis + axis-aware Node::Scroll + feed_wheel/pointer`
  - `sdk: Node::LazyRow + shared realisation helper`
  - `sdk: SnapMode + ScrollAnim::Snap + cubic-out tick`
  - `runtime+wire: inspector v3 — axis byte on ScrollSemantic`
  - `demo: carousel app — 12 cards, page-snap`
  - `test-infra: carousel e2e + visual golden`
  - `docs: CHANGELOG + ROADMAP for Stage 2d`

- **Expected new tests:** ~20 sdk-rust unit, ~3 vitest, ~10
  Playwright e2e, 1 visual golden. Brings totals to ~341 sdk-rust
  lib, ~61 e2e, 6 goldens.

---

## 9. Open questions to revisit during implementation

Not blockers — defaults listed — but a deliberate decision is worth a
sentence once the code is in front of us:

1. **Snap duration** — 250 ms (Compose default) or 200 ms (iOS feel)?
   Default: 250 ms.
2. **Cubic-out vs ease-in-out** — cubic-out feels native on Android
   (deceleration-only). iOS uses ease-in-out for paging. Default:
   cubic-out (the rest of the SDK already mimics Android Compose).
3. **Snap on programmatic `scroll_to`?** — Default: no, `scroll_to`
   is exact. If callers want snap, they call `scroll_to_page(n)`
   (future API).
4. **Single-tap fling-to-next-page** — Compose `HorizontalPager`
   maps a velocity-only-no-distance fling to "advance one page". We
   defer that to a 2d-follow polish commit; first land the
   drag-distance + fling path.
