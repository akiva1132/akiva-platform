// Compositing pipeline — routing layer between the kernel HAL and the
// two backends.
//
// Phase 15.4.1: split the old "Canvas2DBackend does everything" model
// into a paint backend (per-Activity offscreens + paint primitives)
// and a compositor backend (screen ownership + composite). This file
// is the seam: every `hal_*` import in `worker.js` lands here, and we
// dispatch to one of the two backends.
//
// Routing rules (see `docs/phase-15.4-plan.md §1.2`):
//   per-op paint  (clear/fillRect/drawText/...)  → paintBackend
//   beginSurface / endSurface / destroySurface   → paintBackend
//   compositeSurface(id, ...)                    → look up source via
//                                                   paintBackend.getSurfaceCanvas(id),
//                                                   then forward to compositorBackend
//   drawables register / forget                   → paintBackend
//   resize                                        → both
//   present                                       → both
//
// `init` is async because `WebGPUCompositor` requires
// `requestAdapter()` + `requestDevice()`. Falls back to
// `Canvas2DCompositor` if WebGPU is unavailable or its init throws.

import { Canvas2DBackend } from "./canvas2d.js";
import { Canvas2DCompositor } from "./canvas2d_compositor.js";
import { WebGPUCompositor } from "./webgpu_compositor.js";

export class CompositingPipeline {
  constructor() {
    /** @type {Canvas2DBackend|null} */
    this.paint = null;
    /** @type {(Canvas2DCompositor|WebGPUCompositor)|null} */
    this.compositor = null;
    this.metrics = { width_px: 0, height_px: 0, density: 1 };
    /** "webgpu" or "canvas2d" — set after init. */
    this.compositorKind = null;
  }

  /**
   * Boot the pipeline. `opts.preferGPU` defaults to `true`; pass
   * `false` to force Canvas2D (debugging, perf comparison).
   * `opts.compositorFactories` is a test seam — pass
   * `{ webgpu: <ctor>, canvas2d: <ctor> }` to inject mocks.
   */
  async init(screenCanvas, metrics, opts = {}) {
    const preferGPU = opts.preferGPU ?? true;
    const factories = opts.compositorFactories ?? {
      webgpu:   WebGPUCompositor,
      canvas2d: Canvas2DCompositor,
    };

    let compositor = null;
    let kind = null;
    if (preferGPU && (await factories.webgpu.isAvailable())) {
      try {
        compositor = new factories.webgpu();
        await compositor.init(screenCanvas, metrics);
        kind = "webgpu";
      } catch (err) {
        console.warn("[akiva] WebGPU init failed; falling back to Canvas2D:", err?.message ?? err);
        compositor = null;
      }
    }
    if (!compositor) {
      compositor = new factories.canvas2d();
      await compositor.init(screenCanvas, metrics);
      kind = "canvas2d";
    }
    this.compositor = compositor;
    this.compositorKind = kind;
    console.log(`[akiva] using ${kind === "webgpu" ? "WebGPU" : "Canvas2D"} compositor`);

    this.paint = new Canvas2DBackend();
    // Canvas2D mode: paint backend shares the screen ctx with the
    // compositor (both call getContext("2d") on the same canvas, which
    // is idempotent per HTML spec). The legacy DEFAULT_SURFACE / zero-
    // bounds sentinel still paints to the screen.
    //
    // WebGPU mode: the canvas is bound to "webgpu" and can't expose a
    // 2D context. Pass `null` so `Canvas2DBackend.init` leaves its
    // screenCtx null; the legacy sentinel becomes a silent no-op (in
    // production the framework always emits real BeginSurface bounds,
    // so this never triggers).
    this.paint.init(kind === "canvas2d" ? screenCanvas : null, metrics);
    this.metrics = metrics;
  }

  resize(width_px, height_px, density) {
    this.metrics = { width_px, height_px, density };
    this.compositor?.resize(width_px, height_px, density);
    this.paint?.resize(width_px, height_px, density);
  }

  // ── Per-op paint primitives — straight forward to paint backend ───

