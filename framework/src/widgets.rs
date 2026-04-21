//! Widget tree.
//!
//! Widgets are the runtime counterpart of AKML nodes: a tree of objects
//! that participate in layout, input hit-testing, and drawing. MVP set:
//! View (base), Text, Button, Column, Row, Stack, Image.

use crate::layout::{Constraints, LayoutResult};

pub type WidgetId = u64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dirty {
    Clean,
    Layout,
    Paint,
    All,
}

pub trait Widget {
    fn id(&self) -> WidgetId;
    fn measure(&mut self, constraints: Constraints) -> LayoutResult;
    fn paint(&self, target: &mut dyn PaintTarget);
    fn on_input(&mut self, event: &InputEvent) -> InputDisposition { let _ = event; InputDisposition::Unhandled }
}

#[derive(Debug, Clone, Copy)]
pub enum InputDisposition {
    Unhandled,
    Consumed,
}

#[derive(Debug, Clone)]
pub enum InputEvent {
    Tap { x: f32, y: f32 },
    Drag { x: f32, y: f32, dx: f32, dy: f32 },
    KeyDown { code: u32, modifiers: u8 },
}

/// Abstract target for paint commands. In Phase 2 this is backed by the
/// FrameCommands buffer that the compositor hands to the renderer backend.
pub trait PaintTarget {
    fn fill_rect(&mut self, x: f32, y: f32, w: f32, h: f32, rgba: u32);
    fn draw_text(&mut self, x: f32, y: f32, size: f32, rgba: u32, text: &str);
    fn push_clip(&mut self, x: f32, y: f32, w: f32, h: f32);
    fn pop_clip(&mut self);
}
