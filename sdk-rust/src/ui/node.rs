//! Node types + fluent builders.
//!
//! Nodes are plain data — the UI tree is a `Vec<Node>` with edges encoded
//! as indices into that same vec. Builders are short-lived borrows of
//! [`Ui`] that collect config, then `.build()` pushes the final `Node`
//! and returns its id. Keeping nodes flat-and-enum (rather than `Box<dyn
//! Widget>`) avoids heap traffic in `on_render`, which runs every frame.

use super::{CrossAxis, MainAxis, ScrollAxis, SnapMode, Ui};

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
        /// Maximum lines before truncation. `1` (the default the SDK
        /// builder sets) is single-line — long text overflows the
        /// rect's width without wrap, matching the calculator-button
        /// label behaviour on which the rest of the platform was
        /// originally tuned. `> 1` enables greedy word-wrap inside
        /// the available width passed to `measure`; the last line
        /// gets a trailing `…` if more text remains. `u32::MAX`
        /// means "wrap as many lines as needed, never truncate" —
        /// the right choice for body copy that should reflow rather
        /// than spill.
        max_lines: u32,
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
        /// Lock the frame's measured size to `(w, h)`, regardless of
        /// children's intrinsic sizes. `None` (the default) measures
        /// from `max(child_size)` like a normal z-stack. Used for
        /// "card with overflowing content": the bg Rect declares the
        /// visible bounds and the body Text measures inside *those*
        /// bounds, so wrap and ellipsis behave against the visible
        /// width rather than the parent's full slot. Mirrors what a
        /// CSS `width: 360px` does for a flex container.
        size: Option<(f32, f32)>,
        /// Stage 3.3 — opacity applied to the children's paint as a
        /// `push_opacity` / `pop_opacity` wrap. `1.0` is the default
        /// (no wrap; the renderer never sees the pair). Values
        /// outside `[0, 1]` are clamped at paint time. Paint-only:
        /// does NOT affect layout, hit-test, or semantic visibility.
        /// Use [`crate::AnimatedValue::value`] to drive a fade
        /// animation here.
        opacity: f32,
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
    /// Single-child scroll viewport. The child is measured against
    /// unlimited vertical space (so its intrinsic height becomes the
    /// scroll *content height*) and painted with a `-scroll_y` y-offset
    /// inside a clip rect the viewport size. `key` is the caller-stable
    /// identifier the SDK uses to persist scroll position across frames
    /// in `UiState::scroll_states`; pick a string that won't drift
    /// between rebuilds (`"feed"`, `"settings-body"`, …).
    ///
    /// Without `viewport_w`/`viewport_h`, the scroll inherits its slot
    /// size (typical inside a `VStack` with `cross: Stretch`); setting
    /// them locks the viewport exactly the way `Frame::size` locks a
    /// card — useful when the parent slot is bigger than the area you
    /// actually want scrolling to live in.
    Scroll {
        child: NodeId,
        key: String,
        viewport_w: Option<f32>,
        viewport_h: Option<f32>,
        /// Which axis the viewport scrolls along. Stage 2c shipped
        /// `Vertical` only; Stage 2d added `Horizontal` for
        /// carousel-style layouts. The default
        /// (`ScrollAxis::Vertical`) keeps every pre-2d call site
        /// behaving identically.
        axis: ScrollAxis,
    },
    /// Virtualised list — Android `RecyclerView` / Compose `LazyColumn`
    /// in shape. Caller declares `count` items at a uniform `item_h`;
    /// only items in the viewport (+ overscan) are realised. Memory is
    /// bounded by viewport, not item count — a 10 000-row feed costs
    /// the same as a 50-row one.
    ///
    /// `realized` is `(index, NodeId)` for each realised item, in
    /// index order. Layout/paint walk `realized`; index gaps are the
    /// virtualised items the SDK skipped on purpose.
    LazyColumn {
        key: String,
        count: u32,
        item_h: f32,
        gap: f32,
        padding: (f32, f32, f32, f32),
        viewport_w: Option<f32>,
        viewport_h: Option<f32>,
        realized: Vec<(u32, NodeId)>,
        /// Snap behaviour after a fling settles. `SnapMode::None`
        /// (the default) preserves the pre-2d "land wherever the
        /// fling stops" feel; `Item` rounds to the item grid;
        /// `Page` rounds to the viewport grid (rare for vertical
        /// feeds — there mainly for symmetry with LazyRow).
        snap: SnapMode,
    },
    /// Horizontal twin of `LazyColumn` — Compose `LazyRow` / Android
    /// `RecyclerView` with `HORIZONTAL` orientation. Uniform
    /// `item_w`; items in `[first - overscan, last + overscan]` of
    /// the visible x-band are realised.
    ///
    /// Same `realized: Vec<(u32, NodeId)>` shape as LazyColumn; the
    /// builder differs only in `item_w` vs `item_h` and which axis
    /// the realisation window is computed on.
    LazyRow {
        key: String,
        count: u32,
        item_w: f32,
        gap: f32,
        padding: (f32, f32, f32, f32),
        viewport_w: Option<f32>,
        viewport_h: Option<f32>,
        realized: Vec<(u32, NodeId)>,
        /// Snap behaviour after a fling settles. Carousels use
        /// `SnapMode::Page` (one item per page = viewport width).
        snap: SnapMode,
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
    max_lines: u32,
}

