# Stage 2e — spring animations + bouncer demo

> **Status:** Done (2026-05-13). All six sub-stones landed; bouncer
> demo + 6 e2e + visual golden green. Spring + AnimatedValue land
> in one commit (2e.1+2 fused — same module, no natural seam).
> Builds on Stage 2d (stage-2d-carousel-plan.md).
> **Owner:** Claude × akiva1132. **Target:** 5-6 commits, ~25 new tests.
> **Why a follow-up:** Stage 2c.4 gave us *one* animation kind
> (exponential-decay fling), Stage 2d.3 added a *second* (cubic-out
> snap), and the gap is the third Android/iOS primitive: a critically-
> dampened spring. Springs let two things land:
>   1. **Overscroll bounce** — when a scroll hits its edge with
>      velocity, the content rubber-bands past and springs back. The
>      tactile signal "you've hit the end" is one of the load-bearing
>      gestures every native list does and feels jarring to its
>      absence.
>   2. **Generic spring animator** — `AnimatedValue<f32>` that any
>      widget can use (button press scale, item entrance, drag-and-
>      release-to-origin). One physics model, many surfaces.
> **Follow-ups (out of scope here):** spring-based snap (replace the
> cubic-out spec in carousel), shared-element transitions, multi-
> dimensional springs (vector-valued AnimatedValue), and AnimatedColor
> / AnimatedAffine. All ride on the same `Spring` primitive once it's
> in place.

---

## 1. State at start (the receipts)

Behaviour already in place from Stages 2c + 2d:

- `UiState::scroll_states: HashMap<String, ScrollCell>` with `axis`,
  `scroll_y`, `velocity`, `content_h`, `viewport_*`, `last_rect`,
  `snap_*`.
- `tick_animations(dt_ms)` — three-phase state machine: fling decay
  (`v *= 0.92^frames_dt`), snap entry (`(just_settled || pending)
  && mode != None`), snap advance (`cubic_out` lerp).
- **Edge clamp = hard stop** in `tick_animations`: when scroll_y +
  velocity * frames_dt clips past `[0, max]`, velocity = 0 and
  scroll_y pins to the edge. **No rubber-band; no bounce.**
- `Ui::scroll_to`, `Ui::feed_pointer`, `Ui::feed_wheel` all cancel
  snap state when they fire.

Test totals at start of 2e:
- sdk-rust lib: **360**
- framework lib: **294**
- wire crate: **9**
- vitest (web runtime): **244**
- Playwright e2e: **63**
- visual goldens: **6**

---

## 2. Goal — what "done" looks like

After Stage 2e:

1. **`Spring` primitive** — `Spring { stiffness, damping_ratio,
   epsilon }`. Methods:
   - `Spring::settled(value, target, velocity) -> bool` — true when
     both `|value - target| < epsilon` and `|velocity| < epsilon`.
   - `Spring::tick(&self, value, velocity, target, dt_ms) -> (new_v,
     new_velocity)` — semi-implicit Euler step against a spring
     potential well centred on `target`.
   Stiffness presets: `LOW`, `MEDIUM` (default), `HIGH`, `VERY_HIGH`.
   DampingRatio presets: `HIGH_BOUNCY`, `MEDIUM_BOUNCY`,
   `LOW_BOUNCY`, `NO_BOUNCY` (critically damped, the default).

2. **`AnimatedValue<f32>` public API** — wraps the spring math so app
   code can declare a value that springs toward a target:
   ```rust
   let mut bounce = AnimatedValue::new(0.0);
   bounce.spring_to(1.0, Spring::MEDIUM_BOUNCY);
   for _ in 0..60 { bounce.tick(16.667); }
   ```
   Reads via `bounce.value() -> f32`; settled-ness via
   `bounce.is_settled() -> bool`. State persists across rebuilds the
   same way `ScrollCell` does — apps usually carry one per
   animated property in their `UiState`-adjacent struct.

