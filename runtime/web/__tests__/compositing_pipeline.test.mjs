// Compositing pipeline — routing layer between the kernel HAL and the
// paint + compositor backends.
//
// Phase 15.4.1: assert that each method lands on the right backend.
// WebGPU rendering itself isn't unit-testable (no GPU in vitest-node),
// so the WebGPU compositor is exercised here only via mock factories;
// real WebGPU output is verified by browser smoke per the plan.

import { describe, it, expect, beforeEach } from "vitest";
import { CompositingPipeline } from "../renderer/compositing_pipeline.js";

/** Minimal mock 2D context: records every call to a flat trace.
 *  Mirrors the shape used by `canvas2d_paintops.test.mjs`. */
function makeMockContext() {
  const trace = [];
  const ctx = {
    set fillStyle(v)   { trace.push(["set:fillStyle", v]); },
    set strokeStyle(v) { trace.push(["set:strokeStyle", v]); },
    set lineWidth(v)   { trace.push(["set:lineWidth", v]); },
    set lineCap(v)     { trace.push(["set:lineCap", v]); },
    set font(v)        { trace.push(["set:font", v]); },
    set textBaseline(v){ trace.push(["set:textBaseline", v]); },
    set imageSmoothingEnabled(v) { trace.push(["set:imageSmoothingEnabled", v]); },
    set globalAlpha(v) { trace.push(["set:globalAlpha", v]); },
    fillRect:   (...a) => trace.push(["fillRect",   ...a]),
    clearRect:  (...a) => trace.push(["clearRect",  ...a]),
    strokeRect: (...a) => trace.push(["strokeRect", ...a]),
    fillText:   (...a) => trace.push(["fillText",   ...a]),
    beginPath:  ()     => trace.push(["beginPath"]),
    roundRect:  (...a) => trace.push(["roundRect",  ...a]),
    moveTo:     (...a) => trace.push(["moveTo",     ...a]),
    lineTo:     (...a) => trace.push(["lineTo",     ...a]),
    fill:       ()     => trace.push(["fill"]),
    stroke:     ()     => trace.push(["stroke"]),
    save:       ()     => trace.push(["save"]),
    restore:    ()     => trace.push(["restore"]),
    translate:  (...a) => trace.push(["translate",  ...a]),
    scale:      (...a) => trace.push(["scale",      ...a]),
    rect:       (...a) => trace.push(["rect",       ...a]),
    clip:       ()     => trace.push(["clip"]),
    drawImage:  (...a) => trace.push(["drawImage",  a[0], ...a.slice(1)]),
  };
  return { ctx, trace };
}

function makeMockSurface(width = 800, height = 600) {
  const { ctx, trace } = makeMockContext();
  const surface = {
    width, height,
    getContext: (type) => (type === "2d" ? ctx : null),
  };
  ctx.canvas = surface;
  return { surface, ctx, trace };
}

/** Install a global `OffscreenCanvas` stub so the paint backend can
 *  allocate per-Activity surfaces inside vitest-node. Returns a
 *  cleanup function that restores the prior global. */
function installOffscreenCanvasStub(allocated) {
  const prior = globalThis.OffscreenCanvas;
  globalThis.OffscreenCanvas = function MockOffscreenCanvas(w, h) {
    const { ctx, trace } = makeMockContext();
    const obj = {
      width: w, height: h,
      getContext: (type) => (type === "2d" ? ctx : null),
      _trace: trace,
    };
    ctx.canvas = obj;
    allocated.push(obj);
    return obj;
  };
  return () => { globalThis.OffscreenCanvas = prior; };
}

/** A WebGPU compositor mock. Records every call so the test can
 *  assert the pipeline routed `compositeSurface` here (not to the
 *  paint backend's own compositeSurface). */
class MockWebGPUCompositor {
  static available = true;
  static async isAvailable() { return MockWebGPUCompositor.available; }

