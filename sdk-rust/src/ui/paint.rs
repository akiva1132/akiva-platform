//! Paint emitter.
//!
//! Walks the tree in document order and emits `Canvas` ops for every
//! laid-out node. Order matters — later nodes paint over earlier ones.
//! That mirrors the compositor's replay model: paint ops are a flat
//! stream, no z-index or stacking contexts.

use std::collections::HashMap;

use crate::canvas::Canvas;
use super::layout::{wrap_text, TEXT_LINE_H_RATIO};
use super::node::{Node, NodeId};
use super::Rect;

/// Paint `root` and everything below it into `canvas`, using the rects
/// produced by `layout::layout`. `clicked_ids` lists button ids that
/// were pressed this frame — they render with their pressed-colour
/// background so the user gets immediate feedback. The list is
/// typically very short (zero or one), so linear scan is fine.
///
/// `text_width` is the same callback `measure` was invoked with — we
/// re-wrap multi-line Text in paint to avoid threading the wrapped
/// lines through a parallel side-map. Single-line text (the default)
/// pays nothing here; multi-line text pays one extra wrap pass per
/// frame, which is negligible at typical paragraph counts.
pub fn paint(
    nodes: &[Node],
    root: NodeId,
    rects: &HashMap<NodeId, Rect>,
    clicked_ids: &[String],
    text_width: &dyn Fn(&str, f32) -> f32,
    canvas: &mut Canvas<'_>,
) {
    paint_node(nodes, root, rects, clicked_ids, text_width, canvas);
}

fn paint_node(
    nodes: &[Node],
    id: NodeId,
    rects: &HashMap<NodeId, Rect>,
    clicked_ids: &[String],
    text_width: &dyn Fn(&str, f32) -> f32,
    canvas: &mut Canvas<'_>,
) {
    let rect = rects.get(&id).copied().unwrap_or_default();
    match &nodes[id] {
        Node::Text { text, size, color, max_lines } => {
            // Canvas2D `fillText` draws with baseline at y; account
            // for the ~0.8 ascender ratio so the visible glyph sits
            // inside the box layout placed at `rect.y`.
            if *max_lines <= 1 {
                canvas.draw_text(rect.x, rect.y + size * 0.8, *size, *color, text);
            } else {
                // Re-wrap with the same `avail_w` measure used —
                // `rect.w` matches what measure_node reported as the
                // widest line, so the lines come out identical.
                let lines = wrap_text(text, *size, rect.w, *max_lines, text_width);
                let line_h = *size * TEXT_LINE_H_RATIO;
                for (i, line) in lines.iter().enumerate() {
                    let baseline_y = rect.y + i as f32 * line_h + size * 0.8;
                    canvas.draw_text(rect.x, baseline_y, *size, *color, line);
                }
            }
        }
        Node::Rect { color, radius, .. } => {
            if *radius > 0.0 {
                canvas.fill_round_rect(rect.x, rect.y, rect.w, rect.h, *radius, *color);
            } else {
                canvas.fill_rect(rect.x, rect.y, rect.w, rect.h, *color);
            }
        }
        Node::Spacer { .. } => {}
        Node::Image { drawable_id, .. } => {
            // Stone 13.0prep C3 — emit the OP_DRAW_IMAGE; renderer
            // looks up `drawable_id` in its bitmap cache and either
            // blits the cached `ImageBitmap` or paints a magenta
            // placeholder for `0` / unregistered ids.
            canvas.draw_image(*drawable_id, rect.x, rect.y, rect.w, rect.h);
        }
        Node::Button { id: btn_id, label, label_size, bg, bg_pressed, fg, pad, radius, .. } => {
            let is_pressed = clicked_ids.iter().any(|s| s == btn_id);
            let bg_color = if is_pressed { *bg_pressed } else { *bg };
            if *radius > 0.0 {
                canvas.fill_round_rect(rect.x, rect.y, rect.w, rect.h, *radius, bg_color);
            } else {
                canvas.fill_rect(rect.x, rect.y, rect.w, rect.h, bg_color);
            }
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
        Node::Frame { children, clip_radius, .. } => {
            // Document order = paint order. Frame: earlier children
            // paint first (bottom of z-stack), later overlay them —
            // mirrors Android FrameLayout / SwiftUI ZStack.
            //
            // `clip_radius > 0`: wrap children in a rounded clip so
            // bitmaps, gradients, or stripe patterns get masked to
            // a rounded rect / pill / circle without changing their
            // own paint. Cheaper than per-leaf clip discipline; the
            // renderer's save/restore pair is one save() per frame.
            let push_clip = *clip_radius > 0.0;
            if push_clip {
                canvas.push_round_clip(rect.x, rect.y, rect.w, rect.h, *clip_radius);
            }
            for c in children { paint_node(nodes, *c, rects, clicked_ids, text_width, canvas); }
            if push_clip {
                canvas.pop_clip();
            }
        }
        Node::VStack { children, .. }
        | Node::HStack { children, .. }
        | Node::Grid { children, .. } => {
            // Document order = paint order. Grid uses the same
            // traversal: layout gives non-overlapping cell rects, so
            // document order is just row-major paint order.
            for c in children { paint_node(nodes, *c, rects, clicked_ids, text_width, canvas); }
        }
    }
}