3. **Overscroll bounce** on `ScrollCell` — replaces the hard
   edge-clamp in `tick_animations`. When a fling reaches the edge:
   - **Past-edge phase**: content drifts up to `OVERSCROLL_MAX_PX`
     (default 64 dp) past the edge, decaying its velocity quickly.
   - **Spring-back phase**: a spring brings scroll_y back to the
     exact edge with the velocity it carried in. Critically damped
     by default (`SPRING_BOUNCE_DAMPING = 1.0`) — no oscillation,
     just a single overshoot then settle.
   Drag-from-edge: pulling the content past the edge during a drag
   slows the drag (rubber-band feel) and seeds the spring-back on
   release.

4. **Bouncer demo (`apps/bouncer/`)** — single draggable square.
   User drags it anywhere on the surface, releases; it springs back
   to the centre. Three preset chips ("Bouncy", "Stiff", "Wobbly")
   swap the spring config. A "release at velocity" toggle shows the
   difference between settled-state spring-back and a thrown-then-
   springed-back gesture.

5. **Inspector**: no wire bump. Overscroll bounce just changes how
   `scroll_y` evolves over frames — the inspector publishes whatever
   the cell currently holds. Tests verify "scroll_y exceeds max by
   ≤ OVERSCROLL_MAX_PX during a fling-to-edge" without any new fields.

6. **Tests**:
   - sdk-rust unit: spring math (settle, critical damping, overshoot,
     dt-normalisation), AnimatedValue API, overscroll math. Target
     **~20 new**.
   - e2e (Playwright): bouncer drag-and-release, edge bounce on
     feed and carousel, spring chip preset switch. Target **~6 new**.
   - Visual: `bouncer-idle.png`.

---

## 3. Architectural decisions (the load-bearing ones)

### 3.1 Critically-damped Euler spring, not analytic

Compose's `SpringAnimationSpec` solves the ODE in closed form
(`(A + B*t) * exp(-w0*t)` for critical damping). Closed-form is
exact but its branching (underdamped, critical, overdamped) doubles
the math. Semi-implicit Euler:
```
acc = -k * (value - target) - c * velocity
velocity += acc * dt
value += velocity * dt
```
is two lines, numerically stable for `k * dt^2 < 4`, and the dt
clamp from 2c.4 (max 4 frames per tick) keeps that comfortably true
for our stiffness range. **Choice:** Euler, not analytic.

### 3.2 Stiffness and damping as plain `f32`, not enums

Compose ships an enum (`Spring.StiffnessHigh = 10000f`); we expose
the constants directly:
```rust
impl Spring {
    pub const LOW:       Spring = Spring { stiffness:   200.0, damping_ratio: 1.0, epsilon: 0.01 };
    pub const MEDIUM:    Spring = Spring { stiffness:   700.0, damping_ratio: 1.0, epsilon: 0.01 };
    pub const HIGH:      Spring = Spring { stiffness:  1500.0, damping_ratio: 1.0, epsilon: 0.01 };
    pub const VERY_HIGH: Spring = Spring { stiffness: 10000.0, damping_ratio: 1.0, epsilon: 0.01 };
    pub const BOUNCY:    Spring = Spring { stiffness:   400.0, damping_ratio: 0.5, epsilon: 0.01 };
}
```
Apps can also build their own `Spring { stiffness: 350.0,
damping_ratio: 0.6, .. }` literally. **Choice:** constants on the
struct, not an enum — fewer type-conversion hoops in app code.

### 3.3 Overscroll lives in `tick_animations`, not a new phase

The edge-clamp lives inside the fling phase today. We replace its
"velocity = 0; scroll_y = clamp" with "compute overshoot, decay
velocity faster, enter spring-back if past max-overshoot or
velocity reverses". The snap state machine doesn't see this — to
snap, the scroll needs to be at rest at a non-grid position; an
overscroll bounce always settles at the edge, which *is* a grid
position (0 or page-multiple), so snap is naturally skipped. We add
new `ScrollCell` fields:
- `overscroll: f32` — signed past-edge offset. `0` everywhere except
  during a bounce.
- `bounce_velocity: f32` — the velocity captured at the moment the
  fling first crosses the edge. Re-applied by the spring restoring
  force.

State precedence inside `tick_animations`:
1. If a snap is active, advance it (skipping fling).
2. Else if fling velocity != 0, decay; on edge cross, set overscroll
   to current overshoot and clamp velocity sign reversal.
