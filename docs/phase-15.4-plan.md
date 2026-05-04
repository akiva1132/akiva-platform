# Phase 15.4 — WebGPU Compositor — Implementation Plan

> **Status:** plan only. Implementation has not started.
> **Spec (target):** [`docs/compositor-architecture.md §6`](compositor-architecture.md)
> **ROADMAP entry:** Phase 15.4 in [`ROADMAP.md`](../ROADMAP.md)
> **Pre-reqs (closed):** 15.1 (`bc02a47`), 15.2 (`841297d`), 15.3 (`9870eee`)

This document is written so a fresh Claude Code session — with no
memory of the prior conversation — can pick it up and execute. It
states the architecture, the sub-stones, the file-by-file changes,
the WGSL skeletons, the test strategy, and the user-verification
checkpoints. The author of the plan is the same author as 15.1–15.3;
read those commits' messages for the upstream rationale.

---

## 0. State at the start of 15.4

### 0.1 What's already in place

**Per-Activity surfaces (15.1):** `Compositor::finish_frame_in_order`
emits `BeginSurface(id, bounds) … EndSurface … CompositeSurface(id,
bounds, opts) … DestroySurface(id)` opcodes. JS-side
`Canvas2DBackend` allocates an `OffscreenCanvas` per surface id;
paint ops route to that surface's 2D context between begin/end;
`compositeSurface` calls `screenCtx.drawImage(surfaceCanvas, …)`.

**Frozen-pane robustness (15.2):** an Activity that didn't paint
this frame skips Begin/End emission entirely (since `fc.ops` is
empty). The trailing CompositeSurface still fires; the renderer's
backing canvas retains last-frame pixels.

