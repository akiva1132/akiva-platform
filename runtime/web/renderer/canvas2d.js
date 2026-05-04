// Canvas2D renderer backend.
//
// Implements the RendererBackend trait from ARCHITECTURE.md §10 against
// an OffscreenCanvas 2D context. Phase 1a uses this directly; Phase 2
// adds a WebGPU backend and a feature-detection shim that picks the best
// available option at boot.

// Font stack used everywhere we draw text. Must stay in lockstep with
// the measurement path in app_registry.js — any divergence and the
// SDK's intrinsic widths won't match what lands on the canvas.
export const FONT_FAMILY = "system-ui, -apple-system, sans-serif";

/** Canonical `CanvasRenderingContext2D.font` value for a given size. */
export function fontString(size_px) {
  return `${size_px}px ${FONT_FAMILY}`;
}

export class Canvas2DBackend {
  constructor() {
    /** @type {OffscreenCanvas|null} */
    this.screenCanvas = null;
    /** @type {OffscreenCanvasRenderingContext2D|null} */
    this.screenCtx = null;
    /** @type {OffscreenCanvasRenderingContext2D|null} */
    this.ctx = null;  // Current paint target — screen by default,
                       // switches to a per-surface ctx between
                       // beginSurface / endSurface.
    this.metrics = { width_px: 0, height_px: 0, density: 1 };
    // Phase 15.1 — per-Activity surface backing canvases. Keyed by
    // SurfaceId (u32). Each entry is `{ canvas, ctx, w, h }`. Allocated
    // lazily on first beginSurface(id, ...); freed by
    // destroySurfaceCanvas(id). Persists between frames so a surface
    // that didn't paint this frame keeps its last pixels visible —
    // that's the load-bearing robustness property of 15.1.
    /** @type {Map<number, { canvas: OffscreenCanvas, ctx: OffscreenCanvasRenderingContext2D, w: number, h: number }>} */
    this._surfaces = new Map();
    // Stack of paint targets for nested begin/end (no nesting in v1
    // but the stack lets us be defensive: an unmatched endSurface
    // simply does nothing instead of leaving the ctx pointing at a
    // freed surface). v1 emits at most one Begin/End per surface per
    // frame, all at top level.
    /** @type {OffscreenCanvasRenderingContext2D[]} */
    this._ctxStack = [];
  }

  // Backwards-compatibility shim — pre-Phase-15 callers expected
  // `backend.canvas` to be the screen OffscreenCanvas. Some test
  // harnesses still read it; expose a getter that returns the
  // screen surface (never the per-Activity ones).
  get canvas() { return this.screenCanvas; }

  init(canvas, metrics) {
    this.screenCanvas = canvas;
    this.metrics = metrics;
    this.screenCtx = canvas.getContext("2d", { alpha: false });
    if (!this.screenCtx) throw new Error("Canvas2D context unavailable");
    this.screenCtx.imageSmoothingEnabled = true;
    this.screenCtx.textBaseline = "top";
    this.ctx = this.screenCtx;
  }

  resize(width_px, height_px, density) {
    if (!this.screenCanvas) return;
    this.screenCanvas.width = width_px;
    this.screenCanvas.height = height_px;
    this.metrics = { width_px, height_px, density };
    // The context is reset when the canvas is resized; re-apply defaults.
    if (this.screenCtx) {
      this.screenCtx.textBaseline = "top";
      this.screenCtx.imageSmoothingEnabled = true;
    }
    // Per-Activity surfaces don't auto-resize; the framework will
    // re-emit BeginSurface with the new bounds and we'll resize the
    // backing canvas in `beginSurface` if it doesn't match.
  }

  clear(rgba) {
    if (!this.ctx) return;
    // Phase 15.1 — `this.ctx.canvas` is the current paint target's
    // backing canvas (screen or per-Activity surface). Using
    // `this.canvas` would always pick the screen, which is wrong
    // when called between beginSurface / endSurface.
    const c = this.ctx.canvas;
    this.ctx.fillStyle = rgbaToCss(rgba);
    this.ctx.fillRect(0, 0, c.width, c.height);
  }

