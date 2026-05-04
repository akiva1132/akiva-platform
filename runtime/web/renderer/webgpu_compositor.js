// WebGPU compositor backend.
//
// Phase 15.4.2 — identity composite + SurfaceDrawOpts (opacity,
// translate, scale). Each `compositeSurface` copies the source
// `OffscreenCanvas` into a cached per-source `GPUTexture` (path B
// per the plan §1.5) and emits a textured quad at the destination
// rect, transformed by the opts in the vertex shader and alpha-
// multiplied in the fragment shader. Blur / shadow / tint are 15.4.3+.
//
// Why path B and not `importExternalTexture`: as of mid-2026, Chrome /
// Edge / Firefox restrict `importExternalTexture` to
// `HTMLVideoElement` and `VideoFrame` sources. `OffscreenCanvas`
// throws "The provided value is not of type '(HTMLVideoElement or
// VideoFrame)'". Path B (`copyExternalImageToTexture`) is universal at
// the cost of one canvas-pixels → texture copy per surface per frame
// (~4 surfaces × 1080p × 4 bytes × 60 fps ≈ 2 GB/s of memcpy on a
// dGPU; well within bandwidth on every modern machine).
//
// Frame model: `beginFrame` opens a render pass against the swapchain
// texture; each `compositeSurface` enqueues a copy + encodes a draw;
// `endFrame` (called via `present`) ends the pass and submits. The
// pipeline triggers `beginFrame` lazily on the first
// `compositeSurface` per frame, so a frame with no composites is a
// no-op (no submit, no presented texture acquired).

// Embedded WGSL — keeping it in-source avoids a separate fetch path
// and makes the shader visible alongside its bind-group layout. The
// canonical copy lives at `runtime/web/renderer/wgsl/identity.wgsl`;
// regenerating from there is a manual step (no build pipeline yet).
const IDENTITY_WGSL = `
struct Uniforms {
  dst_rect:    vec4<f32>,
  screen_size: vec4<f32>,
  // opts: (opacity, translate_x, translate_y, scale)
  opts:        vec4<f32>,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var s: sampler;
@group(0) @binding(2) var t: texture_2d<f32>;

struct VsOut {
  @builtin(position) clip: vec4<f32>,
  @location(0) uv: vec2<f32>,
};

@vertex
fn vs(@builtin(vertex_index) vid: u32) -> VsOut {
  let corner = vec2<f32>(f32(vid & 1u), f32((vid >> 1u) & 1u));
  // Scale around the dst-rect centre, then translate. Order matches
  // the Canvas2D path's save → translate → scale → drawImage so
  // slide/fade animations look identical between backends.
  let scaled_size = u.dst_rect.zw * u.opts.w;
  let pivot       = u.dst_rect.xy + u.dst_rect.zw * 0.5;
  let pixel       = pivot
                  + (corner - vec2<f32>(0.5, 0.5)) * scaled_size
                  + u.opts.yz;
  let ndc = vec2<f32>(
    pixel.x / u.screen_size.x * 2.0 - 1.0,
    1.0 - pixel.y / u.screen_size.y * 2.0,
  );
  var o: VsOut;
  o.clip = vec4<f32>(ndc, 0.0, 1.0);
  o.uv   = corner;
  return o;
}

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
  let c = textureSample(t, s, in.uv);
  // Pre-multiply on the way out — the canvas configuration uses
  // alphaMode "premultiplied" but our source canvases are straight
  // alpha. Effective alpha = sampled alpha × surface opacity.
  // (15.4.5+ effect mixing happens BEFORE this multiply.)
  let a = c.a * u.opts.x;
  return vec4<f32>(c.rgb * a, a);
}
`;

export class WebGPUCompositor {
  constructor() {
    this.screenCanvas = null;
    /** @type {GPUCanvasContext|null} */
    this.ctx = null;
    /** @type {GPUAdapter|null} */
    this.adapter = null;
    /** @type {GPUDevice|null} */
    this.device = null;
    this.format = null;
    /** @type {GPURenderPipeline|null} */
    this.pipeline = null;
    /** @type {GPUSampler|null} */
    this.sampler = null;
    /** @type {GPUBindGroupLayout|null} */
    this.bindGroupLayout = null;
    this.metrics = { width_px: 0, height_px: 0, density: 1 };
    /** @type {GPUCommandEncoder|null} */
    this._encoder = null;
    /** @type {GPURenderPassEncoder|null} */
    this._pass = null;
    this._frameInProgress = false;
    this._lost = false;
    /** Per-source GPUTexture cache. Keyed by the source canvas object
     *  (paint backend keeps the offscreens alive between frames, so
     *  the WeakMap keys stay valid). Each entry is
     *  `{ texture, w, h }`; we recreate when `(w, h)` changes.
     *  @type {WeakMap<object, {texture: GPUTexture, w: number, h: number}>} */
    this._textureCache = new WeakMap();
  }