**Compositor-level transforms (15.3):** `SurfaceDrawOpts { opacity,
translate_x, translate_y, scale }` is carried on
`CompositeSurface`. Slide/fade animations apply opts at the screen
composite step (not by rewriting the activity's own paint ops).
`apply_props` (the op-list rewriter) is deleted.

### 0.2 Files to know

```
framework/src/compositor.rs      — PaintOp, DrawHal trait, dispatch
framework/src/wasm_activity.rs   — wire codec (encode_op / decode_frame)
framework/src/animation.rs       — Animations::props_for()
framework/src/lib.rs             — Framework::finalize_frame() (calls finish_frame_in_order)
kernel/src/host.rs               — extern "C" hal_* declarations
kernel/src/display_manager.rs    — ExternDrawHal impl forwards to externs

runtime/web/worker.js            — kernel rAF loop; HAL imports → backend
runtime/web/renderer/canvas2d.js — Canvas2DBackend (paint + composite)
runtime/web/paint_decoder.mjs    — JS-side wire decoder (follower path)
runtime/web/op_kinds.mjs         — JS mirror of opcode constants

runtime/web/__tests__/canvas2d_paintops.test.mjs  — 47 vitest cases
framework/src/compositor.rs::tests                — Rust unit tests
```

### 0.3 Test counts (must stay green)

- **Rust:** 294 framework + 247 sdk-rust + 9 wire + 6 result.
- **JS (vitest):** 194 cases across 4 files.
- **Browser smoke:** `cargo run -p akiva -- run`; calculator opens,
  taps update display, slide push/pop animations don't clip.

---

## 1. Architectural decisions

### 1.1 JS-side split: paint backend vs. compositor backend

Today `Canvas2DBackend` does both:
1. **Paint primitives** — `fillRect`, `drawText`, etc., into either
   the screen ctx or a per-Activity surface ctx.
2. **Compositing** — `beginSurface` / `endSurface` /
   `compositeSurface` / `destroySurfaceCanvas`.

15.4 splits these along the trait boundary already implicit in
[`docs/compositor-architecture.md §6`](compositor-architecture.md):

```
┌─────────────────────────────────────────────────────────────┐
│ PaintBackend (always Canvas2D)                              │
│   ─ owns: Map<SurfaceId, OffscreenCanvas + ctx>            │
│   ─ implements: clear, fillRect, fillRoundRect, drawText,  │
│                 stroke*, drawLine, drawImage,              │
│                 drawForeignImage, push/popTransform,       │
│                 push/popClip, setGlobalAlpha,              │
│                 beginSurface, endSurface,                  │
│                 destroySurfaceCanvas                       │
│   ─ does NOT own: the screen canvas                        │
└─────────────────────────────────────────────────────────────┘
                       │
                       │ getSurfaceCanvas(id) → OffscreenCanvas
                       ▼
┌─────────────────────────────────────────────────────────────┐
│ CompositorBackend (Canvas2D OR WebGPU, picked at boot)      │
│   ─ owns: the screen canvas (OffscreenCanvas)               │
│   ─ implements: init(screenCanvas, metrics), resize,        │
│                 beginFrame, compositeSurface(srcCanvas,     │
│                 dst rect, opts), endFrame, destroy          │
│   ─ for WebGPU: also owns adapter/device/pipeline objects   │
└─────────────────────────────────────────────────────────────┘
```

Both are duck-typed JS classes (no formal interface — JS doesn't
enforce them, but the methods are documented and the callers
type-check via JSDoc).

### 1.2 Routing layer: `CompositingPipeline`

A thin wrapper in `runtime/web/renderer/compositing_pipeline.js`
that the kernel's `hal_*` extern imports talk to. Routes:

- Per-op imports (`hal_draw_*`) → `paintBackend.<method>`.
- `hal_begin_surface`, `hal_end_surface`, `hal_destroy_surface`
  → `paintBackend.<method>` (paint backend manages the offscreen
  pool).
- `hal_composite_surface(id, x, y, w, h, opacity, tx, ty, scale)`
  → looks up `paintBackend.getSurfaceCanvas(id)`, then calls
  `compositorBackend.compositeSurface(srcCanvas, x, y, w, h, opts)`.

**Why this routing layer, not direct method dispatch:** worker.js
keeps a single `backend` reference today; we now have two. The
pipeline encapsulates the lookup so worker.js doesn't grow
two-state branching everywhere.

### 1.3 Backend detection + selection

At `attach_display`:

```js
async function selectCompositor(screenCanvas, metrics) {
  if (typeof navigator !== "undefined" && navigator.gpu) {
    try {
      const compositor = new WebGPUCompositor();
      await compositor.init(screenCanvas, metrics);
      return compositor;
    } catch (err) {
      console.warn("[akiva] WebGPU init failed; falling back to Canvas2D:", err);
    }
  }
  const fallback = new Canvas2DCompositor();
  fallback.init(screenCanvas, metrics);
  return fallback;
}
```

`attach_display` is currently sync; 15.4.1 makes it async. The
worker.js side already returns a Promise to `session.start()`, so
this is additive.

### 1.4 Canvas-context exclusivity

A canvas can have ONE rendering context type for its lifetime. The
screen canvas binds to either `"2d"` (Canvas2DCompositor) or
`"webgpu"` (WebGPUCompositor). This means **the screen canvas
binding is a property of the compositor backend**, not of the
paint backend. The paint backend only ever calls `getContext("2d")`
on its own per-Activity offscreens.

### 1.5 Surface texture interop (WebGPU side)

Per-Activity offscreens are Canvas2D `OffscreenCanvas`. The WebGPU
compositor needs to sample them as textures every frame.

**Path A — `device.importExternalTexture({ source: offscreenCanvas })`:**
zero-copy. Browser support: Chrome ≥ 113, Edge ≥ 113, Safari TP.
Firefox: not yet for OffscreenCanvas as of mid-2026.

**Path B — `device.queue.copyExternalImageToTexture({ source: offscreenCanvas }, dest, ...)`:**
copies pixels to a regular `GPUTexture`. Costs one full-frame copy
per surface per frame. Universal support.

**Decision:** Path A by default; fall back to Path B if
`device.importExternalTexture` is missing or throws on
`OffscreenCanvas`. The fallback test is "run path A once at init
on a 1×1 dummy offscreen; if it throws, switch the runtime to
path B."

### 1.6 Wire format extensions for new effects (15.4.3+)

`SurfaceDrawOpts` grows three optional effect fields:

```rust
pub struct SurfaceDrawOpts {
    pub opacity:      f32,
    pub translate_x:  f32,
    pub translate_y:  f32,
    pub scale:        f32,
    // New in 15.4.3+:
    pub backdrop_blur_radius: f32,           // 0.0 = no blur
    pub drop_shadow:          DropShadow,    // alpha=0 = no shadow
    pub tint:                 u32,           // rgba; alpha=0 = no tint
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct DropShadow {
    pub offset_x:    f32,
    pub offset_y:    f32,
    pub blur_radius: f32,
    pub rgba:        u32,  // alpha byte gates rendering
}
```

`COMPOSITE_SURFACE` opcode wire size grows from 37 bytes (15.3) to
**77 bytes** (15.4.6):
- `opcode (1) + id (4) + bounds (4×f32=16) + opts: opacity/tx/ty/scale (4×f32=16) + backdrop_blur (4) + drop_shadow (4×f32=16, then rgba u32=4 → 20) + tint (4)` → 1 + 4 + 16 + 16 + 4 + 20 + 4 = **65** bytes.

Recompute carefully:
- opcode: 1
- id: 4
- bounds (x, y, w, h): 4 × 4 = 16
- opacity, translate_x, translate_y, scale: 4 × 4 = 16
- backdrop_blur_radius: 4
- drop_shadow: offset_x, offset_y, blur_radius (3 × 4 = 12) + rgba (4) = 16
- tint: 4
- Total: 1 + 4 + 16 + 16 + 4 + 16 + 4 = **61 bytes**.

Pin this in a wire-size test (see §3.6.5 below).

### 1.7 Backwards compatibility per sub-stone

Each sub-stone keeps existing tests green and existing apps
rendering. New fields default to "no effect" (zero blur, alpha-zero
shadow, alpha-zero tint).

---

## 2. Sub-stone breakdown

Each sub-stone is independently shippable, has its own commit, and
is verified visually by the user before the next one starts.

### 15.4.1 — Architectural split + WebGPU detection + identity composite

**Goal:** pure refactor + WebGPU bring-up. Visual output identical
to 15.3.

**Files to add:**
- `runtime/web/renderer/canvas2d_compositor.js` — extracts
  `compositeSurface` from `Canvas2DBackend`, owns the screen ctx.
- `runtime/web/renderer/webgpu_compositor.js` — minimal WebGPU
  compositor. Identity composite only (no opts, no effects yet).
- `runtime/web/renderer/wgsl/identity.wgsl` — vertex+fragment for
  textured quad sampling.
- `runtime/web/renderer/compositing_pipeline.js` — routing layer.

**Files to modify:**
- `runtime/web/renderer/canvas2d.js` — keep as `Canvas2DBackend`
  (paint pool only). Remove screen-canvas ownership;
  `compositeSurface` / `screenCanvas` / `screenCtx` move to the
  compositor. Add `getSurfaceCanvas(id) -> OffscreenCanvas | null`
  for the routing layer.
- `runtime/web/worker.js` — `attachDisplay` becomes async; selects
  compositor; HAL imports route through the new pipeline.

**Order of operations:**

1. **Extract Canvas2DCompositor.** Move `screenCanvas`, `screenCtx`,
   `init(screenCanvas, metrics)`, `resize`, `compositeSurface`,
   `present`, and `destroy` (the screen-affecting parts) into the
   new file. Canvas2DBackend keeps the offscreen pool and all paint
   methods, plus `beginSurface` / `endSurface` /
   `destroySurfaceCanvas`. Add `getSurfaceCanvas(id)` returning
   the entry's canvas (or null).

2. **Add CompositingPipeline.** `init(screenCanvas, metrics)`
   creates the paint backend, calls `selectCompositor`, returns a
   Promise. Forwards every method either to paintBackend or to
   compositorBackend per §1.2.

3. **WebGPUCompositor skeleton.** `static async isAvailable()`
   returns `!!navigator.gpu`. `init(screenCanvas, metrics)`:
   - `adapter = await navigator.gpu.requestAdapter()` (throw if null)
   - `device = await adapter.requestDevice()`
   - `format = navigator.gpu.getPreferredCanvasFormat()`
   - `ctx = screenCanvas.getContext("webgpu")` (throw if null)
   - `ctx.configure({ device, format, alphaMode: "premultiplied" })`
   - Compile `identity.wgsl` into a render pipeline.
   - Allocate a sampler (`linear` filter, `clamp-to-edge`).
   - Allocate a uniform buffer for per-composite data (initially
     just `dst rect: vec4<f32>`).

4. **`compositeSurface(srcCanvas, x, y, w, h, opts)`:**
   - `texture = device.importExternalTexture({ source: srcCanvas })`
     (or fall back to copyExternalImageToTexture path).
   - Build a per-call bind group binding the texture + sampler +
     uniform buffer.
   - Encode a render pass: `loadOp: "load"` (don't clear; we're
     compositing on top of prior surfaces this frame).
   - Draw 4 vertices (a fullscreen quad reduced to dst rect via
     vertex shader transform).

5. **Frame structure.** WebGPU is a "begin/draw…/submit" model,
   not the immediate-mode Canvas2D. Add `beginFrame()` and
   `endFrame()` to the CompositorBackend interface:
   - `beginFrame()` — `commandEncoder = device.createCommandEncoder()`;
     start a render pass into the canvas's current texture; load
     the wallpaper color (clear).
   - `compositeSurface(...)` — encode draw into the open render
     pass.
   - `endFrame()` — end the render pass; `device.queue.submit([encoder.finish()])`.

   For Canvas2DCompositor, `beginFrame` clears the canvas with
   the wallpaper color (today's behavior) and `endFrame` is a
   no-op.

6. **Update worker.js HAL imports.** `hal_begin_surface` →
   `pipeline.beginSurface(...)`, etc. The pipeline already
   delegates correctly (§1.2). Add a `hal_present` mapping that
   calls `pipeline.endFrame()` (today maps to `backend.present()`).

7. **Frame-shape change.** The Rust kernel today calls
   `hal_draw_clear` first (per the activity emitting `Clear`), then
   `hal_*` for each op, then `hal_draw_present` at end. With per-
   surface compositing, clear should happen on the SCREEN once per
   frame BEFORE composites. Today's Clear inside an Activity's ops
   clears the surface's offscreen — fine. The screen clear is
   handled by `beginFrame` of the compositor backend.

   **Risk:** the kernel emits `hal_draw_present` at frame end but
   doesn't have a "begin frame" hook. Workaround in 15.4.1: have
   the JS side trigger `beginFrame` on the first
   `hal_begin_surface` of a frame and `endFrame` on
   `hal_draw_present`. Track a `frameInProgress` flag. This is a
   bit ugly but avoids changing the kernel ABI in 15.4.1.

   *Better long-term* (15.4.7+): add `hal_frame_begin` /
   `hal_frame_end` extern declarations and call them from
   `akiva_frame`. Out of scope for 15.4.1.

**WGSL — `identity.wgsl`:**

```wgsl
// Phase 15.4.1 — identity textured quad. No effects, just sample
// the surface texture and emit a quad at (dstRect.xy) with size
// (dstRect.zw) onto the screen.

struct Uniforms {
  // dst rect in screen pixels: (x, y, w, h)
  dst_rect: vec4<f32>,
  // screen size in pixels: (w, h, 0, 0)
  screen_size: vec4<f32>,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var s: sampler;
@group(0) @binding(2) var t: texture_external;  // path A
// (texture_2d<f32> if path B is used; pipeline picks at boot)

struct VsOut {
  @builtin(position) clip: vec4<f32>,
  @location(0) uv: vec2<f32>,
};

@vertex
fn vs(@builtin(vertex_index) vid: u32) -> VsOut {
  // Two triangles forming a quad: vids 0..3 → corners
  let corner = vec2<f32>(f32(vid & 1u), f32((vid >> 1u) & 1u));
  let pixel = u.dst_rect.xy + corner * u.dst_rect.zw;
  // Pixels → NDC (Y flipped: WebGPU is Y-up, screens are Y-down)
  let ndc = vec2<f32>(
    pixel.x / u.screen_size.x * 2.0 - 1.0,
    1.0 - pixel.y / u.screen_size.y * 2.0,
  );
  var o: VsOut;
  o.clip = vec4<f32>(ndc, 0.0, 1.0);
  o.uv = corner;
  return o;
}

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
  return textureSampleBaseClampToEdge(t, s, in.uv);
}
```

Note: emits 4 vertices via `triangle-strip`; vid layout
`(0,0), (1,0), (0,1), (1,1)`.

**Tests:**
- vitest stays green. The existing 47 canvas2d tests now exercise
  Canvas2DBackend's paint methods only (the screen-related ones
  move to a new file).
- New tests in `runtime/web/__tests__/compositing_pipeline.test.mjs`:
  - `pipeline routes per-op calls to paint backend`
  - `pipeline routes compositeSurface to compositor backend with the right canvas`
  - `pipeline.init falls back to Canvas2DCompositor when navigator.gpu is missing`
- WebGPU is not unit-testable in vitest-node (no GPU). Skip; rely
  on browser smoke.

**DoD for 15.4.1:**
- All 194 (or close to) vitest tests still green; new pipeline tests
  added.
- `cargo run -p akiva -- run` on Chrome 113+ shows the calculator
  rendering through the WebGPU path. Console log "[akiva] using
  WebGPU compositor". Pixel output identical to 15.3.
- `cargo run -p akiva -- run` on Firefox shows the calculator
  rendering through the Canvas2D path. Console log "[akiva] using
  Canvas2D compositor".
- `cargo test --workspace --lib` 100% green.

**User checkpoint:** open the calculator, do 5 taps, observe that
animation push from main → "secondary screen" (if any) plays
smoothly. Report any visual regression.

---

### 15.4.2 — Transform / opacity opts on the WebGPU path

**Goal:** WebGPU compositor honors `SurfaceDrawOpts { opacity,
translate_x, translate_y, scale }`. Visual parity with the
Canvas2D path during slide+fade transitions.

**Files to modify:**
- `runtime/web/renderer/wgsl/identity.wgsl` — extend uniforms to
  include opts; vertex shader applies translate+scale; fragment
  multiplies alpha.
- `runtime/web/renderer/webgpu_compositor.js` — uniform buffer
  layout grows; bind group layout matches.

**Uniform layout (16-byte aligned):**

```wgsl
struct Uniforms {
  dst_rect:    vec4<f32>,  // (x, y, w, h) in screen px
  screen_size: vec4<f32>,  // (w, h, 0, 0)
  opts:        vec4<f32>,  // (opacity, translate_x, translate_y, scale)
};
```

**Vertex shader change:**

```wgsl
let scaled = u.dst_rect.zw * u.opts.w;        // scale dst size
let pivot  = u.dst_rect.xy + u.dst_rect.zw * 0.5;
let pixel  = pivot + (corner - vec2<f32>(0.5)) * scaled
                   + u.opts.yz;                // translate
```

(Scale around the dst center; translate after.)

**Fragment shader change:**

```wgsl
let c = textureSampleBaseClampToEdge(t, s, in.uv);
return vec4<f32>(c.rgb, c.a * u.opts.x);
```

**Tests:** add a single integration sketch (manual). vitest unit
tests don't cover GPU.

**DoD:**
- Push/pop slide animations between activities look identical to
  the Canvas2D path.
- Frame budget under 4ms for 1080p × 1 surface.

**User checkpoint:** open calculator, tap a button that pushes a
new screen (or simulate via Shell intent), watch slide-from-right.
Smooth, no jitter, no clipping at edges.

---

### 15.4.3 — Backdrop blur (frosted glass)

**Goal:** add `backdrop_blur_radius` to `SurfaceDrawOpts`. Sheets
and dialogs get live blurred backgrounds.

**Files to modify:**

Rust:
- `framework/src/compositor.rs` — `SurfaceDrawOpts.backdrop_blur_radius: f32`. Update `IDENTITY` (default 0.0). `is_identity` checks new field.
- `framework/src/wasm_activity.rs` — wire codec adds 4 bytes to
  COMPOSITE_SURFACE.
- `kernel/src/host.rs` — extend `hal_composite_surface` signature.
- `kernel/src/display_manager.rs` — pass new field.
- `framework/src/lib.rs::finalize_frame` — overlays may set blur
  in 15.4.3+; default 0.0.

JS:
- `runtime/web/worker.js` — extend HAL import.
- `runtime/web/paint_decoder.mjs` — extend wire decoder.
- `runtime/web/renderer/compositing_pipeline.js` — pass field.
- `runtime/web/renderer/canvas2d_compositor.js` — silently ignore
  (no Canvas2D fallback for live blur).
- `runtime/web/renderer/webgpu_compositor.js` — implement blur
  pipeline.

**WGSL — `backdrop_blur.wgsl`:**

Two-pass Gaussian on a downsampled offscreen render target:

1. **Capture screen region behind the surface** into a 1/4-resolution
   GPU texture. (Render pass: copy a sub-rect of the screen
   texture into the downsample target.)
2. **Horizontal blur pass.** Sample 9 taps with Gaussian weights
   ([0.227, 0.194, 0.121, 0.054, 0.016] kernel for σ ≈ radius / 4).
   Output to scratch texture.
3. **Vertical blur pass.** Same kernel, vertical taps. Output to
   blur result texture.
4. **Composite the surface ON TOP** of the blur result, with opts
   from §2 applied. The blur shows through where the surface is
   transparent / has alpha < 1.

**Pipeline objects:**
- `downsamplePipeline` — bilinear sample of screen → 1/4 RT
- `blurHPipeline` — horizontal Gaussian
- `blurVPipeline` — vertical Gaussian
- `compositeWithBackdropPipeline` — main composite that samples
  both the surface texture AND the blur texture

**Cost:**
- 1 downsample (cheap)
- 9 + 9 = 18 fragment-shader taps per blur pixel × (screen_w/4 ×
  screen_h/4)
- 1 composite (cheap)
- ~0.3–0.8ms on commodity GPU; well within 16.6ms budget.

**Trick — only blur when needed:** if `backdrop_blur_radius == 0`,
skip all three blur passes; use the identity composite path.

**DoD:**
- A test surface with `backdrop_blur_radius = 20.0` over a busy
  background shows live, smooth frosted glass.
- Toggling activity behind it (animations, redraws) shows the blur
  updating each frame.
- Canvas2D path: blur is silently ignored; surface composites
  opaque.

**User checkpoint:** I'll provide a manual test artifact (an
intent that opens a modal with `backdrop_blur_radius=20`). User
visually verifies frosted-glass appearance over the calculator.

