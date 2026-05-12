//! Immediate-mode widget toolkit.
//!
//! Apps describe their UI as a tree built fresh each frame. This module
//! owns the tree, runs measure + layout, and emits paint ops through the
//! SDK [`Canvas`]. The ABI sees only the resulting paint stream — widgets
//! are an SDK abstraction, not part of the host↔app contract.
//!
//! Usage sketch:
//!
//! ```ignore
//! use akiva_sdk::prelude::*;
//! use akiva_sdk::ui::{Ui, UiState};
//!
//! #[derive(Default)]
//! struct MyApp { state: UiState, count: u32 }
//!
//! impl Activity for MyApp {
//!     fn on_pointer(&mut self, _ctx: &mut Context, ev: PointerEvent) {
//!         self.state.feed_pointer(ev);
//!     }
//!     fn on_render(&mut self, ctx: &mut Context, c: &mut Canvas) {
//!         let mut ui = Ui::new(&mut self.state, ctx, c);
//!         let title = ui.text("Hello").size(24.0).build();
//!         let btn = ui.button("inc", "+1");
//!         let root = ui.v_stack(&[title, btn]).gap(12.0).padding(24.0).build();
//!         ui.present(root);
//!         if ui.was_clicked("inc") { self.count += 1; }
//!     }
//! }
//! ```
//!
//! ## State across frames
//!
//! Click detection needs to correlate the `PointerDown` events that
//! arrived between last frame and this one with the hit rects of the
//! widgets *currently on screen*. That means we keep two things in the
//! app-owned [`UiState`]:
//!
//! - `pending_clicks`: pointer-down positions since the last `present()`,
//!   fed by the app from `on_pointer`.
//! - `hit_rects`: map from caller-chosen `id` → rect, populated during
//!   `present()`.
//!
//! `was_clicked(id)` then reads both. Frame N's clicks are visible in
//! frame N (same-frame response) because `on_pointer` fires before
//! `on_render` in the host's dispatch order.
//!
//! Buttons can additionally carry a semantic [`action`][Ui::action]
//! (surfaced as `onClick="…"` in AKML). After `present`, the Activity
//! queries those via [`was_triggered`][Ui::was_triggered] instead of
//! `was_clicked` — the action name is the stable contract, so renaming
//! or reshuffling buttons in AKML doesn't break Rust dispatch.
//!
//! ## Layout model
//!
//! Two-pass: bottom-up `measure` to get intrinsic sizes, top-down `layout`
//! to place every node within its parent. VStack sums children vertically
//! and takes max child width; HStack is the mirror. Leaves (Text, Rect)
//! have a content-derived intrinsic size.
//!
//! Stacks expose [`MainAxis`] and [`CrossAxis`] controls through the
//! builder API. Non-`Start` variants cause the stack to fill its slot on
//! the corresponding axis so there is leftover space to distribute; the
//! default stays `Start`/`Start`, which reproduces the original intrinsic
//! start-aligned layout byte-for-byte.
//!
//! Per-child proportional sizing lives in a parallel [`NodeAttr`] vector
//! on [`Ui`], set via `.grow(factor)` on any builder or
//! [`Ui::grow`][Ui::grow] for nodes that have already been pushed
//! (buttons). A stack with any grow>0 child fills its main axis and
//! distributes leftover space proportionally to the grow factors — the
//! calculator keypad pattern.

pub mod node;
pub mod layout;
pub mod paint;
pub mod semantics;

// Re-export the most-used identifier so callers don't have to write
// `akiva_sdk::ui::node::NodeId` for every builder return type.
pub use node::NodeId;

use std::collections::HashMap;

use crate::activity::{Context, PointerEvent, PointerPhase};
use crate::canvas::Canvas;
use node::Node;

/// How a stack distributes its children along the main axis.
///
/// `Start` keeps the stack intrinsic-sized (no leftover to distribute);
/// every other variant fills the slot on the main axis so alignment has
/// somewhere to apply. `SpaceBetween` adds equal extra gap between each
/// adjacent pair — the explicit `.gap()` setting is the *minimum* and
/// this extra space stacks on top.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MainAxis {
    /// Pack children at the start, no slot expansion. The default.
    #[default]
    Start,
    /// Centre children along the main axis. Stack fills slot.
    Center,
    /// Pack children at the end. Stack fills slot.
    End,
    /// Distribute leftover space evenly between adjacent children.
    /// `.gap()` is treated as the minimum gap; this adds on top.
    SpaceBetween,
}

impl MainAxis {
    /// `true` when layout should expand the stack to its slot's main
    /// dimension. Only `Start` leaves the stack intrinsic.
    pub fn fills_slot(self) -> bool { !matches!(self, MainAxis::Start) }
}

/// How a stack aligns (and optionally sizes) its children on the cross
/// axis. `Stretch` overrides each child's measured cross size with the
/// stack's inner cross dimension.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CrossAxis {
    /// Pack children at the cross-axis start (top for HStack, left for
    /// VStack). The default.
    #[default]
    Start,
    /// Centre children on the cross axis. Stack fills its cross slot.
    Center,
    /// Pack children at the cross-axis end. Stack fills its cross slot.
    End,
    /// Force every child's cross-axis size to match the stack's inner
    /// cross dimension — useful for full-width buttons in a VStack.
    Stretch,
}

impl CrossAxis {
    /// `true` when layout should expand the stack to its slot's cross
    /// dimension — required for `Center`, `End`, and `Stretch` to have
    /// room to apply.
    pub fn fills_slot(self) -> bool { !matches!(self, CrossAxis::Start) }
}

/// Per-node layout attributes kept alongside [`Node`] in a parallel
/// vector on [`Ui`]. Fields that affect paint stay on `Node` itself;
/// `NodeAttr` is strictly the stuff the layout pass consults.
#[derive(Debug, Clone, Copy, Default)]
pub struct NodeAttr {
    /// Main-axis grow factor when this node sits inside a stack. 0 means
    /// "take intrinsic size" (default). A stack with any grow>0 child
    /// fills its main axis and splits the leftover space among grow
    /// children proportionally to their factors.
    pub grow: f32,
}

/// Axis-aligned rectangle. Origin is top-left; `(x + w, y + h)` is
/// exclusive (a point exactly on the right/bottom edge is *outside*).
#[derive(Debug, Clone, Copy, Default)]
pub struct Rect {
    /// Top-left x coordinate in physical pixels.
    pub x: f32,
    /// Top-left y coordinate in physical pixels.
    pub y: f32,
    /// Width in physical pixels.
    pub w: f32,
    /// Height in physical pixels.
    pub h: f32,
}

impl Rect {
    /// True iff the point `(x, y)` lies inside this rect, with the
    /// right/bottom edges treated as exclusive (matching how hit-test
    /// rects are used during pointer dispatch).
    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && x < self.x + self.w
            && y >= self.y && y < self.y + self.h
    }
}

/// Persistent state for one scroll region. Lives in
/// [`UiState::scroll_states`] keyed by the caller-supplied
/// `ScrollBuilder::key`. Survives across frames so scroll position
/// doesn't reset on every rebuild — the same pattern as
/// [`UiState::hit_rects`], applied to scroll instead of clicks.
#[derive(Debug, Clone, Copy, Default)]
pub struct ScrollCell {
    /// How far the content has been scrolled past the top of the
    /// viewport, in physical pixels. `0` = content top aligned with
    /// viewport top; positive values move the content *up* on screen.
    pub scroll_y: f32,
    /// Pixels-per-frame velocity for active flings. Set on
    /// pointer-up after a drag (Stage 2c.4); decays to 0 each tick.
    /// Programmatic `scroll_to` zeroes it.
    pub velocity: f32,
    /// Last-measured intrinsic height of the child (the scroll's
    /// content). Updated every present pass; used to clamp `scroll_y`
    /// against `max = (content_h - viewport_h).max(0)`.
    pub content_h: f32,
    pub viewport_w: f32,
    pub viewport_h: f32,
    /// Viewport rect in screen coordinates from the most recent
    /// present pass. Used by [`Ui::feed_wheel`] in Stage 2c.3 to
    /// hit-test wheel/pointer events against the topmost scroll
    /// region under the cursor.
    pub last_rect: Rect,
}

