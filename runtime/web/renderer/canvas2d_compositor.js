// Canvas2D compositor backend.
//
// Phase 15.4.1 — extracted from `Canvas2DBackend` (paint backend). This
// class owns the SCREEN canvas; the paint backend keeps the per-Activity
// offscreen pool. The split mirrors the trait boundary in
// `docs/compositor-architecture.md §6` and is the pre-req for the
// WebGPU compositor to plug into the same pipeline interface.
//
// Frame model: Canvas2D is immediate-mode, so `beginFrame` /
// `endFrame` are no-ops. The hooks exist purely so the matching
// `WebGPUCompositor` (which DOES need a begin-pass / submit pair per
// frame) can share the same call shape with the routing layer in
// `compositing_pipeline.js`.
//
// Composite signature (`compositeSurface`) takes a SOURCE CANVAS, not a
// SurfaceId — the pipeline already looked up the source via
// `paintBackend.getSurfaceCanvas(id)` before calling us. The compositor
// has no knowledge of surface ids; that lives in the paint backend.

export class Canvas2DCompositor {
  constructor() {
    /** @type {OffscreenCanvas|null} */
    this.screenCanvas = null;
    /** @type {OffscreenCanvasRenderingContext2D|null} */
    this.screenCtx = null;
    this.metrics = { width_px: 0, height_px: 0, density: 1 };
  }

  /** Always available — no GPU dependency. Async to match the WebGPU
   *  compositor's signature for symmetric pipeline initialisation. */
  static async isAvailable() { return true; }

  async init(canvas, metrics) {
    this.screenCanvas = canvas;
    this.metrics = metrics;
    // `getContext("2d")` is idempotent per HTML spec — calling it again
    // (e.g. from the paint backend in Canvas2D mode) returns the same
    // context object. Sharing the screen ctx between the two backends
    // is intentional: it lets the legacy DEFAULT_SURFACE / zero-bounds
    // sentinel paint to the screen via the paint backend without an
    // extra round-trip through this class.
    this.screenCtx = canvas.getContext("2d", { alpha: false });
    if (!this.screenCtx) throw new Error("Canvas2D context unavailable on screen canvas");
    this.screenCtx.imageSmoothingEnabled = true;
    this.screenCtx.textBaseline = "top";
  }

  /** Hand the screen 2D context to the paint backend so its
   *  DEFAULT_SURFACE / zero-bounds path still paints to the screen.
   *  WebGPUCompositor returns null here — the screen is bound to
   *  "webgpu" and can't expose a 2D context. */
  getScreenCtx() { return this.screenCtx; }

  resize(width_px, height_px, density) {
    if (!this.screenCanvas) return;
    this.screenCanvas.width = width_px;
    this.screenCanvas.height = height_px;
    this.metrics = { width_px, height_px, density };
    if (this.screenCtx) {
      // Resizing the canvas resets the context state machine.
      this.screenCtx.imageSmoothingEnabled = true;
      this.screenCtx.textBaseline = "top";
    }
  }

  beginFrame() { /* immediate-mode; no-op */ }
  endFrame()   { /* same */ }

  /** Composite `src` (a canvas-shaped object the paint backend
   *  produced) onto the screen at `(x, y, w, h)` with `SurfaceDrawOpts`.
   *  Identity opts (opacity=1, translate=(0,0), scale=1) take a fast
   *  path with no save/restore; anything else uses
   *  `save → translate → scale → globalAlpha → drawImage → restore`
   *  so frame N's effects can't leak into frame N+1.
   *
   *  Phase 15.4.3 — `_blurRadius` is accepted to match the WebGPU
   *  compositor signature but ignored on the Canvas2D path. CSS
   *  `filter: blur()` on a 60Hz live-blurred sheet is too slow for an
   *  app-grade UI; sheets / dialogs that need frosted glass must boot
   *  on the WebGPU path. */
  compositeSurface(src, x, y, w, h, opacity, translateX, translateY, scale, _blurRadius) {
    if (!this.screenCtx) return;
    if (!src || w <= 0 || h <= 0) return;
    const isIdentity =
      opacity === undefined ||
      (opacity === 1 && translateX === 0 && translateY === 0 && scale === 1);
    if (isIdentity) {
      this.screenCtx.drawImage(src, x, y, w, h);
      return;
    }
    const ctx = this.screenCtx;
    ctx.save();
    if (translateX !== 0 || translateY !== 0) ctx.translate(translateX, translateY);
    if (scale !== 1 && scale > 0) ctx.scale(scale, scale);
    if (opacity !== 1) {
      // Clamp defensively; an animation that overshoots [0, 1] would
      // wash out (>1) or invert (<0) on some browsers.
      ctx.globalAlpha = Math.max(0, Math.min(1, opacity));
    }
    ctx.drawImage(src, x, y, w, h);
    ctx.restore();
  }

  present() {
    // OffscreenCanvas commits implicitly at end-of-task. The deprecated
    // `commit()` API is intentionally unused.
  }

  destroy() {
    this.screenCtx = null;
    this.screenCanvas = null;
  }
}