---

### 15.4.4 — Drop shadow

**Goal:** add `DropShadow { offset_x, offset_y, blur_radius, rgba }`
to `SurfaceDrawOpts`. Modals/popups get soft shadows beneath them.

**Files to modify:** same surface-area as 15.4.3, plus
`runtime/web/renderer/wgsl/drop_shadow.wgsl`.

**Algorithm:**

1. Render the surface's alpha channel into a single-channel scratch
   texture, offset by `(offset_x, offset_y)`, tinted by
   `rgba.rgb`.
2. Separable Gaussian blur on the scratch (radius =
   `blur_radius`).
3. Composite the blur result UNDER the surface's RGBA composite.

**Shader sketch (drop_shadow.wgsl, alpha extraction pass):**

```wgsl
@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
  let surf = textureSampleBaseClampToEdge(t, s, in.uv);
  let shadow_rgb = vec3<f32>(
    f32((u.shadow_rgba >> 16u) & 0xffu) / 255.0,
    f32((u.shadow_rgba >>  8u) & 0xffu) / 255.0,
    f32( u.shadow_rgba         & 0xffu) / 255.0,
  );
  let shadow_a = f32((u.shadow_rgba >> 24u) & 0xffu) / 255.0;
  return vec4<f32>(shadow_rgb * surf.a, surf.a * shadow_a);
}
```

