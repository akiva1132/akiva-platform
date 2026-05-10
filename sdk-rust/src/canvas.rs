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