impl<'u, 's, 'c, 'b> TextBuilder<'u, 's, 'c, 'b> {
    pub(crate) fn new(ui: &'u mut Ui<'s, 'c, 'b>, text: String) -> Self {
        Self { ui, text, size: 14.0, color: 0xfff0f4fa, grow: 0.0, max_lines: 1 }
    }
    pub fn size(mut self, size: f32) -> Self { self.size = size; self }
    pub fn color(mut self, color: u32) -> Self { self.color = color; self }
    /// Main-axis grow factor when placed in a stack. See [`super::NodeAttr::grow`].
    pub fn grow(mut self, factor: f32) -> Self { self.grow = factor; self }
    /// Set a maximum line count. The default is `1` (single line, no
    /// wrap, overflow continues past the rect's right edge — matches
    /// the calculator-button label behaviour). Pass `n > 1` to enable
    /// greedy word-wrap inside the available width; the last line
    /// gets a trailing `…` if more text remains.
    pub fn max_lines(mut self, n: u32) -> Self { self.max_lines = n.max(1); self }
    /// Wrap as many lines as needed, never truncate. Use for body
    /// copy that should reflow rather than spill (article paragraphs,
    /// long descriptions). Equivalent to `max_lines(u32::MAX)`.
    pub fn wrap(mut self) -> Self { self.max_lines = u32::MAX; self }
    pub fn build(self) -> NodeId {
        let id = self.ui.push(Node::Text {
            text: self.text, size: self.size, color: self.color,
            max_lines: self.max_lines,
        });
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
    size: Option<(f32, f32)>,
    opacity: f32,
}

impl<'u, 's, 'c, 'b> FrameBuilder<'u, 's, 'c, 'b> {
    pub(crate) fn new(ui: &'u mut Ui<'s, 'c, 'b>, children: Vec<NodeId>) -> Self {
        Self {
            ui, children, padding: (0.0, 0.0, 0.0, 0.0),
            grow: 0.0, clip_radius: 0.0, size: None, opacity: 1.0,
        }
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
    /// Lock the frame's measured size, overriding what children's
    /// intrinsic sizes would compute. Use for cards whose visible
    /// bounds (the bg Rect) should also bound their content's wrap
    /// width — without this, a long single-line Text inside a card
    /// would stretch the frame to the text's full unwrapped width.
    pub fn size(mut self, w: f32, h: f32) -> Self {
        self.size = Some((w.max(0.0), h.max(0.0)));
        self
    }
    /// Stage 3.3 — paint the frame's content through an opacity
    /// layer. `alpha == 1.0` is a no-op; values outside `[0, 1]`
    /// are clamped at paint time. Paint-only: this does NOT shrink
    /// the frame's measured bounds, change hit-test behaviour, or
    /// affect what the inspector reports about the children. Use
    /// it for fade-in / fade-out groups, dim-while-disabled cards,
    /// and image crossfades driven by `AnimatedValue<f32>`.
    pub fn opacity(mut self, alpha: f32) -> Self {
        self.opacity = alpha.clamp(0.0, 1.0);
        self
    }

    /// Sugar around [`Self::opacity`] for the common case: a
    /// `Spring`-driven `AnimatedValue` whose value is the target
    /// alpha. Reads `.value()` at build time, so the frame snaps
    /// to the current value on each rebuild — drive the animation
    /// from `on_render`'s wall-clock-delta tick and rebuild every
    /// frame to see motion.
    pub fn opacity_av(mut self, av: &crate::ui::AnimatedValue<f32>) -> Self {
        self.opacity = av.value().clamp(0.0, 1.0);
        self
    }
    pub fn build(self) -> NodeId {
        let id = self.ui.push(Node::Frame {
            children: self.children,
            padding: self.padding,
            clip_radius: self.clip_radius,
            size: self.size,
            opacity: self.opacity,
        });
        self.ui.set_grow(id, self.grow);
        id
    }
}

pub struct ScrollBuilder<'u, 's, 'c, 'b> {
    ui: &'u mut Ui<'s, 'c, 'b>,
    child: NodeId,
    key: String,
    viewport_w: Option<f32>,
    viewport_h: Option<f32>,
    axis: ScrollAxis,
    grow: f32,
}

impl<'u, 's, 'c, 'b> ScrollBuilder<'u, 's, 'c, 'b> {
    pub(crate) fn new(ui: &'u mut Ui<'s, 'c, 'b>, child: NodeId) -> Self {
        Self {
            ui, child, key: String::new(),
            viewport_w: None, viewport_h: None,
            axis: ScrollAxis::Vertical, grow: 0.0,
        }
    }
    /// Internal constructor used by `Ui::scroll_h` — seed the builder
    /// already on the horizontal axis. The public API uses
    /// [`Self::axis`] / the `Ui::scroll`+`Ui::scroll_h` factories.
    pub(crate) fn new_with_axis(
        ui: &'u mut Ui<'s, 'c, 'b>, child: NodeId, axis: ScrollAxis,
    ) -> Self {
        Self {
            ui, child, key: String::new(),
            viewport_w: None, viewport_h: None,
            axis, grow: 0.0,
        }
    }
    /// Caller-stable identifier the SDK persists scroll position under
    /// in `UiState::scroll_states`. The same string in the next frame
    /// looks up the same cell. Defaults to empty — anonymous scrolls
    /// still work (each frame starts at scroll_y=0) but tests can't
    /// address them via `__akiva.scrollOf(key)`.
    pub fn key(mut self, key: impl Into<String>) -> Self { self.key = key.into(); self }
    /// Lock the viewport size. Skip to inherit from the slot.
    pub fn size(mut self, w: f32, h: f32) -> Self {
        self.viewport_w = Some(w.max(0.0));
        self.viewport_h = Some(h.max(0.0));
        self
    }
    /// Lock the viewport height only — useful when the scroll lives
    /// inside a `VStack { cross: Stretch }` that handles width.
    pub fn height(mut self, h: f32) -> Self {
        self.viewport_h = Some(h.max(0.0));
        self
    }
    /// Lock the viewport width only — symmetric with [`Self::height`].
    /// Useful for horizontal scrolls inside an `HStack { cross:
    /// Stretch }`.
    pub fn width(mut self, w: f32) -> Self {
        self.viewport_w = Some(w.max(0.0));
        self
    }
    /// Switch the scroll axis. `Vertical` (the default) scrolls along
    /// y; `Horizontal` scrolls along x. Prefer the [`Ui::scroll_h`]
    /// factory for new horizontal scrolls — it's the same effect
    /// with one less builder call.
    pub fn axis(mut self, axis: ScrollAxis) -> Self { self.axis = axis; self }
    pub fn grow(mut self, factor: f32) -> Self { self.grow = factor; self }
    pub fn build(self) -> NodeId {
        let id = self.ui.push(Node::Scroll {
            child: self.child,
            key: self.key,
            viewport_w: self.viewport_w,
            viewport_h: self.viewport_h,
            axis: self.axis,
        });
        self.ui.set_grow(id, self.grow);
        id
    }
}

/// Builder for [`Node::LazyColumn`]. Terminating method is `.items()`
/// rather than `.build()` because the body closure is the load-bearing
/// half of the contract — callers don't get to lazy-evaluate items
/// otherwise.
pub struct LazyColumnBuilder<'u, 's, 'c, 'b> {
    ui: &'u mut Ui<'s, 'c, 'b>,
    key: String,
    count: u32,
    item_h: f32,
    gap: f32,
    padding: (f32, f32, f32, f32),
    viewport_w: Option<f32>,
    viewport_h: Option<f32>,
    overscan: u32,
    snap: SnapMode,
    grow: f32,
}

/// Default per-side overscan (items realised past the visible edge).
/// Smooth-scroll glides one extra item in/out without showing
/// backdrop at the edges; bump if you see pop-in at 60fps scroll.
pub(crate) const DEFAULT_OVERSCAN: u32 = 2;

/// First-frame fallback cap: when the viewport height is unknown
/// (no `.size(_, h)` and no cached cell), the SDK realises the first
/// `LAZY_COLD_START_CAP` items so a first-paint has something to
/// show. Second frame uses the just-measured viewport for real
/// virtualisation.
pub(crate) const LAZY_COLD_START_CAP: u32 = 30;

/// Pure-function realisation window: returns the `[start, end)`
/// index range that LazyColumn / LazyRow should materialise this
/// frame. Shared by both builders so the off-by-one risk only
/// lives in one place.
///
/// - `stride` = item main-axis size + gap (must be `> 0` for the
///   normal path; otherwise falls back to cold-start).
/// - `viewport_main` = viewport size along the scroll axis (height
///   for LazyColumn, width for LazyRow). `≤ 0` triggers cold-start.
/// - `scroll` = current main-axis offset.
/// - `padding_start` = padding at the leading edge along the main
///   axis (`padding.0` for LazyColumn / vertical, `padding.3` for
///   LazyRow / horizontal).
pub(crate) fn realise_window(
    count: u32, stride: f32, viewport_main: f32, scroll: f32,
    padding_start: f32, overscan: u32,
) -> (u32, u32) {
    if count == 0 {
        return (0, 0);
    }
    if viewport_main <= 0.0 || stride <= 0.0 {
        // Cold start — no measurement yet. Realise a bounded slice
        // so first paint shows something.
        return (0, count.min(LAZY_COLD_START_CAP));
    }
    // Visible band runs from `scroll` to `scroll + viewport_main`
    // (in content coords). First fully-or-partly visible item:
    //   first = floor((scroll - padding_start) / stride)
    // Last partly visible:
    //   last  = floor((scroll + viewport_main - padding_start) / stride)
    let band_lo = (scroll - padding_start).max(0.0);
    let band_hi = (scroll + viewport_main - padding_start).max(0.0);
    let first = (band_lo / stride).floor() as i64;
    let last = (band_hi / stride).floor() as i64;
    let s = (first - overscan as i64).max(0) as u32;
    let e = ((last + overscan as i64).max(0) as u32 + 1).min(count);
    (s, e)
}

impl<'u, 's, 'c, 'b> LazyColumnBuilder<'u, 's, 'c, 'b> {
    pub(crate) fn new(ui: &'u mut Ui<'s, 'c, 'b>, count: u32) -> Self {
        Self {
            ui, key: String::new(), count,
            item_h: 0.0, gap: 0.0,
            padding: (0.0, 0.0, 0.0, 0.0),
            viewport_w: None, viewport_h: None,
            overscan: DEFAULT_OVERSCAN,
            snap: SnapMode::None,
            grow: 0.0,
        }
    }
    /// Pick a snap behaviour. Default: `SnapMode::None` (no snap,
    /// fling settles wherever decay finishes). Set to
    /// `SnapMode::Item` for a list that always lands on a card
    /// edge; `SnapMode::Page` for vertical "page swipe" lists
    /// (rare; horizontal carousels are the common case — see
    /// `LazyRowBuilder::snap`).
    pub fn snap(mut self, mode: SnapMode) -> Self { self.snap = mode; self }
    pub fn key(mut self, key: impl Into<String>) -> Self { self.key = key.into(); self }
    /// Required — every item is exactly this tall.
    pub fn item_h(mut self, h: f32) -> Self { self.item_h = h.max(0.0); self }
    pub fn gap(mut self, g: f32) -> Self { self.gap = g.max(0.0); self }
    pub fn padding(mut self, p: f32) -> Self { self.padding = (p, p, p, p); self }
    pub fn padding_trbl(mut self, t: f32, r: f32, b: f32, l: f32) -> Self {
        self.padding = (t, r, b, l); self
    }
    /// Lock the viewport size. Without this the list inherits its slot
    /// size — typical when sitting inside a `VStack { cross: Stretch }`.
    pub fn size(mut self, w: f32, h: f32) -> Self {
        self.viewport_w = Some(w.max(0.0));
        self.viewport_h = Some(h.max(0.0));
        self
    }
    pub fn height(mut self, h: f32) -> Self {
        self.viewport_h = Some(h.max(0.0));
        self
    }
    /// Items realised per side past the visible edge (default 2).
    /// Higher = smoother edge fade-in at 60fps drag; lower = lower
    /// memory + less wasted measure.
    pub fn overscan(mut self, n: u32) -> Self { self.overscan = n; self }
    pub fn grow(mut self, factor: f32) -> Self { self.grow = factor; self }

