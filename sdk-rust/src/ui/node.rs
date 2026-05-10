//! Node types + fluent builders.
//!
//! Nodes are plain data — the UI tree is a `Vec<Node>` with edges encoded
//! as indices into that same vec. Builders are short-lived borrows of
//! [`Ui`] that collect config, then `.build()` pushes the final `Node`
//! and returns its id. Keeping nodes flat-and-enum (rather than `Box<dyn
//! Widget>`) avoids heap traffic in `on_render`, which runs every frame.

use super::{CrossAxis, MainAxis, Ui};

/// Index into the flat node vec inside a [`Ui`]. Valid only for the frame
/// on which it was issued; stale ids from previous frames panic on use.
pub type NodeId = usize;

/// One widget in the tree. Leaves (Text, Rect, Spacer) carry their own
/// content; containers (VStack, HStack, Frame) reference child ids.
/// `Button` is a composite leaf — it paints a rect + centred label and
/// takes part in hit-testing.
///
/// `Frame` stacks children on the z-axis in document order: the first
/// child paints at the bottom, the last on top. All children share the
/// same rect (the frame's inner box after padding), which makes it the
/// natural fit for "background rect + content on top" patterns.
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
        /// Corner radius in dp. `0.0` paints sharp corners (the
        /// classic AKML default); positive values paint rounded
        /// corners via the `OP_FILL_ROUND_RECT` paint op.
        radius: f32,
    },
    Spacer {
        min: (f32, f32),
    },
    /// Stone 13.0prep C3 — bitmap blit. `drawable_id` is whatever
    /// `res::drawable("name")` returned at the call site (`0` ==
    /// missing → renderer paints a magenta placeholder). Intrinsic
    /// size is `min`; layout treats it like a `Rect`.
    Image {
        drawable_id: u32,
        min: (f32, f32),
    },
    VStack {
        children: Vec<NodeId>,
        gap: f32,
        padding: (f32, f32, f32, f32), // top, right, bottom, left
        main: MainAxis,
        cross: CrossAxis,
    },
    HStack {
        children: Vec<NodeId>,
        gap: f32,
        padding: (f32, f32, f32, f32),
        main: MainAxis,
        cross: CrossAxis,
    },
    Frame {
        children: Vec<NodeId>,
        padding: (f32, f32, f32, f32),
        /// Optional rounded clip applied around the frame's content.
        /// `0.0` (the default) is no clip — children paint without
        /// masking, identical to the pre-clip Frame. Positive values
        /// emit `push_round_clip` before children and `pop_clip`
        /// after, so a circular avatar is `Frame::clip_radius(r/2)`
        /// containing an `Image`. Stacks naturally with intermediate
        /// padding: the clip rect is the frame's outer box (after
        /// layout, before padding), matching CSS's `border-radius`
        /// + `overflow: hidden` shape.
        clip_radius: f32,
    },
    /// Fixed-dimension grid. `cols × rows` cells laid out in row-major
    /// order; every cell is the same size. Extra children past
    /// `cols * rows` are ignored (not painted, not laid out) — AKML can
    /// over-declare rows and have empty trailing cells fall out.
    ///
    /// Semantics match Android's GridLayout more than CSS Grid: no
    /// auto-wrap, no span, no per-track sizing. If an app needs those,
    /// nest stacks or a second Grid. The minimal API is the sweet spot
    /// for keypads, toolbars, and icon trays — the cases that make Grid
    /// worth its existence in the first place.
    Grid {
        children: Vec<NodeId>,
        cols: u32,
        rows: u32,
        /// Horizontal spacing between columns (between-cell only — not
        /// before column 0 or after the last one; use `padding` for edge
        /// insets).
        gap_x: f32,
        /// Vertical spacing between rows.
        gap_y: f32,
        padding: (f32, f32, f32, f32),
    },
    Button {
        id: String,
        /// Semantic action fired on click, separate from `id`. When set,
        /// `Ui::was_triggered(action)` returns true on the frame a click
        /// landed inside this button. The split lets AKML rename, split,
        /// or merge buttons without touching Rust dispatch: `id` stays
        /// the layout/hit-test name; `action` is the stable contract the
        /// Activity reads. `None` means the button only responds to
        /// `was_clicked(id)` (the pre-Phase 3b polish API).
        action: Option<String>,
        label: String,
        label_size: f32,
        bg: u32,
        bg_pressed: u32,
        fg: u32,
        pad: (f32, f32), // x, y
        /// Corner radius in dp. The `Ui::button` factory sets `8.0` —
        /// the de-facto OS-native pill-button radius across Android,
        /// iOS, macOS Big Sur+. AKML can override per-button via the
        /// `cornerRadius` attribute. `0.0` produces the pre-11.6.9
        /// sharp-corner look.
        radius: f32,
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
    grow: f32,
}

