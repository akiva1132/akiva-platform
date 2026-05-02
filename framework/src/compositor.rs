//! Compositor.
//!
//! Collects paint output from each visible window, stacks them in z-order,
//! and produces the `FrameCommands` buffer the renderer backend consumes.
//! See ARCHITECTURE.md §10-11 for the renderer/surface model.

/// Abstract target for paint output. `FrameCommands` is the in-framework
/// implementation; activities that want to paint by method rather than by
/// pushing `PaintOp`s directly take `&mut dyn PaintTarget`.
pub trait PaintTarget {
    fn fill_rect(&mut self, x: f32, y: f32, w: f32, h: f32, rgba: u32);
    fn fill_round_rect(&mut self, x: f32, y: f32, w: f32, h: f32, radius: f32, rgba: u32);
    fn draw_text(&mut self, x: f32, y: f32, size: f32, rgba: u32, text: &str);
    fn push_clip(&mut self, x: f32, y: f32, w: f32, h: f32);
    fn pop_clip(&mut self);
}

#[derive(Debug, Default, Clone)]
pub struct FrameCommands {
    pub ops: Vec<PaintOp>,
}

#[derive(Debug, Clone)]
pub enum PaintOp {
    Clear { rgba: u32 },
    FillRect { x: f32, y: f32, w: f32, h: f32, rgba: u32 },
    FillRoundRect { x: f32, y: f32, w: f32, h: f32, radius: f32, rgba: u32 },
    DrawText { x: f32, y: f32, size: f32, rgba: u32, text: String },
    PushClip { x: f32, y: f32, w: f32, h: f32 },
    PopClip,
    PushTransform { dx: f32, dy: f32, scale: f32 },
    PopTransform,
}

impl FrameCommands {
    pub fn new() -> Self { Self::default() }
    pub fn clear(&mut self) { self.ops.clear(); }
    pub fn push(&mut self, op: PaintOp) { self.ops.push(op); }
    pub fn ops(&self) -> &[PaintOp] { &self.ops }
}

// Activities paint through the `PaintTarget` trait; FrameCommands is the
// in-kernel target that ends up getting flushed to the HAL each frame.
impl PaintTarget for FrameCommands {
    fn fill_rect(&mut self, x: f32, y: f32, w: f32, h: f32, rgba: u32) {
        self.ops.push(PaintOp::FillRect { x, y, w, h, rgba });
    }
    fn fill_round_rect(&mut self, x: f32, y: f32, w: f32, h: f32, radius: f32, rgba: u32) {
        self.ops.push(PaintOp::FillRoundRect { x, y, w, h, radius, rgba });
    }
    fn draw_text(&mut self, x: f32, y: f32, size: f32, rgba: u32, text: &str) {
        self.ops.push(PaintOp::DrawText { x, y, size, rgba, text: text.to_string() });
    }
    fn push_clip(&mut self, x: f32, y: f32, w: f32, h: f32) {
        self.ops.push(PaintOp::PushClip { x, y, w, h });
    }
    fn pop_clip(&mut self) {
        self.ops.push(PaintOp::PopClip);
    }
}

pub struct Compositor {
    frame: FrameCommands,
}

impl Compositor {
    pub fn new() -> Self { Self { frame: FrameCommands::new() } }

    pub fn begin_frame(&mut self) {
        self.frame.clear();
    }

    pub fn finish_frame(&mut self) -> &FrameCommands {
        &self.frame
    }

    pub fn emit(&mut self, op: PaintOp) { self.frame.push(op); }

    /// Direct access for activities that want to paint through `PaintTarget`
    /// rather than emit `PaintOp`s by hand.
    pub fn paint_target(&mut self) -> &mut FrameCommands { &mut self.frame }
}
