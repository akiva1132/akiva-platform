// Phase 15.4.3 — separable Gaussian blur (one shader, used for both
// horizontal and vertical passes; the `blur_dir` uniform picks the
// axis).
//
// Pipeline:
//   downsample (identity.wgsl) — scene → scratchA at 1/4 res
//   blur horizontal (this)     — scratchA → scratchB, blur_dir=(dx, 0)
//   blur vertical   (this)     — scratchB → scratchA, blur_dir=(0, dy)
//   composite (identity.wgsl)  — scratchA → scene at surface bounds,
//                                 with src_uv mapping back to the
//                                 surface-bounds region of scratchA.
//
// 9 taps, separable, weights pre-normalised to sum to 1.0:
//   [0.016, 0.054, 0.121, 0.194, 0.227, 0.194, 0.121, 0.054, 0.016]
// At 1/4 resolution this delivers an effective screen-space radius of
// roughly 16 × |blur_dir| pixels per axis. The JS side scales
// `blur_dir` so a `backdrop_blur_radius` of N screen-pixels lands at
// the right tap spacing.
//
// Shares the `Uniforms` struct layout with `identity.wgsl` so the JS
// side writes a single 80-byte uniform shape and only the pipeline
// (and thus the entry points) differ.

struct Uniforms {
  dst_rect:    vec4<f32>,
  target_size: vec4<f32>,
  opts:        vec4<f32>,  // unused by blur
  src_uv:      vec4<f32>,  // sample region; identity blur uses (0,0,1,1)
  blur_dir:    vec4<f32>,  // (du, dv, 0, 0) — per-tap offset in UV space
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
  // Simple corner-mapped quad; no pivot/scale/translate — blur runs
  // over a fixed dst rect into a render-target-sized output.
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