impl<'u, 's, 'c, 'b> TextBuilder<'u, 's, 'c, 'b> {
    pub(crate) fn new(ui: &'u mut Ui<'s, 'c, 'b>, text: String) -> Self {
        Self { ui, text, size: 14.0, color: 0xfff0f4fa, grow: 0.0 }
    }
    pub fn size(mut self, size: f32) -> Self { self.size = size; self }
    pub fn color(mut self, color: u32) -> Self { self.color = color; self }
    /// Main-axis grow factor when placed in a stack. See [`super::NodeAttr::grow`].
    pub fn grow(mut self, factor: f32) -> Self { self.grow = factor; self }
    pub fn build(self) -> NodeId {
        let id = self.ui.push(Node::Text { text: self.text, size: self.size, color: self.color });
        self.ui.set_grow(id, self.grow);
        id
    }
}

pub struct RectBuilder<'u, 's, 'c, 'b> {
    ui: &'u mut Ui<'s, 'c, 'b>,
    color: u32,
    min: (f32, f32),
    radius: f32,
    grow: f32,
}

impl<'u, 's, 'c, 'b> RectBuilder<'u, 's, 'c, 'b> {
    pub(crate) fn new(ui: &'u mut Ui<'s, 'c, 'b>, color: u32) -> Self {
        Self { ui, color, min: (0.0, 0.0), radius: 0.0, grow: 0.0 }
    }
    pub fn size(mut self, w: f32, h: f32) -> Self { self.min = (w, h); self }
    /// Corner radius in dp. Default is `0.0` (sharp corners) so plain
    /// background fills behave as before; UI surfaces that want a
    /// native rounded look opt in with `.radius(8.0)`.
    pub fn radius(mut self, radius: f32) -> Self { self.radius = radius; self }
    pub fn grow(mut self, factor: f32) -> Self { self.grow = factor; self }
    pub fn build(self) -> NodeId {
        let id = self.ui.push(Node::Rect {
            color: self.color, min: self.min, radius: self.radius,
        });
        self.ui.set_grow(id, self.grow);
        id
    }
}

/// Stone 13.0prep C3 — `Image` builder. Set `.size(w, h)` for the
/// destination dp; the source PNG (looked up by `drawable_id`) is
/// stretched to fit. `drawable_id == 0` paints a magenta placeholder
/// — same fallback as the `@color/`/`@string/` missing path.
pub struct ImageBuilder<'u, 's, 'c, 'b> {
    ui: &'u mut Ui<'s, 'c, 'b>,
    drawable_id: u32,
    min: (f32, f32),
    grow: f32,
}

impl<'u, 's, 'c, 'b> ImageBuilder<'u, 's, 'c, 'b> {
    pub(crate) fn new(ui: &'u mut Ui<'s, 'c, 'b>, drawable_id: u32) -> Self {
        Self { ui, drawable_id, min: (0.0, 0.0), grow: 0.0 }
    }
    pub fn size(mut self, w: f32, h: f32) -> Self { self.min = (w, h); self }
    pub fn grow(mut self, factor: f32) -> Self { self.grow = factor; self }
    pub fn build(self) -> NodeId {
        let id = self.ui.push(Node::Image {
            drawable_id: self.drawable_id,
            min: self.min,
        });
        self.ui.set_grow(id, self.grow);
        id
    }
}

