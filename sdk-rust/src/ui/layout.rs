//! Measure + layout for the widget tree.
//!
//! Two passes:
//!
//! 1. [`measure`] — bottom-up recursion that produces an intrinsic size
//!    for every node. Stacks sum their children along the main axis and
//!    take max along the cross; leaves report content-derived sizes.
//!    Text metrics are estimated (`chars * size * 0.55`) because the
//!    host hasn't exposed real font metrics yet; close enough for MVP.
//!
//! 2. [`layout`] — top-down placement. The root gets the full viewport
//!    as its rect; each stack hands its children slices off the main
//!    axis matching their measured size, offset by the current cursor
//!    and gap. No wrapping, no flex growth. A widget that asked for
//!    more than the available space is simply clipped by the canvas.

use std::collections::HashMap;

use super::node::{Node, NodeId};
use super::Rect;

/// Character width estimate (fraction of font size) for mono-ish
/// system-ui. Sans-serif is closer to 0.50; bold closer to 0.60. The
/// middle-ground 0.55 over-estimates narrow text slightly, which
/// leaves a comfortable tap target and avoids clipping.
const TEXT_CHAR_W_RATIO: f32 = 0.55;

/// Line-height multiplier for intrinsic text height — matches the
/// ~1.3 default line-height of most UA stylesheets, which is what
/// Canvas2D `fillText` visually implies.
const TEXT_LINE_H_RATIO: f32 = 1.3;

pub fn measure(
    nodes: &[Node],
    root: NodeId,
    viewport: (f32, f32),
    _density: f32,
) -> HashMap<NodeId, (f32, f32)> {
    let mut out = HashMap::new();
    measure_node(nodes, root, viewport, &mut out);
    out
}

fn measure_node(
    nodes: &[Node],
    id: NodeId,
    avail: (f32, f32),
    out: &mut HashMap<NodeId, (f32, f32)>,
) -> (f32, f32) {
    let size = match &nodes[id] {
        Node::Text { text, size, .. } => {
            let w = text.chars().count() as f32 * *size * TEXT_CHAR_W_RATIO;
            let h = *size * TEXT_LINE_H_RATIO;
            (w, h)
        }
        Node::Rect { min, .. } => *min,
        Node::Spacer { min } => *min,
        Node::Button { label, label_size, pad, .. } => {
            let label_w = label.chars().count() as f32 * *label_size * TEXT_CHAR_W_RATIO;
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
                let (cw, ch) = measure_node(nodes, *c, inner_avail, out);
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
                let (cw, ch) = measure_node(nodes, *c, inner_avail, out);
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