  constructor() { this.calls = []; this.lastSrc = null; }
  async init(canvas, metrics) { this.calls.push(["init", canvas, metrics]); }
  resize(w, h, d) { this.calls.push(["resize", w, h, d]); }
  beginFrame() { this.calls.push(["beginFrame"]); }
  endFrame() { this.calls.push(["endFrame"]); }
  getScreenCtx() { return null; }
  compositeSurface(src, x, y, w, h, opacity, tx, ty, scale) {
    this.lastSrc = src;
    this.calls.push(["compositeSurface", src, x, y, w, h, opacity, tx, ty, scale]);
  }
  present() { this.calls.push(["present"]); }
  destroy() { this.calls.push(["destroy"]); }
}

class FailingWebGPUCompositor {
  static available = true;
  static async isAvailable() { return true; }
  async init() { throw new Error("simulated WebGPU init failure"); }
}

describe("CompositingPipeline — routing", () => {
  let pipeline, screen, allocated, restoreOC;

  beforeEach(async () => {
    allocated = [];
    restoreOC = installOffscreenCanvasStub(allocated);
    screen = makeMockSurface(800, 600);
    pipeline = new CompositingPipeline();
    await pipeline.init(screen.surface, { width_px: 800, height_px: 600, density: 1 }, {
      preferGPU: false,  // force Canvas2DCompositor
    });
  });

  it("falls back to Canvas2DCompositor when preferGPU is false", () => {
    expect(pipeline.compositorKind).toBe("canvas2d");
    restoreOC();
  });

  it("per-op paint forwards to the paint backend", () => {
    // beginSurface a per-Activity offscreen first; in production every
    // paint lives between BeginSurface/EndSurface markers.
    pipeline.beginSurface(7, 0, 0, 200, 100);
    pipeline.fillRect(10, 10, 50, 50, 0xff112233);
    pipeline.drawText(20, 30, 14, 0xffffffff, "hi");
    pipeline.endSurface();
    const surfaceTrace = allocated[0]._trace;
    expect(surfaceTrace).toContainEqual(["fillRect", 10, 10, 50, 50]);
    expect(surfaceTrace).toContainEqual(["fillText", "hi", 20, 30]);
    restoreOC();
  });

  it("compositeSurface looks up the source canvas via paint backend, then forwards to compositor", () => {
    pipeline.beginSurface(7, 0, 0, 200, 100);
    pipeline.endSurface();
    pipeline.compositeSurface(7, 0, 0, 200, 100, 1, 0, 0, 1);
    // Canvas2DCompositor wrote to the screen ctx — drawImage with
    // first arg == the offscreen canvas the paint backend allocated.
    const screenOps = screen.trace;
    const drawImage = screenOps.find(op => op[0] === "drawImage");
    expect(drawImage).toBeTruthy();
    expect(drawImage[1]).toBe(allocated[0]);
    restoreOC();
  });

  it("compositeSurface for an unknown id is a silent no-op", () => {
    pipeline.compositeSurface(999, 0, 0, 100, 100, 1, 0, 0, 1);
    const drawImage = screen.trace.find(op => op[0] === "drawImage");
    expect(drawImage).toBeUndefined();
    restoreOC();
  });

  it("compositeSurface with zero bounds is a silent no-op (DEFAULT_SURFACE sentinel)", () => {
    pipeline.beginSurface(7, 0, 0, 200, 100);
    pipeline.endSurface();
    pipeline.compositeSurface(7, 0, 0, 0, 0, 1, 0, 0, 1);
    const drawImage = screen.trace.find(op => op[0] === "drawImage");
    expect(drawImage).toBeUndefined();
    restoreOC();
  });

  it("destroySurfaceCanvas drops the entry; subsequent composite is a no-op", () => {
    pipeline.beginSurface(7, 0, 0, 200, 100);
    pipeline.endSurface();
    pipeline.destroySurfaceCanvas(7);
    pipeline.compositeSurface(7, 0, 0, 200, 100, 1, 0, 0, 1);
    const drawImage = screen.trace.find(op => op[0] === "drawImage");
    expect(drawImage).toBeUndefined();
    restoreOC();
  });

  it("resize forwards to both backends", () => {
    pipeline.resize(400, 300, 2);
    expect(pipeline.metrics).toEqual({ width_px: 400, height_px: 300, density: 2 });
    // Screen canvas got resized (compositor side).
    expect(screen.surface.width).toBe(400);
    expect(screen.surface.height).toBe(300);
    restoreOC();
  });

  it("registerDrawable / drawImage round-trip via paint backend", () => {
    const fakeBitmap = { _bitmap: true };
    pipeline.registerDrawable(7, fakeBitmap);
    pipeline.beginSurface(1, 0, 0, 100, 100);
    pipeline.drawImage(7, 0, 0, 50, 50);
    pipeline.endSurface();
    const surfaceTrace = allocated[0]._trace;
    expect(surfaceTrace).toContainEqual(["drawImage", fakeBitmap, 0, 0, 50, 50]);
    restoreOC();
  });
});