    /// Build the LazyColumn by invoking `item_fn(ui, i)` for each
    /// index in the visible range (+ overscan), pushing the
    /// `Node::LazyColumn` once the items are realised, and returning
    /// its NodeId. The closure runs *synchronously inside the build
    /// pass* — no lazy futures, no per-frame allocation outside the
    /// realised slice.
    pub fn items<F>(self, mut item_fn: F) -> NodeId
    where
        F: FnMut(&mut Ui<'s, 'c, 'b>, u32) -> NodeId,
    {
        let LazyColumnBuilder {
            ui, key, count, item_h, gap, padding,
            viewport_w, viewport_h, overscan, snap, grow,
        } = self;

        // Pick viewport_h: explicit `.size(_, h)` / `.height(h)` first,
        // then last frame's cached cell, else 0 (cold-start fallback).
        let cached_viewport_h = ui.scroll_cell_viewport_main(&key);
        let cached_scroll = ui.scroll_offset(&key);
        let resolved_vh = viewport_h.unwrap_or(cached_viewport_h);

        // Item stride (item_h + gap). One row of N items takes
        // N*(item_h+gap) - gap height in the content; the start of
        // index i sits at `padding.top + i * stride`.
        let stride = item_h + gap;
        let (start_idx, end_idx) = realise_window(
            count, stride, resolved_vh, cached_scroll,
            padding.0 /* top */, overscan,
        );

        // Materialise visible items via the caller's closure.
        let mut realized: Vec<(u32, NodeId)> = Vec::with_capacity(
            end_idx.saturating_sub(start_idx) as usize
        );
        for i in start_idx..end_idx {
            let id = item_fn(ui, i);
            realized.push((i, id));
        }

        let id = ui.push(Node::LazyColumn {
            key, count, item_h, gap, padding,
            viewport_w, viewport_h, realized, snap,
        });
        ui.set_grow(id, grow);
        id
    }
}

/// Builder for [`Node::LazyRow`]. Mirror of [`LazyColumnBuilder`]
/// with `item_w` taking `item_h`'s role and the realisation window
/// computed along the x-axis. Terminating method is `.items()`.
pub struct LazyRowBuilder<'u, 's, 'c, 'b> {
    ui: &'u mut Ui<'s, 'c, 'b>,
    key: String,
    count: u32,
    item_w: f32,
    gap: f32,
    padding: (f32, f32, f32, f32),
    viewport_w: Option<f32>,
    viewport_h: Option<f32>,
    overscan: u32,
    snap: SnapMode,
    grow: f32,
}

impl<'u, 's, 'c, 'b> LazyRowBuilder<'u, 's, 'c, 'b> {
    pub(crate) fn new(ui: &'u mut Ui<'s, 'c, 'b>, count: u32) -> Self {
        Self {
            ui, key: String::new(), count,
            item_w: 0.0, gap: 0.0,
            padding: (0.0, 0.0, 0.0, 0.0),
            viewport_w: None, viewport_h: None,
            overscan: DEFAULT_OVERSCAN,
            snap: SnapMode::None,
            grow: 0.0,
        }
    }
    /// Pick a snap behaviour. `SnapMode::Page` makes the row
    /// behave like Compose's `HorizontalPager` — one item per
    /// page (page size = viewport width); `SnapMode::Item`
    /// snaps to each card edge instead.
    pub fn snap(mut self, mode: SnapMode) -> Self { self.snap = mode; self }
    pub fn key(mut self, key: impl Into<String>) -> Self { self.key = key.into(); self }
    /// Required — every item is exactly this wide.
    pub fn item_w(mut self, w: f32) -> Self { self.item_w = w.max(0.0); self }
    pub fn gap(mut self, g: f32) -> Self { self.gap = g.max(0.0); self }
    pub fn padding(mut self, p: f32) -> Self { self.padding = (p, p, p, p); self }
    pub fn padding_trbl(mut self, t: f32, r: f32, b: f32, l: f32) -> Self {
        self.padding = (t, r, b, l); self
    }
    /// Lock the viewport size. Without this the row inherits its
    /// slot — typical when sitting inside an `HStack { cross:
    /// Stretch }` or a `VStack` that handles vertical sizing.
    pub fn size(mut self, w: f32, h: f32) -> Self {
        self.viewport_w = Some(w.max(0.0));
        self.viewport_h = Some(h.max(0.0));
        self
    }
    pub fn width(mut self, w: f32) -> Self {
        self.viewport_w = Some(w.max(0.0));
        self
    }
    pub fn height(mut self, h: f32) -> Self {
        self.viewport_h = Some(h.max(0.0));
        self
    }
    pub fn overscan(mut self, n: u32) -> Self { self.overscan = n; self }
    pub fn grow(mut self, factor: f32) -> Self { self.grow = factor; self }

