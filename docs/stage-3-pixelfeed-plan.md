# Stage 3 — Akiva Pixelfeed (Instagram-style feed)

> **Status:** Done (2026-05-13). All 10 sub-stones landed; pixelfeed
> app live with 50 cards × 5 photos, heart-pop spring animation,
> double-tap-to-like, image fade-in on viewport entry. ABI stays
> 0.13.0 — Stage 3 adds opcodes 20/21 additively to the v0 paint
> stream. 12 e2e + 2 visual goldens green.
> **Owner:** Claude × akiva1132. **Target:** ~10 commits, ~55-65 new tests.
> Builds on Stages 2a-2e (clip, text, scroll, lazy-row, snap, spring).
> **Why this stage:** all of 2a-2e gave us one primitive each in
> isolation (rounded clip, multi-line text, vertical scroll,
> horizontal pager, spring/bounce). A real social feed exercises
> every one of them *together*, in one app, with per-card animated
> state — the toughest test of "does the SDK compose cleanly". The
> stage also lands two small new primitives that the demo demanded
> (`Canvas::push_opacity` and `DoubleTapDetector`) but that ride on
> existing wire opcodes and require no ABI bump.
> **Out of scope:** pull-to-refresh, video, sound, real network
> images, comment threads, save/share intents — all future stages.

---

## 1. State at start

Behaviour already in place from Stages 1 + 2a-2e:

- **Clip shapes** (Stage 2a) — `Canvas::push_round_clip`,
  `Frame::clip_radius(r)`. Circular avatars come for free.
- **Multi-line text** (Stage 2b) — `Text::max_lines(n)` with last-
  line ellipsis. Caption truncation comes for free.
- **Vertical scroll + LazyColumn** (Stage 2c) — virtualised list,
  fling, scroll-region semantics in inspector. Feed body comes for
  free.
- **Horizontal scroll + LazyRow + SnapMode::Page** (Stage 2d) —
  per-card photo pager comes for free; just key the LazyRow per
  card.
- **Spring + AnimatedValue + overscroll bounce** (Stage 2e) — per-
  card animated state (heart pop, image fade-in) comes for free
  once the app owns one `AnimatedValue<f32>` per card per
  animation.
- **Image blit** (pre-stage) — `Canvas::draw_image(drawable_id)`,
  `Ui::image(drawable_id)`. Per-photo bitmaps come from app
  resources.

**Test totals at start of Stage 3:**
- sdk-rust lib: **377**
- framework lib: **294**
- wire crate: **9**
- vitest (web runtime): **244**
- Playwright e2e: **69**
- visual goldens: **7**

---

## 2. Goal — what "done" looks like

After Stage 3:

1. **`com.akiva.pixelfeed`** app — a vertical feed of **50 cards**,
   each with:
   - **Header row** — circular avatar (clip_radius) + username +
     "•••" menu button.
   - **Photo carousel** — `LazyRow + SnapMode::Page` with **5
     photos** per card. Per-card scroll key
     (`format!("post-{i}-photos")`) means each card remembers its
     own page across feed scrolls. Page-indicator dots below.
   - **Action row** — heart, comment, share buttons. Heart toggle
     pops with a Spring animation (scale 1.0 → 1.35 → 1.0). Like
     count updates.
   - **Caption** — username (bold) + caption text with
     `max_lines(2)` + ellipsis. "more" button expands to full text
     (re-rendered with `max_lines(u32::MAX)`).
   - **Timestamp** — relative time text ("2h ago", etc.).
2. **Double-tap-to-like** on the photo area triggers the heart-pop
   animation centred on the photo (the Instagram signature). New
   SDK primitive `DoubleTapDetector` powers this.
3. **Image fade-in** — each photo crossfades from opacity 0 → 1
   over ~200ms the first time its card enters the viewport,
   reusing the new `Canvas::push_opacity` op + `AnimatedValue<f32>`
   from 2e.
