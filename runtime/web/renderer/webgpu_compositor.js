// WebGPU compositor backend.
//
// Phase 15.4.3 — backdrop blur (frosted glass) on top of 15.4.1's
// identity composite + 15.4.2's translate/scale/opacity. Each
// `compositeSurface` copies the source `OffscreenCanvas` into a
// cached per-source `GPUTexture` (path B per the plan §1.5) and
// emits a textured quad at the destination rect, transformed by the
// opts in the vertex shader and alpha-multiplied in the fragment
// shader. When `backdrop_blur_radius > 0`, the compositor first
// downsamples the scene texture and runs a separable Gaussian
// (two passes), then composites the blurred result UNDER the surface
// at the surface's bounds — sheets / dialogs see live frosted glass
// of whatever's behind them.
//
// Why path B and not `importExternalTexture`: as of mid-2026, Chrome /
// Edge / Firefox restrict `importExternalTexture` to
// `HTMLVideoElement` and `VideoFrame` sources. `OffscreenCanvas`
// throws "The provided value is not of type '(HTMLVideoElement or
// VideoFrame)'". Path B (`copyExternalImageToTexture`) is universal at
// the cost of one canvas-pixels → texture copy per surface per frame.
//
// Frame model: `beginFrame` opens a render pass against an INTERNAL
// scene texture (full screen size). Per-surface composites encode
// draws into that pass. When a blur is needed we pause the scene
// pass, run downsample + blurH + blurV passes against ping-pong
// scratch textures (1/4 res), resume the scene pass, composite the
// blurred output back at surface bounds, then composite the surface
// itself on top. `endFrame` ends the scene pass, encodes a final
// scene→swapchain blit, and submits.
//
// Memory: one screen-sized RGBA8 scene texture (~8 MB at 1080p) plus
// two 1/4-res scratch textures (~1 MB each). Negligible on any GPU
// that ships in 2024+.

// Embedded WGSL — keeping it in-source avoids a separate fetch path
// and makes the shader visible alongside its bind-group layout. The
// canonical copies live at `runtime/web/renderer/wgsl/identity.wgsl`
// and `blur.wgsl`; regenerating from there is a manual step (no
// build pipeline yet).
//
// Both shaders share the same 80-byte `Uniforms` struct so a single
// uniform-buffer shape covers every draw. Only the pipeline (and
// thus entry points) differs between identity and blur.

const SHARED_UNIFORM_PREFIX = `
struct Uniforms {
  dst_rect:    vec4<f32>,
  target_size: vec4<f32>,
  opts:        vec4<f32>,
  src_uv:      vec4<f32>,
  blur_dir:    vec4<f32>,
};
@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var s: sampler;
@group(0) @binding(2) var t: texture_2d<f32>;

struct VsOut {
  @builtin(position) clip: vec4<f32>,
  @location(0) uv: vec2<f32>,
};
`;

const IDENTITY_WGSL = SHARED_UNIFORM_PREFIX + `
@vertex
fn vs(@builtin(vertex_index) vid: u32) -> VsOut {
  let corner = vec2<f32>(f32(vid & 1u), f32((vid >> 1u) & 1u));
  let scaled_size = u.dst_rect.zw * u.opts.w;
  let pivot       = u.dst_rect.xy + u.dst_rect.zw * 0.5;
  let pixel       = pivot
                  + (corner - vec2<f32>(0.5, 0.5)) * scaled_size
                  + u.opts.yz;
  let ndc = vec2<f32>(
    pixel.x / u.target_size.x * 2.0 - 1.0,
    1.0 - pixel.y / u.target_size.y * 2.0,
  );
  var o: VsOut;
  o.clip = vec4<f32>(ndc, 0.0, 1.0);
  o.uv   = u.src_uv.xy + corner * u.src_uv.zw;
  return o;
}

// Surface composite: source is straight alpha (Canvas2D), output
// is pre-multiplied (matches the swapchain's "premultiplied" alphaMode).
// Phase 15.4.5 — also blends a colour tint over the surface RGB.
// blur_dir.xyz is the tint colour in 0..1 sRGB; blur_dir.w is the
// tint alpha (mix factor). When tint alpha is 0, mix() returns the
// surface's own RGB unchanged — that's the fast path for the common
// "no tint" case.
@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
  let c = textureSample(t, s, in.uv);
  let tint_rgb  = u.blur_dir.xyz;
  let tint_a    = u.blur_dir.w;
  let mixed_rgb = mix(c.rgb, tint_rgb, tint_a);
  let a = c.a * u.opts.x;
  return vec4<f32>(mixed_rgb * a, a);
}

// Pre-multiplied passthrough: used for blits between intermediate
// targets that already hold pre-multiplied pixels (scene → swapchain,
// scene → scratch downsample, scratchA → scene as a blurred backdrop).
// Without this, the surface-composite shader would double-multiply
// alpha and produce a too-dark result.
@fragment
fn fs_blit(in: VsOut) -> @location(0) vec4<f32> {
  return textureSample(t, s, in.uv);
}

// Phase 15.4.4 — alpha-tint silhouette pass for drop shadow. Samples
// the surface texture (straight alpha), keeps only the alpha channel,
// and tints with the shadow colour packed in \`blur_dir.xyz\` (rgb in
// 0..1) and \`blur_dir.w\` (alpha 0..1). Output is pre-multiplied:
//     a = src.a * shadow.a
//     rgb = shadow.rgb * a
@fragment
fn fs_alpha_tint(in: VsOut) -> @location(0) vec4<f32> {
  let src_a   = textureSample(t, s, in.uv).a;
  let tint_a  = u.blur_dir.w;
  let a       = src_a * tint_a;
  return vec4<f32>(u.blur_dir.xyz * a, a);
}
`;