/// App-owned UI state. Carry one in your `Activity` struct (usually via
/// `#[derive(Default)]`). Feed pointer events from `on_pointer`;
/// `Ui::new` + `present()` handle everything else.
#[derive(Default)]
pub struct UiState {
    /// Hit rects from the most recent `present()`, keyed by the caller's
    /// widget id. Populated on paint; consulted on `was_clicked`.
    hit_rects: HashMap<String, Rect>,
    /// `id → action` for buttons that declared one via [`Ui::action`]
    /// (or `onClick=` in AKML). Latched during `present()` alongside
    /// `hit_rects`; consulted when resolving clicks to the frame's
    /// `current_actions` list. Buttons without an action never land
    /// here, so absence = "no action" with no extra cost per lookup.
    hit_actions: HashMap<String, String>,
    /// Pointer-down positions queued up since the last `present()`. A
    /// single press may fire in the middle of a long frame; we batch
    /// them until the next present consumes them.
    pending_clicks: Vec<(f32, f32)>,
    /// Widget ids whose hit rect caught a click this frame. Read by
    /// `was_clicked(id)`.
    current_clicks: Vec<String>,
    /// Semantic action names fired this frame (the `action` attribute
    /// of any clicked button). Read by `was_triggered(action)`. A
    /// single click may contribute both to `current_clicks` (via id)
    /// and `current_actions` (via action) — that's intended, so apps
    /// can mix the two dispatch styles during migration.
    current_actions: Vec<String>,
    /// Per-`Node::Scroll` persistent cells, keyed by the
    /// `ScrollBuilder::key` string. Each cell tracks scroll_y across
    /// frames so a rebuild doesn't reset the scroll position; the
    /// present pass refreshes the cached content / viewport dimensions
    /// each frame.
    scroll_states: HashMap<String, ScrollCell>,
    /// Last-known pointer position from `feed_pointer` (any phase) or
    /// from a hover. Wheel events carry no `(x, y)` of their own, so
    /// `feed_wheel` hit-tests against this cached position to find
    /// the topmost scroll region under the cursor. `None` until the
    /// first pointer event arrives.
    last_pointer: Option<(f32, f32)>,
    /// Currently-tracked drag gesture, or `None`. Lifecycle:
    ///   Down inside a scroll region  → seeded (is_dragging=false)
    ///   Move past DRAG_SLOP           → is_dragging flips to true
    ///   Move while dragging          → applies delta to scroll_y +
    ///                                  updates velocity EMA
    ///   Up                            → taken out; if never dragged,
    ///                                  the original Down promotes
    ///                                  to a tap (pending_clicks)
    ///   Cancel                        → dropped silently
    drag_gesture: Option<DragGesture>,
}

/// In-flight drag state. See [`UiState::drag_gesture`] for the
/// lifecycle. `key` identifies the scroll region the gesture started
/// inside; `started_y` is the y of the down event (for slop
/// thresholding); `last_y` is the y of the most recent Move (for
/// per-frame deltas).
#[derive(Debug, Clone)]
pub(crate) struct DragGesture {
    pub(crate) key: String,
    pub(crate) started_at: (f32, f32),
    pub(crate) last_y: f32,
    pub(crate) is_dragging: bool,
}

/// Drag-detection threshold in dp. Pointer-down + move below this
/// distance still resolves as a tap (so a button inside a scroll
/// region keeps working); past it, the gesture switches to scroll-
/// drag and the original Down does *not* fire as a click.
/// Matches Android `ViewConfiguration.getScaledTouchSlop()` (8 dp on
/// most devices, 6 dp on a few); 6 is a good cross-platform median.
pub(crate) const DRAG_SLOP: f32 = 6.0;

impl UiState {
    /// Empty UI state. Equivalent to `UiState::default()`; the explicit
    /// constructor exists so call-sites can read as
    /// `UiState::new()` rather than reaching for `Default`.
    pub fn new() -> Self { Self::default() }

    /// Feed pointer events so `Ui::was_clicked` and `Node::Scroll`
    /// drag-detection see them. Call this unconditionally from your
    /// `on_pointer` — `UiState` filters by phase internally.
    ///
    /// Down/Move/Up/Cancel together resolve a gesture as one of:
    ///   - **tap** — Down + Up with cumulative movement < `DRAG_SLOP`.
    ///     Click queued for the next `present()` (existing behaviour).
    ///   - **scroll-drag** — Down inside a scroll region + Move past
    ///     `DRAG_SLOP`. Subsequent Moves shift `scroll_y` directly;
    ///     the original Down does *not* fire as a tap, so a button
    ///     accidentally pressed during the start of a drag doesn't
    ///     spuriously trigger.
    /// Tracking is per-gesture (Down…Up) — nothing crosses frames.
    pub fn feed_pointer(&mut self, ev: PointerEvent) {
        self.last_pointer = Some((ev.x, ev.y));
        match ev.phase {
            PointerPhase::Down => {
                // Defer click resolution iff this Down landed inside a
                // scroll region — that gesture might still become a
                // drag. Outside any scroll region, fire the click
                // immediately (preserves the pre-2c.3 tap latency for
                // every button in the app).
                if let Some(key) = self.scroll_region_at(ev.x, ev.y) {
                    self.drag_gesture = Some(DragGesture {
                        key,
                        started_at: (ev.x, ev.y),
                        last_y: ev.y,
                        is_dragging: false,
                    });
                } else {
                    self.pending_clicks.push((ev.x, ev.y));
                }
            }
            PointerPhase::Move => {
                if let Some(g) = self.drag_gesture.as_mut() {
                    // Total Y travel from down. Below slop: gesture
                    // still candidate-tap. Past slop: switch to drag.
                    let total_dy = (ev.y - g.started_at.1).abs();
                    if !g.is_dragging && total_dy >= DRAG_SLOP {
                        g.is_dragging = true;
                    }
                    if g.is_dragging {
                        // Positive delta_y = finger moved up =
                        // content scrolls down (scroll_y increases).
                        let delta_y = g.last_y - ev.y;
                        // Capture before-update for borrow split.
                        let key = g.key.clone();
                        let cell = self.scroll_states.entry(key).or_default();
                        let max = (cell.content_h - cell.viewport_h).max(0.0);
                        cell.scroll_y = (cell.scroll_y + delta_y).clamp(0.0, max);
                        // EMA velocity in px/frame. Used by Stage
                        // 2c.4's fling animator at pointer-up. Half-
                        // life ≈ 1 sample with the 0.5 / 0.5 weights;
                        // bump the new-sample weight if drags feel
                        // sticky.
                        cell.velocity = 0.5 * cell.velocity + 0.5 * delta_y;
                    }
                    g.last_y = ev.y;
                }
            }
            PointerPhase::Up => {
                if let Some(g) = self.drag_gesture.take() {
                    if !g.is_dragging {
                        // Never crossed slop — promote to a tap so
                        // buttons inside the scroll region still
                        // work.
                        self.pending_clicks.push(g.started_at);
                    }
                    // Drag-end: cell.velocity already carries the
                    // EMA; 2c.4 picks it up to drive fling.
                }
            }
            PointerPhase::Cancel => {
                // Drop any candidate / active drag silently. Don't
                // promote to a tap — Cancel is the OS saying "this
                // gesture is invalid", not "Up".
                self.drag_gesture = None;
            }
        }
    }

    /// Feed wheel events so `Node::Scroll` advances under mouse-wheel
    /// or trackpad two-finger swipe. Wheels carry no `(x, y)` of
    /// their own; we hit-test against the last-known pointer position
    /// from `feed_pointer`. No-op if no pointer event has been seen
    /// yet (e.g. wheel before any cursor entry) or no scroll region
    /// is under the cursor.
    pub fn feed_wheel(&mut self, ev: super::activity::WheelEvent) {
        let Some((px, py)) = self.last_pointer else { return; };
        let Some(key) = self.scroll_region_at(px, py) else { return; };
        let cell = self.scroll_states.entry(key).or_default();
        let max = (cell.content_h - cell.viewport_h).max(0.0);
        cell.scroll_y = (cell.scroll_y + ev.dy).clamp(0.0, max);
        // Wheel input is an explicit user override; kill any active
        // fling so the two don't fight.
        cell.velocity = 0.0;
    }

    /// Find the topmost scroll region whose viewport rect contains
    /// `(x, y)`. For nested scrolls, the innermost wins (smallest-
    /// area rect among those that contain the point). `None` if no
    /// scroll region has ever been built or none is under the
    /// cursor. Cheap — scroll regions are O(handful) per activity.
    fn scroll_region_at(&self, x: f32, y: f32) -> Option<String> {
        let mut best: Option<(String, f32)> = None;
        for (key, cell) in &self.scroll_states {
            if cell.last_rect.contains(x, y) {
                let area = cell.last_rect.w * cell.last_rect.h;
                if best.as_ref().is_none_or(|(_, a)| area < *a) {
                    best = Some((key.clone(), area));
                }
            }
        }
        best.map(|(k, _)| k)
    }
}

/// Builder + renderer for one frame. Borrow-bound to the state, the
/// canvas, and the frame's viewport metrics; drop at end of `on_render`.
pub struct Ui<'s, 'c, 'b> {
    state: &'s mut UiState,
    canvas: &'c mut Canvas<'b>,
    viewport: (f32, f32),
    density: f32,
    nodes: Vec<Node>,
    /// Parallel to `nodes`; `attrs[id]` is the layout-only side-info for
    /// the node at `id`. Kept as a separate vec so pattern matches on
    /// `Node` stay tight and paint-time code doesn't destructure fields
    /// it doesn't care about.
    attrs: Vec<NodeAttr>,
    presented: bool,
    /// Activity handle the host registered for the calling slot. Used
    /// only by [`Self::present`] to address the `PUBLISH_SEMANTICS`
    /// fire op at the right activity on the inspector / Playwright
    /// path. `0` is the in-process / test-fixture sentinel — emission
    /// is suppressed in that case (no host to receive the bytes).
    handle: u32,
}

