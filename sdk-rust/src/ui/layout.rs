//! Measure + layout for the widget tree.
//!
//! Two passes:
//!
//! 1. [`measure`] — bottom-up recursion that produces an intrinsic size
//!    for every node. Stacks sum their children along the main axis and
//!    take max along the cross; leaves report content-derived sizes.
//!    Text width comes from the caller-supplied measurer (wired to the
//!    host's `CanvasRenderingContext2D.measureText` in production); the
//!    vertical extent stays a `size × line-height-ratio` computation
//!    because single-line metrics from the host don't add much there.
//!    Alignment is a pure placement concern, so it never influences the
//!    intrinsic sizes reported here.
//!
//! 2. [`layout`] — top-down placement. The root gets the full viewport
//!    as its rect; a stack with non-`Start` [`MainAxis`] / [`CrossAxis`]
//!    first *fills* its slot on the relevant axis, then distributes any
//!    leftover space across its children according to the chosen
//!    variant. No wrapping, no flex growth (see mod docs — `Grow` is a
//!    separate polish step).

use std::collections::HashMap;

use super::node::{Node, NodeId};
use super::{CrossAxis, MainAxis, NodeAttr, Rect};

/// Line-height multiplier for intrinsic text height — matches the
/// ~1.3 default line-height of most UA stylesheets, which is what
/// Canvas2D `fillText` visually implies.
const TEXT_LINE_H_RATIO: f32 = 1.3;

/// Estimate-based width used when the host isn't available (tests,
/// fallback). 0.55 × size × char-count over-estimates narrow glyphs
/// slightly, which leaves a comfortable tap target and avoids
/// clipping. Kept `pub(crate)` so tests can assert against it.
/// Only referenced from tests — in production builds the measurer
/// path goes through `runtime::text_width`.
#[cfg(test)]
pub(crate) fn estimate_text_width(text: &str, size: f32) -> f32 {
    text.chars().count() as f32 * size * 0.55
}

pub fn measure(
    nodes: &[Node],
    root: NodeId,
    viewport: (f32, f32),
    _density: f32,
    text_width: &dyn Fn(&str, f32) -> f32,
) -> HashMap<NodeId, (f32, f32)> {
    let mut out = HashMap::new();
    measure_node(nodes, root, viewport, text_width, &mut out);
    out
}

fn measure_node(
    nodes: &[Node],
    id: NodeId,
    avail: (f32, f32),
    text_width: &dyn Fn(&str, f32) -> f32,
    out: &mut HashMap<NodeId, (f32, f32)>,
) -> (f32, f32) {
    let size = match &nodes[id] {
        Node::Text { text, size, .. } => {
            let w = text_width(text, *size);
            let h = *size * TEXT_LINE_H_RATIO;
            (w, h)
        }
        Node::Rect { min, .. } => *min,
        Node::Spacer { min } => *min,
        Node::Button { label, label_size, pad, .. } => {
            let label_w = text_width(label, *label_size);
            let label_h = *label_size * TEXT_LINE_H_RATIO;
            (label_w + 2.0 * pad.0, label_h + 2.0 * pad.1)
        }
        Node::VStack { children, gap, padding, .. } => {
            let inner_avail = (
                (avail.0 - padding.1 - padding.3).max(0.0),
                (avail.1 - padding.0 - padding.2).max(0.0),
            );
            let mut max_w: f32 = 0.0;
            let mut sum_h: f32 = 0.0;
            for (i, c) in children.iter().enumerate() {
                let (cw, ch) = measure_node(nodes, *c, inner_avail, text_width, out);
                if cw > max_w { max_w = cw; }
                sum_h += ch;
                if i + 1 < children.len() { sum_h += gap; }
            }
            (max_w + padding.1 + padding.3, sum_h + padding.0 + padding.2)
        }
        Node::HStack { children, gap, padding, .. } => {
            let inner_avail = (
                (avail.0 - padding.1 - padding.3).max(0.0),
                (avail.1 - padding.0 - padding.2).max(0.0),
            );
            let mut sum_w: f32 = 0.0;
            let mut max_h: f32 = 0.0;
            for (i, c) in children.iter().enumerate() {
                let (cw, ch) = measure_node(nodes, *c, inner_avail, text_width, out);
                sum_w += cw;
                if ch > max_h { max_h = ch; }
                if i + 1 < children.len() { sum_w += gap; }
            }
            (sum_w + padding.1 + padding.3, max_h + padding.0 + padding.2)
        }
    };
    out.insert(id, size);
    size
}

pub fn layout(
    nodes: &[Node],
    attrs: &[NodeAttr],
    root: NodeId,
    viewport: Rect,
    sizes: &HashMap<NodeId, (f32, f32)>,
    out: &mut HashMap<NodeId, Rect>,
) {
    place(nodes, attrs, root, viewport, /*stretch_w=*/ false, /*stretch_h=*/ false, sizes, out);
}

