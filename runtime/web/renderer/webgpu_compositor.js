// WebGPU compositor backend.
//
// Phase 15.4.1 — minimum viable. Identity composite only: each
// `compositeSurface` samples the source `OffscreenCanvas` as an
// external texture and emits a textured quad at the destination rect.
// SurfaceDrawOpts (`opacity`, `translate_x`, `translate_y`, `scale`)
// are accepted but ignored — that's 15.4.2's stone. Blur, shadow, tint
// are 15.4.3+.
//
// Frame model: `beginFrame` opens a render pass against the swapchain
// texture; each `compositeSurface` encodes a draw into it; `endFrame`
// (called via `present`) ends the pass and submits. The pipeline
// triggers `beginFrame` lazily on the first `compositeSurface` per
// frame, so a frame with no composites is a no-op (no submit, no
// presented texture acquired).
//
// Browser support: Chrome ≥ 113, Edge ≥ 113, Firefox ≥ 147, Safari
// ≥ 18. Init throws on missing `navigator.gpu`, missing adapter, or
// `getContext("webgpu")` returning null. The pipeline catches and
// falls back to `Canvas2DCompositor`.

// Embedded WGSL — keeping it in-source avoids a separate fetch path
// and makes the shader visible alongside its bind-group layout. The
// canonical copy lives at `runtime/web/renderer/wgsl/identity.wgsl`;
// regenerating from there is a manual step (no build pipeline yet).
const IDENTITY_WGSL = `
struct Uniforms {
  dst_rect:    vec4<f32>,
  screen_size: vec4<f32>,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var s: sampler;
@group(0) @binding(2) var t: texture_external;

struct VsOut {
  @builtin(position) clip: vec4<f32>,
  @location(0) uv: vec2<f32>,
};

@vertex
fn vs(@builtin(vertex_index) vid: u32) -> VsOut {
  let corner = vec2<f32>(f32(vid & 1u), f32((vid >> 1u) & 1u));
  let pixel  = u.dst_rect.xy + corner * u.dst_rect.zw;
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
  let c = textureSampleBaseClampToEdge(t, s, in.uv);
  return vec4<f32>(c.rgb * c.a, c.a);
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
    /** @type {GPUBuffer|null} */
    this.uniformBuffer = null;
    /** @type {GPUBindGroupLayout|null} */
    this.bindGroupLayout = null;
    this.metrics = { width_px: 0, height_px: 0, density: 1 };
    /** @type {GPUCommandEncoder|null} */
    this._encoder = null;
    /** @type {GPURenderPassEncoder|null} */
    this._pass = null;
    this._frameInProgress = false;
    this._lost = false;
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
        { binding: 2, visibility: GPUShaderStage.FRAGMENT, externalTexture: {} },
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

    // Two vec4<f32> = 32 bytes. 16-byte aligned per WGSL std140-ish
    // uniform layout rules.
    this.uniformBuffer = this.device.createBuffer({
      size: 32,
      usage: GPUBufferUsage.UNIFORM | GPUBufferUsage.COPY_DST,
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
   *  backend) onto the screen at `(x, y, w, h)`. Opts ignored in
   *  15.4.1 — they land in 15.4.2 by extending the uniform layout
   *  and the vertex shader. */
  compositeSurface(src, x, y, w, h, _opacity, _tx, _ty, _scale) {
    if (!this.device || this._lost || !src || w <= 0 || h <= 0) return;
    if (!this._frameInProgress) {
      this.beginFrame();
      if (!this._frameInProgress) return;  // beginFrame failed
    }
    // Uniform: dst_rect (vec4) + screen_size (vec4).
    const uniforms = new Float32Array(8);
    uniforms[0] = x; uniforms[1] = y; uniforms[2] = w; uniforms[3] = h;
    uniforms[4] = this.metrics.width_px; uniforms[5] = this.metrics.height_px;
    this.device.queue.writeBuffer(this.uniformBuffer, 0, uniforms);

    let texture;
    try {
      // Path A — zero-copy import of the OffscreenCanvas. If this
      // throws on a particular browser we'd ideally fall back to
      // `copyExternalImageToTexture`, but as of 15.4.1 every
      // browser that exposes WebGPU also exposes
      // importExternalTexture for OffscreenCanvas — keep the
      // fallback as a 15.4.x follow-up rather than guessing.
      texture = this.device.importExternalTexture({ source: src });
    } catch (err) {
      console.warn("[akiva] importExternalTexture failed:", err);
      return;
    }

    const bindGroup = this.device.createBindGroup({
      layout: this.bindGroupLayout,
      entries: [
        { binding: 0, resource: { buffer: this.uniformBuffer } },
        { binding: 1, resource: this.sampler },
        { binding: 2, resource: texture },
      ],
    });
    this._pass.setBindGroup(0, bindGroup);
    this._pass.draw(4);
  }

  present() { this.endFrame(); }

  destroy() {
    if (this._frameInProgress) this.endFrame();
    this.uniformBuffer = null;
    this.pipeline = null;
    this.sampler = null;
    this.bindGroupLayout = null;
    this.ctx = null;
    this.device = null;
    this.adapter = null;
    this.screenCanvas = null;
  }
}
