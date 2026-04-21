//! Compositor.
//!
//! Collects paint output from each visible window, stacks them in z-order,
//! and produces the `FrameCommands` buffer the renderer backend consumes.
//! See ARCHITECTURE.md §10-11 for the renderer/surface model.

#[derive(Debug, Default, Clone)]
pub struct FrameCommands {
    pub ops: Vec<PaintOp>,
}

#[derive(Debug, Clone)]
pub enum PaintOp {
    Clear { rgba: u32 },
    FillRect { x: f32, y: f32, w: f32, h: f32, rgba: u32 },
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
}