/// Place `id` inside `slot`. Stacks with non-`Start` alignment expand to
/// fill their slot on the corresponding axis, producing leftover space
/// that the child loop distributes. `stretch_w/_h` come from the parent:
/// a VStack with `CrossAxis::Stretch` sets `stretch_w=true` on each
/// child so they fill the stack's inner width regardless of intrinsic.
fn place(
    nodes: &[Node],
    attrs: &[NodeAttr],
    id: NodeId,
    slot: Rect,
    stretch_w: bool,
    stretch_h: bool,
    sizes: &HashMap<NodeId, (f32, f32)>,
    out: &mut HashMap<NodeId, Rect>,
) {
    let (mw, mh) = *sizes.get(&id).expect("measured before layout");
    // Default: take the intrinsic size (clipped by the slot). Stretch
    // from parent forces full slot size on an axis; stacks additionally
    // fill when their own alignment demands leftover space — or when
    // any child has grow>0 and therefore needs room to consume.
    let mut w = if stretch_w { slot.w } else { mw.min(slot.w) };
    let mut h = if stretch_h { slot.h } else { mh.min(slot.h) };
    match &nodes[id] {
        Node::VStack { main, cross, children, .. } => {
            let any_grow = children.iter().any(|c| attrs[*c].grow > 0.0);
            if main.fills_slot() || any_grow { h = slot.h; }
            if cross.fills_slot() { w = slot.w; }
        }
        Node::HStack { main, cross, children, .. } => {
            let any_grow = children.iter().any(|c| attrs[*c].grow > 0.0);
            if main.fills_slot() || any_grow { w = slot.w; }
            if cross.fills_slot() { h = slot.h; }
        }
        _ => {}
    }
    let rect = Rect { x: slot.x, y: slot.y, w, h };
    out.insert(id, rect);

    match &nodes[id] {
        Node::Text { .. }
        | Node::Rect { .. }
        | Node::Spacer { .. }
        | Node::Button { .. } => {}
        Node::VStack { children, gap, padding, main, cross } => {
            let inner_x = rect.x + padding.3;
            let inner_y = rect.y + padding.0;
            let inner_w = (rect.w - padding.1 - padding.3).max(0.0);
            let inner_h = (rect.h - padding.0 - padding.2).max(0.0);
            place_stack(
                nodes, attrs, children, *gap, *main, *cross,
                /*vertical=*/ true,
                inner_x, inner_y, inner_w, inner_h,
                sizes, out,
            );
        }
        Node::HStack { children, gap, padding, main, cross } => {
            let inner_x = rect.x + padding.3;
            let inner_y = rect.y + padding.0;
            let inner_w = (rect.w - padding.1 - padding.3).max(0.0);
            let inner_h = (rect.h - padding.0 - padding.2).max(0.0);
            place_stack(
                nodes, attrs, children, *gap, *main, *cross,
                /*vertical=*/ false,
                inner_x, inner_y, inner_w, inner_h,
                sizes, out,
            );
        }
    }
}

