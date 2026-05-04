# Compositor Architecture — target spec

> **Status (May 2026):** target spec; not yet implemented. The
> renderer today is a single `OffscreenCanvas` driven by
> `Canvas2DBackend`
> ([`runtime/web/renderer/canvas2d.js`](../runtime/web/renderer/canvas2d.js)),
> with the kernel painting every layer (Wallpaper / App / Overlay
> / System) serially into that one surface every frame. This doc
> describes the **per-Activity surface + decoupled compositor**
> model the platform must move to in order to feel and behave
> like a real OS — and explains why this work is independent of
> (and prerequisite to) any future move to a `WebGPUBackend`.

## 1. Why this exists

Akiva's [`ARCHITECTURE.md §10`](../ARCHITECTURE.md) defines an
abstract `RendererBackend` trait so the per-op rendering API can
be swapped (Canvas2D today, WebGPU later). That trait is correct
but it is **only the leaf** of the rendering pipeline. Above it
sits a question the architecture document leaves open: *what is
the relationship between an Activity's paint ops and the pixels
that reach the screen?*

The current answer is "they are the same operation": the kernel
walks every visible surface in z-order and replays its `PaintOp`
list directly into the one `OffscreenCanvas` every frame. This
is the simplest possible compositor and it shipped Phases 3–13.
It has three consequences that block "real-OS-grade" robustness
and feel:

1. **Coupled frame timing.** A slow Activity stalls the frame.
   If `apply_props` for one app takes 12 ms, the system bar and
   every other surface paint at 14 ms — every other surface
   pays for one app's cost.
2. **No surface isolation in pixels.** All apps share the same
   render thread and the same canvas. A bug in one app's paint
   list can't cross worker boundaries (workers are isolated),
   but a *missed frame* in one app means the *whole screen*
   missed a frame.
3. **No room for compositor effects.** macOS's frosted-glass
   vibrancy, Android's elevation shadows, iOS's dynamic-island
   morphs — they all rely on a separate compositor pass that
   samples already-painted surfaces and runs effects on them.
   With a single direct-paint surface there is nothing to
   sample.

The fix is the standard fix every desktop and mobile OS uses:
**give each Activity its own surface, and add a compositor pass
that samples those surfaces into the screen surface.** Once
that's in place, the compositor pass can later be rewritten in
WebGPU to add effects — but the architectural change is the
critical part. WebGPU on its own, layered onto today's
single-canvas model, would not improve robustness.

## 2. Layers of the rendering pipeline

```
┌─────────────────────────────────────────────────────────────┐
│  Activity (wasm worker)                                     │
│  ─ owns: widget tree, layout, PaintOp emission              │
│  ─ outputs: ordered PaintOp list per frame                  │
└────────────────────┬────────────────────────────────────────┘
                     │ host_op_done → kernel
                     ▼
┌─────────────────────────────────────────────────────────────┐
│  Activity surface (one OffscreenCanvas per Activity)        │
│  ─ owns: a 2D backend instance bound to that canvas         │
│  ─ inputs: the Activity's PaintOp list                      │
│  ─ outputs: a rendered texture (the canvas itself)          │
│  ─ lifecycle: created on Activity Resumed, destroyed on     │
│    Stopped, retained on Paused (for fast resume)            │
└────────────────────┬────────────────────────────────────────┘
                     │ texture handle (ImageBitmap / canvas ref)
                     ▼
┌─────────────────────────────────────────────────────────────┐
│  Compositor pass (separate scheduler)                       │
│  ─ owns: the screen OffscreenCanvas                         │
│  ─ inputs: every Activity surface + system surfaces         │
│            + WindowManager z-order + bounds                 │
│  ─ outputs: the screen frame                                │
│  ─ scheduler: rAF on its own loop, decoupled from per-      │
│    Activity paint cadence                                   │
└─────────────────────────────────────────────────────────────┘
```

Three roles, three surfaces: Activity paints into its own
texture; the compositor samples textures into the screen; the
screen surface goes to the display via `transferControlToOffscreen`.
This is the same shape as macOS WindowServer + CoreAnimation,
Android SurfaceFlinger + HWC, and Windows DWM.