3. Else if overscroll != 0, run a spring tick on (overscroll, 0)
   with target 0; when settled, clamp scroll_y = the edge, clear
   overscroll.

### 3.4 Wheel events bypass overscroll

A user wheeling past the end shouldn't see a bouncing list — wheel
is a deliberate input, not a flick. `feed_wheel` continues to clamp
hard (no overshoot, no bounce). Drag-past-edge during a *touch*
gesture DOES get rubber-band — that's the canonical "I tried to
scroll past" affordance. Different code paths, different
expectations.

### 3.5 `AnimatedValue<f32>` is a free struct, not an opt-in macro

```rust
pub struct AnimatedValue<T> {
    value: T,
    velocity: T,
    target: T,
    spring: Spring,
}
```
Methods: `new`, `set_immediately`, `spring_to`, `tick`, `value`,
`velocity`, `is_settled`. Apps own one per animated property; the
SDK doesn't track them globally (unlike `ScrollCell`s which need
hit-test against the inspector tree). Generic `T` so we can add
`f32`, `Vec2`, color later — but only `f32` lands in 2e.

---

## 4. Sub-stones (commits)

### 2e.1 — `Spring` primitive + tick math

- `sdk-rust/src/ui/spring.rs` (new module). `pub struct Spring`,
  constants `LOW`/`MEDIUM`/`HIGH`/`VERY_HIGH`/`BOUNCY`. `tick`,
  `settled` methods.
- `sdk-rust/src/ui/mod.rs`: `pub use spring::Spring;`.
- **Tests:** monotonic approach to target, critical damping has no
  overshoot, low damping ratio overshoots once, dt-normalised
  settling time, settled() threshold. ~8 unit tests.

### 2e.2 — `AnimatedValue<f32>` public API

- `sdk-rust/src/ui/spring.rs`: `pub struct AnimatedValue<f32>`,
  methods per §3.5.
- Re-export from `prelude` so `use akiva_sdk::prelude::*` brings
  `AnimatedValue` and `Spring` into scope.
- **Tests:** spring_to advances toward target, set_immediately
  skips animation, is_settled flips when within epsilon, multiple
  spring_to calls retarget mid-flight. ~6 unit tests.

### 2e.3 — Overscroll bounce on `ScrollCell`

- `ScrollCell` adds: `overscroll: f32`, `bounce_velocity: f32`,
  `bounce_spring: Spring`. The spring config is the same across all
  scroll cells today (Compose has `OverscrollEffect` per-list, but
  one knob suffices for MVP); default = `Spring::MEDIUM` with the
  damping bumped slightly above 1.0 (`1.1`) so a critically-damped
  return is achievable without oscillation.
- Constant `OVERSCROLL_MAX_PX: f32 = 64.0` — how far a fling can
  carry past the edge before velocity force-cuts.
- `tick_animations` rewritten per §3.3.
- `feed_pointer` Down/Move during overscroll: cancel the
  bounce-back spring (overscroll = 0 immediately, scroll_y clamps),
  let the gesture take over.
- `feed_wheel`: unchanged (no overshoot — see §3.4).
- **Tests:** fling-to-edge produces overscroll > 0, overscroll
  springs back to 0 in finite ticks, |scroll_y - max| ≤
  OVERSCROLL_MAX_PX at all times, drag-Down cancels active bounce.
  ~8 unit tests.

### 2e.4 — `apps/bouncer/` demo app

- `apps/bouncer/Cargo.toml`, `manifest.toml` (id =
  `com.akiva.bouncer`, min_abi = "0.13.0").
- `apps/bouncer/src/lib.rs`:
  - Single 80-dp draggable square, drawn as a Rect with radius.
  - `AnimatedValue<f32>` for x and y. On drag, set value
    immediately; on release, `spring_to(centre, current_preset)`.
  - Three chip-buttons at the bottom: "Bouncy" (medium-bouncy),
    "Stiff" (high stiffness), "Wobbly" (low damping). Tapping a
    chip swaps the preset.
- Order alphabetically: bouncer = `com.akiva.bouncer` sits at row 1
  (after articles, before calculator).

### 2e.5 — Bouncer e2e + visual golden + edge-bounce e2e