  fillRect(x, y, w, h, rgba) {
    if (!this.ctx) return;
    this.ctx.fillStyle = rgbaToCss(rgba);
    this.ctx.fillRect(x, y, w, h);
  }

  fillRoundRect(x, y, w, h, radius, rgba) {
    if (!this.ctx) return;
    this.ctx.fillStyle = rgbaToCss(rgba);
    if (radius <= 0) {
      this.ctx.fillRect(x, y, w, h);
      return;
    }
    this.ctx.beginPath();
    this.ctx.roundRect(x, y, w, h, radius);
    this.ctx.fill();
  }

  drawText(x, y, size_px, rgba, text) {
    if (!this.ctx) return;
    this.ctx.fillStyle = rgbaToCss(rgba);
    this.ctx.font = fontString(size_px);
    this.ctx.fillText(text, x, y);
  }

  // Stone 13.0prep — outline ops. `width` is the stroke width in dp;
  // strokes are center-aligned (half inside, half outside) per Canvas
  // 2D's default.

  strokeRect(x, y, w, h, width, rgba) {
    if (!this.ctx) return;
    this.ctx.strokeStyle = rgbaToCss(rgba);
    this.ctx.lineWidth = width;
    this.ctx.strokeRect(x, y, w, h);
  }

  strokeRoundRect(x, y, w, h, radius, width, rgba) {
    if (!this.ctx) return;
    this.ctx.strokeStyle = rgbaToCss(rgba);
    this.ctx.lineWidth = width;
    if (radius <= 0) {
      this.ctx.strokeRect(x, y, w, h);
      return;
    }
    this.ctx.beginPath();
    this.ctx.roundRect(x, y, w, h, radius);
    this.ctx.stroke();
  }

  drawLine(x0, y0, x1, y1, width, rgba) {
    if (!this.ctx) return;
    this.ctx.strokeStyle = rgbaToCss(rgba);
    this.ctx.lineWidth = width;
    this.ctx.lineCap = "round";
    this.ctx.beginPath();
    this.ctx.moveTo(x0, y0);
    this.ctx.lineTo(x1, y1);
    this.ctx.stroke();
  }

  // Stone 13.3 — transform stack. Used by activity push/pop slide
  // animations: apply_props wraps the activity's ops with
  // `PushTransform { dx, dy, scale }` … `PopTransform` to translate
  // (and optionally scale) every nested op without per-op rewriting.
  // Implemented via `ctx.save()` + `ctx.translate()` / `ctx.scale()` so
  // matrix composition follows Canvas 2D's defined semantics.
  pushTransform(dx, dy, scale) {
    if (!this.ctx) return;
    this.ctx.save();
    if (dx !== 0 || dy !== 0) this.ctx.translate(dx, dy);
    if (scale !== 1 && scale > 0) this.ctx.scale(scale, scale);
  }

  popTransform() {
    if (!this.ctx) return;
    this.ctx.restore();
  }

  // Push a rectangular clip. Subsequent paint ops are masked to the
  // intersection of all active clips. `popClip` restores the previous
  // mask. Implemented via the canvas state stack so the Activity's
  // push/pop discipline (the SDK enforces matched pairs in
  // `Canvas::push_clip`/`pop_clip`) maps directly to save/restore.
  pushClip(x, y, w, h) {
    if (!this.ctx) return;
    this.ctx.save();
    this.ctx.beginPath();
    this.ctx.rect(x, y, w, h);
    this.ctx.clip();
  }

  popClip() {
    if (!this.ctx) return;
    this.ctx.restore();
  }

  // Set `ctx.globalAlpha` to a value in `[0, 1]`. Animations use this
  // to fade a surface in O(1) instead of multiplying alpha into every
  // op's rgba: with `globalAlpha = a` the GPU multiplies during
  // composition, and a `save() / setGlobalAlpha / restore()` scope
  // resets it cleanly. The previous animation pipeline cloned the
  // activity's full ops vec and rewrote every color-bearing op's
  // alpha byte per frame; that's gone.
  setGlobalAlpha(alpha) {
    if (!this.ctx) return;
    // Clamp defensively — an animation that overshoots [0, 1] would
    // either wash out (>1) or invert (<0) on some browsers.
    this.ctx.globalAlpha = Math.max(0, Math.min(1, alpha));
  }