## 3. Activity surface

Every `WindowManager::create_surface` call also allocates a
backing canvas:

```rust
struct ActivitySurface {
    surface_id: SurfaceId,         // existing WM concept
    activity:   ActivityId,
    bounds:     Rect,              // logical px in display space
    canvas:     OffscreenCanvas,   // backing texture, lives in the worker
    backend:    Canvas2DBackend,   // bound to `canvas`
    last_paint: FrameNumber,       // frame at which we last redrew
    dirty:      bool,              // true if PaintOps arrived since last_paint
}
```

The Activity's `apply_props` invocation paints into this
backend, not into the screen backend. A `PaintOp` list reaching
the kernel for Activity A:

1. Look up `surfaces[A]`.
2. Mark `dirty = true`.
3. (Optionally, deferred) replay ops into `surface.backend` now,
   or queue for the compositor pre-pass.

**Sizing.** The surface canvas sizes to the Activity's bounds
(plus a small margin for transitions that translate the
surface). Resizes happen only when bounds change; mere z-order
or focus changes never resize.

**Lifecycle.** An Activity surface is allocated on `Resumed`
(the first frame the WM intends to show it), retained through
`Paused` so that a return to the Activity is one `drawImage`
away, and freed on `Stopped`. This matches the existing
`ensure_activity_surface` / `release_activity_surface` shape in
[`framework/src/lib.rs`](../framework/src/lib.rs) — only the
backing canvas and backend are added.

**Memory.** Each surface costs `width × height × 4 × dpr²` bytes
of GPU memory (the browser may keep a CPU copy too). On a 1080p
display at dpr 2, one fullscreen surface is ~16 MB. With 4
Resumed/Paused activities that's ~64 MB — within reach but a
real cost. The LMK already in [`framework/src/lmk.rs`](../framework/src/lmk.rs)
should account for surface memory in its budget.

## 4. Compositor pass

A new module — `framework/src/compositor.rs` — owns the screen
canvas and runs once per display frame:

```rust
fn composite_frame(display: DisplayId, frame: FrameNumber) {
    let backend = display.screen_backend;
    backend.clear(WALLPAPER_COLOR);
    for surface in window_manager.surfaces_in_z_order(display) {
        // For a clean dirty-rect implementation: skip surfaces whose
        // bounds intersect nothing dirty. v1 just draws all of them.
        backend.drawImage(surface.canvas, surface.bounds);
    }
    backend.present();
}
```

This is **the only place in the system that draws to the
screen.** Everything else paints into its own surface canvas.
The compositor never executes app-supplied PaintOps — it only
samples textures the Activity surface backends already painted.

### Why this matters for robustness

- **A slow Activity does not stall the frame.** If Activity A
  hasn't repainted its surface this frame (because its worker
  is busy or its `apply_props` is mid-flight), the compositor
  uses the surface's *previous* texture content and moves on.
  Activity A's *animation* freezes; the rest of the screen
  doesn't.
- **A crashed Activity leaves a frozen pane, not a black
  screen.** The surface canvas retains its last drawn pixels
  even after the worker is terminated. The compositor keeps
  drawing them until the WM tears the surface down.
- **System UI is independent.** Status bar, dialogs, the
  intent-chooser overlay all live in their own (kernel-owned)
  surfaces. They keep painting at 60 fps regardless of what
  any Activity is doing.

This is the meaning of "OS-grade rendering": *no app's render
cost is on the critical path of any other surface.*

### Dirty-rect optimization (v2)

Once per-surface caching is in place, the compositor can skip
intersection tests cheaply: the WM already knows which bounds
moved this frame and which surfaces were marked dirty. v1 of
this design redraws everything every frame; v2 introduces a
dirty-region accumulator that limits compositor work to the
union of (changed bounds ∪ moved surfaces ∪ z-order swaps).
This is the same optimization Wayland's `damage_region` and
macOS's `setNeedsDisplayInRect` provide.

## 5. Frame scheduling

Today there is one rAF loop in [`runtime/web/worker.js`](../runtime/web/worker.js)
that drives kernel-side scheduling, app dispatch, and screen
paint together. The new model splits this into two cadences:

### Compositor cadence (display rate, hard 60 / 120 Hz)

A dedicated rAF in the kernel worker that runs `composite_frame`
for each connected display. Never blocks on app workers; never
calls into Activity code. This loop *always* runs.

### Activity cadence (per-Activity, soft 60 Hz)

The framework still drives `apply_props` for each visible
Activity, but it does so on its own clock — it can fall to
30 Hz under pressure without stalling the compositor. When an
Activity finishes painting into its surface, it marks the
surface `dirty`. The compositor reads the dirty flag on the
*next* compositor tick, not synchronously.

**Inter-cadence ordering.** The compositor uses whatever pixels
the surface holds at composite-pass time. There is no waiting
for app frames. This is the same trade-off Android made with
Choreographer: smoother screen, occasionally an app shows a
frame slightly older than the screen refresh.

## 6. Effects pipeline (where WebGPU finally fits)

The compositor in §4 is a pure `drawImage`-loop — Canvas2D can
do this at 60 fps on commodity hardware for ~10 surfaces, no
problem. Where Canvas2D *can't* keep up is **effects on
already-painted surfaces:** live blur behind a sheet, drop
shadows under a floating window, color grading for night-mode
ramp, transitions that morph between two surfaces.

Once §3–§5 are in place, the compositor pass is the natural
home for these effects, and **only the compositor needs to
become WebGPU**. Activity backends stay Canvas2D (or whatever
the boot-time selection picks). The effects budget is bounded
and predictable: it's `O(visible_surfaces × screen_pixels)`,
not `O(app_paint_ops)`.

The architecture for this is:

```rust
trait CompositorBackend {
    fn begin_frame(&mut self, screen: &mut RenderSurface);
    fn draw_surface(&mut self, surface: &SurfaceTexture, bounds: Rect, opts: SurfaceDrawOpts);
    fn end_frame(&mut self);
}

struct SurfaceDrawOpts {
    opacity:       f32,
    transform:     Affine2,           // for slide / scale transitions
    backdrop_blur: Option<f32>,       // sample-and-blur of pixels behind
    drop_shadow:   Option<DropShadow>,
    tint:          Option<Rgba>,      // night-mode ramp, dim during multitasking
    clip:          Option<RoundedRect>,
}
```

Two implementations:
- `Canvas2DCompositor` — `backdrop_blur`, `drop_shadow`, `tint`
  fall back to no-op or to slow `ctx.filter` paths. Acceptable
  for v1.
- `WebGPUCompositor` — implements all `SurfaceDrawOpts` as
  shader passes; backdrop_blur via two-pass Gaussian on a
  downsampled texture; drop_shadow via separable blur on the
  surface's alpha channel; tint via fragment-shader color
  ramp.

`gpu_available()` selects between them at boot. The Activity
backends are unaffected.

## 7. Migration plan

This is the order things must happen in. Each step is
independently shippable; none of them require the next to be
useful.

### Step 1 — per-Activity surface canvases (Canvas2D)

- Allocate an `OffscreenCanvas` + `Canvas2DBackend` per
  Activity surface in `ensure_activity_surface`.
- Route the Activity's PaintOp list to *its* backend instead
  of the screen backend.
- Compositor pass: simple `drawImage` of every surface in
  z-order into the screen.
- WindowManager already tracks bounds and z-order (Stone 13.0);
  this step wires the compositor to read them.

DoD: a busy-loop in one Activity's `apply_props` no longer
stalls the system bar.

### Step 2 — decoupled frame scheduler

- Split rAF into compositor-loop + activity-loop.
- Compositor uses the most-recent dirty texture; never waits
  for an Activity to finish a frame.
- Surface retains last pixels across worker restarts.

DoD: kill -9 on an Activity worker shows a frozen surface, not
a black region. Slow `apply_props` halves Activity refresh
rate but leaves the compositor at 60 fps.

### Step 3 — compositor effects (Canvas2D fallbacks)

- Introduce `SurfaceDrawOpts` and the `CompositorBackend`
  trait.
- Implement transitions (slide / fade) as compositor effects
  on the surface — opacity and translate. The Activity stops
  knowing about animations entirely; today's
  [`framework/src/animation.rs`](../framework/src/animation.rs)
  becomes a compositor input rather than a paint-op rewriter.