impl<'s, 'c, 'b> Ui<'s, 'c, 'b> {
    /// Borrow the per-activity [`UiState`], the per-frame [`Context`],
    /// and the SDK [`Canvas`] together for one frame's worth of UI
    /// construction. Resets per-frame derived state (hit rects, current
    /// clicks/actions) so a previous frame's leftovers don't bleed in.
    pub fn new(
        state: &'s mut UiState,
        ctx: &Context<'_>,
        canvas: &'c mut Canvas<'b>,
    ) -> Self {
        // Reset per-frame derived state up front. Hit rects / actions
        // will be repopulated by `present()`; clicks / actions consumed
        // then too.
        state.hit_rects.clear();
        state.hit_actions.clear();
        state.current_clicks.clear();
        state.current_actions.clear();
        Self {
            state,
            canvas,
            viewport: (ctx.metrics.width_px as f32, ctx.metrics.height_px as f32),
            density: if ctx.metrics.density > 0.0 { ctx.metrics.density } else { 1.0 },
            nodes: Vec::new(),
            attrs: Vec::new(),
            presented: false,
            handle: ctx.handle,
        }
    }

    pub(crate) fn push(&mut self, node: Node) -> NodeId {
        let id = self.nodes.len();
        self.nodes.push(node);
        self.attrs.push(NodeAttr::default());
        id
    }

    /// Private setter used by builders after `push` to attach a grow
    /// factor without re-borrowing through a public API that would also
    /// work for callers.
    pub(crate) fn set_grow(&mut self, id: NodeId, grow: f32) {
        self.attrs[id].grow = grow;
    }

    /// Set the main-axis grow factor for a node that's already been
    /// pushed. Intended for `Ui::button`, which returns a `NodeId`
    /// directly and therefore can't chain `.grow()` the way the other
    /// builders can. Silently no-ops on a stale or out-of-range id —
    /// the layout pass treats missing attrs as grow=0 anyway.
    pub fn grow(&mut self, id: NodeId, factor: f32) {
        if let Some(a) = self.attrs.get_mut(id) {
            a.grow = factor;
        }
    }

    /// Bytes written to the canvas so far. Test-only — exists so we can
    /// assert "double-present produced no new paint ops" without having
    /// to drop `Ui` mid-frame.
    #[cfg(test)]
    pub(crate) fn canvas_len(&self) -> usize { self.canvas.buf_len() }

    /// Whether a widget with this id registered a hit rect during the
    /// most recent [`present`][Self::present]. Test-only — used by
    /// sibling modules (e.g. `akml::inflate`) that need a yes/no
    /// answer without reaching into private state.
    #[cfg(test)]
    pub(crate) fn has_hit_rect(&self, id: &str) -> bool {
        self.state.hit_rects.contains_key(id)
    }

    // ── Builders ─────────────────────────────────────────────────────
    //
    // All builders return borrowing configurators that terminate with
    // `.build() -> NodeId`. That keeps the call-site fluent without
    // committing to a pile of overloaded positional constructors.

    /// Begin a [`Text`][node::Node::Text] node. Chain `.size`, `.color`,
    /// `.grow` and friends, then `.build()` for the [`NodeId`].
    pub fn text(&mut self, text: impl Into<String>) -> node::TextBuilder<'_, 's, 'c, 'b> {
        node::TextBuilder::new(self, text.into())
    }

    /// Begin a [`Rect`][node::Node::Rect] node — a flat colour fill with
    /// optional rounding. Chain `.size`, `.radius`, `.grow`.
    pub fn rect(&mut self, color: u32) -> node::RectBuilder<'_, 's, 'c, 'b> {
        node::RectBuilder::new(self, color)
    }

    /// Begin a [`Spacer`][node::Node::Spacer] node — empty intrinsic-zero
    /// node useful as a flexible gap inside stacks (chain `.grow(1.0)`).
    pub fn spacer(&mut self) -> node::SpacerBuilder<'_, 's, 'c, 'b> {
        node::SpacerBuilder::new(self)
    }

    /// Begin an [`Image`][node::Node::Image] node — bitmap blit by
    /// `drawable_id`. Get the id from [`crate::res::drawable`]; chain
    /// `.size(w, h)` for the destination dimensions in dp.
    /// `drawable_id == 0` paints a magenta placeholder (same fallback
    /// as missing `@color/`/`@string/`).
    pub fn image(&mut self, drawable_id: u32) -> node::ImageBuilder<'_, 's, 'c, 'b> {
        node::ImageBuilder::new(self, drawable_id)
    }

    /// Push a [`Button`][node::Node::Button] node. Returns the
    /// [`NodeId`] directly — buttons skip the builder pattern because
    /// the defaults look right and the hot path is "id + label".
    /// `id` is the lookup key for [`Self::was_clicked`]; pass it back
    /// after `present()` to query whether a click landed.
    pub fn button(&mut self, id: impl Into<String>, label: impl Into<String>) -> NodeId {
        // Buttons keep simpler (non-builder) defaults because they're
        // the common case and the defaults look right in both the
        // Akiva dark theme and anything else.
        self.push(Node::Button {
            id: id.into(),
            action: None,
            label: label.into(),
            label_size: 14.0,
            bg: 0xff2a3a50,
            bg_pressed: 0xff3a5070,
            fg: 0xfff0f4fa,
            pad: (16.0, 10.0),
            radius: 8.0,
        })
    }

    /// Override the corner radius on a button after construction.
    /// Mirrors [`Ui::action`] in taking the node id post-hoc. Silent
    /// no-op on a non-button or stale id so a stray AKML attribute
    /// can't panic the frame.
    pub fn corner_radius(&mut self, id: NodeId, radius: f32) {
        match self.nodes.get_mut(id) {
            Some(Node::Button { radius: slot, .. }) => *slot = radius,
            Some(Node::Rect    { radius: slot, .. }) => *slot = radius,
            _ => {}
        }
    }

    /// Attach a semantic action name to an already-pushed button. After
    /// `present`, `Ui::was_triggered(action)` returns true for the frame
    /// a click landed on this button. Mirrors [`Ui::grow`] in taking the
    /// node id post-hoc — buttons are built without a builder, so this
    /// is the attach point for optional attributes. Silent no-op on a
    /// non-button or stale id so a stray call from AKML inflate can't
    /// panic the whole frame.
    pub fn action(&mut self, id: NodeId, action: impl Into<String>) {
        if let Some(Node::Button { action: slot, .. }) = self.nodes.get_mut(id) {
            *slot = Some(action.into());
        }
    }

    /// Vertical stack — children laid out top-to-bottom. Chain `.gap`,
    /// `.padding`, `.main`, `.cross`, `.grow`. Children come from this
    /// `Ui` (you build them first, then pass the `NodeId`s here).
    pub fn v_stack(&mut self, children: &[NodeId]) -> node::StackBuilder<'_, 's, 'c, 'b> {
        node::StackBuilder::new_v(self, children.to_vec())
    }

    /// Horizontal stack — children laid out left-to-right. Same builder
    /// surface as [`Self::v_stack`].
    pub fn h_stack(&mut self, children: &[NodeId]) -> node::StackBuilder<'_, 's, 'c, 'b> {
        node::StackBuilder::new_h(self, children.to_vec())
    }

    /// Frame: stack children on the z-axis. Document order is paint order
    /// (first child bottom, last child top); every child gets the same
    /// rect — the frame's inner box after padding. The common pattern is
    /// a background `Rect` + content on top.
    pub fn frame(&mut self, children: &[NodeId]) -> node::FrameBuilder<'_, 's, 'c, 'b> {
        node::FrameBuilder::new(self, children.to_vec())
    }

    /// Grid: `cols × rows` equal-sized cells laid out in row-major order.
    /// Children past `cols * rows` are silently ignored, so AKML can
    /// over-declare rows and let empty trailing cells fall out naturally.
    /// Both `cols` and `rows` are clamped to ≥ 1 to avoid divide-by-zero
    /// in layout.
    pub fn grid(&mut self, children: &[NodeId], cols: u32, rows: u32)
        -> node::GridBuilder<'_, 's, 'c, 'b>
    {
        node::GridBuilder::new(self, children.to_vec(), cols, rows)
    }

    /// Single-child scroll viewport. Chain `.key(...)` to address the
    /// region across frames (so the scroll position persists), then
    /// `.size(w, h)` or `.height(h)` if you want to lock the viewport
    /// to a specific size rather than inheriting from the slot.
    pub fn scroll(&mut self, child: NodeId) -> node::ScrollBuilder<'_, 's, 'c, 'b> {
        node::ScrollBuilder::new(self, child)
    }

    /// Programmatic scroll. Clamps `y` to `[0, content_h - viewport_h]`
    /// against the last-measured content height (so calling before the
    /// first `present()` simply records 0 and the first paint will
    /// honour it once measurement catches up). Zeroes any active
    /// fling velocity — programmatic scroll is "user intent
    /// overrides physics", same as Android.
    pub fn scroll_to(&mut self, key: &str, y: f32) {
        let cell = self.state.scroll_states.entry(key.to_string()).or_default();
        let max = (cell.content_h - cell.viewport_h).max(0.0);
        cell.scroll_y = y.clamp(0.0, max);
        cell.velocity = 0.0;
    }

    /// Current scroll position for `key`. Returns `0.0` if no scroll
    /// region by that key has ever been built — cheaper than panicking
    /// on first-frame reads.
    pub fn scroll_offset(&self, key: &str) -> f32 {
        self.state.scroll_states.get(key).map(|c| c.scroll_y).unwrap_or(0.0)
    }

