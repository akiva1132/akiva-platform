// Phase 15.4.3 — textured-quad composite with SurfaceDrawOpts and a
// configurable source UV region.
//
// Used for three things in the WebGPU compositor:
//   1. Composite an Activity surface onto the scene texture (with
//      opts: opacity, translate, scale around the dst centre).
//   2. Downsample the scene texture into the 1/4-resolution scratch
//      texture before a blur pass (opts identity, src_uv = full).
//   3. Final scene → swapchain blit at endFrame (opts identity,
//      src_uv = full, dst = full screen).
//
// The struct layout is shared with `blur.wgsl` so the JS side can
// write a single 80-byte uniform shape per draw and pick the
// appropriate render pipeline.

struct Uniforms {
  // (x, y, w, h) of the destination rect in target-pixel space.
  dst_rect:    vec4<f32>,
  // (w, h, 0, 0) of the render target (scene/scratch/swapchain).
  target_size: vec4<f32>,
  // (opacity, translate_x, translate_y, scale). Identity = (1,0,0,1).
  opts:        vec4<f32>,
  // (u0, v0, du, dv) — region of the source texture to sample.
  // Identity = (0,0,1,1) → sample the whole source.
  src_uv:      vec4<f32>,
  // Unused by identity; defined here to keep one struct layout
  // shared with blur.wgsl. (`blur.wgsl` uses this for the per-tap
  // offset in UV space.)
  blur_dir:    vec4<f32>,
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
@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
  let c = textureSample(t, s, in.uv);
  // Effective alpha = sampled alpha × surface opacity.
  // (15.4.5+ effect mixing happens BEFORE this pre-multiply.)
  let a = c.a * u.opts.x;
  return vec4<f32>(c.rgb * a, a);
}

// Pre-multiplied passthrough: used for blits between intermediate
// targets that already hold pre-multiplied pixels (scene → swapchain,
// scene → scratch downsample, scratchA → scene as a blurred backdrop).
// Without this, `fs` would double-multiply alpha and produce a
// too-dark result.
@fragment
fn fs_blit(in: VsOut) -> @location(0) vec4<f32> {
  return textureSample(t, s, in.uv);
}