pub struct SpacerBuilder<'u, 's, 'c, 'b> {
    ui: &'u mut Ui<'s, 'c, 'b>,
    min: (f32, f32),
    grow: f32,
}

impl<'u, 's, 'c, 'b> SpacerBuilder<'u, 's, 'c, 'b> {
    pub(crate) fn new(ui: &'u mut Ui<'s, 'c, 'b>) -> Self {
        Self { ui, min: (0.0, 0.0), grow: 0.0 }
    }
    pub fn size(mut self, w: f32, h: f32) -> Self { self.min = (w, h); self }
    /// A grow>0 spacer with min (0,0) is the "push-to-end" pattern —
    /// consume all leftover main-axis space between neighbours.
    pub fn grow(mut self, factor: f32) -> Self { self.grow = factor; self }
    pub fn build(self) -> NodeId {
        let id = self.ui.push(Node::Spacer { min: self.min });
        self.ui.set_grow(id, self.grow);
        id
    }
}

pub struct FrameBuilder<'u, 's, 'c, 'b> {
    ui: &'u mut Ui<'s, 'c, 'b>,
    children: Vec<NodeId>,
    padding: (f32, f32, f32, f32),
    grow: f32,
    clip_radius: f32,
}

impl<'u, 's, 'c, 'b> FrameBuilder<'u, 's, 'c, 'b> {
    pub(crate) fn new(ui: &'u mut Ui<'s, 'c, 'b>, children: Vec<NodeId>) -> Self {
        Self { ui, children, padding: (0.0, 0.0, 0.0, 0.0), grow: 0.0, clip_radius: 0.0 }
    }
    /// Uniform padding on all four sides.
    pub fn padding(mut self, p: f32) -> Self {
        self.padding = (p, p, p, p); self
    }
    pub fn padding_trbl(mut self, top: f32, right: f32, bottom: f32, left: f32) -> Self {
        self.padding = (top, right, bottom, left); self
    }
    /// Main-axis grow factor for when this frame is itself a child of a
    /// stack — same semantics as the equivalent builder on `StackBuilder`.
    pub fn grow(mut self, factor: f32) -> Self { self.grow = factor; self }
    /// Apply a rounded clip around this frame's content. `radius` is
    /// in dp. Common values: `r/2` for a circle when the frame is
    /// square (avatars), `h/2` for a pill (chips, primary buttons),
    /// `8.0`–`14.0` for a card. `0.0` disables the clip and matches
    /// the pre-`clip_radius` Frame behaviour exactly.
    pub fn clip_radius(mut self, radius: f32) -> Self {
        self.clip_radius = radius.max(0.0);
        self
    }
    pub fn build(self) -> NodeId {
        let id = self.ui.push(Node::Frame {
            children: self.children,
            padding: self.padding,
            clip_radius: self.clip_radius,
        });
        self.ui.set_grow(id, self.grow);
        id
    }
}

pub struct GridBuilder<'u, 's, 'c, 'b> {
    ui: &'u mut Ui<'s, 'c, 'b>,
    children: Vec<NodeId>,
    cols: u32,
    rows: u32,
    gap_x: f32,
    gap_y: f32,
    padding: (f32, f32, f32, f32),
    grow: f32,
}