describe("CompositingPipeline — backend selection", () => {
  let allocated, restoreOC;
  beforeEach(() => {
    allocated = [];
    restoreOC = installOffscreenCanvasStub(allocated);
  });

  it("picks WebGPU when factory.isAvailable() resolves true and init succeeds", async () => {
    const screen = makeMockSurface(800, 600);
    const pipeline = new CompositingPipeline();
    await pipeline.init(screen.surface, { width_px: 800, height_px: 600, density: 1 }, {
      preferGPU: true,
      compositorFactories: {
        webgpu:   MockWebGPUCompositor,
        canvas2d: (await import("../renderer/canvas2d_compositor.js")).Canvas2DCompositor,
      },
    });
    expect(pipeline.compositorKind).toBe("webgpu");
    restoreOC();
  });

  it("falls back to Canvas2DCompositor when WebGPU init throws", async () => {
    const screen = makeMockSurface(800, 600);
    const pipeline = new CompositingPipeline();
    await pipeline.init(screen.surface, { width_px: 800, height_px: 600, density: 1 }, {
      preferGPU: true,
      compositorFactories: {
        webgpu:   FailingWebGPUCompositor,
        canvas2d: (await import("../renderer/canvas2d_compositor.js")).Canvas2DCompositor,
      },
    });
    expect(pipeline.compositorKind).toBe("canvas2d");
    restoreOC();
  });

  it("compositeSurface routes to the WebGPU mock, not the paint backend's compositeSurface", async () => {
    const screen = makeMockSurface(800, 600);
    const pipeline = new CompositingPipeline();
    await pipeline.init(screen.surface, { width_px: 800, height_px: 600, density: 1 }, {
      preferGPU: true,
      compositorFactories: {
        webgpu:   MockWebGPUCompositor,
        canvas2d: (await import("../renderer/canvas2d_compositor.js")).Canvas2DCompositor,
      },
    });
    pipeline.beginSurface(7, 0, 0, 200, 100);
    pipeline.endSurface();
    pipeline.compositeSurface(7, 0, 0, 200, 100, 0.5, 10, 0, 1);
    const compositor = pipeline.compositor;
    // The mock recorded the call with the offscreen canvas as src.
    const compositeCall = compositor.calls.find(c => c[0] === "compositeSurface");
    expect(compositeCall).toBeTruthy();
    expect(compositeCall[1]).toBe(allocated[0]);  // src
    expect(compositeCall.slice(2)).toEqual([0, 0, 200, 100, 0.5, 10, 0, 1]);
    // Screen ctx didn't see drawImage (the WebGPU mock doesn't touch
    // screen.trace). This proves the routing didn't double-fire.
    expect(screen.trace.find(op => op[0] === "drawImage")).toBeUndefined();
    restoreOC();
  });

  it("beginSurface triggers compositor.beginFrame (lazy frame lifecycle)", async () => {
    const screen = makeMockSurface(800, 600);
    const pipeline = new CompositingPipeline();
    await pipeline.init(screen.surface, { width_px: 800, height_px: 600, density: 1 }, {
      preferGPU: true,
      compositorFactories: {
        webgpu:   MockWebGPUCompositor,
        canvas2d: (await import("../renderer/canvas2d_compositor.js")).Canvas2DCompositor,
      },
    });
    pipeline.beginSurface(7, 0, 0, 100, 100);
    const compositor = pipeline.compositor;
    expect(compositor.calls.find(c => c[0] === "beginFrame")).toBeTruthy();
    pipeline.endSurface();
    pipeline.present();
    expect(compositor.calls.find(c => c[0] === "endFrame")).toBeTruthy();
    restoreOC();
  });
});