    /// Most recent measured content / viewport dimensions for `key`,
    /// as `(content_h, viewport_h)`. Both are `0.0` until the first
    /// present pass measures the scroll. Useful for tests and for
    /// activities that want to know whether a scroll can advance
    /// any further.
    pub fn scroll_extent(&self, key: &str) -> (f32, f32) {
        self.state.scroll_states.get(key)
            .map(|c| (c.content_h, c.viewport_h))
            .unwrap_or((0.0, 0.0))
    }

    // ── Present ──────────────────────────────────────────────────────

    /// Measure, lay out and paint the tree rooted at `root`. Consumes
    /// any `pending_clicks` that landed inside a Button and records
    /// them for `was_clicked` lookups below. Idempotent-ish — calling
    /// twice in one frame double-paints and is probably a bug, so we
    /// set a flag and no-op the second call.
    pub fn present(&mut self, root: NodeId) {
        if self.presented { return; }
        self.presented = true;

        // Wire the layout engine to the host's text measurer. The
        // indirection (rather than calling `runtime::text_width`
        // directly inside `measure`) keeps layout independently
        // testable — tests pass a deterministic fake.
        let sizes = layout::measure(
            &self.nodes, root, self.viewport, self.density,
            &|text, size| crate::runtime::text_width(text, size),
        );

        // Snapshot the persistent scroll offsets so the layout pass
        // can apply them to `Node::Scroll` children without needing
        // a mutable handle to `UiState`. The cache itself updates
        // *after* layout — clamping the just-read offset against the
        // new content/viewport heights — so a content shrink doesn't
        // strand `scroll_y` past the new end.
        let scroll_offsets: HashMap<String, f32> = self.state.scroll_states
            .iter().map(|(k, c)| (k.clone(), c.scroll_y)).collect();

        let mut rects: HashMap<NodeId, Rect> = HashMap::new();
        layout::layout(
            &self.nodes, &self.attrs, root,
            Rect { x: 0.0, y: 0.0, w: self.viewport.0, h: self.viewport.1 },
            &sizes, &scroll_offsets, &mut rects,
        );

        // Refresh every Scroll cell with this frame's measured
        // content/viewport sizes + viewport rect, then clamp scroll_y
        // against the fresh max. A node's first frame initialises the
        // cell at scroll_y=0 (the default) so the very first paint
        // shows the top of the content — programmatic `scroll_to`
        // called before any present() honours the same default until
        // the cell catches up.
        for (id, node) in self.nodes.iter().enumerate() {
            if let Node::Scroll { child, key, .. } = node {
                let viewport_rect = rects.get(&id).copied().unwrap_or_default();
                let (_cw, ch) = sizes.get(child).copied().unwrap_or((0.0, 0.0));
                let cell = self.state.scroll_states.entry(key.clone()).or_default();
                cell.content_h = ch;
                cell.viewport_w = viewport_rect.w;
                cell.viewport_h = viewport_rect.h;
                cell.last_rect = viewport_rect;
                let max = (cell.content_h - cell.viewport_h).max(0.0);
                if cell.scroll_y > max { cell.scroll_y = max; }
                if cell.scroll_y < 0.0 { cell.scroll_y = 0.0; }
            }
        }

        // Record hit rects + actions for buttons before paint, so
        // `was_clicked` / `was_triggered` queries that come after
        // `present()` see them.
        for (node_id, rect) in &rects {
            if let Node::Button { id, action, .. } = &self.nodes[*node_id] {
                self.state.hit_rects.insert(id.clone(), *rect);
                if let Some(a) = action {
                    self.state.hit_actions.insert(id.clone(), a.clone());
                }
            }
        }

        // Resolve clicks against the just-latched hit rects. A pending
        // click point falling inside a button's rect contributes the
        // button's `id` to `current_clicks` and — if the button had an
        // `action` — the action string to `current_actions`. Pending
        // clicks are drained so a single press fires once.
        for (x, y) in self.state.pending_clicks.drain(..) {
            for (id, rect) in &self.state.hit_rects {
                if rect.contains(x, y) {
                    self.state.current_clicks.push(id.clone());
                    if let Some(action) = self.state.hit_actions.get(id) {
                        self.state.current_actions.push(action.clone());
                    }
                }
            }
        }

        paint::paint(
            &self.nodes, root, &rects, &self.state.current_clicks,
            &|t, s| crate::runtime::text_width(t, s),
            self.canvas,
        );

        // Test-infra phase — publish the semantic projection of this
        // frame's tree. The host's AppRegistry forwards the bytes to
        // the leader tab, which projects to hidden ARIA-decorated DOM
        // only when `?inspector=1` is set in the URL. `handle == 0`
        // is the in-process / test fixture sentinel — skip emission
        // there since there's no host to receive it.
        if self.handle != 0 {
            let tree = build_semantic_tree(&self.nodes, root, &rects);
            if !tree.is_empty() {
                let bytes = tree.encode();
                crate::runtime::host_publish_semantics(self.handle, &bytes);
            }
        }
    }

    /// True if the widget registered with `id` was pressed during this
    /// frame's pending clicks. Call *after* `present()`. Safe to call
    /// multiple times — returns the same answer until next frame.
    pub fn was_clicked(&self, id: &str) -> bool {
        self.state.current_clicks.iter().any(|s| s == id)
    }

    /// True if any button whose `action` attribute equals `action` was
    /// pressed this frame. Call *after* `present()`. Unlike
    /// [`Ui::was_clicked`], this is keyed on the *semantic* action name
    /// (the `onClick=` attribute in AKML), not the widget id — so AKML
    /// can rename a button's `id` or add/remove buttons sharing one
    /// action without the Activity's dispatch code noticing.
    pub fn was_triggered(&self, action: &str) -> bool {
        self.state.current_actions.iter().any(|s| s == action)
    }
}

