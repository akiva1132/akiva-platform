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

use std::collections::HashMap;

use crate::activity::{Context, PointerEvent, PointerPhase};
use crate::canvas::Canvas;
use node::{Node, NodeId};

/// How a stack distributes its children along the main axis.
///
/// `Start` keeps the stack intrinsic-sized (no leftover to distribute);
/// every other variant fills the slot on the main axis so alignment has
/// somewhere to apply. `SpaceBetween` adds equal extra gap between each
/// adjacent pair — the explicit `.gap()` setting is the *minimum* and
/// this extra space stacks on top.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MainAxis {
    #[default]
    Start,
    Center,
    End,
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
    #[default]
    Start,
    Center,
    End,
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
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && x < self.x + self.w
            && y >= self.y && y < self.y + self.h
    }
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
}

impl UiState {
    pub fn new() -> Self { Self::default() }

    /// Feed pointer events so `Ui::was_clicked` can correlate presses to
    /// widgets. Call this unconditionally from your `on_pointer` — it
    /// filters to `Down` internally and costs ~nothing otherwise.
    pub fn feed_pointer(&mut self, ev: PointerEvent) {
        if matches!(ev.phase, PointerPhase::Down) {
            self.pending_clicks.push((ev.x, ev.y));
        }
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
}

impl<'s, 'c, 'b> Ui<'s, 'c, 'b> {
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

    pub fn text(&mut self, text: impl Into<String>) -> node::TextBuilder<'_, 's, 'c, 'b> {
        node::TextBuilder::new(self, text.into())
    }

    pub fn rect(&mut self, color: u32) -> node::RectBuilder<'_, 's, 'c, 'b> {
        node::RectBuilder::new(self, color)
    }

    pub fn spacer(&mut self) -> node::SpacerBuilder<'_, 's, 'c, 'b> {
        node::SpacerBuilder::new(self)
    }

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
        })
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

    pub fn v_stack(&mut self, children: &[NodeId]) -> node::StackBuilder<'_, 's, 'c, 'b> {
        node::StackBuilder::new_v(self, children.to_vec())
    }

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
        let mut rects: HashMap<NodeId, Rect> = HashMap::new();
        layout::layout(
            &self.nodes, &self.attrs, root,
            Rect { x: 0.0, y: 0.0, w: self.viewport.0, h: self.viewport.1 },
            &sizes, &mut rects,
        );

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

        paint::paint(&self.nodes, root, &rects, &self.state.current_clicks, self.canvas);
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
}
