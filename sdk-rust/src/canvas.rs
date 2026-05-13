//! Paint-op encoder for app `on_render` callbacks.
//!
//! `Canvas` is a thin wrapper around a byte buffer that accumulates paint
//! ops in the v0 wire format documented in [docs/abi.md §2.5]. The SDK
//! owns the buffer lifetime (one per activity handle, in `runtime.rs`);
//! `Canvas` is a scoped borrow that only lives for one frame.
//!
//! The opcodes here must stay byte-for-byte identical to
//! `framework::wasm_activity::opcode::*` on the decode side — that's the
//! ABI. Any change is a v0-breaking event and goes through the §1.2
//! narrow-ABI review.

// ── Opcodes (mirror framework/src/wasm_activity.rs) ───────────────────

const OP_CLEAR:            u8 = 0;
const OP_FILL_RECT:        u8 = 1;
const OP_DRAW_TEXT:        u8 = 2;
const OP_PUSH_CLIP:        u8 = 3;
const OP_POP_CLIP:         u8 = 4;
const OP_PUSH_TRANSFORM:   u8 = 5;
const OP_POP_TRANSFORM:    u8 = 6;
const OP_FILL_ROUND_RECT:  u8 = 7;
// Stone 13.0prep — outline ops. Each carries a `width: f32` LE.
const OP_STROKE_RECT:       u8 = 8;
const OP_STROKE_ROUND_RECT: u8 = 9;
const OP_DRAW_LINE:         u8 = 10;
// Stone 13.0prep C — bitmap blit. drawable_id (u32) is allocated by
// `res::drawable(name)` from the host; `0` means missing (renderer
// paints a magenta placeholder, matching @color/@string fallback).
const OP_DRAW_IMAGE:        u8 = 11;
// Foundation gap — bitmap blit by `(package_id, drawable_name)`.
// Used by the Shell to render foreign-package launcher icons. The
// renderer's foreign-drawable cache is populated when each package's
// `_loadDrawableBitmaps` runs.
const OP_DRAW_FOREIGN_IMAGE: u8 = 12;
// Stage 2a — rounded clip. Same wire shape as OP_PUSH_CLIP plus a
// trailing `radius: f32`. Shares the clip stack with the sharp
// variant, popped by the same OP_POP_CLIP.
const OP_PUSH_ROUND_CLIP:   u8 = 19;
// Stage 3.2 — push/pop opacity layer. Carries one `alpha: f32` LE.
// Renderer maps these to `ctx.save() / globalAlpha *= a / restore()`.
// Nested pushes compose multiplicatively, matching the CSS `opacity`
// cascade: `Frame::opacity(0.5)` inside another `Frame::opacity(0.5)`
// paints at effective 0.25. Differs from the flat OP_SET_GLOBAL_ALPHA
// (= 13) which the framework uses internally for surface composites
// — that one persists until the next set, doesn't compose, and isn't
// exposed to apps.
const OP_PUSH_OPACITY:      u8 = 20;
const OP_POP_OPACITY:       u8 = 21;

/// Paint surface handed to `Activity::on_render`. All coordinates are in
/// dp (density-independent pixels); colors are **`0xAARRGGBB`** — alpha
/// first, then red, green, blue. That matches the renderer backend on
/// the host side and the Shell's conventions; the name `rgba` on the
/// parameter is a legacy from the wire format and does *not* indicate
/// byte order.
///
/// The canvas is write-only and order-preserving: the host replays ops in
/// the exact sequence issued here. Apps that want retained-mode rendering
/// should build their own view tree and flush it here each frame.
pub struct Canvas<'a> {
    buf: &'a mut Vec<u8>,
}