  // Stone 13.0prep C — drawable cache. The host populates this via
  // `registerDrawable(id, bitmap)` at install/boot time; `drawImage`
  // reads from it. Map<u32 id, ImageBitmap | OffscreenCanvas>.
  // Missing id (0 or unregistered) paints a magenta placeholder, same
  // fallback shape as missing `@color/`/`@string/`.
  //
  // Note on the per-(pkg,name) parallel cache: this map is keyed by
  // the package's OWN per-package drawable_id (1-based, sequential),
  // so two packages can collide. The foreign cache below
  // (`_foreignDrawables`) is the cross-package-safe path keyed by
  // `(package_id_str, name_str)`. `_loadDrawableBitmaps` populates
  // BOTH so a package's own draws still hit the fast `drawable_id`
  // path while cross-package draws (Shell rendering app icons) use
  // the `(pkg, name)` path.
  registerDrawable(id, source) {
    if (!this._drawables) this._drawables = new Map();
    this._drawables.set(id >>> 0, source);
  }

  forgetDrawable(id) {
    if (!this._drawables) return;
    this._drawables.delete(id >>> 0);
  }

  drawImage(drawableId, x, y, w, h) {
    if (!this.ctx) return;
    const source = this._drawables?.get(drawableId);
    if (source) {
      this.ctx.drawImage(source, x, y, w, h);
      return;
    }
    // Magenta placeholder for missing drawable. Matches the fallback
    // shape used by missing color / missing string lookups.
    this.ctx.fillStyle = "#ff00ff";
    this.ctx.fillRect(x, y, w, h);
  }

  // Foundation gap — foreign-drawable cache. Keyed by
  // `(packageId, drawableName)` so cross-package draws (Shell
  // rendering each installed app's launcher icon) can never collide
  // with one another. Sparse: only packages with `manifest.app.icon`
  // declared register entries here.
  registerForeignDrawable(packageId, name, source) {
    if (!this._foreignDrawables) this._foreignDrawables = new Map();
    let pkgMap = this._foreignDrawables.get(packageId);
    if (!pkgMap) {
      pkgMap = new Map();
      this._foreignDrawables.set(packageId, pkgMap);
    }
    pkgMap.set(name, source);
  }

  /** Drop every entry for `packageId`. Called on uninstall. */
  forgetForeignDrawables(packageId) {
    if (!this._foreignDrawables) return;
    this._foreignDrawables.delete(packageId);
  }

  drawForeignImage(packageId, name, x, y, w, h) {
    if (!this.ctx) return;
    const source = this._foreignDrawables?.get(packageId)?.get(name);
    if (source) {
      this.ctx.drawImage(source, x, y, w, h);
      return;
    }
    // Same magenta-placeholder fallback as `drawImage`.
    this.ctx.fillStyle = "#ff00ff";
    this.ctx.fillRect(x, y, w, h);
  }

  present() {
    // Canvas2D commits implicitly at end of task. OffscreenCanvas on some
    // browsers requires explicit `commit()`, but that API is deprecated.
    // No-op.
  }

  // ── Phase 15.1 — per-Activity surface compositor ─────────────────────
  //
  // Frame shape from the kernel for each rendered display:
  //   for each surface S in z-order:
  //     beginSurface(id, x, y, w, h)
  //       ...S's paint ops (fillRect, drawText, …) routed to S's ctx
  //     endSurface()
  //   for each surface S in z-order:
  //     compositeSurface(id, x, y, w, h)   // drawImage S onto screen
  //   trailing destroySurfaceCanvas(id) for surfaces released since the
  //   last frame
  //
  // Zero-bounds (w == 0 || h == 0) is the legacy DEFAULT_SURFACE
  // sentinel — the renderer paints straight to the screen with no
  // offscreen allocation, and the matching compositeSurface is a no-op.
  // This keeps single-canvas test fixtures and pre-Phase-15 callers
  // working.