- `tests/e2e/bouncer.spec.ts` (~6 tests): drag from centre,
  release-snaps-back, preset chips switch behaviour.
- `tests/e2e/visual/bouncer.visual.spec.ts`: `bouncer-idle.png`.
- Optional add-on: 2 tests in `feed.spec.ts` for overscroll bounce
  ("scroll_y exceeds max during fling-end" + "settles back to
  exact max"). Optional add-on: 1 test in `carousel.spec.ts` for
  bounce-at-page-edge.
- `tests/e2e/helpers/akiva.ts`: `launchBouncer(page)` at row 1.
  Shift `launchCalculator` 1 → 2, `launchCarousel` 2 → 3,
  `launchClipShapes` 3 → 4, `launchFeed` 4 → 5.

### 2e.6 — CHANGELOG + plan front-matter

- `CHANGELOG.md`: Unreleased / Stage 2e block describing the spring
  primitive, AnimatedValue, overscroll bounce, bouncer demo.
- `docs/stage-2e-springs-plan.md` — mark Done.

---

## 5. Test strategy

1. **Unit (sdk-rust)** — pure math. Spring tick monotonicity,
   damping behaviour, settled threshold, dt clamp, AnimatedValue
   lifecycle.
2. **Integration (sdk-rust)** — overscroll math inside
   `tick_animations`. Fling-to-edge produces a finite overshoot.
   Drag-cancel mid-bounce clears overscroll.
3. **e2e (Playwright)** — bouncer drag-and-release, preset chip
   swap, feed/carousel edge-bounce signature.
4. **Visual** — `bouncer-idle.png` golden.

---

## 6. Risks and mitigations

| # | Risk | Mitigation |
|---|------|------------|
| 1 | Semi-implicit Euler diverges at very high stiffness / large dt. | dt clamp (already 4-frame max) keeps `k * dt^2 < 4` for stiffness ≤ 10 000 and dt ≤ 67 ms. Tests assert convergence for `Spring::VERY_HIGH`. |
| 2 | Overscroll bounce + active snap fight. | snap takes precedence in `tick_animations`; overscroll only triggers from the fling path (no snap mode = bounce; snap mode = snap eats the velocity at the edge). |
| 3 | "Drag past the edge" feel is hard to get right. | First land bounce-on-fling only; drag-past-edge rubber-band is a 2e-follow. The bouncer demo demonstrates plain spring, the scroll edge-bounce demonstrates fling→spring. Either alone is a meaningful improvement. |
| 4 | AnimatedValue generic over T tempts gold-plating. | Concrete `AnimatedValue<f32>` only in 2e; `AnimatedValue<Vec2>` waits for the first real consumer. |
| 5 | Bouncer demo's two AnimatedValues (x, y) tempt a `AnimatedVec2`. | One bouncer property per axis is fine — two `tick(dt)` calls cost nothing. The Vec2 abstraction lands when a second consumer appears. |

---

## 7. Out of scope (Stage 2e-follow or later)

- **Spring-based snap** (carousel/feed). Cubic-out is fine for
  pagers; the spring upgrade is a polish pass once the math is
  proven.
- **Vector / Color / Affine AnimatedValue.** One concrete consumer
  at a time.
- **Pull-to-refresh.** Builds on overscroll bounce + a "release
  threshold" gesture. Stage 3 or 2f.
- **Shared-element transitions across activities.** Compose
  `LookaheadScope`. Stage 3.
- **Per-list overscroll config.** One global stiffness for now.

---

## 8. Estimated work and commit shape

- **5-6 commits**, one per sub-stone, plus docs at the end:
  - `sdk: Spring primitive — semi-implicit Euler + presets`
  - `sdk: AnimatedValue<f32> for app-owned spring animations`
  - `sdk: overscroll bounce on ScrollCell — fling → spring-back`
  - `demo: bouncer app — drag-and-release with preset chips`
  - `test-infra: bouncer e2e + visual golden + edge-bounce checks`
  - `docs: CHANGELOG entry + plan Done`

- **Expected new tests:** ~22 sdk-rust unit, ~6 Playwright e2e,
  1 visual golden. Brings totals to ~382 sdk-rust lib, ~69 e2e, 7
  goldens.
