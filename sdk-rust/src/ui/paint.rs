//! Paint emitter.
//!
//! Walks the tree in document order and emits `Canvas` ops for every
//! laid-out node. Order matters — later nodes paint over earlier ones.
//! That mirrors the compositor's replay model: paint ops are a flat
//! stream, no z-index or stacking contexts.

use std::collections::HashMap;

use crate::canvas::Canvas;
use super::node::{Node, NodeId};
use super::Rect;

/// Paint `root` and everything below it into `canvas`, using the rects
/// produced by `layout::layout`. `clicked_ids` lists button ids that
/// were pressed this frame — they render with their pressed-colour
/// background so the user gets immediate feedback. The list is
/// typically very short (zero or one), so linear scan is fine.
pub fn paint(
    nodes: &[Node],
    root: NodeId,
    rects: &HashMap<NodeId, Rect>,
    clicked_ids: &[String],
    canvas: &mut Canvas<'_>,
) {
    paint_node(nodes, root, rects, clicked_ids, canvas);
}

fn paint_node(
    nodes: &[Node],
    id: NodeId,
    rects: &HashMap<NodeId, Rect>,
    clicked_ids: &[String],
    canvas: &mut Canvas<'_>,
) {
    let rect = rects.get(&id).copied().unwrap_or_default();
    match &nodes[id] {
        Node::Text { text, size, color } => {
            // Canvas2D `fillText` draws with baseline at y; account for
            // the ~0.8 ascender ratio so the visible glyph sits inside
            // the box layout placed at `rect.y`.
            canvas.draw_text(rect.x, rect.y + size * 0.8, *size, *color, text);
        }
        Node::Rect { color, .. } => {
            canvas.fill_rect(rect.x, rect.y, rect.w, rect.h, *color);
        }
        Node::Spacer { .. } => {}
        Node::Button { id: btn_id, label, label_size, bg, bg_pressed, fg, pad } => {
            let is_pressed = clicked_ids.iter().any(|s| s == btn_id);
            let bg_color = if is_pressed { *bg_pressed } else { *bg };
            canvas.fill_rect(rect.x, rect.y, rect.w, rect.h, bg_color);
            // Centre the label inside the button box. We estimated the
            // intrinsic size as label + 2*pad; if the parent gave us a
            // wider slot the label still sits left-ish at the pad
            // offset rather than true-centred, which is what the
            // minimal MVP demands.
            canvas.draw_text(
                rect.x + pad.0,
                rect.y + pad.1 + label_size * 0.8,
                *label_size,
                *fg,
                label,
            );
        }
        Node::VStack { children, .. } | Node::HStack { children, .. } => {
            for c in children { paint_node(nodes, *c, rects, clicked_ids, canvas); }
        }
    }
}
