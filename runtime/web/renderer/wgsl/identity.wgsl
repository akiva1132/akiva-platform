// Phase 15.4.2 — textured-quad composite with SurfaceDrawOpts.
//
// Samples the source surface as a regular `texture_2d<f32>` (the
// pipeline copies the OffscreenCanvas pixels into this texture each
// frame via `copyExternalImageToTexture` — `importExternalTexture`
// only accepts video sources in current browsers). Emits a quad at
// `dst_rect` in screen pixels, transformed by the SurfaceDrawOpts
// (translate, scale around the dst centre, alpha multiplier). No
// effects (blur / shadow / tint) yet — those are 15.4.3+.
//
// Vertex order is `triangle-strip` over vids 0..3 producing the corners
// (0,0) (1,0) (0,1) (1,1).

struct Uniforms {
  // (x, y, w, h) of the destination rect in screen pixels.
  dst_rect:    vec4<f32>,
  // (w, h, 0, 0) of the screen, used to convert pixels → NDC.
  screen_size: vec4<f32>,
  // Phase 15.4.2 — SurfaceDrawOpts:
  //   x = opacity ∈ [0, 1]
  //   y = translate_x in screen pixels
  //   z = translate_y in screen pixels
  //   w = scale (1.0 = identity)
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
  // Scale around the dst-rect centre, then translate. Doing it in
  // this order matches the Canvas2D path's
  // save → translate(tx,ty) → scale(s) → drawImage(x,y,w,h):
  // a slide animation moves the entire surface; a scale animation
  // grows/shrinks it from its own centre rather than from the
  // origin.
  let scaled_size = u.dst_rect.zw * u.opts.w;
  let pivot       = u.dst_rect.xy + u.dst_rect.zw * 0.5;
  let pixel       = pivot
                  + (corner - vec2<f32>(0.5, 0.5)) * scaled_size
                  + u.opts.yz;
  // Pixels → NDC. WebGPU is Y-up in NDC; screen pixels are Y-down.
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
  // Effective alpha = sampled alpha × surface opacity. The screen
  // surface is configured `alphaMode: "premultiplied"`, so the source
  // (which is straight alpha from a Canvas2D context) is pre-multiplied
  // here on the way out. (15.4.5+ effect mixing happens BEFORE the
  // pre-multiply.)
  let a = c.a * u.opts.x;
  return vec4<f32>(c.rgb * a, a);
}