impl<'u, 's, 'c, 'b> GridBuilder<'u, 's, 'c, 'b> {
    pub(crate) fn new(
        ui: &'u mut Ui<'s, 'c, 'b>, children: Vec<NodeId>, cols: u32, rows: u32,
    ) -> Self {
        Self {
            ui, children, cols: cols.max(1), rows: rows.max(1),
            gap_x: 0.0, gap_y: 0.0,
            padding: (0.0, 0.0, 0.0, 0.0), grow: 0.0,
        }
    }
    /// Uniform gap applied on both axes. Shorthand for the common case
    /// where column and row spacing are equal.
    pub fn gap(mut self, gap: f32) -> Self {
        self.gap_x = gap; self.gap_y = gap; self
    }
    pub fn gap_xy(mut self, gap_x: f32, gap_y: f32) -> Self {
        self.gap_x = gap_x; self.gap_y = gap_y; self
    }
    pub fn padding(mut self, p: f32) -> Self {
        self.padding = (p, p, p, p); self
    }
    pub fn padding_trbl(mut self, top: f32, right: f32, bottom: f32, left: f32) -> Self {
        self.padding = (top, right, bottom, left); self
    }
    /// Main-axis grow factor for when this grid sits inside a stack.
    pub fn grow(mut self, factor: f32) -> Self { self.grow = factor; self }
    pub fn build(self) -> NodeId {
        let id = self.ui.push(Node::Grid {
            children: self.children,
            cols: self.cols, rows: self.rows,
            gap_x: self.gap_x, gap_y: self.gap_y,
            padding: self.padding,
        });
        self.ui.set_grow(id, self.grow);
        id
    }
}

pub struct StackBuilder<'u, 's, 'c, 'b> {
    ui: &'u mut Ui<'s, 'c, 'b>,
    vertical: bool,
    children: Vec<NodeId>,
    gap: f32,
    padding: (f32, f32, f32, f32),
    main: MainAxis,
    cross: CrossAxis,
    grow: f32,
}

impl<'u, 's, 'c, 'b> StackBuilder<'u, 's, 'c, 'b> {
    pub(crate) fn new_v(ui: &'u mut Ui<'s, 'c, 'b>, children: Vec<NodeId>) -> Self {
        Self {
            ui, vertical: true, children, gap: 0.0, padding: (0.0, 0.0, 0.0, 0.0),
            main: MainAxis::default(), cross: CrossAxis::default(), grow: 0.0,
        }
    }
    pub(crate) fn new_h(ui: &'u mut Ui<'s, 'c, 'b>, children: Vec<NodeId>) -> Self {
        Self {
            ui, vertical: false, children, gap: 0.0, padding: (0.0, 0.0, 0.0, 0.0),
            main: MainAxis::default(), cross: CrossAxis::default(), grow: 0.0,
        }
    }
    pub fn gap(mut self, gap: f32) -> Self { self.gap = gap; self }
    /// Uniform padding on all four sides.
    pub fn padding(mut self, p: f32) -> Self {
        self.padding = (p, p, p, p); self
    }
    pub fn padding_trbl(mut self, top: f32, right: f32, bottom: f32, left: f32) -> Self {
        self.padding = (top, right, bottom, left); self
    }
    /// Main-axis distribution for children. Non-`Start` values cause the
    /// stack to fill its slot on the main axis so there is leftover space
    /// to distribute. Default: `Start`.
    pub fn main(mut self, main: MainAxis) -> Self { self.main = main; self }
    /// Cross-axis alignment (and stretching, for `Stretch`). Non-`Start`
    /// values cause the stack to fill its slot on the cross axis. Default:
    /// `Start`.
    pub fn cross(mut self, cross: CrossAxis) -> Self { self.cross = cross; self }
    /// Main-axis grow factor for when this stack is itself a child of
    /// another stack (nested layouts).
    pub fn grow(mut self, factor: f32) -> Self { self.grow = factor; self }
    pub fn build(self) -> NodeId {
        let node = if self.vertical {
            Node::VStack {
                children: self.children, gap: self.gap, padding: self.padding,
                main: self.main, cross: self.cross,
            }
        } else {
            Node::HStack {
                children: self.children, gap: self.gap, padding: self.padding,
                main: self.main, cross: self.cross,
            }
        };
        let id = self.ui.push(node);
        self.ui.set_grow(id, self.grow);
        id
    }
}