  /** Switch the renderer's paint target to surface `id`. Allocates a
   *  backing OffscreenCanvas of `(w, h)` if first sighting; resizes
   *  the existing one if `(w, h)` changed. The canvas is cleared on
   *  every begin so an Activity that paints with a transparent
   *  background gets crisp compositing without ghosting from the
   *  previous frame. */
  beginSurface(id, x, y, w, h) {
    if (!this.screenCtx) return;
    // Zero-bounds sentinel: keep painting to the screen ctx.
    if (w <= 0 || h <= 0) {
      this._ctxStack.push(this.ctx);
      this.ctx = this.screenCtx;
      return;
    }
    let entry = this._surfaces.get(id);
    if (!entry) {
      const canvas = new OffscreenCanvas(Math.max(1, w | 0), Math.max(1, h | 0));
      const ctx = canvas.getContext("2d", { alpha: true });
      if (!ctx) {
        // Highly unlikely on a fresh OffscreenCanvas; degrade to the
        // screen ctx so we never leave `this.ctx` pointing at null.
        this._ctxStack.push(this.ctx);
        this.ctx = this.screenCtx;
        return;
      }
      ctx.imageSmoothingEnabled = true;
      ctx.textBaseline = "top";
      entry = { canvas, ctx, w: canvas.width, h: canvas.height };
      this._surfaces.set(id, entry);
    } else if (entry.w !== (w | 0) || entry.h !== (h | 0)) {
      // Bounds changed → resize. Resizing an OffscreenCanvas wipes its
      // content, so a moved/resized surface naturally gets a fresh
      // start. Re-apply ctx defaults (resize resets the state machine).
      entry.canvas.width = Math.max(1, w | 0);
      entry.canvas.height = Math.max(1, h | 0);
      entry.w = entry.canvas.width;
      entry.h = entry.canvas.height;
      entry.ctx.imageSmoothingEnabled = true;
      entry.ctx.textBaseline = "top";
    } else {
      // Same size — clear before re-paint. Activity ops are always a
      // full re-paint (compositor::begin_frame clears their FrameCommands
      // each frame), so we can't preserve the prior pixels and rely on
      // them as a damage-mask.
      entry.ctx.clearRect(0, 0, entry.w, entry.h);
    }
    this._ctxStack.push(this.ctx);
    this.ctx = entry.ctx;
  }

  /** Restore the renderer's paint target to whatever it was before the
   *  most recent unmatched `beginSurface`. Pops the ctx stack; if the
   *  stack is empty (unmatched), reverts to the screen ctx. */
  endSurface() {
    if (!this.screenCtx) return;
    this.ctx = this._ctxStack.pop() ?? this.screenCtx;
  }

  /** Sample surface `id`'s backing canvas onto the screen at
   *  `(x, y, w, h)`. Always paints to the screen ctx — composite is
   *  the explicit "this is what reaches the display" step. Unknown
   *  ids and zero-bounds are no-ops (zero-bounds is the legacy
   *  DEFAULT_SURFACE sentinel — nothing was offscreen to composite). */
  compositeSurface(id, x, y, w, h) {
    if (!this.screenCtx) return;
    if (w <= 0 || h <= 0) return;
    const entry = this._surfaces.get(id);
    if (!entry) return;
    // drawImage from one OffscreenCanvas into another is GPU-accelerated
    // in every modern browser — same code path as `drawImage` from an
    // ImageBitmap, no readback, no copy.
    this.screenCtx.drawImage(entry.canvas, x, y, w, h);
  }

  /** Free the backing canvas for surface `id`. Idempotent — destroying
   *  a surface that was never allocated is a silent no-op (some test
   *  fixtures destroy surfaces that never rendered). */
  destroySurfaceCanvas(id) {
    this._surfaces.delete(id);
  }

  destroy() {
    this.ctx = null;
    this.screenCtx = null;
    this.screenCanvas = null;
    this._surfaces.clear();
    this._ctxStack.length = 0;
  }
}

function rgbaToCss(u32) {
  // u32 is AARRGGBB.
  const a = ((u32 >>> 24) & 0xff) / 255;
  const r = (u32 >>> 16) & 0xff;
  const g = (u32 >>> 8) & 0xff;
  const b = u32 & 0xff;
  return `rgba(${r},${g},${b},${a})`;
}