4. **Inspector + e2e** — every interactive widget has a stable id;
   per-card pager state is queryable via `__akiva.scrollOf("post-N-
   photos")`.
5. **Visual goldens** — idle frame + one "after-double-tap" frame
   captured.

**Test totals at end of Stage 3 (target):**
- sdk-rust lib: ≥ **430** (+50)
- framework lib: 294 (unchanged)
- wire crate: 9 (unchanged)
- vitest: 244 (unchanged unless we touch inspector)
- Playwright e2e: ≥ **82** (+13)
- visual goldens: **9** (+2)

ABI: stays **0.13.0** — adds opcode 20 (`OP_PUSH_OPACITY`) and 21
(`OP_POP_OPACITY`) but these are *new* op codes appended to the
v0 paint stream, not changes to existing ones. Old apps that never
emit them keep working, and the renderer falls back to "alpha = 1"
when no opacity op is on the stack. Same shape as 2a's
`OP_PUSH_ROUND_CLIP` (= 19).

---

## 3. Architectural decisions

### 3.1 Per-card animation state lives in the app, not the SDK

`UiState` already owns `scroll_states: HashMap<String, ScrollCell>`
keyed by scroll-region key, so per-card *scroll* state (each photo
pager) is solved. For per-card *animation* state (heart pop,
image fade-in) we'd need a parallel HashMap inside `UiState` — but
that requires the SDK to know the *shape* of app animation state
(how many AnimatedValues, what keys, what springs). That couples
the SDK to the app.

The cleaner split: **apps own their animated state**, the SDK
provides the primitives (`AnimatedValue<f32>` + `Spring`). The
pixelfeed app keeps a `HashMap<u32, CardAnim>` next to its
`UiState` field, where `u32` is the post index. Pre-render, it
splits the borrow:

```rust
let cards = &mut self.cards;
let mut ui = Ui::new(&mut self.ui, ctx, c);
let list = ui.lazy_column(N).items(|ui, i| {
    let anim = cards.entry(i).or_insert_with(CardAnim::new);
    post_card(ui, i, anim)
});
```

Two disjoint borrows (`self.ui` and `self.cards`), one closure
that captures `cards` only — borrow-checker passes.

### 3.2 Image fade-in: opacity layer, not opacity-per-image

Adding an `opacity: f32` field to every `Node` variant would balloon
the wire format and the layout passes. Adding it to *just*
`Node::Image` would split the model. The clean shape is
**`Canvas::push_opacity(a) / pop_opacity()`** as paint ops — same
shape as `push_clip`/`pop_clip`. Any tree that wants to fade emits
a `Frame::opacity(av)` wrapper which compiles down to:

```
push_opacity(av.value())
… paint children …
pop_opacity()
```

The renderer maps this to `ctx.save() / globalAlpha=a / … /
restore()` — which canvas2d already does for composited surfaces
(the kernel uses it for slide-in transitions). So the runtime is
already plumbed; we just expose it to apps.

`Frame::opacity(f32)` is the builder method. `Frame::opacity_av(&AnimatedValue<f32>)`
is convenience sugar that calls `.value()` for you. Both clamp to
[0, 1].

### 3.3 Double-tap as an app-owned detector, not a Ui callback

We could add `Ui::on_double_tap(rect, callback)` but callbacks
inside `on_render` fight with the closure / borrow-checker patterns
the framework already established (`was_clicked` is *polled* after
`present`, not callback-style). The matching shape here is:

```rust
pub struct DoubleTapDetector {
    last_tap_ts: f64,    // 0.0 = never
    last_tap_x: f32,
    last_tap_y: f32,
}

impl DoubleTapDetector {
    pub fn feed(&mut self, ev: PointerEvent, now_ms: f64) -> Option<(f32, f32)> {
        // Only Up phase fires the candidate-tap check. Returns
        // Some((x, y)) when this Up completes a double-tap, None
        // otherwise. Caller's responsibility: filter by hit region
        // *before* calling feed, so taps outside the photo don't
        // reset the streak.
    }
}
```