(Blur passes after this work the same as backdrop blur §3.)

**Composition order per surface (when both blur and shadow set):**
1. Capture backdrop (blur).
2. Render shadow blur.
3. Composite backdrop blur into screen at surface bounds.
4. Composite shadow at surface bounds + shadow offset.
5. Composite surface at surface bounds.

**DoD:**
- A surface with `drop_shadow { offset: (0, 4), blur: 20, rgba: 0x40000000 }`
  shows a soft 20px shadow underneath, offset 4px down.
- Canvas2D path: silently ignored.
- Shadow + blur compose correctly (shadow bleeds onto blur, surface
  on top of both).

**User checkpoint:** dialog over calculator shows shadow underneath.

---

### 15.4.5 — Color tint

**Goal:** add `tint: u32` to `SurfaceDrawOpts`. Inactive
activities can dim/tint during multitasking transitions; macOS-
style "background app fade".

**Files to modify:** same surface-area as 15.4.3.

**Shader extension (in the main composite WGSL):**

```wgsl
let surf = textureSampleBaseClampToEdge(t, s, in.uv);
let tint_rgb = unpack_rgb(u.tint);
let tint_a   = unpack_a(u.tint);
let mixed = mix(surf.rgb, tint_rgb, tint_a);
return vec4<f32>(mixed, surf.a * u.opts.x);
```