  static async isAvailable() {
    return typeof navigator !== "undefined" && !!navigator.gpu;
  }

  async init(canvas, metrics) {
    if (!(await WebGPUCompositor.isAvailable())) throw new Error("WebGPU unavailable");
    this.adapter = await navigator.gpu.requestAdapter();
    if (!this.adapter) throw new Error("requestAdapter returned null");
    this.device = await this.adapter.requestDevice();
    // Mark device as lost asynchronously — subsequent compositeSurface
    // calls become silent no-ops. The pipeline doesn't currently
    // hot-swap to Canvas2D on loss; that's a follow-up.
    this.device.lost.then((info) => {
      console.warn("[akiva] WebGPU device lost:", info?.message ?? info);
      this._lost = true;
    });
    this.format = navigator.gpu.getPreferredCanvasFormat();
    this.ctx = canvas.getContext("webgpu");
    if (!this.ctx) throw new Error("getContext('webgpu') returned null");
    // `alphaMode: "premultiplied"` — the shader pre-multiplies on the
    // way out so straight-alpha source canvases composite correctly
    // against this configuration.
    this.ctx.configure({ device: this.device, format: this.format, alphaMode: "premultiplied" });

    this.screenCanvas = canvas;
    this.metrics = metrics;

    const module = this.device.createShaderModule({ code: IDENTITY_WGSL });

    this.bindGroupLayout = this.device.createBindGroupLayout({
      entries: [
        { binding: 0, visibility: GPUShaderStage.VERTEX | GPUShaderStage.FRAGMENT, buffer: { type: "uniform" } },
        { binding: 1, visibility: GPUShaderStage.FRAGMENT, sampler: {} },
        { binding: 2, visibility: GPUShaderStage.FRAGMENT, texture: { sampleType: "float" } },
      ],
    });

    this.pipeline = this.device.createRenderPipeline({
      layout: this.device.createPipelineLayout({ bindGroupLayouts: [this.bindGroupLayout] }),
      vertex:   { module, entryPoint: "vs" },
      fragment: {
        module, entryPoint: "fs",
        targets: [{
          format: this.format,
          // Standard "over" blending against the existing screen pixels.
          // Surface ops are emitted in z-order so each composite blends
          // on top of the prior one this frame.
          blend: {
            color: { srcFactor: "one", dstFactor: "one-minus-src-alpha", operation: "add" },
            alpha: { srcFactor: "one", dstFactor: "one-minus-src-alpha", operation: "add" },
          },
        }],
      },
      primitive: { topology: "triangle-strip" },
    });

    this.sampler = this.device.createSampler({
      magFilter: "linear", minFilter: "linear",
      addressModeU: "clamp-to-edge", addressModeV: "clamp-to-edge",
    });
  }

  /** Mirror Canvas2DCompositor's API — paint backend asks for a screen
   *  ctx in WebGPU mode and gets `null`, since the canvas is bound to
   *  `"webgpu"`. The legacy DEFAULT_SURFACE / zero-bounds sentinel
   *  becomes a silent no-op in this mode (in production the framework
   *  always emits real `BeginSurface` bounds). */
  getScreenCtx() { return null; }

  resize(width_px, height_px, density) {
    if (!this.screenCanvas) return;
    this.screenCanvas.width = width_px;
    this.screenCanvas.height = height_px;
    this.metrics = { width_px, height_px, density };
    // The swapchain auto-resizes to match the canvas pixel dimensions on
    // the next `getCurrentTexture()`. No explicit reconfigure needed.
  }

  beginFrame() {
    if (!this.device || !this.ctx || this._lost) return;
    if (this._frameInProgress) return;
    let view;
    try {
      view = this.ctx.getCurrentTexture().createView();
    } catch (err) {
      // Browsers throw if the canvas hasn't been configured yet or has
      // a degenerate size. Skip the frame; the next rAF tick re-tries.
      console.warn("[akiva] WebGPU getCurrentTexture failed:", err);
      return;
    }
    this._encoder = this.device.createCommandEncoder();
    this._pass = this._encoder.beginRenderPass({
      colorAttachments: [{
        view,
        loadOp: "clear",
        // Black background. The wallpaper / primary activity covers
        // the whole screen each frame, so this is rarely visible; if
        // it is, it's the safe choice (won't smear last-frame pixels).
        clearValue: { r: 0, g: 0, b: 0, a: 1 },
        storeOp: "store",
      }],
    });
    this._pass.setPipeline(this.pipeline);
    this._frameInProgress = true;
  }