/// Shared child-distribution for VStack and HStack. `vertical=true`
/// treats y as the main axis (VStack); otherwise x is the main axis
/// (HStack). Kept as one function because the only per-variant difference
/// is which axis is "main" — swapping avoids duplicating the distribution
/// maths in two near-identical branches where a fix has to land twice.
#[allow(clippy::too_many_arguments)]
fn place_stack(
    nodes: &[Node],
    attrs: &[NodeAttr],
    children: &[NodeId],
    gap: f32,
    main: MainAxis,
    cross: CrossAxis,
    vertical: bool,
    inner_x: f32,
    inner_y: f32,
    inner_w: f32,
    inner_h: f32,
    sizes: &HashMap<NodeId, (f32, f32)>,
    out: &mut HashMap<NodeId, Rect>,
) {
    let (inner_main, inner_cross) = if vertical { (inner_h, inner_w) } else { (inner_w, inner_h) };

    // Sum intrinsic main sizes + gaps. Grow children keep their intrinsic
    // size as a baseline (flexbox's flex-basis=auto semantics) and pick
    // up extra space afterwards.
    let n = children.len();
    let mut content_main: f32 = 0.0;
    let mut total_grow: f32 = 0.0;
    for (i, c) in children.iter().enumerate() {
        let (cw, ch) = sizes[c];
        content_main += if vertical { ch } else { cw };
        total_grow += attrs[*c].grow.max(0.0);
        if i + 1 < n { content_main += gap; }
    }

    let leftover = (inner_main - content_main).max(0.0);

    // With any grow>0 child, the leftover is consumed by grow and
    // `MainAxis` alignment has nothing left to distribute. Without grow,
    // the original alignment rule stands.
    let (leading, between) = if total_grow > 0.0 {
        (0.0, 0.0)
    } else {
        match main {
            MainAxis::Start => (0.0, 0.0),
            MainAxis::Center => (leftover * 0.5, 0.0),
            MainAxis::End => (leftover, 0.0),
            MainAxis::SpaceBetween => {
                if n < 2 { (0.0, 0.0) } else { (0.0, leftover / (n - 1) as f32) }
            }
        }
    };

    let mut cursor = if vertical { inner_y } else { inner_x } + leading;
    for (i, c) in children.iter().enumerate() {
        let (cw, ch) = sizes[c];
        let (base_main, child_cross) = if vertical { (ch, cw) } else { (cw, ch) };

        // Grow: base main + share of leftover proportional to this
        // child's factor over the group total. Non-grow children use
        // the base (intrinsic) size.
        let grow = attrs[*c].grow.max(0.0);
        let child_main = if total_grow > 0.0 && grow > 0.0 {
            base_main + leftover * (grow / total_grow)
        } else {
            base_main
        };

        // Cross-axis placement. `Stretch` overrides the measured cross
        // size with the inner cross; the others just shift the origin.
        let (cross_off, cross_size) = match cross {
            CrossAxis::Start   => (0.0, child_cross),
            CrossAxis::Center  => (((inner_cross - child_cross).max(0.0)) * 0.5, child_cross),
            CrossAxis::End     => ((inner_cross - child_cross).max(0.0), child_cross),
            CrossAxis::Stretch => (0.0, inner_cross),
        };

        let child_slot = if vertical {
            Rect { x: inner_x + cross_off, y: cursor, w: cross_size, h: child_main }
        } else {
            Rect { x: cursor, y: inner_y + cross_off, w: child_main, h: cross_size }
        };
        // Stretch-from-parent flags: cross from `Stretch`, main from
        // grow>0. Both let the child bypass its intrinsic clamp in
        // `place` so containers (nested stacks) actually fill the
        // slot they've been given.
        let stretch_cross = matches!(cross, CrossAxis::Stretch);
        let stretch_main  = grow > 0.0;
        let (child_stretch_w, child_stretch_h) = if vertical {
            (stretch_cross, stretch_main)
        } else {
            (stretch_main, stretch_cross)
        };
        place(nodes, attrs, *c, child_slot, child_stretch_w, child_stretch_h, sizes, out);

        cursor += child_main;
        if i + 1 < n { cursor += gap + between; }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // `f32` equality is fraught, but every value we compute here comes
    // out of a deterministic chain of adds/mults with small integer
    // operands, so exact equality works. If a test starts tripping over
    // fp drift, switch *that one* to `approx_eq` — don't rewrite the
    // whole suite.
    fn approx(a: f32, b: f32) -> bool { (a - b).abs() < 1e-4 }

    fn text(s: &str, size: f32) -> Node {
        Node::Text { text: s.into(), size, color: 0 }
    }
    fn rect(w: f32, h: f32) -> Node {
        Node::Rect { color: 0, min: (w, h) }
    }
    /// Default-aligned VStack — alignment defaults match pre-C1 behaviour
    /// (Start/Start) so tests written against the original intrinsic
    /// layout keep passing unchanged.
    fn vstack(children: Vec<NodeId>, gap: f32, padding: (f32, f32, f32, f32)) -> Node {
        Node::VStack { children, gap, padding, main: MainAxis::Start, cross: CrossAxis::Start }
    }
    fn hstack(children: Vec<NodeId>, gap: f32, padding: (f32, f32, f32, f32)) -> Node {
        Node::HStack { children, gap, padding, main: MainAxis::Start, cross: CrossAxis::Start }
    }
    fn vstack_aligned(
        children: Vec<NodeId>, gap: f32, main: MainAxis, cross: CrossAxis,
    ) -> Node {
        Node::VStack { children, gap, padding: (0.0, 0.0, 0.0, 0.0), main, cross }
    }
    fn hstack_aligned(
        children: Vec<NodeId>, gap: f32, main: MainAxis, cross: CrossAxis,
    ) -> Node {
        Node::HStack { children, gap, padding: (0.0, 0.0, 0.0, 0.0), main, cross }
    }
    /// Default attrs for a tree of `n` nodes — no grow anywhere. Used by
    /// every test that doesn't exercise grow specifically.
    fn default_attrs(n: usize) -> Vec<NodeAttr> {
        vec![NodeAttr::default(); n]
    }
    /// Convenience wrapper around [`layout`] for tests that don't care
    /// about grow: derives default attrs from `nodes.len()`.
    fn layout_default(
        nodes: &[Node],
        root: NodeId,
        viewport: Rect,
        sizes: &HashMap<NodeId, (f32, f32)>,
        out: &mut HashMap<NodeId, Rect>,
    ) {
        let attrs = default_attrs(nodes.len());
        layout(nodes, &attrs, root, viewport, sizes, out);
    }

    // ── Measure ──────────────────────────────────────────────────────

    #[test]
    fn text_measures_from_char_count_and_size() {
        let nodes = vec![text("abc", 10.0)];
        let sizes = measure(&nodes, 0, (1000.0, 1000.0), 1.0, &estimate_text_width);
        // 3 chars × 10.0 × 0.55 = 16.5;   10.0 × 1.3 = 13.0
        assert_eq!(sizes[&0], (16.5, 13.0));
    }

    #[test]
    fn rect_reports_its_min_size() {
        let nodes = vec![rect(40.0, 20.0)];
        let sizes = measure(&nodes, 0, (1000.0, 1000.0), 1.0, &estimate_text_width);
        assert_eq!(sizes[&0], (40.0, 20.0));
    }

    #[test]
    fn button_includes_padding_around_label() {
        let nodes = vec![Node::Button {
            id: "b".into(), label: "ok".into(), label_size: 10.0,
            bg: 0, bg_pressed: 0, fg: 0, pad: (8.0, 4.0),
        }];
        let sizes = measure(&nodes, 0, (1000.0, 1000.0), 1.0, &estimate_text_width);
        // label: 2 × 10.0 × 0.55 = 11.0 + 2×8 = 27; 13.0 + 2×4 = 21
        assert_eq!(sizes[&0], (27.0, 21.0));
    }

    #[test]
    fn vstack_sums_child_heights_and_gaps_takes_max_width() {
        let nodes = vec![
            rect(10.0, 5.0), rect(20.0, 7.0),
            vstack(vec![0, 1], 3.0, (0.0, 0.0, 0.0, 0.0)),
        ];
        let sizes = measure(&nodes, 2, (1000.0, 1000.0), 1.0, &estimate_text_width);
        // max(10, 20) = 20; 5 + 7 + 3 = 15
        assert_eq!(sizes[&2], (20.0, 15.0));
    }

    #[test]
    fn hstack_is_the_mirror_of_vstack() {
        let nodes = vec![
            rect(10.0, 5.0), rect(20.0, 7.0),
            hstack(vec![0, 1], 3.0, (0.0, 0.0, 0.0, 0.0)),
        ];
        let sizes = measure(&nodes, 2, (1000.0, 1000.0), 1.0, &estimate_text_width);
        // 10 + 20 + 3 = 33; max(5, 7) = 7
        assert_eq!(sizes[&2], (33.0, 7.0));
    }

    #[test]
    fn stack_padding_adds_to_both_axes() {
        let nodes = vec![
            rect(10.0, 10.0),
            vstack(vec![0], 0.0, (4.0, 5.0, 6.0, 7.0)),
        ];
        let sizes = measure(&nodes, 1, (1000.0, 1000.0), 1.0, &estimate_text_width);
        // w: 10 + 7 + 5 = 22;  h: 10 + 4 + 6 = 20
        assert_eq!(sizes[&1], (22.0, 20.0));
    }

    #[test]
    fn empty_stack_has_only_its_padding() {
        let nodes = vec![
            vstack(vec![], 10.0, (3.0, 3.0, 3.0, 3.0)),
        ];
        let sizes = measure(&nodes, 0, (1000.0, 1000.0), 1.0, &estimate_text_width);
        // gap doesn't apply to zero children; padding remains.
        assert_eq!(sizes[&0], (6.0, 6.0));
    }

    #[test]
    fn single_child_stack_has_no_gap_applied() {
        let nodes = vec![
            rect(10.0, 5.0),
            vstack(vec![0], 100.0, (0.0, 0.0, 0.0, 0.0)),
        ];
        let sizes = measure(&nodes, 1, (1000.0, 1000.0), 1.0, &estimate_text_width);
        assert_eq!(sizes[&1], (10.0, 5.0));
    }

    // ── Place ────────────────────────────────────────────────────────

    #[test]
    fn vstack_stacks_children_top_down_with_gap() {
        let nodes = vec![
            rect(10.0, 5.0), rect(10.0, 7.0),
            vstack(vec![0, 1], 3.0, (0.0, 0.0, 0.0, 0.0)),
        ];
        let sizes = measure(&nodes, 2, (1000.0, 1000.0), 1.0, &estimate_text_width);
        let mut rects = HashMap::new();
        layout_default(&nodes,2, Rect { x: 100.0, y: 200.0, w: 500.0, h: 500.0 }, &sizes, &mut rects);
        // Root at slot origin.
        assert!(approx(rects[&2].x, 100.0) && approx(rects[&2].y, 200.0));
        // First child starts at root's origin (no padding).
        assert!(approx(rects[&0].y, 200.0));
        // Second child: 200 + 5 (first h) + 3 (gap) = 208.
        assert!(approx(rects[&1].y, 208.0));
    }

    #[test]
    fn hstack_stacks_children_left_right_with_gap() {
        let nodes = vec![
            rect(10.0, 5.0), rect(15.0, 5.0),
            hstack(vec![0, 1], 4.0, (0.0, 0.0, 0.0, 0.0)),
        ];
        let sizes = measure(&nodes, 2, (1000.0, 1000.0), 1.0, &estimate_text_width);
        let mut rects = HashMap::new();
        layout_default(&nodes,2, Rect { x: 50.0, y: 0.0, w: 500.0, h: 500.0 }, &sizes, &mut rects);
        assert!(approx(rects[&0].x, 50.0));
        // 50 + 10 + 4 = 64.
        assert!(approx(rects[&1].x, 64.0));
    }

    #[test]
    fn padding_offsets_children_from_stack_origin() {
        let nodes = vec![
            rect(10.0, 10.0),
            vstack(vec![0], 0.0, (4.0, 5.0, 6.0, 7.0)),
        ];
        let sizes = measure(&nodes, 1, (1000.0, 1000.0), 1.0, &estimate_text_width);
        let mut rects = HashMap::new();
        layout_default(&nodes,1, Rect { x: 0.0, y: 0.0, w: 500.0, h: 500.0 }, &sizes, &mut rects);
        // Child shifted right by left-pad (7) and down by top-pad (4).
        assert!(approx(rects[&0].x, 7.0));
        assert!(approx(rects[&0].y, 4.0));
    }

    #[test]
    fn oversized_node_is_clamped_to_slot() {
        // Slot smaller than measured intrinsic — place() should clamp so
        // paint doesn't overdraw the slot.
        let nodes = vec![rect(100.0, 100.0)];
        let sizes = measure(&nodes, 0, (1000.0, 1000.0), 1.0, &estimate_text_width);
        let mut rects = HashMap::new();
        layout_default(&nodes,0, Rect { x: 0.0, y: 0.0, w: 40.0, h: 30.0 }, &sizes, &mut rects);
        assert!(approx(rects[&0].w, 40.0));
        assert!(approx(rects[&0].h, 30.0));
    }

    #[test]
    fn nested_stacks_compound_positions() {
        // HStack of two VStacks; verify the inner leaves end up where
        // we'd expect after two levels of placement.
        let nodes = vec![
            /* 0 */ rect(5.0, 10.0),
            /* 1 */ rect(5.0, 10.0),
            /* 2 */ vstack(vec![0, 1], 2.0, (0.0, 0.0, 0.0, 0.0)),
            /* 3 */ rect(8.0, 10.0),
            /* 4 */ vstack(vec![3], 0.0, (0.0, 0.0, 0.0, 0.0)),
            /* 5 */ hstack(vec![2, 4], 3.0, (0.0, 0.0, 0.0, 0.0)),
        ];
        let sizes = measure(&nodes, 5, (1000.0, 1000.0), 1.0, &estimate_text_width);
        let mut rects = HashMap::new();
        layout_default(&nodes,5, Rect { x: 0.0, y: 0.0, w: 500.0, h: 500.0 }, &sizes, &mut rects);
        // Left VStack at x=0; its children also at x=0.
        assert!(approx(rects[&0].x, 0.0) && approx(rects[&0].y, 0.0));
        assert!(approx(rects[&1].x, 0.0) && approx(rects[&1].y, 12.0)); // 10 + gap 2
        // Right VStack at x = left_w (5) + gap (3) = 8.
        assert!(approx(rects[&4].x, 8.0));
        assert!(approx(rects[&3].x, 8.0) && approx(rects[&3].y, 0.0));
    }

    // ── Alignment (C1) ───────────────────────────────────────────────
    //
    // These tests pin the alignment semantics so a later refactor can't
    // silently shift a button centre by a pixel and still pass. Each
    // stack fills its slot exactly when alignment is non-Start.

    #[test]
    fn main_start_leaves_stack_intrinsic_and_packs_from_start() {
        // Sanity: default stays byte-identical to pre-C1.
        let nodes = vec![
            rect(10.0, 10.0), rect(10.0, 10.0),
            vstack(vec![0, 1], 4.0, (0.0, 0.0, 0.0, 0.0)),
        ];
        let sizes = measure(&nodes, 2, (1000.0, 1000.0), 1.0, &estimate_text_width);
        let mut rects = HashMap::new();
        layout_default(&nodes,2, Rect { x: 0.0, y: 0.0, w: 100.0, h: 100.0 }, &sizes, &mut rects);
        // Stack intrinsic: w=10, h=10+4+10 = 24. Slot was 100×100; Start → no fill.
        assert!(approx(rects[&2].w, 10.0) && approx(rects[&2].h, 24.0));
        assert!(approx(rects[&0].y, 0.0) && approx(rects[&1].y, 14.0));
    }

    #[test]
    fn main_center_fills_slot_and_centres_content() {
        let nodes = vec![
            rect(10.0, 20.0),
            vstack_aligned(vec![0], 0.0, MainAxis::Center, CrossAxis::Start),
        ];
        let sizes = measure(&nodes, 1, (1000.0, 1000.0), 1.0, &estimate_text_width);
        let mut rects = HashMap::new();
        layout_default(&nodes,1, Rect { x: 0.0, y: 0.0, w: 100.0, h: 100.0 }, &sizes, &mut rects);
        // Stack fills main: h = slot.h = 100. Content h = 20. Leftover = 80 →
        // leading = 40 → child y = 40.
        assert!(approx(rects[&1].h, 100.0));
        assert!(approx(rects[&0].y, 40.0));
    }

    #[test]
    fn main_end_pushes_content_to_the_far_edge() {
        let nodes = vec![
            rect(10.0, 20.0),
            vstack_aligned(vec![0], 0.0, MainAxis::End, CrossAxis::Start),
        ];
        let sizes = measure(&nodes, 1, (1000.0, 1000.0), 1.0, &estimate_text_width);
        let mut rects = HashMap::new();
        layout_default(&nodes,1, Rect { x: 0.0, y: 0.0, w: 100.0, h: 100.0 }, &sizes, &mut rects);
        // leading = leftover = 80 → child y = 80.
        assert!(approx(rects[&0].y, 80.0));
    }

    #[test]
    fn main_space_between_distributes_gap_between_children() {
        // Three 10-px children in a 100-px tall slot: 70 leftover split over
        // 2 gaps → 35 each. y positions: 0, 10+35=45, 45+10+35=90.
        let nodes = vec![
            rect(10.0, 10.0), rect(10.0, 10.0), rect(10.0, 10.0),
            vstack_aligned(vec![0, 1, 2], 0.0, MainAxis::SpaceBetween, CrossAxis::Start),
        ];
        let sizes = measure(&nodes, 3, (1000.0, 1000.0), 1.0, &estimate_text_width);
        let mut rects = HashMap::new();
        layout_default(&nodes,3, Rect { x: 0.0, y: 0.0, w: 100.0, h: 100.0 }, &sizes, &mut rects);
        assert!(approx(rects[&0].y, 0.0));
        assert!(approx(rects[&1].y, 45.0));
        assert!(approx(rects[&2].y, 90.0));
    }

    #[test]
    fn space_between_falls_back_to_start_with_single_child() {
        // Edge case: one child has no "between" to distribute into.
        let nodes = vec![
            rect(10.0, 20.0),
            vstack_aligned(vec![0], 0.0, MainAxis::SpaceBetween, CrossAxis::Start),
        ];
        let sizes = measure(&nodes, 1, (1000.0, 1000.0), 1.0, &estimate_text_width);
        let mut rects = HashMap::new();
        layout_default(&nodes,1, Rect { x: 0.0, y: 0.0, w: 100.0, h: 100.0 }, &sizes, &mut rects);
        assert!(approx(rects[&0].y, 0.0));
    }

    #[test]
    fn cross_center_centres_children_on_cross_axis() {
        // VStack with cross=Center in a wide slot: child x should be
        // (inner_w - child_w) / 2.
        let nodes = vec![
            rect(20.0, 10.0),
            vstack_aligned(vec![0], 0.0, MainAxis::Start, CrossAxis::Center),
        ];
        let sizes = measure(&nodes, 1, (1000.0, 1000.0), 1.0, &estimate_text_width);
        let mut rects = HashMap::new();
        layout_default(&nodes,1, Rect { x: 0.0, y: 0.0, w: 100.0, h: 100.0 }, &sizes, &mut rects);
        // Stack fills cross (w = 100). Child centred → x = (100-20)/2 = 40.
        assert!(approx(rects[&1].w, 100.0));
        assert!(approx(rects[&0].x, 40.0));
    }

    #[test]
    fn cross_end_right_aligns_children_in_vstack() {
        let nodes = vec![
            rect(20.0, 10.0),
            vstack_aligned(vec![0], 0.0, MainAxis::Start, CrossAxis::End),
        ];
        let sizes = measure(&nodes, 1, (1000.0, 1000.0), 1.0, &estimate_text_width);
        let mut rects = HashMap::new();
        layout_default(&nodes,1, Rect { x: 0.0, y: 0.0, w: 100.0, h: 100.0 }, &sizes, &mut rects);
        // Child x = inner_w - child_w = 100 - 20 = 80.
        assert!(approx(rects[&0].x, 80.0));
    }

    #[test]
    fn cross_stretch_overrides_child_cross_size() {
        let nodes = vec![
            rect(20.0, 10.0),
            vstack_aligned(vec![0], 0.0, MainAxis::Start, CrossAxis::Stretch),
        ];
        let sizes = measure(&nodes, 1, (1000.0, 1000.0), 1.0, &estimate_text_width);
        let mut rects = HashMap::new();
        layout_default(&nodes,1, Rect { x: 0.0, y: 0.0, w: 100.0, h: 100.0 }, &sizes, &mut rects);
        // Child stretched to inner_w = 100; height stays measured (10).
        assert!(approx(rects[&0].w, 100.0));
        assert!(approx(rects[&0].h, 10.0));
        assert!(approx(rects[&0].x, 0.0));
    }

    #[test]
    fn hstack_cross_center_centres_children_vertically() {
        // Mirror of the VStack cross test — in an HStack the cross axis
        // is y, so Center moves the child down by (inner_h - child_h)/2.
        let nodes = vec![
            rect(10.0, 20.0),
            hstack_aligned(vec![0], 0.0, MainAxis::Start, CrossAxis::Center),
        ];
        let sizes = measure(&nodes, 1, (1000.0, 1000.0), 1.0, &estimate_text_width);
        let mut rects = HashMap::new();
        layout_default(&nodes,1, Rect { x: 0.0, y: 0.0, w: 100.0, h: 100.0 }, &sizes, &mut rects);
        // Stack fills cross (h = 100). Child y = (100 - 20) / 2 = 40.
        assert!(approx(rects[&1].h, 100.0));
        assert!(approx(rects[&0].y, 40.0));
    }

    // ── Grow (C2) ─────────────────────────────────────────────────────
    //
    // Grow is a per-child attr that splits the stack's leftover main-axis
    // space proportionally to the factor. These tests pin the semantics:
    //   - baseline (grow=0) keeps intrinsic size
    //   - grow>0 forces the stack to fill its slot on the main axis
    //   - leftover is divided by factor / total_grow
    //   - non-grow siblings stay at intrinsic alongside grow siblings
    //   - grow suppresses MainAxis alignment (there's nothing left to
    //     distribute after grow consumes it)

    #[test]
    fn single_grow_child_fills_main_axis() {
        // One child with grow=1 inside a 100-tall slot, intrinsic 10.
        // Expected: child height = 100 (fills inner_h).
        let nodes = vec![
            rect(10.0, 10.0),
            vstack_aligned(vec![0], 0.0, MainAxis::Start, CrossAxis::Start),
        ];
        let mut attrs = default_attrs(nodes.len());
        attrs[0].grow = 1.0;
        let sizes = measure(&nodes, 1, (1000.0, 1000.0), 1.0, &estimate_text_width);
        let mut rects = HashMap::new();
        layout(&nodes, &attrs, 1, Rect { x: 0.0, y: 0.0, w: 100.0, h: 100.0 }, &sizes, &mut rects);
        // Stack filled by grow: h = slot.h = 100.
        assert!(approx(rects[&1].h, 100.0));
        // Child got intrinsic (10) + leftover (90) = 100.
        assert!(approx(rects[&0].h, 100.0));
    }

    #[test]
    fn two_equal_grow_children_split_space_fifty_fifty() {
        // Two grow=1 children, no intrinsic mass: leftover 100 split 50/50.
        let nodes = vec![
            rect(10.0, 0.0), rect(10.0, 0.0),
            vstack_aligned(vec![0, 1], 0.0, MainAxis::Start, CrossAxis::Start),
        ];
        let mut attrs = default_attrs(nodes.len());
        attrs[0].grow = 1.0;
        attrs[1].grow = 1.0;
        let sizes = measure(&nodes, 2, (1000.0, 1000.0), 1.0, &estimate_text_width);
        let mut rects = HashMap::new();
        layout(&nodes, &attrs, 2, Rect { x: 0.0, y: 0.0, w: 100.0, h: 100.0 }, &sizes, &mut rects);
        assert!(approx(rects[&0].h, 50.0));
        assert!(approx(rects[&1].h, 50.0));
        // Positions: first at y=0; second starts where first ended.
        assert!(approx(rects[&0].y, 0.0));
        assert!(approx(rects[&1].y, 50.0));
    }

    #[test]
    fn grow_factors_split_proportionally() {
        // grow factors 1 and 3 → split leftover 25 / 75.
        let nodes = vec![
            rect(10.0, 0.0), rect(10.0, 0.0),
            vstack_aligned(vec![0, 1], 0.0, MainAxis::Start, CrossAxis::Start),
        ];
        let mut attrs = default_attrs(nodes.len());
        attrs[0].grow = 1.0;
        attrs[1].grow = 3.0;
        let sizes = measure(&nodes, 2, (1000.0, 1000.0), 1.0, &estimate_text_width);
        let mut rects = HashMap::new();
        layout(&nodes, &attrs, 2, Rect { x: 0.0, y: 0.0, w: 100.0, h: 100.0 }, &sizes, &mut rects);
        assert!(approx(rects[&0].h, 25.0));
        assert!(approx(rects[&1].h, 75.0));
    }

    #[test]
    fn non_grow_sibling_keeps_intrinsic_alongside_grow() {
        // One fixed-height 20 child + one grow=1 child in 100-tall slot.
        // leftover = 100 - 20 = 80 → grow child gets 80, fixed stays at 20.
        let nodes = vec![
            rect(10.0, 20.0), rect(10.0, 0.0),
            vstack_aligned(vec![0, 1], 0.0, MainAxis::Start, CrossAxis::Start),
        ];
        let mut attrs = default_attrs(nodes.len());
        attrs[1].grow = 1.0;
        let sizes = measure(&nodes, 2, (1000.0, 1000.0), 1.0, &estimate_text_width);
        let mut rects = HashMap::new();
        layout(&nodes, &attrs, 2, Rect { x: 0.0, y: 0.0, w: 100.0, h: 100.0 }, &sizes, &mut rects);
        assert!(approx(rects[&0].h, 20.0));
        assert!(approx(rects[&1].h, 80.0));
        assert!(approx(rects[&1].y, 20.0));
    }

    #[test]
    fn grow_accounts_for_gap_between_children() {
        // Two grow children with gap 10 in 100-tall slot: leftover
        // = 100 - 10 (gap) = 90, split 45/45.
        let nodes = vec![
            rect(10.0, 0.0), rect(10.0, 0.0),
            vstack_aligned(vec![0, 1], 10.0, MainAxis::Start, CrossAxis::Start),
        ];
        let mut attrs = default_attrs(nodes.len());
        attrs[0].grow = 1.0;
        attrs[1].grow = 1.0;
        let sizes = measure(&nodes, 2, (1000.0, 1000.0), 1.0, &estimate_text_width);
        let mut rects = HashMap::new();
        layout(&nodes, &attrs, 2, Rect { x: 0.0, y: 0.0, w: 100.0, h: 100.0 }, &sizes, &mut rects);
        assert!(approx(rects[&0].h, 45.0));
        assert!(approx(rects[&1].h, 45.0));
        assert!(approx(rects[&1].y, 55.0)); // 45 + 10 gap
    }

    #[test]
    fn grow_suppresses_main_alignment() {
        // With any grow child, leftover is fully consumed → MainAxis
        // Center no longer adds leading offset. The grow child starts at
        // y=0, not y=half.
        let nodes = vec![
            rect(10.0, 0.0),
            vstack_aligned(vec![0], 0.0, MainAxis::Center, CrossAxis::Start),
        ];
        let mut attrs = default_attrs(nodes.len());
        attrs[0].grow = 1.0;
        let sizes = measure(&nodes, 1, (1000.0, 1000.0), 1.0, &estimate_text_width);
        let mut rects = HashMap::new();
        layout(&nodes, &attrs, 1, Rect { x: 0.0, y: 0.0, w: 100.0, h: 100.0 }, &sizes, &mut rects);
        assert!(approx(rects[&0].y, 0.0));
        assert!(approx(rects[&0].h, 100.0));
    }

    #[test]
    fn grow_spacer_pushes_sibling_to_the_far_edge() {
        // Classic "spacer grow" pattern: a Spacer with grow=1 between a
        // rect at origin and one that should land at the bottom.
        let nodes = vec![
            rect(10.0, 10.0),
            Node::Spacer { min: (0.0, 0.0) },
            rect(10.0, 10.0),
            vstack_aligned(vec![0, 1, 2], 0.0, MainAxis::Start, CrossAxis::Start),
        ];
        let mut attrs = default_attrs(nodes.len());
        attrs[1].grow = 1.0;
        let sizes = measure(&nodes, 3, (1000.0, 1000.0), 1.0, &estimate_text_width);
        let mut rects = HashMap::new();
        layout(&nodes, &attrs, 3, Rect { x: 0.0, y: 0.0, w: 100.0, h: 100.0 }, &sizes, &mut rects);
        // First rect at y=0; spacer consumes 100-10-10 = 80; last rect
        // lands at y = 10 + 80 = 90.
        assert!(approx(rects[&0].y, 0.0));
        assert!(approx(rects[&2].y, 90.0));
    }

    #[test]
    fn grow_works_in_hstack_the_same_way() {
        // HStack mirror: grow distributes on x instead of y.
        let nodes = vec![
            rect(0.0, 10.0), rect(0.0, 10.0), rect(0.0, 10.0),
            hstack_aligned(vec![0, 1, 2], 0.0, MainAxis::Start, CrossAxis::Start),
        ];
        let mut attrs = default_attrs(nodes.len());
        attrs[0].grow = 1.0; attrs[1].grow = 1.0; attrs[2].grow = 1.0;
        let sizes = measure(&nodes, 3, (1000.0, 1000.0), 1.0, &estimate_text_width);
        let mut rects = HashMap::new();
        layout(&nodes, &attrs, 3, Rect { x: 0.0, y: 0.0, w: 300.0, h: 100.0 }, &sizes, &mut rects);
        // Three equal-grow children split 300px evenly: 100 each.
        assert!(approx(rects[&0].w, 100.0) && approx(rects[&0].x, 0.0));
        assert!(approx(rects[&1].w, 100.0) && approx(rects[&1].x, 100.0));
        assert!(approx(rects[&2].w, 100.0) && approx(rects[&2].x, 200.0));
    }

    #[test]
    fn nested_grow_stack_fills_parent_then_lays_out_its_own_children() {
        // Outer HStack has a grow=1 VStack child, which itself holds
        // two grow=1 rects. The inner stack should fill the outer's
        // width, and its children should split that width in half.
        let nodes = vec![
            /* 0 */ rect(0.0, 10.0),
            /* 1 */ rect(0.0, 10.0),
            /* 2 */ vstack_aligned(vec![0, 1], 0.0, MainAxis::Start, CrossAxis::Start),
            /* 3 */ hstack_aligned(vec![2], 0.0, MainAxis::Start, CrossAxis::Stretch),
        ];
        let mut attrs = default_attrs(nodes.len());
        // Grow on the inner VStack (via outer HStack's main axis = x),
        // plus grow on the inner rects (via VStack's main axis = y).
        attrs[2].grow = 1.0;
        attrs[0].grow = 1.0;
        attrs[1].grow = 1.0;
        let sizes = measure(&nodes, 3, (1000.0, 1000.0), 1.0, &estimate_text_width);
        let mut rects = HashMap::new();
        layout(&nodes, &attrs, 3, Rect { x: 0.0, y: 0.0, w: 200.0, h: 80.0 }, &sizes, &mut rects);
        // Inner VStack fills outer: w=200, h=80 (Stretch on cross).
        assert!(approx(rects[&2].w, 200.0));
        assert!(approx(rects[&2].h, 80.0));
        // Inner children split 80px height evenly.
        assert!(approx(rects[&0].h, 40.0));
        assert!(approx(rects[&1].h, 40.0));
    }

    #[test]
    fn alignment_respects_parent_padding() {
        // Padding eats into the "inner" box first; alignment distributes
        // leftover only within the padded area.
        let nodes = vec![
            rect(10.0, 10.0),
            Node::VStack {
                children: vec![0], gap: 0.0, padding: (10.0, 10.0, 10.0, 10.0),
                main: MainAxis::Center, cross: CrossAxis::Center,
            },
        ];
        let sizes = measure(&nodes, 1, (1000.0, 1000.0), 1.0, &estimate_text_width);
        let mut rects = HashMap::new();
        layout_default(&nodes,1, Rect { x: 0.0, y: 0.0, w: 100.0, h: 100.0 }, &sizes, &mut rects);
        // inner box = 80×80, child 10×10 → centre offset (80-10)/2 = 35,
        // plus the 10-px top/left padding → 45.
        assert!(approx(rects[&0].x, 45.0));
        assert!(approx(rects[&0].y, 45.0));
    }
}