**DoD:**
- A surface with `tint = 0x80000000` (50% black overlay) renders
  noticeably dimmed.
- `tint = 0` (alpha zero) is a fast-path no-op.
- Compositionally cheap (single fragment-shader op; no extra pipeline).

---

### 15.4.6 — Wire format consolidation

**Goal:** all four 15.4.x effect fields land in `SurfaceDrawOpts`
on a single wire format bump. Single `opcode::COMPOSITE_SURFACE`
size of 61 bytes (see §1.6).

If 15.4.3–5 each bumped wire size separately, this is the
clean-up commit; if they were batched, this stone is the
batched rollout.

**Pin a wire-size test:**
```rust
#[test]
fn composite_surface_wire_size_pinned_for_15_4() {
    let mut buf = Vec::new();
    encode_op(&mut buf, &PaintOp::CompositeSurface { /* … all defaults … */ });
    assert_eq!(buf.len(), 61, "Phase 15.4.6 — full SurfaceDrawOpts wire layout");
}
```

---

## 3. Implementation order — what to do session-by-session

### Session 1 (15.4.1)
**Estimated effort:** 4–6 hours focused work.
- Steps in §2 / 15.4.1.
- Commit message: `phase 15.4.1: split paint/compositor backends, WebGPU detection + identity composite`.

