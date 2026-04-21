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
//!
//! 2. [`layout`] — top-down placement. The root gets the full viewport
//!    as its rect; each stack hands its children slices off the main
//!    axis matching their measured size, offset by the current cursor
//!    and gap. No wrapping, no flex growth. A widget that asked for
//!    more than the available space is simply clipped by the canvas.

use std::collections::HashMap;

use super::node::{Node, NodeId};
use super::Rect;

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
        Node::VStack { children, gap, padding } => {
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
        Node::HStack { children, gap, padding } => {
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
    root: NodeId,
    viewport: Rect,
    sizes: &HashMap<NodeId, (f32, f32)>,
    out: &mut HashMap<NodeId, Rect>,
) {
    place(nodes, root, viewport, sizes, out);
}

/// Place `id` inside `slot`. Slot is the area the parent assigned; the
/// node uses its measured size to position itself at the slot's origin
/// (start/start alignment) and then recurses into children using that
/// origin plus any padding.
fn place(
    nodes: &[Node],
    id: NodeId,
    slot: Rect,
    sizes: &HashMap<NodeId, (f32, f32)>,
    out: &mut HashMap<NodeId, Rect>,
) {
    let (mw, mh) = *sizes.get(&id).expect("measured before layout");
    // A node never claims more than its slot. Stacks size themselves to
    // fit content but the root's slot is the viewport, which may be
    // larger — a too-big tree is simply clipped at the slot's bounds.
    let w = mw.min(slot.w);
    let h = mh.min(slot.h);
    let rect = Rect { x: slot.x, y: slot.y, w, h };
    out.insert(id, rect);

    match &nodes[id] {
        Node::Text { .. }
        | Node::Rect { .. }
        | Node::Spacer { .. }
        | Node::Button { .. } => {}
        Node::VStack { children, gap, padding } => {
            let mut y = rect.y + padding.0;
            let inner_x = rect.x + padding.3;
            let inner_w = (rect.w - padding.1 - padding.3).max(0.0);
            for (i, c) in children.iter().enumerate() {
                let (_, ch) = sizes[c];
                let child_slot = Rect { x: inner_x, y, w: inner_w, h: ch };
                place(nodes, *c, child_slot, sizes, out);
                y += ch;
                if i + 1 < children.len() { y += gap; }
            }
        }
        Node::HStack { children, gap, padding } => {
            let mut x = rect.x + padding.3;
            let inner_y = rect.y + padding.0;
            let inner_h = (rect.h - padding.0 - padding.2).max(0.0);
            for (i, c) in children.iter().enumerate() {
                let (cw, _) = sizes[c];
                let child_slot = Rect { x, y: inner_y, w: cw, h: inner_h };
                place(nodes, *c, child_slot, sizes, out);
                x += cw;
                if i + 1 < children.len() { x += gap; }
            }
        }
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
            Node::VStack { children: vec![0, 1], gap: 3.0, padding: (0.0, 0.0, 0.0, 0.0) },
        ];
        let sizes = measure(&nodes, 2, (1000.0, 1000.0), 1.0, &estimate_text_width);
        // max(10, 20) = 20; 5 + 7 + 3 = 15
        assert_eq!(sizes[&2], (20.0, 15.0));
    }

    #[test]
    fn hstack_is_the_mirror_of_vstack() {
        let nodes = vec![
            rect(10.0, 5.0), rect(20.0, 7.0),
            Node::HStack { children: vec![0, 1], gap: 3.0, padding: (0.0, 0.0, 0.0, 0.0) },
        ];
        let sizes = measure(&nodes, 2, (1000.0, 1000.0), 1.0, &estimate_text_width);
        // 10 + 20 + 3 = 33; max(5, 7) = 7
        assert_eq!(sizes[&2], (33.0, 7.0));
    }

    #[test]
    fn stack_padding_adds_to_both_axes() {
        let nodes = vec![
            rect(10.0, 10.0),
            Node::VStack { children: vec![0], gap: 0.0, padding: (4.0, 5.0, 6.0, 7.0) },
        ];
        let sizes = measure(&nodes, 1, (1000.0, 1000.0), 1.0, &estimate_text_width);
        // w: 10 + 7 + 5 = 22;  h: 10 + 4 + 6 = 20
        assert_eq!(sizes[&1], (22.0, 20.0));
    }

    #[test]
    fn empty_stack_has_only_its_padding() {
        let nodes = vec![
            Node::VStack { children: vec![], gap: 10.0, padding: (3.0, 3.0, 3.0, 3.0) },
        ];
        let sizes = measure(&nodes, 0, (1000.0, 1000.0), 1.0, &estimate_text_width);
        // gap doesn't apply to zero children; padding remains.
        assert_eq!(sizes[&0], (6.0, 6.0));
    }

    #[test]
    fn single_child_stack_has_no_gap_applied() {
        let nodes = vec![
            rect(10.0, 5.0),
            Node::VStack { children: vec![0], gap: 100.0, padding: (0.0, 0.0, 0.0, 0.0) },
        ];
        let sizes = measure(&nodes, 1, (1000.0, 1000.0), 1.0, &estimate_text_width);
        assert_eq!(sizes[&1], (10.0, 5.0));
    }

    // ── Place ────────────────────────────────────────────────────────

    #[test]
    fn vstack_stacks_children_top_down_with_gap() {
        let nodes = vec![
            rect(10.0, 5.0), rect(10.0, 7.0),
            Node::VStack { children: vec![0, 1], gap: 3.0, padding: (0.0, 0.0, 0.0, 0.0) },
        ];
        let sizes = measure(&nodes, 2, (1000.0, 1000.0), 1.0, &estimate_text_width);
        let mut rects = HashMap::new();
        layout(&nodes, 2, Rect { x: 100.0, y: 200.0, w: 500.0, h: 500.0 }, &sizes, &mut rects);
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
            Node::HStack { children: vec![0, 1], gap: 4.0, padding: (0.0, 0.0, 0.0, 0.0) },
        ];
        let sizes = measure(&nodes, 2, (1000.0, 1000.0), 1.0, &estimate_text_width);
        let mut rects = HashMap::new();
        layout(&nodes, 2, Rect { x: 50.0, y: 0.0, w: 500.0, h: 500.0 }, &sizes, &mut rects);
        assert!(approx(rects[&0].x, 50.0));
        // 50 + 10 + 4 = 64.
        assert!(approx(rects[&1].x, 64.0));
    }

    #[test]
    fn padding_offsets_children_from_stack_origin() {
        let nodes = vec![
            rect(10.0, 10.0),
            Node::VStack { children: vec![0], gap: 0.0, padding: (4.0, 5.0, 6.0, 7.0) },
        ];
        let sizes = measure(&nodes, 1, (1000.0, 1000.0), 1.0, &estimate_text_width);
        let mut rects = HashMap::new();
        layout(&nodes, 1, Rect { x: 0.0, y: 0.0, w: 500.0, h: 500.0 }, &sizes, &mut rects);
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
        layout(&nodes, 0, Rect { x: 0.0, y: 0.0, w: 40.0, h: 30.0 }, &sizes, &mut rects);
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
            /* 2 */ Node::VStack { children: vec![0, 1], gap: 2.0, padding: (0.0,0.0,0.0,0.0) },
            /* 3 */ rect(8.0, 10.0),
            /* 4 */ Node::VStack { children: vec![3], gap: 0.0, padding: (0.0,0.0,0.0,0.0) },
            /* 5 */ Node::HStack { children: vec![2, 4], gap: 3.0, padding: (0.0,0.0,0.0,0.0) },
        ];
        let sizes = measure(&nodes, 5, (1000.0, 1000.0), 1.0, &estimate_text_width);
        let mut rects = HashMap::new();
        layout(&nodes, 5, Rect { x: 0.0, y: 0.0, w: 500.0, h: 500.0 }, &sizes, &mut rects);
        // Left VStack at x=0; its children also at x=0.
        assert!(approx(rects[&0].x, 0.0) && approx(rects[&0].y, 0.0));
        assert!(approx(rects[&1].x, 0.0) && approx(rects[&1].y, 12.0)); // 10 + gap 2
        // Right VStack at x = left_w (5) + gap (3) = 8.
        assert!(approx(rects[&4].x, 8.0));
        assert!(approx(rects[&3].x, 8.0) && approx(rects[&3].y, 0.0));
    }
}
