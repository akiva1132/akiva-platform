//! Node types + fluent builders.
//!
//! Nodes are plain data — the UI tree is a `Vec<Node>` with edges encoded
//! as indices into that same vec. Builders are short-lived borrows of
//! [`Ui`] that collect config, then `.build()` pushes the final `Node`
//! and returns its id. Keeping nodes flat-and-enum (rather than `Box<dyn
//! Widget>`) avoids heap traffic in `on_render`, which runs every frame.

use super::Ui;

/// Index into the flat node vec inside a [`Ui`]. Valid only for the frame
/// on which it was issued; stale ids from previous frames panic on use.
pub type NodeId = usize;

/// One widget in the tree. Leaves (Text, Rect, Spacer) carry their own
/// content; containers (VStack, HStack, Padding) reference child ids.
/// `Button` is a composite leaf — it paints a rect + centred label and
/// takes part in hit-testing.
#[derive(Debug)]
pub enum Node {
    Text {
        text: String,
        size: f32,
        color: u32,
    },
    Rect {
        color: u32,
        min: (f32, f32),
    },
    Spacer {
        min: (f32, f32),
    },
    VStack {
        children: Vec<NodeId>,
        gap: f32,
        padding: (f32, f32, f32, f32), // top, right, bottom, left
    },
    HStack {
        children: Vec<NodeId>,
        gap: f32,
        padding: (f32, f32, f32, f32),
    },
    Button {
        id: String,
        label: String,
        label_size: f32,
        bg: u32,
        bg_pressed: u32,
        fg: u32,
        pad: (f32, f32), // x, y
    },
}

// ── Builders ─────────────────────────────────────────────────────────
//
// Each builder borrows &mut Ui for the duration of its chain. `.build()`
// consumes the builder and commits the node. Required once — if callers
// drop the builder without `.build()`, nothing is inserted (safe but
// confusing). A Drop impl would panic on unused builders, but we prefer
// silent no-op to keep the lifetime story simple.

pub struct TextBuilder<'u, 's, 'c, 'b> {
    ui: &'u mut Ui<'s, 'c, 'b>,
    text: String,
    size: f32,
    color: u32,
}

impl<'u, 's, 'c, 'b> TextBuilder<'u, 's, 'c, 'b> {
    pub(crate) fn new(ui: &'u mut Ui<'s, 'c, 'b>, text: String) -> Self {
        Self { ui, text, size: 14.0, color: 0xfff0f4fa }
    }
    pub fn size(mut self, size: f32) -> Self { self.size = size; self }
    pub fn color(mut self, color: u32) -> Self { self.color = color; self }
    pub fn build(self) -> NodeId {
        self.ui.push(Node::Text { text: self.text, size: self.size, color: self.color })
    }
}

pub struct RectBuilder<'u, 's, 'c, 'b> {
    ui: &'u mut Ui<'s, 'c, 'b>,
    color: u32,
    min: (f32, f32),
}

impl<'u, 's, 'c, 'b> RectBuilder<'u, 's, 'c, 'b> {
    pub(crate) fn new(ui: &'u mut Ui<'s, 'c, 'b>, color: u32) -> Self {
        Self { ui, color, min: (0.0, 0.0) }
    }
    pub fn size(mut self, w: f32, h: f32) -> Self { self.min = (w, h); self }
    pub fn build(self) -> NodeId {
        self.ui.push(Node::Rect { color: self.color, min: self.min })
    }
}

pub struct SpacerBuilder<'u, 's, 'c, 'b> {
    ui: &'u mut Ui<'s, 'c, 'b>,
    min: (f32, f32),
}

impl<'u, 's, 'c, 'b> SpacerBuilder<'u, 's, 'c, 'b> {
    pub(crate) fn new(ui: &'u mut Ui<'s, 'c, 'b>) -> Self {
        Self { ui, min: (0.0, 0.0) }
    }
    pub fn size(mut self, w: f32, h: f32) -> Self { self.min = (w, h); self }
    pub fn build(self) -> NodeId {
        self.ui.push(Node::Spacer { min: self.min })
    }
}

pub struct StackBuilder<'u, 's, 'c, 'b> {
    ui: &'u mut Ui<'s, 'c, 'b>,
    vertical: bool,
    children: Vec<NodeId>,
    gap: f32,
    padding: (f32, f32, f32, f32),
}

impl<'u, 's, 'c, 'b> StackBuilder<'u, 's, 'c, 'b> {
    pub(crate) fn new_v(ui: &'u mut Ui<'s, 'c, 'b>, children: Vec<NodeId>) -> Self {
        Self { ui, vertical: true, children, gap: 0.0, padding: (0.0, 0.0, 0.0, 0.0) }
    }
    pub(crate) fn new_h(ui: &'u mut Ui<'s, 'c, 'b>, children: Vec<NodeId>) -> Self {
        Self { ui, vertical: false, children, gap: 0.0, padding: (0.0, 0.0, 0.0, 0.0) }
    }
    pub fn gap(mut self, gap: f32) -> Self { self.gap = gap; self }
    /// Uniform padding on all four sides.
    pub fn padding(mut self, p: f32) -> Self {
        self.padding = (p, p, p, p); self
    }
    pub fn padding_trbl(mut self, top: f32, right: f32, bottom: f32, left: f32) -> Self {
        self.padding = (top, right, bottom, left); self
    }
    pub fn build(self) -> NodeId {
        let node = if self.vertical {
            Node::VStack { children: self.children, gap: self.gap, padding: self.padding }
        } else {
            Node::HStack { children: self.children, gap: self.gap, padding: self.padding }
        };
        self.ui.push(node)
    }
}