### Session 2 (15.4.2)
**Estimated effort:** 2–3 hours.
- Extend uniforms; vertex shader handles translate/scale; fragment
  multiplies alpha.
- Commit message: `phase 15.4.2: WebGPU compositor honors translate / scale / opacity`.

### Session 3 (15.4.3)
**Estimated effort:** 6–8 hours. Most shader-heavy.
- Backdrop blur pipeline. Test against a busy background.
- Commit message: `phase 15.4.3: backdrop blur (frosted glass) on WebGPU compositor`.

### Session 4 (15.4.4)
**Estimated effort:** 4–6 hours. Reuses blur infrastructure from
15.4.3.
- Drop shadow pipeline.
- Commit message: `phase 15.4.4: drop shadow on WebGPU compositor`.

### Session 5 (15.4.5)
**Estimated effort:** 2 hours.
- Tint shader.
- Commit message: `phase 15.4.5: color tint on WebGPU compositor`.

### Session 6 (15.4.6)
**Estimated effort:** 2 hours, mostly wire-format consolidation.
- Pin sizes; sweep to ensure no leftover field mismatches.
- Commit message: `phase 15.4.6: SurfaceDrawOpts wire format consolidation`.

**Total:** 20–27 hours. Conservative estimate 4 weeks calendar
time with user-verification round trips.