Tunables:
- `MAX_INTERVAL_MS: f64 = 300.0` — Material Design default.
- `MAX_MOVE_PX: f32 = 32.0` — single-fingertip slop on a phone.

Apps own one detector per region they care about (one per card's
photo area). Per-card detectors live in the same
`HashMap<u32, CardAnim>` from §3.1.

### 3.4 Photo data — synthetic gradients, not bundled PNGs

50 cards × 5 photos = 250 unique images. Even at 80×80 thumbnails
that's a ~1 MB drawable bundle, which bloats the wasm and slows
boot. We instead **synthesise photos at paint time** as Frame
stacks of coloured Rects with rounded clips, producing a unique
"gradient block" per `(card, photo)` index. The visual is
deliberately abstract (the demo is about scroll/spring/clip
behaviour, not photography), and the data shape stays at zero
bytes added to the app.bundle. Implementation: a small palette
function `photo_block(i, j) -> Vec<NodeId>` that returns a Frame
with 2-3 rotated rounded rects.

Trade-off: image fade-in is over the *synthetic* photo, not a real
bitmap. That's fine — the fade is what's being tested; the bitmap
backing is incidental.

### 3.5 Heart-pop curve

Two-stage spring: on like-toggle, `av.spring_to(1.35,
Spring::BOUNCY)`; we watch for `av.value() >= 1.34` and then
`av.spring_to(1.0, Spring::HIGH)` to settle. This is the iOS-style
"pop-and-return" curve. Implemented in the app's per-frame tick:

```rust
match anim.heart_phase {
    HeartPhase::Idle | HeartPhase::Settled => {}
    HeartPhase::Popping => {
        if anim.heart_scale.value() >= 1.34 {
            anim.heart_scale.spring_to(1.0, Spring::HIGH);
            anim.heart_phase = HeartPhase::Returning;
        }
    }
    HeartPhase::Returning => {
        if anim.heart_scale.is_settled() {
            anim.heart_phase = HeartPhase::Settled;
        }
    }
}
```

(`heart_phase` keeps us from re-issuing `spring_to(1.0)` every
frame, which would be a no-op but feels wrong.)

---

## 4. Sub-stones

### 3.1 — Plan doc (this commit)

This file. Reference for every subsequent stone.

### 3.2 — Canvas opacity-layer primitive

**SDK side** (~30 LoC):
- Add `OP_PUSH_OPACITY: u8 = 20` and `OP_POP_OPACITY: u8 = 21` to
  `sdk-rust/src/canvas.rs`.
- `Canvas::push_opacity(&mut self, alpha: f32)` — encodes one f32
  LE clamped to [0, 1]. `pop_opacity` is a single byte.
- Matching opcodes in `framework/src/wasm_activity.rs` decoder.

**Renderer side** (~30 LoC):
- `runtime/web/renderer/canvas2d.js` — two new methods
  `pushOpacity(a)` and `popOpacity()` that wrap `ctx.save() /
  ctx.globalAlpha *= a / restore`. Nested opacities compose
  multiplicatively (matching CSS `opacity` cascade behaviour).
  The renderer's existing `compositeSurface` path already knows
  the save/restore pattern.
- Dispatch table entries 20 and 21 in
  `runtime/web/op_kinds.mjs` + `runtime/web/wasm_runtime.js`.

**Tests** (~6 new):
- sdk-rust: push/pop op encoding round-trips (`canvas.rs`
  module).
- framework: opcode decoder accepts 20/21 and produces matching
  PaintOp variants (`wasm_activity.rs` module).
- vitest: `canvas2d.test.mjs` — `globalAlpha` multiplies on push,
  restores on pop, nesting composes 0.5 × 0.5 = 0.25.

