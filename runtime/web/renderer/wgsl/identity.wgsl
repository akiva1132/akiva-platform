// Phase 15.4.1 — identity textured-quad composite.
//
// Samples the source surface as an external texture (path A) and emits
// a quad at `dst_rect` (in screen pixels) onto the screen. No
// translate / scale / opacity yet — those land in 15.4.2. No effects
// (blur / shadow / tint) — those are 15.4.3+.
//
// Vertex order is `triangle-strip` over vids 0..3 producing the corners
// (0,0) (1,0) (0,1) (1,1).

struct Uniforms {
  // (x, y, w, h) of the destination rect in screen pixels.
  dst_rect:    vec4<f32>,
  // (w, h, 0, 0) of the screen, used to convert pixels → NDC.
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
  let c = textureSampleBaseClampToEdge(t, s, in.uv);
  // The screen surface is configured `alphaMode: "premultiplied"`. The
  // source canvas produces straight alpha, so pre-multiply on the way
  // out. (15.4.5+ effect mixing happens BEFORE this multiply.)
  return vec4<f32>(c.rgb * c.a, c.a);
}