---

## 4. Risk register

### 4.1 WebGPU device loss mid-session

Rare but happens (driver crash, GPU reset). On `device.lost`
event:
1. Log warning.
2. Mark compositor as failed.
3. On next frame, `pipeline` reinitializes via `selectCompositor` —
   most likely falls back to Canvas2D (since WebGPU recovery isn't
   automatic).

Per-Activity surfaces are Canvas2D and unaffected.

### 4.2 `importExternalTexture` quirks across browsers

Safari sometimes fails on `OffscreenCanvas` sources. The fallback
to `copyExternalImageToTexture` covers this, at the cost of one
copy per surface per frame (typically 4 surfaces × 1080p × 4 bytes
× 60 fps = ~2 GB/s of memcpy; significant but workable).

Detection: try import on a 1×1 dummy at boot. If it throws, use
copy path for the session.

### 4.3 Canvas-context exclusivity

If the screen canvas was previously bound to "2d" (because the
old code initialized Canvas2DBackend on it), `getContext("webgpu")`
will fail. Means: you can't switch backends at runtime on the same
canvas. Consequence: detection must happen before ANY paint to
the screen. The 15.4.1 init order enforces this.

### 4.4 Pre-multiplied vs. straight alpha

WebGPU canvas configuration uses
`alphaMode: "premultiplied"` by convention. The Canvas2D
offscreens we sample produce **straight** alpha. The shader must
pre-multiply when sampling: `output.rgb *= output.a` before
returning. Easy to miss; pin via a visual test (white surface
with 50% alpha should look 50% gray, not 50% transparent white).