### 3.3 — ImageBuilder::opacity + Frame::opacity passthrough

**SDK side** (~80 LoC):
- `Frame` node gains `opacity: f32` (default 1.0, stored in
  `Node::Frame`). When `opacity < 1.0`, paint emits
  `push_opacity(opacity) / … children … / pop_opacity()`.
- `FrameBuilder::opacity(f32)` builder (clamped).
- `FrameBuilder::opacity_av(&AnimatedValue<f32>)` convenience.
- `ImageBuilder::opacity(f32)` — implemented as "wrap me in a
  Frame at lower the level"; documented to be a thin sugar.
- Layout: opacity is paint-only — does not affect size.

**Tests** (~15 new):
- Frame paint emits push/pop pair when opacity < 1.0; emits
  nothing extra when opacity == 1.0 (no-op fast path).
- Nested opaque Frames compose correctly.
- `opacity_av` reads value at paint time (not at build time —
  so the same NodeId animates over rebuilds).

### 3.4 — DoubleTapDetector gesture helper

**SDK side** (~80 LoC, new file `sdk-rust/src/ui/gesture.rs`):
- `DoubleTapDetector` struct as described in §3.3.
- `feed(&mut self, ev: PointerEvent, now_ms: f64) -> Option<(f32, f32)>`
  state machine.
- Re-exported in prelude.

**Tests** (~12 new):
- Up phase with no prior tap returns None and records the tap.
- Two Ups within 300 ms and 32 px return `Some((x, y))` of the
  second tap.
- Two Ups > 300 ms apart return None and start fresh.
- Two Ups > 32 px apart return None (drag, not double-tap).
- Down/Move phases never fire; only Up.
- Cancel resets state.

### 3.5 — pixelfeed app skeleton

`apps/pixelfeed/` — new Cargo workspace member.

**Files:**
- `apps/pixelfeed/Cargo.toml`
- `apps/pixelfeed/src/lib.rs` — Activity impl, helper functions,
  `akiva_app!` macro.
- `runtime/web/dev-apps/pixelfeed/manifest.toml`
- Build step copies `app.wasm` into the dev-apps dir (existing
  Makefile pattern).