  endFrame() {
    if (!this._frameInProgress) return;
    this._pass.end();
    this.device.queue.submit([this._encoder.finish()]);
    this._pass = null;
    this._encoder = null;
    this._frameInProgress = false;
  }

  /** Composite `src` (an OffscreenCanvas allocated by the paint
   *  backend) onto the screen at `(x, y, w, h)`, transformed by the
   *  SurfaceDrawOpts (`opacity`, `translateX`, `translateY`, `scale`).
   *  Defaults match `SurfaceDrawOpts::IDENTITY`: opacity=1, no
   *  translate, scale=1. */
  compositeSurface(src, x, y, w, h, opacity = 1, translateX = 0, translateY = 0, scale = 1) {
    if (!this.device || this._lost || !src || w <= 0 || h <= 0) return;
    if (!this._frameInProgress) {
      this.beginFrame();
      if (!this._frameInProgress) return;  // beginFrame failed
    }

    // Look up or allocate a GPUTexture matching the source's current
    // pixel dimensions. Source canvases are stable across frames
    // (paint backend reuses them), so the WeakMap keys persist; the
    // texture is recreated only when the activity resizes its surface.
    const srcW = Math.max(1, src.width  | 0);
    const srcH = Math.max(1, src.height | 0);
    let entry = this._textureCache.get(src);
    if (!entry || entry.w !== srcW || entry.h !== srcH) {
      if (entry) entry.texture.destroy();
      const texture = this.device.createTexture({
        size:   [srcW, srcH, 1],
        format: "rgba8unorm",
        usage:  GPUTextureUsage.TEXTURE_BINDING
              | GPUTextureUsage.COPY_DST
              | GPUTextureUsage.RENDER_ATTACHMENT,
      });
      entry = { texture, w: srcW, h: srcH, view: texture.createView() };
      this._textureCache.set(src, entry);
    }

    // Snapshot the canvas pixels into the texture. Synchronous from
    // the JS side; the GPU sees the copy in queue order (before the
    // encoder we're about to submit).
    try {
      this.device.queue.copyExternalImageToTexture(
        { source: src },
        { texture: entry.texture },
        [srcW, srcH, 1],
      );
    } catch (err) {
      console.warn("[akiva] copyExternalImageToTexture failed:", err);
      return;
    }

    // Per-call uniform buffer (48 bytes = 3 × vec4<f32>). We can't
    // reuse one buffer across multiple composites in the same
    // submission — `writeBuffer` ordering vs. `draw` ordering isn't
    // preserved within a single submit, so a shared buffer would
    // deliver the LAST frame's uniforms to every draw. A small
    // per-call allocation is correct and the GPU/driver pools small
    // uniforms efficiently.
    const uniforms = new Float32Array(12);
    // dst_rect
    uniforms[0] = x; uniforms[1] = y; uniforms[2] = w; uniforms[3] = h;
    // screen_size
    uniforms[4] = this.metrics.width_px;
    uniforms[5] = this.metrics.height_px;
    // opts (opacity, translate_x, translate_y, scale) — clamp opacity
    // defensively to match the Canvas2D path's `setGlobalAlpha`
    // clamping; an animation that overshoots [0,1] otherwise washes
    // out (>1) or inverts (<0) on some GPUs.
    uniforms[8]  = Math.max(0, Math.min(1, opacity));
    uniforms[9]  = translateX;
    uniforms[10] = translateY;
    uniforms[11] = scale > 0 ? scale : 1;
    const uniformBuffer = this.device.createBuffer({
      size: 48,
      usage: GPUBufferUsage.UNIFORM | GPUBufferUsage.COPY_DST,
    });
    this.device.queue.writeBuffer(uniformBuffer, 0, uniforms);

    const bindGroup = this.device.createBindGroup({
      layout: this.bindGroupLayout,
      entries: [
        { binding: 0, resource: { buffer: uniformBuffer } },
        { binding: 1, resource: this.sampler },
        { binding: 2, resource: entry.view },
      ],
    });
    this._pass.setBindGroup(0, bindGroup);
    this._pass.draw(4);
  }

  present() { this.endFrame(); }

  destroy() {
    if (this._frameInProgress) this.endFrame();
    this.pipeline = null;
    this.sampler = null;
    this.bindGroupLayout = null;
    this.ctx = null;
    this.device = null;
    this.adapter = null;
    this.screenCanvas = null;
    // _textureCache is a WeakMap; entries get GC'd when their source
    // canvases do. We don't `destroy()` each texture explicitly —
    // device teardown reclaims them.
  }
}