/// Walk the laid-out widget tree depth-first and project it into a
/// [`semantics::SemanticTree`]. Every reachable node lands in the
/// tree, including decorative `Rect`/`Spacer` — the host overlay
/// filters those at projection time so an inspector overlay can still
/// see them if it wants. Skipped silently: nodes that have no entry
/// in `rects` (out-of-tree leftovers from earlier construction) — the
/// layout pass only populates `rects` for nodes reachable from the
/// presented root.
fn build_semantic_tree(
    nodes: &[Node],
    root: NodeId,
    rects: &HashMap<NodeId, Rect>,
) -> semantics::SemanticTree {
    use semantics::{SemanticNode, SemanticRole, SemanticTree};
    let mut tree = SemanticTree::new();
    fn visit(
        nodes: &[Node],
        rects: &HashMap<NodeId, Rect>,
        tree: &mut SemanticTree,
        node_id: NodeId,
        parent: Option<u32>,
    ) {
        let Some(rect) = rects.get(&node_id).copied() else { return; };
        let Some(node) = nodes.get(node_id) else { return; };
        let (role, id, label) = match node {
            Node::VStack { .. } | Node::HStack { .. } | Node::Frame { .. } | Node::Grid { .. } => {
                (SemanticRole::Group, None, None)
            }
            Node::Scroll { key, .. } => {
                let id = if key.is_empty() { None } else { Some(key.clone()) };
                (SemanticRole::Scroll, id, None)
            }
            Node::Text { text, .. } => (SemanticRole::Text, None, Some(text.clone())),
            Node::Button { id, label, .. } => {
                (SemanticRole::Button, Some(id.clone()), Some(label.clone()))
            }
            Node::Image { drawable_id, .. } => {
                // No label resolution today — drawable name lives at
                // the call site (`res::drawable("name")`), not on the
                // Node. Surface the numeric id as the value so a
                // future inspector can correlate; that channel is
                // already separate from `label` so a real name added
                // later won't conflict.
                let _ = drawable_id;
                (SemanticRole::Image, None, None)
            }
            Node::Rect    { .. } => (SemanticRole::Rect,   None, None),
            Node::Spacer  { .. } => (SemanticRole::Spacer, None, None),
        };
        let value = if let Node::Image { drawable_id, .. } = node {
            Some(format!("{drawable_id}"))
        } else {
            None
        };
        let idx = tree.push(SemanticNode {
            parent, role,
            x: rect.x, y: rect.y, w: rect.w, h: rect.h,
            id, label, value,
        });
        // Recurse into containers. Order matches paint order so the
        // semantic tree's z-order reflects what's on screen — useful
        // when an inspector wants to highlight the topmost hit.
        match node {
            Node::VStack { children, .. }
            | Node::HStack { children, .. }
            | Node::Frame  { children, .. }
            | Node::Grid   { children, .. } => {
                for c in children {
                    visit(nodes, rects, tree, *c, Some(idx));
                }
            }
            Node::Scroll { child, .. } => {
                visit(nodes, rects, tree, *child, Some(idx));
            }
            _ => {}
        }
    }
    visit(nodes, rects, &mut tree, root, None);
    tree
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::activity::{Metrics, PointerPhase};

    fn pointer(x: f32, y: f32, phase: PointerPhase) -> PointerEvent {
        PointerEvent { x, y, button: 0, phase }
    }

    fn ctx<'a>(class: &'a str) -> Context<'a> {
        Context {
            metrics: Metrics { width_px: 800, height_px: 600, density: 1.0 },
            timestamp_ms: 0.0,
            pointer: None,
            class,
            handle: 0,
        }
    }

    #[test]
    fn new_state_is_empty() {
        let s = UiState::new();
        assert!(s.hit_rects.is_empty());
        assert!(s.pending_clicks.is_empty());
        assert!(s.current_clicks.is_empty());
    }

    #[test]
    fn feed_pointer_captures_down_ignores_others() {
        let mut s = UiState::new();
        s.feed_pointer(pointer(10.0, 10.0, PointerPhase::Move));
        s.feed_pointer(pointer(10.0, 10.0, PointerPhase::Up));
        s.feed_pointer(pointer(10.0, 10.0, PointerPhase::Cancel));
        assert!(s.pending_clicks.is_empty());

        s.feed_pointer(pointer(42.0, 7.0, PointerPhase::Down));
        assert_eq!(s.pending_clicks, vec![(42.0, 7.0)]);
    }

    #[test]
    fn was_clicked_is_false_before_any_present() {
        let s = UiState::new();
        // No Ui built at all — was_clicked on a bare state reads the
        // still-empty `current_clicks`. We emulate by borrowing through
        // a throwaway Ui that never presents.
        let c = ctx("t");
        let mut buf = Vec::new();
        let mut canvas = Canvas::new(&mut buf);
        let mut ui_state = s;
        let ui = Ui::new(&mut ui_state, &c, &mut canvas);
        assert!(!ui.was_clicked("anything"));
    }

    #[test]
    fn click_inside_button_rect_registers() {
        let mut state = UiState::new();
        // Button placed at origin. A "Go" label (2 chars, 14pt, pad
        // 16/10) gives roughly 47×38 — so (10, 10) is safely inside
        // without tripping the half-open right/bottom edges.
        state.feed_pointer(pointer(10.0, 10.0, PointerPhase::Down));

        let c = ctx("t");
        let mut buf = Vec::new();
        let mut canvas = Canvas::new(&mut buf);
        let mut ui = Ui::new(&mut state, &c, &mut canvas);
        let btn = ui.button("go", "Go");
        ui.present(btn);
        assert!(ui.was_clicked("go"));
        assert!(!ui.was_clicked("nope"));
    }

    #[test]
    fn click_outside_any_button_registers_nothing() {
        let mut state = UiState::new();
        // Button will be small and near origin; click far to the right.
        state.feed_pointer(pointer(500.0, 500.0, PointerPhase::Down));

        let c = ctx("t");
        let mut buf = Vec::new();
        let mut canvas = Canvas::new(&mut buf);
        let mut ui = Ui::new(&mut state, &c, &mut canvas);
        let btn = ui.button("go", "Go");
        ui.present(btn);
        assert!(!ui.was_clicked("go"));
    }

    #[test]
    fn was_triggered_fires_when_button_with_matching_action_is_clicked() {
        // A button with action="eat" clicked inside its rect should
        // surface through `was_triggered("eat")`, not `was_triggered`
        // for any other string.
        let mut state = UiState::new();
        state.feed_pointer(pointer(10.0, 10.0, PointerPhase::Down));

        let c = ctx("t");
        let mut buf = Vec::new();
        let mut canvas = Canvas::new(&mut buf);
        let mut ui = Ui::new(&mut state, &c, &mut canvas);
        let btn = ui.button("b", "B");
        ui.action(btn, "eat");
        ui.present(btn);
        assert!(ui.was_triggered("eat"));
        assert!(!ui.was_triggered("other"));
        // The click still registers under the original id, too — the
        // two dispatch styles coexist during migration.
        assert!(ui.was_clicked("b"));
    }

    #[test]
    fn was_triggered_is_false_for_buttons_without_an_action() {
        // A vanilla button (no `.action(...)`) must not accidentally
        // trigger *any* action query — the absence is the "no action"
        // signal, and we want was_triggered("") in particular to be
        // false (empty-string convention shouldn't leak through).
        let mut state = UiState::new();
        state.feed_pointer(pointer(10.0, 10.0, PointerPhase::Down));

        let c = ctx("t");
        let mut buf = Vec::new();
        let mut canvas = Canvas::new(&mut buf);
        let mut ui = Ui::new(&mut state, &c, &mut canvas);
        let btn = ui.button("b", "B");
        ui.present(btn);
        assert!(ui.was_clicked("b"));
        assert!(!ui.was_triggered(""));
        assert!(!ui.was_triggered("b"));
        assert!(!ui.was_triggered("anything"));
    }

    #[test]
    fn two_buttons_sharing_an_action_both_trigger_the_same_query() {
        // The headline value of `was_triggered` vs `was_clicked`:
        // AKML can split one semantic button into two (e.g. a primary
        // and a keyboard-shortcut variant) sharing one onClick, and the
        // Rust dispatch doesn't need to know. Click either → same
        // action fires.
        let mut state = UiState::new();
        let c = ctx("t");

        // First pass: figure out where button B sits, feed a click
        // there, then rebuild the tree and present.
        let rect_b;
        {
            let mut buf = Vec::new();
            let mut canvas = Canvas::new(&mut buf);
            let mut ui = Ui::new(&mut state, &c, &mut canvas);
            let a = ui.button("a1", "A1");
            ui.action(a, "go");
            let b = ui.button("a2", "A2");
            ui.action(b, "go");
            let row = ui.h_stack(&[a, b]).gap(4.0).build();
            ui.present(row);
            rect_b = *state.hit_rects.get("a2").expect("a2 rect");
        }
        state.feed_pointer(pointer(
            rect_b.x + rect_b.w / 2.0,
            rect_b.y + rect_b.h / 2.0,
            PointerPhase::Down,
        ));

        let mut buf = Vec::new();
        let mut canvas = Canvas::new(&mut buf);
        let mut ui = Ui::new(&mut state, &c, &mut canvas);
        let a = ui.button("a1", "A1");
        ui.action(a, "go");
        let b = ui.button("a2", "A2");
        ui.action(b, "go");
        let row = ui.h_stack(&[a, b]).gap(4.0).build();
        ui.present(row);
        assert!(ui.was_triggered("go"));
        assert!(ui.was_clicked("a2"));
        assert!(!ui.was_clicked("a1"));
    }

    #[test]
    fn ui_action_on_non_button_is_a_noop() {
        // Defensive: calling `action` on a Text or Rect id shouldn't
        // panic or mislabel anything. Subsequent was_triggered queries
        // must stay false.
        let mut state = UiState::new();
        let c = ctx("t");
        let mut buf = Vec::new();
        let mut canvas = Canvas::new(&mut buf);
        let mut ui = Ui::new(&mut state, &c, &mut canvas);
        let txt = ui.text("hello").size(10.0).build();
        ui.action(txt, "ignored");
        ui.action(9999, "stale"); // out-of-range id
        ui.present(txt);
        assert!(!ui.was_triggered("ignored"));
        assert!(!ui.was_triggered("stale"));
    }

    #[test]
    fn multiple_buttons_resolve_to_the_hit_one() {
        let mut state = UiState::new();

        // Build the tree once to learn where button B ends up, feed a
        // click there, then rebuild + present. Two Ui lifetimes in
        // sequence matches real usage: on_pointer runs against last
        // frame's rects, on_render rebuilds.
        let c = ctx("t");

        let rect_b;
        {
            let mut buf = Vec::new();
            let mut canvas = Canvas::new(&mut buf);
            let mut ui = Ui::new(&mut state, &c, &mut canvas);
            let a = ui.button("a", "A");
            let b = ui.button("b", "B");
            let row = ui.h_stack(&[a, b]).gap(4.0).build();
            ui.present(row);
            rect_b = *state.hit_rects.get("b").expect("b rect");
        }
        // Click in the middle of B.
        state.feed_pointer(pointer(rect_b.x + rect_b.w / 2.0, rect_b.y + rect_b.h / 2.0, PointerPhase::Down));

        let mut buf = Vec::new();
        let mut canvas = Canvas::new(&mut buf);
        let mut ui = Ui::new(&mut state, &c, &mut canvas);
        let a = ui.button("a", "A");
        let b = ui.button("b", "B");
        let row = ui.h_stack(&[a, b]).gap(4.0).build();
        ui.present(row);
        assert!(ui.was_clicked("b"));
        assert!(!ui.was_clicked("a"));
    }

    #[test]
    fn pending_click_is_consumed_by_present() {
        let mut state = UiState::new();
        state.feed_pointer(pointer(5.0, 5.0, PointerPhase::Down));

        let c = ctx("t");
        {
            let mut buf = Vec::new();
            let mut canvas = Canvas::new(&mut buf);
            let mut ui = Ui::new(&mut state, &c, &mut canvas);
            let btn = ui.button("g", "G");
            ui.present(btn);
            assert!(ui.was_clicked("g"));
        }
        // Next frame: no new clicks fed → was_clicked must be false.
        let mut buf = Vec::new();
        let mut canvas = Canvas::new(&mut buf);
        let mut ui = Ui::new(&mut state, &c, &mut canvas);
        let btn = ui.button("g", "G");
        ui.present(btn);
        assert!(!ui.was_clicked("g"));
    }

    #[test]
    fn hit_rects_reset_between_frames() {
        let mut state = UiState::new();
        let c = ctx("t");

        // Frame 1: register button "a".
        {
            let mut buf = Vec::new();
            let mut canvas = Canvas::new(&mut buf);
            let mut ui = Ui::new(&mut state, &c, &mut canvas);
            let btn = ui.button("a", "A");
            ui.present(btn);
            assert!(state.hit_rects.contains_key("a"));
        }
        // Frame 2: different tree — "a" must vanish from hit_rects so
        // an old click in its former location can't fire a ghost tap.
        let mut buf = Vec::new();
        let mut canvas = Canvas::new(&mut buf);
        let mut ui = Ui::new(&mut state, &c, &mut canvas);
        let btn = ui.button("b", "B");
        ui.present(btn);
        assert!(!state.hit_rects.contains_key("a"));
        assert!(state.hit_rects.contains_key("b"));
    }

    #[test]
    fn double_present_is_a_noop() {
        // Calling present twice in one frame should neither double-paint
        // nor consume extra clicks. The protection is cosmetic — an
        // honest mistake by the app shouldn't corrupt state.
        let mut state = UiState::new();
        state.feed_pointer(pointer(10.0, 10.0, PointerPhase::Down));

        let c = ctx("t");
        let mut buf = Vec::new();
        let mut canvas = Canvas::new(&mut buf);
        let mut ui = Ui::new(&mut state, &c, &mut canvas);
        let btn = ui.button("g", "G");
        ui.present(btn);
        let bytes_after_first = ui.canvas_len();
        ui.present(btn);
        assert_eq!(ui.canvas_len(), bytes_after_first);
    }

    #[test]
    fn ui_grow_setter_widens_button_in_hstack() {
        // Exercises the post-hoc `Ui::grow` path used for buttons (which
        // don't return a builder). Two buttons in an HStack, one
        // grown — the grown one should end up meaningfully wider.
        let mut state = UiState::new();
        let c = ctx("t");
        let mut buf = Vec::new();
        let mut canvas = Canvas::new(&mut buf);
        let a_rect;
        let b_rect;
        {
            let mut ui = Ui::new(&mut state, &c, &mut canvas);
            let a = ui.button("a", "A");
            let b = ui.button("b", "B");
            ui.grow(b, 1.0);
            let row = ui.h_stack(&[a, b]).build();
            ui.present(row);
            a_rect = *ui.state.hit_rects.get("a").expect("a rect");
            b_rect = *ui.state.hit_rects.get("b").expect("b rect");
        }
        // Viewport is 800px wide; non-grow "A" stays at its intrinsic
        // button size, "B" absorbs the rest. The exact widths depend on
        // the stub text measurer but B must be strictly wider.
        assert!(b_rect.w > a_rect.w, "expected B wider than A, got {} vs {}", b_rect.w, a_rect.w);
    }

    #[test]
    fn ui_grow_on_stale_id_is_noop() {
        // Defensive: out-of-range id shouldn't panic. Layout treats
        // missing attrs as grow=0, so the call should silently do
        // nothing and later presents still work.
        let mut state = UiState::new();
        let c = ctx("t");
        let mut buf = Vec::new();
        let mut canvas = Canvas::new(&mut buf);
        let mut ui = Ui::new(&mut state, &c, &mut canvas);
        ui.grow(9999, 5.0);  // no such node yet
        let btn = ui.button("g", "G");
        ui.present(btn);
        // Nothing crashed; button laid out at intrinsic.
        assert!(ui.state.hit_rects.contains_key("g"));
    }

    // ── Stage 2c.3: feed_wheel + drag-as-scroll ───────────────────────

    /// Pre-seed a scroll cell so `scroll_region_at` has a viewport rect
    /// to hit-test against without running a full present pass.
    fn seed_scroll_cell(
        state: &mut UiState, key: &str,
        rect: Rect, content_h: f32, scroll_y: f32,
    ) {
        state.scroll_states.insert(key.to_string(), ScrollCell {
            scroll_y,
            velocity: 0.0,
            content_h,
            viewport_w: rect.w,
            viewport_h: rect.h,
            last_rect: rect,
        });
    }

    fn wheel(dy: f32) -> crate::activity::WheelEvent {
        crate::activity::WheelEvent { dx: 0.0, dy }
    }

    #[test]
    fn feed_wheel_in_region_updates_scroll_y() {
        let mut state = UiState::new();
        seed_scroll_cell(
            &mut state, "feed",
            Rect { x: 0.0, y: 0.0, w: 200.0, h: 400.0 },
            1000.0, 0.0,
        );
        // Hover the pointer over the region first — wheel needs
        // last_pointer to hit-test.
        state.feed_pointer(pointer(100.0, 200.0, PointerPhase::Move));
        state.feed_wheel(wheel(100.0));
        assert_eq!(state.scroll_states["feed"].scroll_y, 100.0);
    }

    #[test]
    fn feed_wheel_outside_region_is_ignored() {
        let mut state = UiState::new();
        seed_scroll_cell(
            &mut state, "feed",
            Rect { x: 0.0, y: 0.0, w: 200.0, h: 400.0 },
            1000.0, 50.0,
        );
        state.feed_pointer(pointer(500.0, 500.0, PointerPhase::Move));
        state.feed_wheel(wheel(100.0));
        // No change — pointer was outside the region.
        assert_eq!(state.scroll_states["feed"].scroll_y, 50.0);
    }

    #[test]
    fn feed_wheel_clamps_at_bottom() {
        let mut state = UiState::new();
        seed_scroll_cell(
            &mut state, "feed",
            Rect { x: 0.0, y: 0.0, w: 200.0, h: 400.0 },
            500.0, 50.0,
        );
        state.feed_pointer(pointer(50.0, 50.0, PointerPhase::Move));
        // 500 - 400 = 100 max; ask for far past the end.
        state.feed_wheel(wheel(10_000.0));
        assert_eq!(state.scroll_states["feed"].scroll_y, 100.0);
    }

    #[test]
    fn feed_wheel_clamps_at_top() {
        let mut state = UiState::new();
        seed_scroll_cell(
            &mut state, "feed",
            Rect { x: 0.0, y: 0.0, w: 200.0, h: 400.0 },
            1000.0, 30.0,
        );
        state.feed_pointer(pointer(50.0, 50.0, PointerPhase::Move));
        state.feed_wheel(wheel(-1_000.0));
        assert_eq!(state.scroll_states["feed"].scroll_y, 0.0);
    }

    #[test]
    fn feed_wheel_zeroes_velocity() {
        let mut state = UiState::new();
        seed_scroll_cell(
            &mut state, "feed",
            Rect { x: 0.0, y: 0.0, w: 200.0, h: 400.0 },
            1000.0, 0.0,
        );
        state.scroll_states.get_mut("feed").unwrap().velocity = 12.5;
        state.feed_pointer(pointer(50.0, 50.0, PointerPhase::Move));
        state.feed_wheel(wheel(10.0));
        assert_eq!(state.scroll_states["feed"].velocity, 0.0,
            "wheel kills active fling so the two don't fight");
    }

    #[test]
    fn feed_wheel_without_pointer_history_is_noop() {
        let mut state = UiState::new();
        seed_scroll_cell(
            &mut state, "feed",
            Rect { x: 0.0, y: 0.0, w: 200.0, h: 400.0 },
            1000.0, 25.0,
        );
        // No prior feed_pointer — last_pointer is None.
        state.feed_wheel(wheel(100.0));
        assert_eq!(state.scroll_states["feed"].scroll_y, 25.0);
    }

    #[test]
    fn pointer_down_outside_scroll_is_immediate_tap() {
        let mut state = UiState::new();
        seed_scroll_cell(
            &mut state, "feed",
            Rect { x: 100.0, y: 100.0, w: 200.0, h: 200.0 },
            1000.0, 0.0,
        );
        // Down at (500, 500) — outside the scroll region. Should
        // fire as a click immediately (preserve pre-2c.3 behaviour).
        state.feed_pointer(pointer(500.0, 500.0, PointerPhase::Down));
        assert_eq!(state.pending_clicks, vec![(500.0, 500.0)]);
        // No drag candidate was seeded.
        assert!(state.drag_gesture.is_none());
    }

    #[test]
    fn pointer_down_inside_scroll_defers_click_seeds_drag_candidate() {
        let mut state = UiState::new();
        seed_scroll_cell(
            &mut state, "feed",
            Rect { x: 0.0, y: 0.0, w: 200.0, h: 400.0 },
            1000.0, 0.0,
        );
        state.feed_pointer(pointer(50.0, 50.0, PointerPhase::Down));
        // No click queued yet — gesture might still become a drag.
        assert!(state.pending_clicks.is_empty());
        let g = state.drag_gesture.as_ref().expect("drag gesture seeded");
        assert_eq!(g.key, "feed");
        assert!(!g.is_dragging);
    }

    #[test]
    fn pointer_up_below_slop_promotes_to_tap() {
        let mut state = UiState::new();
        seed_scroll_cell(
            &mut state, "feed",
            Rect { x: 0.0, y: 0.0, w: 200.0, h: 400.0 },
            1000.0, 0.0,
        );
        state.feed_pointer(pointer(50.0, 50.0, PointerPhase::Down));
        // Move within slop (DRAG_SLOP = 6.0).
        state.feed_pointer(pointer(52.0, 54.0, PointerPhase::Move));
        state.feed_pointer(pointer(50.0, 50.0, PointerPhase::Up));
        // Tap promoted: the original (down) position is the click.
        assert_eq!(state.pending_clicks, vec![(50.0, 50.0)]);
        // Scroll didn't move.
        assert_eq!(state.scroll_states["feed"].scroll_y, 0.0);
        // Drag state cleared.
        assert!(state.drag_gesture.is_none());
    }

    #[test]
    fn pointer_move_past_slop_starts_dragging_no_tap_on_up() {
        let mut state = UiState::new();
        seed_scroll_cell(
            &mut state, "feed",
            Rect { x: 0.0, y: 0.0, w: 200.0, h: 400.0 },
            1000.0, 0.0,
        );
        state.feed_pointer(pointer(50.0, 100.0, PointerPhase::Down));
        // Move past slop (50 > 6).
        state.feed_pointer(pointer(50.0, 50.0, PointerPhase::Move));
        // scroll_y should have increased — finger went up by 50, so
        // content scrolls up by 50.
        assert_eq!(state.scroll_states["feed"].scroll_y, 50.0);
        state.feed_pointer(pointer(50.0, 50.0, PointerPhase::Up));
        // No tap (the gesture was a drag).
        assert!(state.pending_clicks.is_empty());
        // Drag cleared.
        assert!(state.drag_gesture.is_none());
    }

    #[test]
    fn drag_velocity_ema_accumulates_from_move_deltas() {
        let mut state = UiState::new();
        seed_scroll_cell(
            &mut state, "feed",
            Rect { x: 0.0, y: 0.0, w: 200.0, h: 400.0 },
            1000.0, 0.0,
        );
        state.feed_pointer(pointer(50.0, 100.0, PointerPhase::Down));
        // Cross slop and a couple more moves to accumulate velocity.
        state.feed_pointer(pointer(50.0, 80.0, PointerPhase::Move));   // delta +20
        state.feed_pointer(pointer(50.0, 60.0, PointerPhase::Move));   // delta +20
        state.feed_pointer(pointer(50.0, 40.0, PointerPhase::Move));   // delta +20
        state.feed_pointer(pointer(50.0, 40.0, PointerPhase::Up));
        let v = state.scroll_states["feed"].velocity;
        // Three +20 samples through EMA(0.5/0.5): 10 → 15 → 17.5.
        assert!(v > 0.0, "velocity should be positive after upward drag, got {v}");
        assert!(v <= 20.0,
            "velocity capped by latest sample (EMA can't exceed input), got {v}");
    }

    #[test]
    fn cancel_drops_gesture_without_tap() {
        let mut state = UiState::new();
        seed_scroll_cell(
            &mut state, "feed",
            Rect { x: 0.0, y: 0.0, w: 200.0, h: 400.0 },
            1000.0, 0.0,
        );
        state.feed_pointer(pointer(50.0, 50.0, PointerPhase::Down));
        state.feed_pointer(pointer(50.0, 50.0, PointerPhase::Cancel));
        assert!(state.pending_clicks.is_empty());
        assert!(state.drag_gesture.is_none());
    }

    #[test]
    fn nested_scroll_innermost_wins_for_wheel_routing() {
        let mut state = UiState::new();
        seed_scroll_cell(
            &mut state, "outer",
            Rect { x: 0.0, y: 0.0, w: 800.0, h: 600.0 },
            10_000.0, 0.0,
        );
        seed_scroll_cell(
            &mut state, "inner",
            Rect { x: 100.0, y: 100.0, w: 300.0, h: 200.0 },
            5_000.0, 0.0,
        );
        // Point lands inside both viewport rects; inner has smaller
        // area so wheel routes there.
        state.feed_pointer(pointer(200.0, 200.0, PointerPhase::Move));
        state.feed_wheel(wheel(80.0));
        assert_eq!(state.scroll_states["inner"].scroll_y, 80.0);
        assert_eq!(state.scroll_states["outer"].scroll_y, 0.0);
        // Point outside inner but inside outer routes to outer.
        state.feed_pointer(pointer(600.0, 400.0, PointerPhase::Move));
        state.feed_wheel(wheel(50.0));
        assert_eq!(state.scroll_states["outer"].scroll_y, 50.0);
    }

    #[test]
    fn drag_outside_scroll_region_still_clicks() {
        // Pre-2c.3 invariant: a click on a button that isn't inside a
        // scroll region must still register on Down. Drag tracking
        // only kicks in for Downs inside a scroll region.
        let mut state = UiState::new();
        // No scroll states at all.
        state.feed_pointer(pointer(50.0, 50.0, PointerPhase::Down));
        // Wiggle around — these are Move events outside any scroll.
        state.feed_pointer(pointer(60.0, 60.0, PointerPhase::Move));
        state.feed_pointer(pointer(60.0, 60.0, PointerPhase::Up));
        // Click fired on Down (preserve pre-2c.3 timing).
        assert_eq!(state.pending_clicks, vec![(50.0, 50.0)]);
    }

    #[test]
    fn rect_contains_is_half_open() {
        // Left + top edges inclusive, right + bottom exclusive. This
        // matches typical pixel-grid conventions and means a click at
        // the exact boundary between two widgets resolves to the one
        // further down-right.
        let r = Rect { x: 10.0, y: 20.0, w: 30.0, h: 40.0 };
        assert!(r.contains(10.0, 20.0));   // top-left corner
        assert!(r.contains(39.999, 59.999));
        assert!(!r.contains(40.0, 20.0));  // just past the right edge
        assert!(!r.contains(10.0, 60.0));  // just past the bottom edge
        assert!(!r.contains(9.9, 20.0));
    }

    // ── Stage 2c.2: Node::Scroll ──────────────────────────────────────

    /// Build a Scroll containing a 200×1000 rect, present it, then run
    /// `body` against the resulting `UiState` (which now has the scroll
    /// cell populated). Viewport is 200×400 unless overridden.
    fn with_scroll_state<F: FnOnce(&UiState)>(
        viewport_w: f32, viewport_h: f32,
        content_w: f32, content_h: f32,
        key: &str,
        prep: impl FnOnce(&mut UiState),
        body: F,
    ) {
        let mut state = UiState::new();
        prep(&mut state);
        let c = ctx("scroll-test");
        let mut buf = Vec::new();
        let mut canvas = Canvas::new(&mut buf);
        let mut ui = Ui::new(&mut state, &c, &mut canvas);
        let rect_id = ui.rect(0xff00ffff).size(content_w, content_h).build();
        let scroll = ui.scroll(rect_id).key(key).size(viewport_w, viewport_h).build();
        ui.present(scroll);
        body(&state);
    }

    #[test]
    fn scroll_state_starts_at_zero_then_persists_after_first_present() {
        with_scroll_state(200.0, 400.0, 200.0, 1000.0, "feed", |_| {}, |state| {
            let cell = state.scroll_states.get("feed").expect("cell present");
            assert_eq!(cell.scroll_y, 0.0, "first frame starts at top");
            assert_eq!(cell.content_h, 1000.0);
            assert_eq!(cell.viewport_h, 400.0);
            assert_eq!(cell.viewport_w, 200.0);
            assert_eq!(cell.last_rect.w, 200.0);
            assert_eq!(cell.last_rect.h, 400.0);
        });
    }

    #[test]
    fn scroll_to_clamps_within_content() {
        // Programmatic scroll_to before any present uses default
        // content/viewport (both 0), so it gets clamped to 0. The
        // first present then refreshes the dims; a second present
        // round with a manually-set offset shows the clamp works.
        let mut state = UiState::new();
        // Manually pre-populate so scroll_to has dims to clamp against.
        state.scroll_states.insert("feed".to_string(), ScrollCell {
            content_h: 1000.0, viewport_h: 400.0, ..Default::default()
        });
        // Out-of-range high → clamped to (1000 - 400) = 600.
        {
            let c = ctx("t");
            let mut buf = Vec::new();
            let mut canvas = Canvas::new(&mut buf);
            let mut ui = Ui::new(&mut state, &c, &mut canvas);
            ui.scroll_to("feed", 1_000_000.0);
            assert_eq!(ui.scroll_offset("feed"), 600.0);
        }
        // Negative → clamped to 0.
        {
            let c = ctx("t");
            let mut buf = Vec::new();
            let mut canvas = Canvas::new(&mut buf);
            let mut ui = Ui::new(&mut state, &c, &mut canvas);
            ui.scroll_to("feed", -50.0);
            assert_eq!(ui.scroll_offset("feed"), 0.0);
        }
    }

    #[test]
    fn scroll_to_zeroes_velocity() {
        // Stage 2c.4 sets velocity from a drag-release; 2c.2 just
        // pins the contract — `scroll_to` is a hard override, not a
        // gentle nudge.
        let mut state = UiState::new();
        state.scroll_states.insert("feed".to_string(), ScrollCell {
            content_h: 1000.0, viewport_h: 400.0, velocity: 25.0,
            ..Default::default()
        });
        let c = ctx("t");
        let mut buf = Vec::new();
        let mut canvas = Canvas::new(&mut buf);
        let mut ui = Ui::new(&mut state, &c, &mut canvas);
        ui.scroll_to("feed", 100.0);
        assert_eq!(state.scroll_states["feed"].velocity, 0.0);
    }

    #[test]
    fn scroll_offset_returns_zero_for_unknown_key() {
        let mut state = UiState::new();
        let c = ctx("t");
        let mut buf = Vec::new();
        let mut canvas = Canvas::new(&mut buf);
        let ui = Ui::new(&mut state, &c, &mut canvas);
        assert_eq!(ui.scroll_offset("never-built"), 0.0);
        assert_eq!(ui.scroll_extent("never-built"), (0.0, 0.0));
    }

    #[test]
    fn scroll_position_persists_across_frames() {
        // Frame 1: present + scroll_to(200). Frame 2: rebuild same
        // Scroll node; scroll_y should still be 200.
        let mut state = UiState::new();
        // Frame 1
        {
            let c = ctx("t");
            let mut buf = Vec::new();
            let mut canvas = Canvas::new(&mut buf);
            let mut ui = Ui::new(&mut state, &c, &mut canvas);
            let r = ui.rect(0xff00ffff).size(200.0, 1000.0).build();
            let s = ui.scroll(r).key("feed").size(200.0, 400.0).build();
            ui.present(s);
            ui.scroll_to("feed", 200.0);
        }
        // Frame 2 — same key, but a wholly new builder.
        {
            let c = ctx("t");
            let mut buf = Vec::new();
            let mut canvas = Canvas::new(&mut buf);
            let mut ui = Ui::new(&mut state, &c, &mut canvas);
            let r = ui.rect(0xff00ffff).size(200.0, 1000.0).build();
            let s = ui.scroll(r).key("feed").size(200.0, 400.0).build();
            ui.present(s);
            assert_eq!(ui.scroll_offset("feed"), 200.0);
        }
    }

    #[test]
    fn scroll_content_shrink_re_clamps_existing_offset() {
        // Frame 1: tall content + offset near the bottom. Frame 2:
        // content shrinks → present() must clamp the cached offset
        // against the new (smaller) max. Without re-clamp, the
        // viewport would point past the end and paint backdrop.
        let mut state = UiState::new();
        {
            let c = ctx("t");
            let mut buf = Vec::new();
            let mut canvas = Canvas::new(&mut buf);
            let mut ui = Ui::new(&mut state, &c, &mut canvas);
            let r = ui.rect(0xff00ffff).size(200.0, 1000.0).build();
            let s = ui.scroll(r).key("feed").size(200.0, 400.0).build();
            ui.present(s);
            ui.scroll_to("feed", 500.0); // valid against 1000-400=600 max
        }
        // Frame 2: child shrinks to 300 tall, so max scroll = 0.
        {
            let c = ctx("t");
            let mut buf = Vec::new();
            let mut canvas = Canvas::new(&mut buf);
            let mut ui = Ui::new(&mut state, &c, &mut canvas);
            let r = ui.rect(0xff00ffff).size(200.0, 300.0).build();
            let s = ui.scroll(r).key("feed").size(200.0, 400.0).build();
            ui.present(s);
            assert_eq!(
                ui.scroll_offset("feed"), 0.0,
                "shrinking content past viewport must clamp existing offset",
            );
        }
    }

    #[test]
    fn scroll_with_viewport_taller_than_content_pins_offset_to_zero() {
        // Content fits in viewport: max = (200 - 400).max(0) = 0.
        let mut state = UiState::new();
        state.scroll_states.insert("feed".to_string(), ScrollCell {
            scroll_y: 50.0, ..Default::default()
        });
        let c = ctx("t");
        let mut buf = Vec::new();
        let mut canvas = Canvas::new(&mut buf);
        let mut ui = Ui::new(&mut state, &c, &mut canvas);
        let r = ui.rect(0xff00ffff).size(200.0, 200.0).build();
        let s = ui.scroll(r).key("feed").size(200.0, 400.0).build();
        ui.present(s);
        assert_eq!(ui.scroll_offset("feed"), 0.0);
    }

    #[test]
    fn scroll_publishes_scroll_role_with_key_as_id() {
        // The semantic projection emits `SemanticRole::Scroll` and
        // surfaces the caller's key as the node's `id` so tests can
        // address the region via `findById`. We build the tree
        // bottom-up by hand here because present()'s emission path
        // skips when handle=0 (test fixture default).
        let nodes = vec![
            Node::Rect { color: 0xff00ffff, min: (200.0, 1000.0), radius: 0.0 },
            Node::Scroll {
                child: 0, key: "feed".to_string(),
                viewport_w: Some(200.0), viewport_h: Some(400.0),
            },
        ];
        let attrs: Vec<NodeAttr> = (0..nodes.len()).map(|_| NodeAttr::default()).collect();
        let sizes = layout::measure(&nodes, 1, (800.0, 600.0), 1.0,
            &layout::estimate_text_width);
        let scroll_offsets: HashMap<String, f32> = HashMap::new();
        let mut rects: HashMap<NodeId, Rect> = HashMap::new();
        layout::layout(
            &nodes, &attrs, 1,
            Rect { x: 0.0, y: 0.0, w: 800.0, h: 600.0 },
            &sizes, &scroll_offsets, &mut rects,
        );
        let tree = build_semantic_tree(&nodes, 1, &rects);
        assert!(!tree.nodes.is_empty());
        let root = &tree.nodes[0];
        assert_eq!(root.role, semantics::SemanticRole::Scroll);
        assert_eq!(root.id.as_deref(), Some("feed"));
    }

    #[test]
    fn scroll_paint_pushes_clip_around_child() {
        // Paint emits a push_clip(viewport) → child paint → pop_clip,
        // so off-screen child paint can't bleed onto other widgets.
        // Canvas op tag bytes are wire-stable (see canvas.rs:
        // `OP_PUSH_CLIP = 3`, `OP_POP_CLIP = 4`); the byte-search
        // avoids exposing the constants publicly just for the test.
        const OP_PUSH_CLIP: u8 = 3;
        const OP_POP_CLIP:  u8 = 4;
        let mut state = UiState::new();
        let c = ctx("t");
        let mut buf = Vec::new();
        {
            let mut canvas = Canvas::new(&mut buf);
            let mut ui = Ui::new(&mut state, &c, &mut canvas);
            let r = ui.rect(0xff00ffff).size(200.0, 1000.0).build();
            let s = ui.scroll(r).key("feed").size(200.0, 400.0).build();
            ui.present(s);
        }
        // Byte-scan: any occurrence of the op tag byte is "an op of
        // that kind was emitted". We don't have to skip per-op
        // payloads — false matches against payload bytes are
        // possible but vanishingly unlikely with rect coords like
        // 0/200/400. The test is good-enough sentinel for "clip is
        // there at all".
        assert!(buf.iter().any(|&b| b == OP_PUSH_CLIP),
            "Scroll must emit push_clip around its child");
        assert!(buf.iter().any(|&b| b == OP_POP_CLIP),
            "Scroll must emit a matching pop_clip");
    }

    #[test]
    fn scrolled_offset_shifts_child_rect_upward() {
        // After scroll_to(N), the child's placed rect should be at
        // `y = viewport.y - N`. We seed the scroll cell, then present
        // once to let the layout pass shift the child slot. The Rect
        // child fully renders at the shifted y; descendants of a
        // VStack/Frame inside Scroll would inherit the same origin
        // naturally, so this single-child case is the load-bearing
        // geometric check.
        let mut state = UiState::new();
        state.scroll_states.insert("feed".to_string(), ScrollCell {
            content_h: 1000.0, viewport_h: 400.0, scroll_y: 250.0,
            ..Default::default()
        });
        let c = ctx("t");
        let mut buf = Vec::new();
        let mut canvas = Canvas::new(&mut buf);
        let mut ui = Ui::new(&mut state, &c, &mut canvas);
        let r = ui.rect(0xff00ffff).size(200.0, 1000.0).build();
        let s = ui.scroll(r).key("feed").size(200.0, 400.0).build();
        ui.present(s);
        // Re-read rects via the state's last_rect for the Scroll,
        // and inspect the child rect via a fresh layout pass that
        // mirrors what present() did internally.
        let scroll_offsets: HashMap<String, f32> =
            state.scroll_states.iter().map(|(k, c)| (k.clone(), c.scroll_y)).collect();
        let nodes = vec![
            Node::Rect { color: 0xff00ffff, min: (200.0, 1000.0), radius: 0.0 },
            Node::Scroll {
                child: 0, key: "feed".to_string(),
                viewport_w: Some(200.0), viewport_h: Some(400.0),
            },
        ];
        let attrs: Vec<NodeAttr> = (0..nodes.len()).map(|_| NodeAttr::default()).collect();
        let sizes = layout::measure(&nodes, 1, (800.0, 600.0), 1.0,
            &layout::estimate_text_width);
        let mut rects: HashMap<NodeId, Rect> = HashMap::new();
        layout::layout(
            &nodes, &attrs, 1,
            Rect { x: 0.0, y: 0.0, w: 800.0, h: 600.0 },
            &sizes, &scroll_offsets, &mut rects,
        );

        // Scroll viewport sits at y=0; child rect is at y = 0 - 250.
        assert_eq!(rects[&1].y, 0.0);
        assert_eq!(rects[&0].y, -250.0);
        // Child height is the content_h (1000), not the viewport.
        assert_eq!(rects[&0].h, 1000.0);
    }
}
