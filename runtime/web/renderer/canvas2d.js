// Canvas2D renderer backend.
//
// Implements the RendererBackend trait from ARCHITECTURE.md §10 against
// an OffscreenCanvas 2D context. Phase 1a uses this directly; Phase 2
// adds a WebGPU backend and a feature-detection shim that picks the best
// available option at boot.

export class Canvas2DBackend {
  constructor() {
    /** @type {OffscreenCanvas|null} */
    this.canvas = null;
    /** @type {OffscreenCanvasRenderingContext2D|null} */
    this.ctx = null;
    this.metrics = { width_px: 0, height_px: 0, density: 1 };
  }

  init(canvas, metrics) {
    this.canvas = canvas;
    this.metrics = metrics;
    this.ctx = canvas.getContext("2d", { alpha: false });
    if (!this.ctx) throw new Error("Canvas2D context unavailable");
    this.ctx.imageSmoothingEnabled = true;
    this.ctx.textBaseline = "top";
  }

  resize(width_px, height_px, density) {
    if (!this.canvas) return;
    this.canvas.width = width_px;
    this.canvas.height = height_px;
    this.metrics = { width_px, height_px, density };
    // The context is reset when the canvas is resized; re-apply defaults.
    if (this.ctx) {
      this.ctx.textBaseline = "top";
      this.ctx.imageSmoothingEnabled = true;
    }
  }

  clear(rgba) {
    if (!this.ctx) return;
    this.ctx.fillStyle = rgbaToCss(rgba);
    this.ctx.fillRect(0, 0, this.canvas.width, this.canvas.height);
  }

  fillRect(x, y, w, h, rgba) {
    if (!this.ctx) return;
    this.ctx.fillStyle = rgbaToCss(rgba);
    this.ctx.fillRect(x, y, w, h);
  }

  drawText(x, y, size_px, rgba, text) {
    if (!this.ctx) return;
    this.ctx.fillStyle = rgbaToCss(rgba);
    this.ctx.font = `${size_px}px system-ui, -apple-system, sans-serif`;
    this.ctx.fillText(text, x, y);
  }

  present() {
    // Canvas2D commits implicitly at end of task. OffscreenCanvas on some
    // browsers requires explicit `commit()`, but that API is deprecated.
    // No-op.
  }

  destroy() {
    this.ctx = null;
    this.canvas = null;
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