**App structure** (no animation yet; that's 3.6/3.7):
- `PixelfeedMain { ui: UiState, cards: HashMap<u32, CardAnim>, last_ts: f64 }`.
- `CardAnim::default()` for newly-realised cards.
- `on_pointer` forwards to `ui.feed_pointer(ev)` plus per-card
  double-tap dispatch (gated behind the actual photo rect).
- `on_render`:
  - Title strip "Akiva Pixelfeed".
  - LazyColumn of 50 cards, key `"feed"`, item_h = 500.0
    (header 56 + photo 360 + actions 44 + caption 36 + meta 12,
    roughly).
  - Each card: VStack { header, photo_pager, action_row, caption_row,
    timestamp }, wrapped in a Frame { bg, content } card.

**Tests** (~6 unit, ~3 e2e):
- Card layout calculates expected height.
- Per-card photo pager renders 5 photos.
- Per-card scroll key is unique (no leakage across cards).

### 3.6 — Heart-pop animation + double-tap-to-like

**SDK side**: none new (3.4 already shipped DoubleTapDetector).

**App side** (~60 LoC):
- `CardAnim` gains `liked: bool`, `like_count: u32`,
  `heart_scale: AnimatedValue<f32>`, `heart_phase: HeartPhase`,
  `dt: DoubleTapDetector`.
- `on_pointer` routes to the per-card detector based on which
  card's photo rect contains the event.
- Heart-button tap (via `was_clicked`) toggles `liked`, increments
  `like_count`, kicks off the pop spring.
- Double-tap from the detector — same path, plus draws a brief
  heart overlay on the photo (fades in/out via a separate
  `AnimatedValue`).
- `on_render` per-card section reads `heart_scale.value()` and
  scales the heart icon via a `push_transform(centered, scale)`
  wrap.

**Tests** (~8 unit, ~3 e2e):
- Tap heart button: like count goes up, heart_phase = Popping,
  scale advances past 1.0.
- Tap again: count goes down, scale returns to 1.0.
- Double-tap on photo: same effect as heart button.
- Single tap on photo: no effect.

### 3.7 — Image fade-in

**App side** (~40 LoC):
- `CardAnim::image_alpha: AnimatedValue<f32>` (default 0.0).
- On first realisation of a card (entry into viewport),
  `image_alpha.spring_to(1.0, Spring::MEDIUM)`.
- Photo block wrapped in `Frame::opacity_av(&anim.image_alpha)`.
- Visibility detection: `LazyColumn::items` only realises visible
  rows, so the first call to `cards.entry(i)` *is* the entry
  event. Track `seen: bool` to avoid re-firing on subsequent
  rebuilds.

**Tests** (~4 unit, ~2 e2e):
- Newly entered card: alpha animation kicks off.
- Already-seen card: alpha stays at 1.0.
- Inspector reports the photo rect (visual smoke).

### 3.8 — e2e tests

`tests/e2e/pixelfeed.spec.ts` — full behavioural suite:
- Title and 50-card feed publishes on first paint.
- First card's photo pager has 5 photos; `__akiva.scrollOf("post-
  0-photos")` returns axis=horizontal, itemCount=5.
- Wheel-right on first card's pager advances `firstVisibleIndex`.
- Drag-left+release on photo pager triggers fling + snap.
- Tap heart button on first card: like-count flips from 0 to 1.
- Double-tap photo: like-count flips, heart-overlay text "♥"
  briefly visible.
- Fling-down feed: first card scrolls out of view, last card's
  pager state is queryable (proves per-card key isolation).

`tests/e2e/helpers/akiva.ts` — adds `launchPixelfeed(page)`. With
pixelfeed installed the alphabetical row order shifts: pixelfeed
sorts between `network` (`com.akiva.network`) and `shell`
(`com.akiva.shell`). The launcher row helpers care only about
articles..feed (rows 0-5), so existing helpers don't need to
re-number. `launchPixelfeed` clicks whatever row pixelfeed lands
at (computed from the installed list, not hardcoded).

### 3.9 — Visual goldens

`tests/e2e/visual/pixelfeed.visual.spec.ts`:
- `pixelfeed-idle.png` — first paint, top of feed.
- `pixelfeed-post-like.png` — after tapping heart on first card
  (heart filled + count incremented). Captured *after* the pop
  animation settles via `waitForStableLayout`.

### 3.10 — CHANGELOG + plan front-matter Done

- Add `## [Unreleased] — Stage 3 Pixelfeed (2026-05-XX)` block to
  `CHANGELOG.md` covering opacity-layer + DoubleTapDetector +
  pixelfeed app + tests.
- Flip this doc's status to "Done".

---

## 5. Test plan summary

| Layer | Existing | Stage 3 adds | Stage 3 total |
|-------|----------|--------------|---------------|
| sdk-rust lib | 377 | ~55 | ~432 |
| framework lib | 294 | ~4 (opcode decoder) | ~298 |
| wire crate | 9 | 0 | 9 |
| vitest | 244 | ~3 (canvas2d opacity) | ~247 |
| Playwright e2e | 69 | ~13 | ~82 |
| visual goldens | 7 | 2 | 9 |

---

## 6. Out of scope (explicit)

- **Real network images** — Akiva's HAL has no fetch layer yet;
  drawables are pre-decoded at app install. Stage 4 territory.
- **Video playback** — needs a video element + frame-pump.
  Eventually a separate "media" stage.
- **Pull-to-refresh** — would need a "scroll-released-at-top"
  callback + a refresh-spinner widget. Pure UX nicety, separate
  stage.
- **Comments / replies** — needs a sub-activity push + a text-
  input primitive. Text input is its own multi-stone effort
  (Stage 4?).
- **Stories strip** — horizontal LazyRow of circular avatars at
  the top of the feed. We have all the primitives; deferred to
  keep this stage focused on the *card* compose problem. Maybe
  a `Stage 3.x` follow-up.
- **Hebrew / RTL captions** — per the project-level MVP focus,
  i18n is design-doc-only; captions are LTR English strings.
- **Save / share intents** — needs the intent system to be
  exercised from an app, which the demo doesn't need.

---

## 7. ABI / wire impact

- **Paint stream:** +2 opcodes (`OP_PUSH_OPACITY = 20`,
  `OP_POP_OPACITY = 21`). Appended to the v0 paint stream; old
  decoders that never see these from old apps keep working. Same
  back-compat shape as Stage 2a's rounded-clip op.
- **Semantic tree wire:** **no change** — opacity is paint-only;
  it doesn't affect hit-test rects or scroll regions.
- **Inspector wire:** **no change** — opacity isn't a semantic
  attribute. (If we ever want "fade-this-button-is-disabled" we'd
  revisit, but for now the visual is purely cosmetic.)
- **ABI version:** stays **0.13.0**. Stage 3 is additive in the
  same shape as 2a-2e.

---

## 8. Risks + mitigations

- **Borrow-checker fight on per-card state.** The `let cards =
  &mut self.cards;` split was confirmed possible by inspection
  but until the closure captures it for real the compiler hasn't
  weighed in. Mitigation: write the simplest two-card variant
  first as a sanity check before scaling to 50.
- **50 cards × 5 photos = 250 synthetic photo blocks per frame.**
  LazyColumn realises only visible rows (~2-3 cards = 10-15
  photos at a time), so steady-state paint cost is fine. But
  every visible card builds its full photo pager every frame —
  LazyRow inside LazyColumn realises only the visible *photos*
  within each pager. That's the trick: nested virtualisation
  keeps the per-frame node count bounded. Mitigation: assert
  visible node count in a unit test.
- **Heart-pop spring with `Spring::BOUNCY` overshoots past 1.35
  briefly.** The `>= 1.34` threshold is conservative; if the
  spring undershoots (BOUNCY is bouncy but critically damped at
  ratio 0.7-0.8), the trigger fires before peak. Mitigation:
  use `Spring::LOW` damping for the pop-up so the curve actually
  reaches 1.35, then `Spring::HIGH` for the return; sanity-test
  in a unit test that the trigger fires within 30 frames.
- **Image fade-in fires on every realisation, not just first
  entry.** LazyColumn rebuilds items each frame they're visible,
  so `cards.entry(i).or_insert` only inserts *once*, but the
  `seen` flag must be set inside the `image_alpha.spring_to`
  call's parent block — not on the entry creation. Mitigation:
  unit test "two consecutive realisations of the same card do not
  re-fire `spring_to`".

---

## 9. Commit plan

```
docs: Stage 3 plan — pixelfeed (combined-primitives demo)         3.1
stage 3.2: Canvas push_opacity / pop_opacity ops                  3.2
stage 3.3: Frame::opacity + Image::opacity builder sugar          3.3
stage 3.4: DoubleTapDetector gesture helper                       3.4
stage 3.5: pixelfeed app skeleton — header, pager, actions, caption  3.5
stage 3.6: pixelfeed heart-pop + double-tap-to-like               3.6
stage 3.7: pixelfeed image fade-in on viewport entry              3.7
stage 3.8: pixelfeed e2e — 13 tests + launchPixelfeed helper      3.8
stage 3.9: pixelfeed visual goldens — idle + post-like            3.9
docs: CHANGELOG Stage 3 + plan front-matter Done                  3.10
```

Each commit is independently green (`cargo test` + `cargo build`
+ relevant vitest pass). E2e + visual gate the final two commits
where the app is fully alive on canvas.
