//! Layout engine.
//!
//! Flexbox-ish single-pass layout. Each widget gets `measure(constraints)`
//! and returns a `LayoutResult { size, baseline }`. Parent widgets then
//! position their children.

#[derive(Debug, Clone, Copy)]
pub struct Constraints {
    pub min_width: f32,
    pub max_width: f32,
    pub min_height: f32,
    pub max_height: f32,
}

impl Constraints {
    pub fn tight(w: f32, h: f32) -> Self {
        Self { min_width: w, max_width: w, min_height: h, max_height: h }
    }
    pub fn loose(w: f32, h: f32) -> Self {
        Self { min_width: 0.0, max_width: w, min_height: 0.0, max_height: h }
    }
    pub fn clamp(&self, w: f32, h: f32) -> (f32, f32) {
        (w.clamp(self.min_width, self.max_width), h.clamp(self.min_height, self.max_height))
    }
}

#[derive(Debug, Clone, Copy)]
pub struct LayoutResult {
    pub width: f32,
    pub height: f32,
    pub baseline: Option<f32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Axis {
    Horizontal,
    Vertical,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MainAxisAlignment {
    Start,
    Center,
    End,
    SpaceBetween,
    SpaceAround,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CrossAxisAlignment {
    Start,
    Center,
    End,
    Stretch,
}