- Drop-shadow, dim-overlay, night tint as Canvas2D-stretched
  no-ops on the compositor.

DoD: push/pop transitions are pixel-identical to Stone 13.3
output but driven by compositor opts, not by ops rewriting.

### Step 4 — WebGPU compositor

- Implement `WebGPUCompositor` against the `CompositorBackend`
  trait — only the compositor pass, not the per-Activity
  backends.
- Feature-detect at boot; fall back to `Canvas2DCompositor`.
- Land real backdrop blur, drop shadows, color grading.

DoD: a sheet rendering over a busy app shows live frosted-glass
blur of the app's pixels. Detection works; older browsers hit
the Canvas2D path with no visual loss for the basic shapes.

### Step 5 — WebGPU per-Activity backend (optional)

This is the *original* `RendererBackend` swap from
[`ARCHITECTURE.md §10`](../ARCHITECTURE.md). It becomes
relevant only when an Activity hits Canvas2D limits — heavy
charts, custom 3D, particle systems. Not a default; a per-app
opt-in via manifest.

## 8. Surface lifecycle interaction with isolation

A Worker dying mid-frame must leave the surface in a defined
state. Rules:

- The Activity backend is owned by **the kernel worker**, not
  the Activity worker. PaintOps stream over the existing host
  channel ([`docs/abi.md`](abi.md) `IO::APPLY_PROPS`); the
  kernel applies them to the Activity's surface backend. This
  is already the path in production.
- An Activity worker terminating leaves the surface canvas
  intact. The kernel marks the surface "frozen" — compositor
  keeps drawing it.
- On Activity restart (auto-relaunch), the new worker starts
  with a fresh AKML inflate; the kernel clears the surface
  canvas before the first new PaintOp list lands.

This is consistent with `docs/isolation-design.md` — the worker
boundary is the fault boundary; the surface canvas is the
**memory of what was last on screen**, owned by a more durable
authority (the kernel).

## 9. Cost summary

What this design buys, in one table:

| property                                            | today | after step 1 | after step 2 | after step 4 |
|-----------------------------------------------------|-------|--------------|--------------|--------------|
| Slow app stalls system bar                          | yes   | no           | no           | no           |
| Crashed app blanks its pane                         | yes   | no           | no           | no           |
| Compositor maintains 60 fps under app pressure      | no    | partial      | yes          | yes          |
| Live blur / drop-shadow / vibrancy                  | no    | no           | partial      | yes          |
| Predictable per-frame budget                        | no    | yes          | yes          | yes          |
| Texture memory cost (4 fullscreen surfaces, 1080p)  | 0     | ~64 MB       | ~64 MB       | ~64 MB       |
| Code complexity                                     | low   | medium       | medium       | high         |

Steps 1 and 2 are the load-bearing wins. Steps 3 and 4 are
polish. **Do not invert the order**: a WebGPU compositor on
top of today's single-surface model gains nothing of
substance.

## 10. Out of scope (deliberate)

- **Per-app GPU sandbox.** A WebGPU compositor running
  shaders is process-local to the kernel worker; the apps
  never see the GPU directly. There is no plan to expose
  WebGPU to apps in v0.x.
- **HDR / wide-gamut.** All backends operate in sRGB. HDR is
  a v0.6+ topic.
- **Hardware overlays.** Browsers don't expose them. Direct-
  to-display fast paths (video planes, cursor planes) are not
  a thing on the web.
- **Custom display sync.** The browser owns vsync; we ride
  rAF. No `display.requestRefreshRate(120)` or similar.

---

**Cross-references:**
- [`ARCHITECTURE.md §10`](../ARCHITECTURE.md) — the
  `RendererBackend` trait this design composes with.
- [`docs/window-manager.md`](window-manager.md) — surface
  ownership, z-order, focus.
- [`docs/isolation-design.md`](isolation-design.md) — worker
  fault boundary.
- [`docs/perf-baseline.md`](perf-baseline.md) — frame budgets
  the compositor must hit.
- [`framework/src/lmk.rs`](../framework/src/lmk.rs) — surface
  memory accounting.