    /// Build the LazyRow by invoking `item_fn(ui, i)` for each
    /// realised index, pushing the `Node::LazyRow`, and returning
    /// its NodeId. Same shape as `LazyColumnBuilder::items` — the
    /// closure runs synchronously in the build pass.
    pub fn items<F>(self, mut item_fn: F) -> NodeId
    where
        F: FnMut(&mut Ui<'s, 'c, 'b>, u32) -> NodeId,
    {
        let LazyRowBuilder {
            ui, key, count, item_w, gap, padding,
            viewport_w, viewport_h, overscan, snap, grow,
        } = self;

        // Main-axis viewport for a LazyRow is the row's *width*
        // (the cell's `viewport_h` field — see ScrollCell docs).
        let cached_viewport_main = ui.scroll_cell_viewport_main(&key);
        let cached_scroll = ui.scroll_offset(&key);
        let resolved_vw = viewport_w.unwrap_or(cached_viewport_main);

        let stride = item_w + gap;
        let (start_idx, end_idx) = realise_window(
            count, stride, resolved_vw, cached_scroll,
            padding.3 /* left */, overscan,
        );

        let mut realized: Vec<(u32, NodeId)> = Vec::with_capacity(
            end_idx.saturating_sub(start_idx) as usize
        );
        for i in start_idx..end_idx {
            let id = item_fn(ui, i);
            realized.push((i, id));
        }

        let id = ui.push(Node::LazyRow {
            key, count, item_w, gap, padding,
            viewport_w, viewport_h, realized, snap,
        });
        ui.set_grow(id, grow);
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
