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

const OP_CLEAR:           u8 = 0;
const OP_FILL_RECT:       u8 = 1;
const OP_DRAW_TEXT:       u8 = 2;
const OP_PUSH_CLIP:       u8 = 3;
const OP_POP_CLIP:        u8 = 4;
const OP_PUSH_TRANSFORM:  u8 = 5;
const OP_POP_TRANSFORM:   u8 = 6;

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
}