const BLUR_WGSL = SHARED_UNIFORM_PREFIX + `
@vertex
fn vs(@builtin(vertex_index) vid: u32) -> VsOut {
  let corner = vec2<f32>(f32(vid & 1u), f32((vid >> 1u) & 1u));
  let pixel  = u.dst_rect.xy + corner * u.dst_rect.zw;
  let ndc = vec2<f32>(
    pixel.x / u.target_size.x * 2.0 - 1.0,
    1.0 - pixel.y / u.target_size.y * 2.0,
  );
  var o: VsOut;
  o.clip = vec4<f32>(ndc, 0.0, 1.0);
  o.uv   = u.src_uv.xy + corner * u.src_uv.zw;
  return o;
}

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
  let d = u.blur_dir.xy;
  var c = vec4<f32>(0.0, 0.0, 0.0, 0.0);
  c = c + textureSample(t, s, in.uv - d * 4.0) * 0.016;
  c = c + textureSample(t, s, in.uv - d * 3.0) * 0.054;
  c = c + textureSample(t, s, in.uv - d * 2.0) * 0.121;
  c = c + textureSample(t, s, in.uv - d * 1.0) * 0.194;
  c = c + textureSample(t, s, in.uv          ) * 0.227;
  c = c + textureSample(t, s, in.uv + d * 1.0) * 0.194;
  c = c + textureSample(t, s, in.uv + d * 2.0) * 0.121;
  c = c + textureSample(t, s, in.uv + d * 3.0) * 0.054;
  c = c + textureSample(t, s, in.uv + d * 4.0) * 0.016;
  return c;
}
`;