  clear(rgba)                                        { this.paint?.clear(rgba); }
  fillRect(x, y, w, h, rgba)                         { this.paint?.fillRect(x, y, w, h, rgba); }
  fillRoundRect(x, y, w, h, r, rgba)                 { this.paint?.fillRoundRect(x, y, w, h, r, rgba); }
  drawText(x, y, size, rgba, text)                   { this.paint?.drawText(x, y, size, rgba, text); }
  strokeRect(x, y, w, h, width, rgba)                { this.paint?.strokeRect(x, y, w, h, width, rgba); }
  strokeRoundRect(x, y, w, h, r, width, rgba)        { this.paint?.strokeRoundRect(x, y, w, h, r, width, rgba); }
  drawLine(x0, y0, x1, y1, width, rgba)              { this.paint?.drawLine(x0, y0, x1, y1, width, rgba); }
  drawImage(id, x, y, w, h)                          { this.paint?.drawImage(id, x, y, w, h); }
  drawForeignImage(pkg, name, x, y, w, h)            { this.paint?.drawForeignImage(pkg, name, x, y, w, h); }
  pushTransform(dx, dy, scale)                       { this.paint?.pushTransform(dx, dy, scale); }
  popTransform()                                     { this.paint?.popTransform(); }
  pushClip(x, y, w, h)                               { this.paint?.pushClip(x, y, w, h); }
  pushRoundClip(x, y, w, h, radius)                  { this.paint?.pushRoundClip(x, y, w, h, radius); }
  popClip()                                          { this.paint?.popClip(); }
  setGlobalAlpha(alpha)                              { this.paint?.setGlobalAlpha(alpha); }

  // ── Surface markers ───────────────────────────────────────────────

  beginSurface(id, x, y, w, h) {
    // Open the compositor's frame on the first surface of the frame.
    // Canvas2D's beginFrame is a no-op; WebGPU's opens a render pass.
    // Lazy: a frame with no surfaces (rare) skips the begin/submit.
    this.compositor?.beginFrame?.();
    this.paint?.beginSurface(id, x, y, w, h);
  }

  endSurface() {
    this.paint?.endSurface();
  }

  compositeSurface(
    id, x, y, w, h, opacity, tx, ty, scale,
    blurRadius, shadowOX, shadowOY, shadowBlur, shadowRgba, tint,
  ) {
    // Zero-bounds DEFAULT_SURFACE sentinel. In Canvas2D mode the
    // activity painted directly to the screen ctx during BeginSurface,
    // so there's nothing to composite — return. In WebGPU mode the
    // matching beginSurface allocated a screen-sized offscreen; we
    // composite it at full screen bounds.
    //
    // Why bounds=0 in production: the framework calls
    // `bounds_of(id) → Rect::ZERO` for an Activity whose surface is
    // registered but the WindowManager hasn't recorded layout yet.
    // That happens for the first ~25ms of every boot, between
    // `register(...)` and `init_ok`. Until 15.4.1's WebGPU compositor,
    // those frames painted directly to the screen ctx — we restore
    // that contract here.
    if (w <= 0 || h <= 0) {
      if (this.compositorKind !== "webgpu") return;
      w = this.metrics.width_px;
      h = this.metrics.height_px;
      if (w <= 0 || h <= 0) return;
      x = 0; y = 0;
    }
    const src = this.paint?.getSurfaceCanvas(id);
    if (!src) return;
    this.compositor?.compositeSurface(
      src, x, y, w, h, opacity, tx, ty, scale,
      blurRadius, shadowOX, shadowOY, shadowBlur, shadowRgba, tint,
    );
  }

  // ── Frame lifecycle ───────────────────────────────────────────────

  present() {
    this.paint?.present();
    this.compositor?.endFrame?.();
    this.compositor?.present?.();
  }

  destroySurfaceCanvas(id) {
    this.paint?.destroySurfaceCanvas(id);
  }

  // ── Drawables — paint backend owns the cache ──────────────────────

  registerDrawable(id, source)                  { this.paint?.registerDrawable(id, source); }
  forgetDrawable(id)                            { this.paint?.forgetDrawable(id); }
  registerForeignDrawable(pkg, name, source)    { this.paint?.registerForeignDrawable(pkg, name, source); }
  forgetForeignDrawables(pkg)                   { this.paint?.forgetForeignDrawables(pkg); }

  destroy() {
    this.compositor?.destroy?.();
    this.paint?.destroy?.();
    this.compositor = null;
    this.paint = null;
  }
}