### 4.5 Browser support check

| Browser            | WebGPU | importExternalTexture |
|--------------------|--------|-----------------------|
| Chrome ≥ 113       | ✅     | ✅                    |
| Edge ≥ 113         | ✅     | ✅                    |
| Safari ≥ 18.0      | ✅     | partial (TP only)     |
| Firefox ≥ 147      | ✅     | ✅                    |
| Older browsers     | ❌     | n/a — Canvas2D path   |

Safari pre-18 falls through to Canvas2D — no regression.

### 4.6 Performance regression on Canvas2D path

The architectural split moves screen ownership from
`Canvas2DBackend` to `Canvas2DCompositor`. Same drawImage at the
end. **Should be byte-identical performance.** Smoke-test with
a 60fps animation; any regression → investigate the routing
layer's per-call overhead.

---

## 5. Testing strategy

### 5.1 What's unit-testable (vitest-node)

- The routing layer: which method calls land on which backend.
- Canvas2DCompositor: same as today's tests, just on a different
  class (move + adapt 5–10 tests).
- Wire codec round-trips for the new opt fields.

### 5.2 What's NOT unit-testable

- Actual WebGPU rendering output. No GPU in vitest-node.
- Visual correctness of blur, shadow, tint.

### 5.3 Browser smoke checklist (per sub-stone)

User runs `cargo run -p akiva -- run` and verifies (in 30 seconds):

- [ ] Console: "[akiva] using WebGPU compositor" (or Canvas2D fallback message)
- [ ] Calculator opens; buttons render; taps register
- [ ] No JS errors in DevTools console
- [ ] FPS counter (browser perf monitor) ≥ 55
- [ ] Stone-specific visual property (slide animation smooth, blur
      visible, shadow soft, tint applied)

### 5.4 Cross-browser matrix

Run smoke in: Chrome ≥ 113, Firefox ≥ 147, Safari ≥ 18 (if user
has Mac). Document on each commit which browsers were verified.

---

## 6. Definition of "Phase 15.4 complete"

- All six sub-stones merged.
- `SurfaceDrawOpts` carries opacity, translate, scale, backdrop_blur_radius, drop_shadow, tint — all tested round-trip.
- WebGPU compositor implements all five effects (identity, transform/opacity, blur, shadow, tint).
- Canvas2D compositor implements transform/opacity (15.3 baseline); silently ignores blur/shadow/tint.
- Boot detection works; fallback graceful; no panics on Firefox.
- Browser smoke passed on at least Chrome + Firefox.
- Total Rust test count: ~300 framework + 247 sdk-rust + 9 wire + 6 result.
- Total vitest count: ~200 (a few new pipeline + wire tests).
- ROADMAP Phase 15.4 marked closed; CHANGELOG entries written.

---

## 7. Out of scope (deliberately)

- **Hardware overlays.** Browsers don't expose them. Not on the
  roadmap.
- **Direct canvas-to-display fast paths.** Same.
- **Dynamic refresh rates.** Browser owns vsync; we ride rAF.
- **WebGPU compute shaders for non-rendering work.** App-level
  WebGPU is 15.5 (separate, opt-in stone).
- **Memory profiling of GPU textures.** Add when the LMK starts
  caring (it currently tracks only worker memory).
- **Multi-display WebGPU.** 15.4 ships single-display only;
  follower tabs continue via Canvas2D paint stream over
  `hal_post_to_display` (existing path).

---

## 8. Cross-references

- [`docs/compositor-architecture.md`](compositor-architecture.md) — design spec (the "what")
- [`ARCHITECTURE.md §10`](../ARCHITECTURE.md) — `RendererBackend` trait
- [`ROADMAP.md` Phase 15](../ROADMAP.md) — sub-stone tracking
- 15.1 commit `bc02a47` — per-Activity surfaces
- 15.2 commit `841297d` — frozen pane fix
- 15.3 commit `9870eee` — compositor-level effects