const UNIFORM_BUFFER_SIZE = 80;          // 5 × vec4<f32>
const SCRATCH_DOWNSAMPLE   = 4;          // 1/4-resolution blur scratch
// Standard "over" blending — surfaces composite in z-order onto whatever
// the prior pass deposited.
const OVER_BLEND = {
  color: { srcFactor: "one", dstFactor: "one-minus-src-alpha", operation: "add" },
  alpha: { srcFactor: "one", dstFactor: "one-minus-src-alpha", operation: "add" },
};
// Replace blending — used for the blur passes (downsample / blurH /
// blurV), where we WANT the destination to be exactly the shader's
// output, not blended with whatever was there.
const REPLACE_BLEND = {
  color: { srcFactor: "one", dstFactor: "zero", operation: "add" },
  alpha: { srcFactor: "one", dstFactor: "zero", operation: "add" },
};

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
    this.pipeline = null;          // identity-blend (over); reads identity-vs/fs
    /** @type {GPURenderPipeline|null} */
    this.blitPipeline = null;      // identity-replace; reads identity-vs/fs_blit
    /** @type {GPURenderPipeline|null} */
    this.blurPipeline = null;      // blur-vs/fs, replace blend
    /** @type {GPURenderPipeline|null} */
    this.alphaTintPipeline = null; // identity-vs + fs_alpha_tint, replace blend
    /** @type {GPUSampler|null} */
    this.sampler = null;
    /** @type {GPUBindGroupLayout|null} */
    this.bindGroupLayout = null;
    this.metrics = { width_px: 0, height_px: 0, density: 1 };

    // Scene + scratch state.
    /** @type {GPUTexture|null} */
    this.sceneTexture = null;
    /** @type {GPUTextureView|null} */
    this.sceneView = null;
    /** @type {GPUTexture|null} */
    this.scratchA = null;
    /** @type {GPUTextureView|null} */
    this.scratchAView = null;
    /** @type {GPUTexture|null} */
    this.scratchB = null;
    /** @type {GPUTextureView|null} */
    this.scratchBView = null;
    this._scratchW = 0;
    this._scratchH = 0;

    // Frame state.
    /** @type {GPUCommandEncoder|null} */
    this._encoder = null;
    /** @type {GPURenderPassEncoder|null} */
    this._scenePass = null;
    this._frameInProgress = false;
    this._lost = false;

    /** Per-source GPUTexture cache. Keyed by the source canvas object
     *  (paint backend keeps the offscreens alive between frames, so
     *  the WeakMap keys stay valid). Each entry is
     *  `{ texture, w, h, view }`; we recreate when `(w, h)` changes.
     *  @type {WeakMap<object, {texture: GPUTexture, w: number, h: number, view: GPUTextureView}>} */
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
    this.device.lost.then((info) => {
      console.warn("[akiva] WebGPU device lost:", info?.message ?? info);
      this._lost = true;
    });
    this.format = navigator.gpu.getPreferredCanvasFormat();
    this.ctx = canvas.getContext("webgpu");
    if (!this.ctx) throw new Error("getContext('webgpu') returned null");
    this.ctx.configure({ device: this.device, format: this.format, alphaMode: "premultiplied" });

    this.screenCanvas = canvas;
    this.metrics = metrics;

    const idModule   = this.device.createShaderModule({ code: IDENTITY_WGSL });
    const blurModule = this.device.createShaderModule({ code: BLUR_WGSL });

    this.bindGroupLayout = this.device.createBindGroupLayout({
      entries: [
        { binding: 0, visibility: GPUShaderStage.VERTEX | GPUShaderStage.FRAGMENT, buffer: { type: "uniform" } },
        { binding: 1, visibility: GPUShaderStage.FRAGMENT, sampler: {} },
        { binding: 2, visibility: GPUShaderStage.FRAGMENT, texture: { sampleType: "float" } },
      ],
    });
    const pipelineLayout = this.device.createPipelineLayout({ bindGroupLayouts: [this.bindGroupLayout] });

    // Surface-composite pipeline: identity vs/fs, OVER blending so
    // surfaces stack in z-order onto the scene.
    this.pipeline = this.device.createRenderPipeline({
      layout: pipelineLayout,
      vertex:   { module: idModule, entryPoint: "vs" },
      fragment: { module: idModule, entryPoint: "fs",
                  targets: [{ format: this.format, blend: OVER_BLEND }] },
      primitive: { topology: "triangle-strip" },
    });
    // Blit / downsample pipeline: identity vs + fs_blit (pre-mult
    // passthrough), REPLACE blending. Used for:
    //   - scene → swapchain (final blit)
    //   - scene → scratchA (blur downsample)
    //   - scratchA → scene at surface bounds (blurred backdrop)
    // All three sample pre-multiplied content; the surface-composite
    // shader (`fs`) would double-multiply, so we use `fs_blit`.
    this.blitPipeline = this.device.createRenderPipeline({
      layout: pipelineLayout,
      vertex:   { module: idModule, entryPoint: "vs" },
      fragment: { module: idModule, entryPoint: "fs_blit",
                  targets: [{ format: this.format, blend: REPLACE_BLEND }] },
      primitive: { topology: "triangle-strip" },
    });
    // Blur pipeline: blur vs/fs, REPLACE blending. Used for both
    // horizontal and vertical passes (the per-call uniform's
    // `blur_dir` selects the axis).
    this.blurPipeline = this.device.createRenderPipeline({
      layout: pipelineLayout,
      vertex:   { module: blurModule, entryPoint: "vs" },
      fragment: { module: blurModule, entryPoint: "fs",
                  targets: [{ format: this.format, blend: REPLACE_BLEND }] },
      primitive: { topology: "triangle-strip" },
    });
    // Alpha-tint pipeline: identity vs + fs_alpha_tint, REPLACE blend.
    // Phase 15.4.4 — first pass of the drop-shadow pipeline. Reads the
    // surface's alpha, tints with the shadow rgba (packed in
    // `blur_dir`), writes a pre-multiplied silhouette into scratchA.
    this.alphaTintPipeline = this.device.createRenderPipeline({
      layout: pipelineLayout,
      vertex:   { module: idModule, entryPoint: "vs" },
      fragment: { module: idModule, entryPoint: "fs_alpha_tint",
                  targets: [{ format: this.format, blend: REPLACE_BLEND }] },
      primitive: { topology: "triangle-strip" },
    });

    this.sampler = this.device.createSampler({
      magFilter: "linear", minFilter: "linear",
      addressModeU: "clamp-to-edge", addressModeV: "clamp-to-edge",
    });

    this._allocSceneAndScratch();
  }

  /** (Re)allocate the scene texture + 1/4-res blur scratches. Called
   *  from `init` and from `resize`. Old textures are explicitly
   *  destroyed so the driver returns the memory; the WeakMap of
   *  source-canvas → texture is unaffected (those track per-Activity
   *  surfaces, not the compositor's internals). */
  _allocSceneAndScratch() {
    const w = Math.max(1, this.metrics.width_px  | 0);
    const h = Math.max(1, this.metrics.height_px | 0);
    const sw = Math.max(1, (w / SCRATCH_DOWNSAMPLE) | 0);
    const sh = Math.max(1, (h / SCRATCH_DOWNSAMPLE) | 0);

    this.sceneTexture?.destroy();
    this.sceneTexture = this.device.createTexture({
      size:   [w, h, 1],
      format: this.format,
      usage:  GPUTextureUsage.RENDER_ATTACHMENT | GPUTextureUsage.TEXTURE_BINDING,
    });
    this.sceneView = this.sceneTexture.createView();

    this.scratchA?.destroy();
    this.scratchA = this.device.createTexture({
      size:   [sw, sh, 1],
      format: this.format,
      usage:  GPUTextureUsage.RENDER_ATTACHMENT | GPUTextureUsage.TEXTURE_BINDING,
    });
    this.scratchAView = this.scratchA.createView();

    this.scratchB?.destroy();
    this.scratchB = this.device.createTexture({
      size:   [sw, sh, 1],
      format: this.format,
      usage:  GPUTextureUsage.RENDER_ATTACHMENT | GPUTextureUsage.TEXTURE_BINDING,
    });
    this.scratchBView = this.scratchB.createView();

    this._scratchW = sw;
    this._scratchH = sh;
  }

  getScreenCtx() { return null; }

  resize(width_px, height_px, density) {
    if (!this.screenCanvas) return;
    this.screenCanvas.width = width_px;
    this.screenCanvas.height = height_px;
    this.metrics = { width_px, height_px, density };
    this._allocSceneAndScratch();
  }

  beginFrame() {
    if (!this.device || this._lost) return;
    if (this._frameInProgress) return;
    this._encoder = this.device.createCommandEncoder();
    this._scenePass = this._encoder.beginRenderPass({
      colorAttachments: [{
        view: this.sceneView,
        loadOp: "clear",
        clearValue: { r: 0, g: 0, b: 0, a: 1 },
        storeOp: "store",
      }],
    });
    this._scenePass.setPipeline(this.pipeline);
    this._frameInProgress = true;
  }

  endFrame() {
    if (!this._frameInProgress) return;
    this._scenePass.end();
    this._scenePass = null;

    // Final blit: scene → swapchain. REPLACE blending so the swapchain
    // gets the scene texture pixel-for-pixel.
    let view;
    try {
      view = this.ctx.getCurrentTexture().createView();
    } catch (err) {
      console.warn("[akiva] WebGPU getCurrentTexture failed:", err);
      this._encoder = null;
      this._frameInProgress = false;
      return;
    }
    const blitPass = this._encoder.beginRenderPass({
      colorAttachments: [{
        view,
        loadOp: "clear",
        clearValue: { r: 0, g: 0, b: 0, a: 1 },
        storeOp: "store",
      }],
    });
    blitPass.setPipeline(this.blitPipeline);
    const w = this.metrics.width_px, h = this.metrics.height_px;
    this._encodeIdentityDraw(blitPass, this.sceneView, {
      dstRect:    [0, 0, w, h],
      targetSize: [w, h],
      opts:       [1, 0, 0, 1],
      srcUv:      [0, 0, 1, 1],
    });
    blitPass.end();

    this.device.queue.submit([this._encoder.finish()]);
    this._encoder = null;
    this._frameInProgress = false;
  }

  /** Composite `src` (an OffscreenCanvas allocated by the paint
   *  backend) onto the scene at `(x, y, w, h)`, transformed by the
   *  SurfaceDrawOpts. When `blurRadius > 0`, runs a backdrop-blur
   *  pass; when `shadowRgba` has a non-zero alpha byte, also runs a
   *  drop-shadow pass. Order matches the plan §15.4.4: shadow first
   *  (extends past surface edges), then backdrop blur (samples scene
   *  including shadow at surface bounds), then surface on top. */
  compositeSurface(
    src, x, y, w, h,
    opacity = 1, translateX = 0, translateY = 0, scale = 1,
    blurRadius = 0,
    shadowOX = 0, shadowOY = 0, shadowBlur = 0, shadowRgba = 0,
    tint = 0,
  ) {
    if (!this.device || this._lost || !src || w <= 0 || h <= 0) return;
    if (!this._frameInProgress) {
      this.beginFrame();
      if (!this._frameInProgress) return;
    }

    // Snapshot src into a per-source GPUTexture before any pass that
    // might depend on it.
    const srcEntry = this._uploadSourceCanvas(src);
    if (!srcEntry) return;

    const shadowAlpha = (shadowRgba >>> 24) & 0xff;
    if (shadowAlpha > 0) {
      this._encodeDropShadow(srcEntry, x, y, w, h, shadowOX, shadowOY, shadowBlur, shadowRgba);
    }

    if (blurRadius > 0) {
      this._encodeBackdropBlur(x, y, w, h, blurRadius);
    }

    // Phase 15.4.5 — unpack tint rgba (alpha=0 = no tint, fast path
    // in the shader's `mix()`). Packed into `blur_dir` to fit the
    // shared 80-byte uniform shape.
    const tintR = ((tint >>> 16) & 0xff) / 255;
    const tintG = ((tint >>>  8) & 0xff) / 255;
    const tintB = ( tint         & 0xff) / 255;
    const tintA = ((tint >>> 24) & 0xff) / 255;

    // Surface composite onto the scene (over any shadow / blurred
    // backdrop deposited above).
    this._encodeIdentityDraw(this._scenePass, srcEntry.view, {
      dstRect:    [x, y, w, h],
      targetSize: [this.metrics.width_px, this.metrics.height_px],
      opts:       [
        Math.max(0, Math.min(1, opacity)),
        translateX, translateY,
        scale > 0 ? scale : 1,
      ],
      srcUv:      [0, 0, 1, 1],
      blurDir:    [tintR, tintG, tintB, tintA],
    });
  }

  /** Upload (or reuse) a GPUTexture for the source canvas. Returns
   *  `{texture, view, w, h}` or `null` on copy failure. */
  _uploadSourceCanvas(src) {
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
    try {
      this.device.queue.copyExternalImageToTexture(
        { source: src },
        { texture: entry.texture },
        [srcW, srcH, 1],
      );
    } catch (err) {
      console.warn("[akiva] copyExternalImageToTexture failed:", err);
      return null;
    }
    return entry;
  }

  /** End the scene pass, render an alpha-tint silhouette of the
   *  surface into scratchA at the surface's screen position (1/4
   *  res), blur the silhouette, then composite the result into the
   *  scene at `(offset_x, offset_y, screen_w, screen_h)` so the soft
   *  shadow lands at `(x + offset_x, y + offset_y, w, h)` plus its
   *  blur halo.
   *
   *  Uses scratchA + scratchB for ping-pong blur. Same as
   *  backdrop-blur except the source for downsample is the surface
   *  texture (not the scene), and we write alpha-tinted output
   *  instead of pre-multiplied colour passthrough. */
  _encodeDropShadow(srcEntry, x, y, w, h, offsetX, offsetY, blurRadius, rgba) {
    this._scenePass.end();
    this._scenePass = null;

    const sw = this._scratchW, sh = this._scratchH;
    const screenW = this.metrics.width_px, screenH = this.metrics.height_px;
    const tintR = ((rgba >>> 16) & 0xff) / 255;
    const tintG = ((rgba >>>  8) & 0xff) / 255;
    const tintB = ( rgba         & 0xff) / 255;
    const tintA = ((rgba >>> 24) & 0xff) / 255;

    // 1) Render alpha-tinted silhouette of the surface into scratchA
    //    at (x/4, y/4, w/4, h/4). Clear scratchA first so the rest
    //    of the scratch is transparent — the blur will smear the
    //    silhouette outward into that empty area, giving the soft
    //    shadow halo.
    const sx = x / SCRATCH_DOWNSAMPLE;
    const sy = y / SCRATCH_DOWNSAMPLE;
    const sWidth  = w / SCRATCH_DOWNSAMPLE;
    const sHeight = h / SCRATCH_DOWNSAMPLE;
    const tintPass = this._encoder.beginRenderPass({
      colorAttachments: [{
        view: this.scratchAView,
        loadOp: "clear",
        clearValue: { r: 0, g: 0, b: 0, a: 0 },
        storeOp: "store",
      }],
    });
    tintPass.setPipeline(this.alphaTintPipeline);
    // The tint shader reads `blur_dir.xyz` for rgb and `blur_dir.w`
    // for alpha — reuse the existing uniform shape rather than add a
    // dedicated layout. opts is unread by this fragment.
    this._uploadAndBindDraw(tintPass, srcEntry.view, {
      dstRect:    [sx, sy, sWidth, sHeight],
      targetSize: [sw, sh],
      opts:       [1, 0, 0, 1],
      srcUv:      [0, 0, 1, 1],
      blurDir:    [tintR, tintG, tintB, tintA],
    });
    tintPass.end();

    // 2) Blur silhouette in scratch space. Same kernel as backdrop
    //    blur — see `_encodeBackdropBlur` for the radius math.
    const tapPx = blurRadius / (4 * SCRATCH_DOWNSAMPLE);
    const blurHPass = this._encoder.beginRenderPass({
      colorAttachments: [{
        view: this.scratchBView,
        loadOp: "clear",
        clearValue: { r: 0, g: 0, b: 0, a: 0 },
        storeOp: "store",
      }],
    });
    blurHPass.setPipeline(this.blurPipeline);
    this._encodeBlurDraw(blurHPass, this.scratchAView, {
      dstRect:    [0, 0, sw, sh],
      targetSize: [sw, sh],
      srcUv:      [0, 0, 1, 1],
      blurDir:    [tapPx / sw, 0],
    });
    blurHPass.end();

    const blurVPass = this._encoder.beginRenderPass({
      colorAttachments: [{
        view: this.scratchAView,
        loadOp: "clear",
        clearValue: { r: 0, g: 0, b: 0, a: 0 },
        storeOp: "store",
      }],
    });
    blurVPass.setPipeline(this.blurPipeline);
    this._encodeBlurDraw(blurVPass, this.scratchBView, {
      dstRect:    [0, 0, sw, sh],
      targetSize: [sw, sh],
      srcUv:      [0, 0, 1, 1],
      blurDir:    [0, tapPx / sh],
    });
    blurVPass.end();

    // 3) Resume scene pass with loadOp=load, then composite the
    //    blurred shadow onto scene as a full-screen draw offset by
    //    (offsetX, offsetY). UV (0,0) → top-left of scratch → screen
    //    pixel (offsetX, offsetY); UV (1,1) → bottom-right of scratch
    //    → screen pixel (offsetX + screenW, offsetY + screenH). The
    //    silhouette in scratch at (sx,sy) thus lands on screen at
    //    (x + offsetX, y + offsetY). OVER blend so the shadow
    //    composites correctly with prior surfaces underneath.
    this._scenePass = this._encoder.beginRenderPass({
      colorAttachments: [{
        view: this.sceneView,
        loadOp: "load",
        storeOp: "store",
      }],
    });
    this._scenePass.setPipeline(this.pipeline);  // OVER blend
    this._uploadAndBindDraw(this._scenePass, this.scratchAView, {
      dstRect:    [offsetX, offsetY, screenW, screenH],
      targetSize: [screenW, screenH],
      opts:       [1, 0, 0, 1],
      srcUv:      [0, 0, 1, 1],
      blurDir:    [0, 0, 0, 0],
    });
    // pipeline is already OVER blend — leave it set so the surface
    // composite that follows reuses it.
  }

  /** Internal helper — encode a draw against `pass` whichever pipeline
   *  is currently set on it. Identical to `_encodeIdentityDraw`
   *  except the `blurDir` is caller-supplied (the alpha-tint shader
   *  reads it as the shadow rgba). */
  _uploadAndBindDraw(pass, srcView, args) {
    const buf = this._uploadUniforms(args);
    const bindGroup = this.device.createBindGroup({
      layout: this.bindGroupLayout,
      entries: [
        { binding: 0, resource: { buffer: buf } },
        { binding: 1, resource: this.sampler },
        { binding: 2, resource: srcView },
      ],
    });
    pass.setBindGroup(0, bindGroup);
    pass.draw(4);
  }

  /** End the scene pass, run downsample + blurH + blurV, then resume
   *  the scene pass and composite the blurred scratch back at the
   *  surface bounds (REPLACE-blended via blitPipeline so it overwrites
   *  whatever was at those bounds). The surface itself composites on
   *  top in `compositeSurface` after this returns.
   *
   *  blurRadius is in screen pixels. The kernel covers ±4 taps;
   *  with the 1/4 downsample we want the effective screen-space
   *  radius to be ~radius pixels, so each tap moves ~radius / 16
   *  scratch pixels. */
  _encodeBackdropBlur(x, y, w, h, blurRadius) {
    this._scenePass.end();
    this._scenePass = null;

    const sw = this._scratchW, sh = this._scratchH;
    const screenW = this.metrics.width_px, screenH = this.metrics.height_px;

    // 1) Downsample scene → scratchA at full scratch size, REPLACE.
    const downsamplePass = this._encoder.beginRenderPass({
      colorAttachments: [{
        view: this.scratchAView,
        loadOp: "clear",
        clearValue: { r: 0, g: 0, b: 0, a: 1 },
        storeOp: "store",
      }],
    });
    downsamplePass.setPipeline(this.blitPipeline);
    this._encodeIdentityDraw(downsamplePass, this.sceneView, {
      dstRect:    [0, 0, sw, sh],
      targetSize: [sw, sh],
      opts:       [1, 0, 0, 1],
      srcUv:      [0, 0, 1, 1],
    });
    downsamplePass.end();

    // Per-tap UV offsets so the effective screen-space radius lands
    // at ~blurRadius pixels. 4 taps × SCRATCH_DOWNSAMPLE × tap_step
    // = blurRadius → tap_step (in scratch px) = blurRadius / 16.
    const tapPx = blurRadius / (4 * SCRATCH_DOWNSAMPLE);

    // 2) Blur horizontal: scratchA → scratchB, dir = (tapPx/sw, 0).
    const blurHPass = this._encoder.beginRenderPass({
      colorAttachments: [{
        view: this.scratchBView,
        loadOp: "clear",
        clearValue: { r: 0, g: 0, b: 0, a: 1 },
        storeOp: "store",
      }],
    });
    blurHPass.setPipeline(this.blurPipeline);
    this._encodeBlurDraw(blurHPass, this.scratchAView, {
      dstRect:    [0, 0, sw, sh],
      targetSize: [sw, sh],
      srcUv:      [0, 0, 1, 1],
      blurDir:    [tapPx / sw, 0],
    });
    blurHPass.end();

    // 3) Blur vertical: scratchB → scratchA, dir = (0, tapPx/sh).
    const blurVPass = this._encoder.beginRenderPass({
      colorAttachments: [{
        view: this.scratchAView,
        loadOp: "clear",
        clearValue: { r: 0, g: 0, b: 0, a: 1 },
        storeOp: "store",
      }],
    });
    blurVPass.setPipeline(this.blurPipeline);
    this._encodeBlurDraw(blurVPass, this.scratchBView, {
      dstRect:    [0, 0, sw, sh],
      targetSize: [sw, sh],
      srcUv:      [0, 0, 1, 1],
      blurDir:    [0, tapPx / sh],
    });
    blurVPass.end();

    // 4) Resume scene pass with loadOp=load (preserve prior content).
    this._scenePass = this._encoder.beginRenderPass({
      colorAttachments: [{
        view: this.sceneView,
        loadOp: "load",
        storeOp: "store",
      }],
    });
    this._scenePass.setPipeline(this.blitPipeline);

    // 5) Composite blurred scratchA back into the scene at surface
    //    bounds. We sample only the surface-region of scratchA,
    //    mapped back through screen-space UV (since downsample was
    //    full screen → full scratch, the UV ratio is the same:
    //    (x/screenW, y/screenH, w/screenW, h/screenH)). REPLACE
    //    blending → the blurred backdrop overwrites whatever was at
    //    those bounds (which is what frosted glass looks like).
    this._encodeIdentityDraw(this._scenePass, this.scratchAView, {
      dstRect:    [x, y, w, h],
      targetSize: [screenW, screenH],
      opts:       [1, 0, 0, 1],
      srcUv:      [x / screenW, y / screenH, w / screenW, h / screenH],
    });

    // 6) Switch back to OVER-blend pipeline so the surface itself
    //    composites with proper alpha.
    this._scenePass.setPipeline(this.pipeline);
  }

  /** Encode one identity textured-quad draw into `pass`. The pipeline
   *  must already be set on the pass (caller's choice of OVER vs
   *  REPLACE blend). `args.blurDir` is the surface composite shader's
   *  tint slot (15.4.5) when this is going through `this.pipeline`;
   *  it's unread by `fs_blit` (the blit pipeline) so blits can leave
   *  the default. */
  _encodeIdentityDraw(pass, srcView, args) {
    const buf = this._uploadUniforms({
      dstRect:    args.dstRect,
      targetSize: args.targetSize,
      opts:       args.opts,
      srcUv:      args.srcUv,
      blurDir:    args.blurDir ?? [0, 0, 0, 0],
    });
    const bindGroup = this.device.createBindGroup({
      layout: this.bindGroupLayout,
      entries: [
        { binding: 0, resource: { buffer: buf } },
        { binding: 1, resource: this.sampler },
        { binding: 2, resource: srcView },
      ],
    });
    pass.setBindGroup(0, bindGroup);
    pass.draw(4);
  }

  /** Encode one blur draw into `pass`. The blur pipeline must already
   *  be set on the pass. */
  _encodeBlurDraw(pass, srcView, args) {
    const buf = this._uploadUniforms({
      dstRect:    args.dstRect,
      targetSize: args.targetSize,
      opts:       [0, 0, 0, 1],   // unused by blur shader
      srcUv:      args.srcUv,
      blurDir:    args.blurDir,
    });
    const bindGroup = this.device.createBindGroup({
      layout: this.bindGroupLayout,
      entries: [
        { binding: 0, resource: { buffer: buf } },
        { binding: 1, resource: this.sampler },
        { binding: 2, resource: srcView },
      ],
    });
    pass.setBindGroup(0, bindGroup);
    pass.draw(4);
  }

  /** Allocate a fresh per-call uniform buffer and upload the packed
   *  args. Per-call allocation is mandatory: `writeBuffer` ordering
   *  vs `draw` ordering isn't preserved within a single submission,
   *  so a shared buffer would deliver the LAST frame's uniforms to
   *  every draw. The driver pools small uniforms efficiently. */
  _uploadUniforms({ dstRect, targetSize, opts, srcUv, blurDir }) {
    const u = new Float32Array(20);  // 80 bytes / 4
    u[0]  = dstRect[0];    u[1]  = dstRect[1];    u[2]  = dstRect[2];    u[3]  = dstRect[3];
    u[4]  = targetSize[0]; u[5]  = targetSize[1]; u[6]  = 0;             u[7]  = 0;
    u[8]  = opts[0];       u[9]  = opts[1];       u[10] = opts[2];       u[11] = opts[3];
    u[12] = srcUv[0];      u[13] = srcUv[1];      u[14] = srcUv[2];      u[15] = srcUv[3];
    u[16] = blurDir[0];    u[17] = blurDir[1];    u[18] = 0;             u[19] = 0;
    const buf = this.device.createBuffer({
      size: UNIFORM_BUFFER_SIZE,
      usage: GPUBufferUsage.UNIFORM | GPUBufferUsage.COPY_DST,
    });
    this.device.queue.writeBuffer(buf, 0, u);
    return buf;
  }

  present() { this.endFrame(); }

  destroy() {
    if (this._frameInProgress) this.endFrame();
    this.sceneTexture?.destroy();
    this.scratchA?.destroy();
    this.scratchB?.destroy();
    this.sceneTexture = this.scratchA = this.scratchB = null;
    this.sceneView = this.scratchAView = this.scratchBView = null;
    this.pipeline = null;
    this.blitPipeline = null;
    this.blurPipeline = null;
    this.alphaTintPipeline = null;
    this.sampler = null;
    this.bindGroupLayout = null;
    this.ctx = null;
    this.device = null;
    this.adapter = null;
    this.screenCanvas = null;
  }
}