impl<'a> Canvas<'a> {
    pub(crate) fn new(buf: &'a mut Vec<u8>) -> Self { Self { buf } }

    /// Current byte count in the paint stream. Only exposed for tests
    /// inside the SDK that need to assert "the canvas didn't grow"; app
    /// code has no business reading this.
    #[cfg(test)]
    pub(crate) fn buf_len(&self) -> usize { self.buf.len() }

    /// Fill the entire surface with a single color. Paints at the bottom
    /// of the stack — a typical frame starts with `clear`.
    pub fn clear(&mut self, rgba: u32) {
        self.buf.push(OP_CLEAR);
        self.buf.extend_from_slice(&rgba.to_le_bytes());
    }

    /// Axis-aligned filled rectangle. Top-left origin; y grows downward.
    pub fn fill_rect(&mut self, x: f32, y: f32, w: f32, h: f32, rgba: u32) {
        self.buf.push(OP_FILL_RECT);
        self.buf.extend_from_slice(&x.to_le_bytes());
        self.buf.extend_from_slice(&y.to_le_bytes());
        self.buf.extend_from_slice(&w.to_le_bytes());
        self.buf.extend_from_slice(&h.to_le_bytes());
        self.buf.extend_from_slice(&rgba.to_le_bytes());
    }

    /// Filled rectangle with a uniform corner radius. `radius == 0.0`
    /// behaves identically to [`Self::fill_rect`] but goes through the
    /// roundRect host path; for a hot path that paints thousands of
    /// sharp-cornered rects per frame, prefer `fill_rect`.
    pub fn fill_round_rect(&mut self, x: f32, y: f32, w: f32, h: f32, radius: f32, rgba: u32) {
        self.buf.push(OP_FILL_ROUND_RECT);
        self.buf.extend_from_slice(&x.to_le_bytes());
        self.buf.extend_from_slice(&y.to_le_bytes());
        self.buf.extend_from_slice(&w.to_le_bytes());
        self.buf.extend_from_slice(&h.to_le_bytes());
        self.buf.extend_from_slice(&radius.to_le_bytes());
        self.buf.extend_from_slice(&rgba.to_le_bytes());
    }

    /// Draw a UTF-8 string. `size` is the font size in sp-equivalent
    /// (renderer applies density). `(x, y)` is the **baseline** origin,
    /// matching Canvas 2D `fillText` semantics on the host side.
    pub fn draw_text(&mut self, x: f32, y: f32, size: f32, rgba: u32, text: &str) {
        let bytes = text.as_bytes();
        self.buf.push(OP_DRAW_TEXT);
        self.buf.extend_from_slice(&x.to_le_bytes());
        self.buf.extend_from_slice(&y.to_le_bytes());
        self.buf.extend_from_slice(&size.to_le_bytes());
        self.buf.extend_from_slice(&rgba.to_le_bytes());
        self.buf.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
        self.buf.extend_from_slice(bytes);
    }

    /// Push a clip rectangle. Subsequent ops are masked to this region
    /// until the matching `pop_clip`. Nests.
    pub fn push_clip(&mut self, x: f32, y: f32, w: f32, h: f32) {
        self.buf.push(OP_PUSH_CLIP);
        self.buf.extend_from_slice(&x.to_le_bytes());
        self.buf.extend_from_slice(&y.to_le_bytes());
        self.buf.extend_from_slice(&w.to_le_bytes());
        self.buf.extend_from_slice(&h.to_le_bytes());
    }

    /// Push a rounded-corner clip. Shares the clip stack with
    /// [`Self::push_clip`] — popped by the same [`Self::pop_clip`].
    /// `radius` is in dp; the renderer constructs a rounded-rect
    /// path internally. Useful for circular avatars
    /// (`radius == w/2 == h/2`), pill cards (`radius == h/2`), and
    /// any "clip an Image to a rounded shape" pattern. Activities
    /// that want a sharp clip stay on `push_clip`; `radius == 0.0`
    /// here works but takes the slower rounded-path code path on
    /// the renderer.
    pub fn push_round_clip(&mut self, x: f32, y: f32, w: f32, h: f32, radius: f32) {
        self.buf.push(OP_PUSH_ROUND_CLIP);
        self.buf.extend_from_slice(&x.to_le_bytes());
        self.buf.extend_from_slice(&y.to_le_bytes());
        self.buf.extend_from_slice(&w.to_le_bytes());
        self.buf.extend_from_slice(&h.to_le_bytes());
        self.buf.extend_from_slice(&radius.to_le_bytes());
    }

    pub fn pop_clip(&mut self) {
        self.buf.push(OP_POP_CLIP);
    }

    /// Push an opacity layer. Subsequent paint ops multiply their
    /// effective alpha by `alpha` (clamped to `[0, 1]`) until the
    /// matching [`Self::pop_opacity`]. Nests: pushing 0.5 inside an
    /// outer 0.5 paints at 0.25, matching the CSS `opacity` cascade.
    ///
    /// Cheaper than rewriting each op's rgba byte: the renderer maps
    /// the pair to a `ctx.save() / globalAlpha *= alpha / restore`
    /// triple, which Canvas2D applies as a single GPU multiply per
    /// drawn primitive. Use it for fade-in / fade-out groups
    /// (`AnimatedValue<f32>` driving alpha), disabled-card dimming,
    /// or any place an opacity *cascade* over a sub-tree beats
    /// per-op alpha bookkeeping.
    ///
    /// `alpha == 1.0` is a legal call but wastes two ops — apps that
    /// build trees dynamically should skip the pair when their
    /// `AnimatedValue.value() == 1.0` on settled animations.
    pub fn push_opacity(&mut self, alpha: f32) {
        let a = alpha.clamp(0.0, 1.0);
        self.buf.push(OP_PUSH_OPACITY);
        self.buf.extend_from_slice(&a.to_le_bytes());
    }

    pub fn pop_opacity(&mut self) {
        self.buf.push(OP_POP_OPACITY);
    }

    /// Translate + uniform scale. Useful for panning / zooming a sub-tree
    /// without rewriting every coordinate.
    pub fn push_transform(&mut self, dx: f32, dy: f32, scale: f32) {
        self.buf.push(OP_PUSH_TRANSFORM);
        self.buf.extend_from_slice(&dx.to_le_bytes());
        self.buf.extend_from_slice(&dy.to_le_bytes());
        self.buf.extend_from_slice(&scale.to_le_bytes());
    }

    pub fn pop_transform(&mut self) {
        self.buf.push(OP_POP_TRANSFORM);
    }

    /// Stone 13.0prep — outlined rectangle. `width` is the stroke
    /// width in dp (renderer applies density). Center-stroked: half the
    /// width extends inside the rect bounds, half outside, matching
    /// Canvas 2D `strokeRect` semantics.
    pub fn stroke_rect(&mut self, x: f32, y: f32, w: f32, h: f32, width: f32, rgba: u32) {
        self.buf.push(OP_STROKE_RECT);
        self.buf.extend_from_slice(&x.to_le_bytes());
        self.buf.extend_from_slice(&y.to_le_bytes());
        self.buf.extend_from_slice(&w.to_le_bytes());
        self.buf.extend_from_slice(&h.to_le_bytes());
        self.buf.extend_from_slice(&width.to_le_bytes());
        self.buf.extend_from_slice(&rgba.to_le_bytes());
    }

    /// Stone 13.0prep — outlined rounded rectangle. Same convention as
    /// [`Self::fill_round_rect`] for `radius`; `width` is the stroke
    /// width in dp.
    pub fn stroke_round_rect(
        &mut self, x: f32, y: f32, w: f32, h: f32,
        radius: f32, width: f32, rgba: u32,
    ) {
        self.buf.push(OP_STROKE_ROUND_RECT);
        self.buf.extend_from_slice(&x.to_le_bytes());
        self.buf.extend_from_slice(&y.to_le_bytes());
        self.buf.extend_from_slice(&w.to_le_bytes());
        self.buf.extend_from_slice(&h.to_le_bytes());
        self.buf.extend_from_slice(&radius.to_le_bytes());
        self.buf.extend_from_slice(&width.to_le_bytes());
        self.buf.extend_from_slice(&rgba.to_le_bytes());
    }

    /// Stone 13.0prep — straight line from `(x0, y0)` to `(x1, y1)`.
    /// `width` is the stroke width in dp; line caps are round.
    pub fn draw_line(
        &mut self, x0: f32, y0: f32, x1: f32, y1: f32,
        width: f32, rgba: u32,
    ) {
        self.buf.push(OP_DRAW_LINE);
        self.buf.extend_from_slice(&x0.to_le_bytes());
        self.buf.extend_from_slice(&y0.to_le_bytes());
        self.buf.extend_from_slice(&x1.to_le_bytes());
        self.buf.extend_from_slice(&y1.to_le_bytes());
        self.buf.extend_from_slice(&width.to_le_bytes());
        self.buf.extend_from_slice(&rgba.to_le_bytes());
    }

    /// Stone 13.0prep C — blit a `@drawable/`-resolved image. Get the
    /// `drawable_id` from [`crate::res::drawable`]; `0` is the
    /// "missing drawable" sentinel which the renderer paints as a
    /// magenta placeholder (parallel to the `@color/`/`@string/`
    /// fallback). The `(x, y)` is the top-left corner; `(w, h)` is
    /// the destination size in dp — the source image is stretched to
    /// fit, no aspect-ratio enforcement.
    pub fn draw_image(
        &mut self, drawable_id: u32, x: f32, y: f32, w: f32, h: f32,
    ) {
        self.buf.push(OP_DRAW_IMAGE);
        self.buf.extend_from_slice(&drawable_id.to_le_bytes());
        self.buf.extend_from_slice(&x.to_le_bytes());
        self.buf.extend_from_slice(&y.to_le_bytes());
        self.buf.extend_from_slice(&w.to_le_bytes());
        self.buf.extend_from_slice(&h.to_le_bytes());
    }

    /// Foundation gap — blit a drawable owned by another package.
    /// Looked up against the renderer's foreign-drawable cache by
    /// `(package_id, name)` — see [`crate::pkgs::PackageEntry`].
    /// Empty `name` (the package declared no `app.icon`) paints the
    /// magenta missing-drawable placeholder, same fallback shape as
    /// every other drawable miss.
    ///
    /// The Shell uses this to render launcher icons; ordinary apps
    /// almost never need it (their own drawables go through the
    /// faster [`Self::draw_image`] path).
    pub fn draw_foreign_image(
        &mut self, package_id: &str, name: &str, x: f32, y: f32, w: f32, h: f32,
    ) {
        self.buf.push(OP_DRAW_FOREIGN_IMAGE);
        let pkg_bytes = package_id.as_bytes();
        let name_bytes = name.as_bytes();
        let pkg_len = pkg_bytes.len().min(u16::MAX as usize);
        let name_len = name_bytes.len().min(u16::MAX as usize);
        self.buf.extend_from_slice(&(pkg_len as u16).to_le_bytes());
        self.buf.extend_from_slice(&pkg_bytes[..pkg_len]);
        self.buf.extend_from_slice(&(name_len as u16).to_le_bytes());
        self.buf.extend_from_slice(&name_bytes[..name_len]);
        self.buf.extend_from_slice(&x.to_le_bytes());
        self.buf.extend_from_slice(&y.to_le_bytes());
        self.buf.extend_from_slice(&w.to_le_bytes());
        self.buf.extend_from_slice(&h.to_le_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `push_opacity` writes opcode 20 followed by the clamped alpha
    /// as a little-endian f32. Five bytes total.
    #[test]
    fn push_opacity_encodes_five_bytes_with_alpha_le_f32() {
        let mut buf = Vec::new();
        let mut c = Canvas::new(&mut buf);
        c.push_opacity(0.25);
        assert_eq!(buf.len(), 5, "push_opacity emits opcode + f32 (5 bytes)");
        assert_eq!(buf[0], OP_PUSH_OPACITY);
        let alpha_bytes: [u8; 4] = buf[1..5].try_into().unwrap();
        assert!((f32::from_le_bytes(alpha_bytes) - 0.25).abs() < 1e-7);
    }

    /// `pop_opacity` writes a single opcode byte (no payload).
    #[test]
    fn pop_opacity_encodes_one_byte() {
        let mut buf = Vec::new();
        let mut c = Canvas::new(&mut buf);
        c.pop_opacity();
        assert_eq!(buf, vec![OP_POP_OPACITY]);
    }

    /// Alpha is clamped to `[0, 1]` on encode — apps that overshoot
    /// (animation past 1.0 due to spring overshoot, or below 0 due
    /// to bouncy undershoot) emit the boundary, not the raw value.
    /// The renderer would clamp anyway, but encoding the clamp here
    /// makes the wire round-trip lossless and the test surface clear.
    #[test]
    fn push_opacity_clamps_alpha_to_zero_one_inclusive() {
        for (input, want) in [(-0.5_f32, 0.0_f32), (1.5, 1.0), (0.0, 0.0), (1.0, 1.0)] {
            let mut buf = Vec::new();
            let mut c = Canvas::new(&mut buf);
            c.push_opacity(input);
            let alpha_bytes: [u8; 4] = buf[1..5].try_into().unwrap();
            let got = f32::from_le_bytes(alpha_bytes);
            assert!(
                (got - want).abs() < 1e-7,
                "push_opacity({input}) should encode {want}, got {got}",
            );
        }
    }

    /// Nested push/pop pairs build a stack-shaped byte stream — pops
    /// after pushes, no resync magic, just the bytes the renderer
    /// expects to walk in order.
    #[test]
    fn nested_opacity_layers_emit_balanced_push_pop_pairs() {
        let mut buf = Vec::new();
        let mut c = Canvas::new(&mut buf);
        c.push_opacity(0.5);
        c.push_opacity(0.5);
        c.fill_rect(0.0, 0.0, 10.0, 10.0, 0xff_11_22_33);
        c.pop_opacity();
        c.pop_opacity();
        // Two pushes (5 + 5) + one fill_rect (21) + two pops (1 + 1) = 33.
        assert_eq!(buf.len(), 5 + 5 + 21 + 1 + 1);
        // Byte at idx 0 / 5 = push opcode; idx 31 / 32 = pop opcode.
        assert_eq!(buf[0], OP_PUSH_OPACITY);
        assert_eq!(buf[5], OP_PUSH_OPACITY);
        assert_eq!(buf[31], OP_POP_OPACITY);
        assert_eq!(buf[32], OP_POP_OPACITY);
    }

    /// `push_opacity(1.0)` is a legal (but wasteful) call — encodes
    /// the full 5 bytes the same as any other alpha. Apps avoid the
    /// waste at build time (`if value < 1.0 { push; … pop }`), not
    /// in the encoder.
    #[test]
    fn push_opacity_one_point_zero_still_encodes_full_op() {
        let mut buf = Vec::new();
        let mut c = Canvas::new(&mut buf);
        c.push_opacity(1.0);
        assert_eq!(buf.len(), 5);
        assert_eq!(buf[0], OP_PUSH_OPACITY);
    }
}
