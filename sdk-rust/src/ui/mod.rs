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
//! ## Layout model
//!
//! Two-pass: bottom-up `measure` to get intrinsic sizes, top-down `layout`
//! to place every node within its parent. VStack sums children vertically
//! and takes max child width; HStack is the mirror. Leaves (Text, Rect)
//! have a content-derived intrinsic size. No wrapping, no flex, no
//! alignment controls beyond start/start — that's Phase 3b.

pub mod node;
pub mod layout;
pub mod paint;

use std::collections::HashMap;

use crate::activity::{Context, PointerEvent, PointerPhase};
use crate::canvas::Canvas;
use node::{Node, NodeId};

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
    /// Pointer-down positions queued up since the last `present()`. A
    /// single press may fire in the middle of a long frame; we batch
    /// them until the next present consumes them.
    pending_clicks: Vec<(f32, f32)>,
    /// Hit rects latched during `present()` — used by same-frame
    /// `was_clicked` calls after present has run.
    current_clicks: Vec<String>,
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
    presented: bool,
}

impl<'s, 'c, 'b> Ui<'s, 'c, 'b> {
    pub fn new(
        state: &'s mut UiState,
        ctx: &Context<'_>,
        canvas: &'c mut Canvas<'b>,
    ) -> Self {
        // Reset per-frame derived state up front. Hit rects will be
        // repopulated by `present()`; clicks are consumed then too.
        state.hit_rects.clear();
        state.current_clicks.clear();
        Self {
            state,
            canvas,
            viewport: (ctx.metrics.width_px as f32, ctx.metrics.height_px as f32),
            density: if ctx.metrics.density > 0.0 { ctx.metrics.density } else { 1.0 },
            nodes: Vec::new(),
            presented: false,
        }
    }

    pub(crate) fn push(&mut self, node: Node) -> NodeId {
        let id = self.nodes.len();
        self.nodes.push(node);
        id
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
            label: label.into(),
            label_size: 14.0,
            bg: 0xff2a3a50,
            bg_pressed: 0xff3a5070,
            fg: 0xfff0f4fa,
            pad: (16.0, 10.0),
        })
    }

    pub fn v_stack(&mut self, children: &[NodeId]) -> node::StackBuilder<'_, 's, 'c, 'b> {
        node::StackBuilder::new_v(self, children.to_vec())
    }

    pub fn h_stack(&mut self, children: &[NodeId]) -> node::StackBuilder<'_, 's, 'c, 'b> {
        node::StackBuilder::new_h(self, children.to_vec())
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

        let sizes = layout::measure(&self.nodes, root, self.viewport, self.density);
        let mut rects: HashMap<NodeId, Rect> = HashMap::new();
        layout::layout(&self.nodes, root, Rect { x: 0.0, y: 0.0, w: self.viewport.0, h: self.viewport.1 }, &sizes, &mut rects);

        // Record hit rects for buttons before paint, so `was_clicked`
        // queries that come after `present()` see them.
        for (node_id, rect) in &rects {
            if let Node::Button { id, .. } = &self.nodes[*node_id] {
                self.state.hit_rects.insert(id.clone(), *rect);
            }
        }

        // Resolve clicks against the just-latched hit rects: an `id`
        // is considered clicked this frame if any pending click point
        // falls inside its rect. Pending clicks are drained (a click
        // should fire once).
        for (x, y) in self.state.pending_clicks.drain(..) {
            for (id, rect) in &self.state.hit_rects {
                if rect.contains(x, y) {
                    self.state.current_clicks.push(id.clone());
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
}
