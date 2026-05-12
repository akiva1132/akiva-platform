//! Semantic tree — the test/inspector projection of the UI.
//!
//! Akiva renders to canvas, so the browser's a11y tree is empty and
//! `document.querySelector` finds nothing. Without an external
//! description of what's on screen, Playwright tests can only click by
//! pixel coordinates and visual regression is the only option — both
//! are brittle. This module is the runtime side of the introspection
//! layer planned in [`docs/testing.md §7`](../../../../docs/testing.md)
//! and [`ARCHITECTURE.md §16`](../../../../ARCHITECTURE.md).
//!
//! Pattern follows Flutter web: each frame the SDK builds a flat tree
//! describing what the user sees (role + bounds + label/value + stable
//! id), serialises it, and fires it at the host as a side-effect. The
//! host (`runtime/web/inspector.js`) decodes the tree and exposes it
//! as `window.__akiva` — a JS-only query API (`findById`, `findByRole`,
//! `findByText`, `snapshot`) — so Playwright tests via `page.evaluate`
//! can resolve canvas widgets without a DOM mirror. Same tree is the
//! foundation for a future screen reader — no separate a11y wiring
//! needed.
//!
//! ## Lifecycle
//!
//! - Always emitted (cheap — a few hundred bytes per frame for a
//!   typical screen). The host filters at the projection layer:
//!   tabs without `?inspector=1` ignore the message.
//! - Built inside [`crate::ui::Ui::present`] from the same tree the
//!   layout pass laid out. Roles are derived from the [`crate::ui::node::Node`]
//!   variant; bounds come from the layout pass output.
//! - One [`SemanticTree`] per Activity per frame. The activity handle
//!   keys it on the host side; multiple activities composing on one
//!   display each emit their own tree.
//!
//! ## Wire format (version 3, all little-endian)
//!
//! ```text
//! [version: u8 = 3]
//! [count: u32]
//! repeat count:
//!   [parent_idx: i32]   // -1 for the root; otherwise an index into this same blob
//!   [role: u8]          // SemanticRole repr
//!   [x: f32 | y: f32 | w: f32 | h: f32]   // physical pixels
//!   [flags: u8]         // bit 0 has_id, bit 1 has_label, bit 2 has_value,
//!                       // bit 3 has_scroll
//!   [id_len: u16    | id: utf8 ]?         // present iff flags & 1
//!   [label_len: u16 | label: utf8]?       // present iff flags & 2
//!   [value_len: u16 | value: utf8]?       // present iff flags & 4
//!   [scroll: 29 bytes]?                   // present iff flags & 8 (v3)
//!     scroll = scroll_y: f32 | content_h: f32 | viewport_h: f32 | viewport_w: f32
//!            | first_visible: u32 (or 0xFFFFFFFF = null)
//!            | last_visible:  u32 (or 0xFFFFFFFF = null)
//!            | item_count:    u32 (or 0xFFFFFFFF = null)
//!            | axis: u8 (0 = Vertical, 1 = Horizontal)         // NEW in v3
//! ```
//!
//! Version history:
//! - **v1**: parent_idx/role/bounds/flags + id/label/value strings.
//! - **v2**: added the optional `scroll` payload (28 bytes) gated by
//!   flag bit 3.
//! - **v3**: the `scroll` payload grew by 1 byte (29 total) carrying
//!   the `axis` enum. v2 decoders that read exactly 28 bytes after
//!   the flag will leave the trailing axis byte unread; they ignore
//!   it cleanly and default to `Vertical`.
//!
//! Old decoders reading v1 bytes still work as before; v2 → v3
//! readers must accept either size — see `decode` for the per-
//! version bookkeeping.
//!
//! The `parent_idx` channel is enough for a host to reconstruct the
//! tree structure; a depth-first walk of the input naturally produces
//! parents-before-children, but we don't rely on order — the host
//! decoder can slot every node into `nodes[idx]` and link by parent.

/// Semantic role of a UI element. Matches a small subset of ARIA
/// roles plus a few platform extensions (`Rect`, `Spacer`) for
/// completeness during the inspector phase. The host overlay
/// translates these to ARIA roles when projecting to DOM.
///
/// Numbering is wire-stable. Add new variants by appending — never
/// reorder.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SemanticRole {
    /// VStack, HStack, Frame, Grid — anything that lays out children.
    /// Maps to ARIA `role="group"` in the DOM projection.
    Group   = 0,
    /// Static text. The text content lives in `label`. Maps to
    /// `aria-label` + visible text content (no ARIA role — `text`
    /// isn't a valid ARIA role, and the text content alone makes
    /// the element findable via `getByText`).
    Text    = 1,
    /// Tappable button. `label` carries the visible text; `id`
    /// carries the layout id; ARIA `role="button"`.
    Button  = 2,
    /// Bitmap. `id` if any; ARIA `role="img"` with `aria-label` set
    /// from the source `@drawable/...` ref name (when available).
    Image   = 3,
    /// Decorative coloured rectangle. Skipped by the default DOM
    /// projection (noise) but still emitted so a debug overlay can
    /// show it. ARIA: none.
    Rect    = 4,
    /// Layout-only filler. Same projection treatment as `Rect`.
    Spacer  = 5,
    /// Scroll viewport — `Node::Scroll` / `Node::LazyColumn`. The
    /// caller-supplied key lives in `id`; the inspector publishes
    /// scroll position + content/viewport dimensions on a separate
    /// wire extension (added in the inspector polish stone).
    /// ARIA: `role="region"` with a `data-akiva-scroll` flag the
    /// projection layer reads to decorate scrolling regions.
    Scroll  = 6,
}

impl SemanticRole {
    /// True for nodes the default DOM projection should skip. Decorative
    /// rectangles and spacers add no information for tests; including
    /// them in the ARIA tree would just dilute `getByRole("group")`
    /// queries with noise. Inspector overlays that want the full tree
    /// can still opt in by reading the wire format directly.
    pub fn is_decorative(self) -> bool {
        matches!(self, SemanticRole::Rect | SemanticRole::Spacer)
    }
}

/// Scroll-region metadata published alongside a [`SemanticRole::Scroll`]
/// node. `first_visible` / `last_visible` / `item_count` are `None`
/// for plain `Node::Scroll` (only meaningful for virtualised lists);
/// `Node::LazyColumn` / `Node::LazyRow` populate all three.
///
/// Names follow the vertical convention (`scroll_y`, `content_h`,
/// `viewport_h`/`viewport_w`) — for horizontal scrolls those carry
/// main-axis-projected values. The `axis` field disambiguates;
/// callers that want to be precise read it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScrollSemantic {
    pub scroll_y:      f32,
    pub content_h:     f32,
    pub viewport_h:    f32,
    pub viewport_w:    f32,
    pub first_visible: Option<u32>,
    pub last_visible:  Option<u32>,
    pub item_count:    Option<u32>,
    /// Scroll orientation. `Vertical` (the default, matching v2 byte
    /// shape) for `Node::Scroll`/`Node::LazyColumn`; `Horizontal`
    /// for `Node::LazyRow` and `Ui::scroll_h(...)`. Added in
    /// wire v3 (Stage 2d.4) — v2 decoders default this to
    /// `Vertical` on read.
    pub axis: super::ScrollAxis,
}

/// One node in a [`SemanticTree`]. `parent` is `None` for the root,
/// `Some(idx)` for everything else where `idx` references an earlier
/// position in the same tree's `nodes` vector.
#[derive(Debug, Clone, PartialEq)]
pub struct SemanticNode {
    pub parent: Option<u32>,
    pub role:   SemanticRole,
    /// Physical-pixel rect in screen space. Same coordinate system as
    /// pointer events — a Playwright click at `(x + w/2, y + h/2)`
    /// reaches the same hit target the SDK would.
    pub x:      f32,
    pub y:      f32,
    pub w:      f32,
    pub h:      f32,
    /// Stable string id from the AKML `id="..."` attribute or the
    /// `Ui::button(id, ...)` argument. Test selector key — when set,
    /// projects to `data-akiva-id="..."` on the DOM mirror. `None` for
    /// nodes without an explicit id (decorative rects, spacers,
    /// auto-laid-out containers).
    pub id:     Option<String>,
    /// Visible text or accessible name. For Text nodes this is the
    /// text content; for Button nodes the visible label; for Image
    /// nodes the drawable name (when known).
    pub label:  Option<String>,
    /// Live state value separate from the static label. Currently
    /// unused — reserved for future widgets (Input field text,
    /// Checkbox checked state, Slider position, etc.). Decoders that
    /// don't recognise it ignore.
    pub value:  Option<String>,
    /// Stage 2c.6 — scroll-region metadata, present iff this node is
    /// a [`SemanticRole::Scroll`]. Surfaces scroll position +
    /// content extent + visible-range so tests can assert "scroll_y
    /// in [380, 420] after wheel(400)" without sampling pixels.
    pub scroll: Option<ScrollSemantic>,
}

/// Flat semantic projection of one frame's worth of widget tree.
/// `nodes[0]` is the root; nodes are indexed by `u32` to keep the
/// wire format compact (a screen with 65k widgets is unrealistic).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SemanticTree {
    pub nodes: Vec<SemanticNode>,
}

/// Sentinel u32 written for optional `first_visible`/`last_visible`/
/// `item_count` fields when the source value is `None`. Picked at
/// `u32::MAX` because a real count / index would never reach there.
const SCROLL_NULL_U32: u32 = 0xFFFF_FFFF;

impl SemanticTree {
    pub const WIRE_VERSION: u8 = 3;

    pub fn new() -> Self { Self { nodes: Vec::new() } }

    pub fn is_empty(&self) -> bool { self.nodes.is_empty() }

    /// Append a node and return its index. Caller is responsible for
    /// passing a `parent` index that already exists in `self.nodes`
    /// (or `None` for the root). The first node pushed must have
    /// `parent == None` — the host decoder uses that as a sentinel.
    pub fn push(&mut self, node: SemanticNode) -> u32 {
        let idx = self.nodes.len() as u32;
        self.nodes.push(node);
        idx
    }

    /// Encode to the v1 wire format. See module-level docs for the
    /// byte layout.
    pub fn encode(&self) -> Vec<u8> {
        // Estimate: header (5) + per-node fixed (22) + each string
        // (avg ~12). Over-reserve a little so we don't reallocate for
        // typical screens.
        let mut out = Vec::with_capacity(5 + self.nodes.len() * 32);
        out.push(Self::WIRE_VERSION);
        out.extend_from_slice(&(self.nodes.len() as u32).to_le_bytes());
        for n in &self.nodes {
            let parent_idx: i32 = match n.parent {
                Some(p) => p as i32,
                None    => -1,
            };
            out.extend_from_slice(&parent_idx.to_le_bytes());
            out.push(n.role as u8);
            out.extend_from_slice(&n.x.to_le_bytes());
            out.extend_from_slice(&n.y.to_le_bytes());
            out.extend_from_slice(&n.w.to_le_bytes());
            out.extend_from_slice(&n.h.to_le_bytes());
            let mut flags: u8 = 0;
            if n.id.is_some()    { flags |= 0b0001; }
            if n.label.is_some() { flags |= 0b0010; }
            if n.value.is_some() { flags |= 0b0100; }
            if n.scroll.is_some() { flags |= 0b1000; }
            out.push(flags);
            if let Some(s) = &n.id    { write_str(&mut out, s); }
            if let Some(s) = &n.label { write_str(&mut out, s); }
            if let Some(s) = &n.value { write_str(&mut out, s); }
            if let Some(s) = &n.scroll {
                out.extend_from_slice(&s.scroll_y.to_le_bytes());
                out.extend_from_slice(&s.content_h.to_le_bytes());
                out.extend_from_slice(&s.viewport_h.to_le_bytes());
                out.extend_from_slice(&s.viewport_w.to_le_bytes());
                out.extend_from_slice(&s.first_visible.unwrap_or(SCROLL_NULL_U32).to_le_bytes());
                out.extend_from_slice(&s.last_visible.unwrap_or(SCROLL_NULL_U32).to_le_bytes());
                out.extend_from_slice(&s.item_count.unwrap_or(SCROLL_NULL_U32).to_le_bytes());
                // v3 — single byte enum for orientation.
                out.push(match s.axis {
                    super::ScrollAxis::Vertical => 0,
                    super::ScrollAxis::Horizontal => 1,
                });
            }
        }
        out
    }

    /// Decode from v2 or v3 wire bytes. Returns `None` on truncated /
    /// malformed input — same loose contract as other SDK decoders.
    /// Test-only today; the host has its own JS decoder. Kept for
    /// round-trip tests that pin the wire shape.
    #[allow(dead_code)]
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        let mut p = 0usize;
        let version = *bytes.get(p)?;
        if version != 2 && version != Self::WIRE_VERSION { return None; }
        p += 1;
        if bytes.len() < p + 4 { return None; }
        let count = u32::from_le_bytes(bytes[p..p + 4].try_into().ok()?) as usize;
        p += 4;
        let mut tree = Self { nodes: Vec::with_capacity(count) };
        for _ in 0..count {
            if bytes.len() < p + 22 { return None; }
            let parent_i32 = i32::from_le_bytes(bytes[p..p + 4].try_into().ok()?);
            p += 4;
            let parent = if parent_i32 < 0 { None } else { Some(parent_i32 as u32) };
            let role = match bytes[p] {
                0 => SemanticRole::Group,
                1 => SemanticRole::Text,
                2 => SemanticRole::Button,
                3 => SemanticRole::Image,
                4 => SemanticRole::Rect,
                5 => SemanticRole::Spacer,
                6 => SemanticRole::Scroll,
                _ => return None,
            };
            p += 1;
            let x = f32::from_le_bytes(bytes[p..p + 4].try_into().ok()?); p += 4;
            let y = f32::from_le_bytes(bytes[p..p + 4].try_into().ok()?); p += 4;
            let w = f32::from_le_bytes(bytes[p..p + 4].try_into().ok()?); p += 4;
            let h = f32::from_le_bytes(bytes[p..p + 4].try_into().ok()?); p += 4;
            let flags = bytes[p]; p += 1;
            let id    = if flags & 0b0001 != 0 { Some(read_str(bytes, &mut p)?) } else { None };
            let label = if flags & 0b0010 != 0 { Some(read_str(bytes, &mut p)?) } else { None };
            let value = if flags & 0b0100 != 0 { Some(read_str(bytes, &mut p)?) } else { None };
            let scroll = if flags & 0b1000 != 0 {
                if bytes.len() < p + 28 { return None; }
                let scroll_y   = f32::from_le_bytes(bytes[p..p + 4].try_into().ok()?); p += 4;
                let content_h  = f32::from_le_bytes(bytes[p..p + 4].try_into().ok()?); p += 4;
                let viewport_h = f32::from_le_bytes(bytes[p..p + 4].try_into().ok()?); p += 4;
                let viewport_w = f32::from_le_bytes(bytes[p..p + 4].try_into().ok()?); p += 4;
                let fv = u32::from_le_bytes(bytes[p..p + 4].try_into().ok()?); p += 4;
                let lv = u32::from_le_bytes(bytes[p..p + 4].try_into().ok()?); p += 4;
                let ic = u32::from_le_bytes(bytes[p..p + 4].try_into().ok()?); p += 4;
                // v3 trailing byte: axis enum. v2 input has no byte
                // here — default to Vertical so the per-version
                // shapes round-trip cleanly.
                let axis = if version >= 3 {
                    if bytes.len() < p + 1 { return None; }
                    let v = bytes[p]; p += 1;
                    match v {
                        0 => super::ScrollAxis::Vertical,
                        1 => super::ScrollAxis::Horizontal,
                        _ => return None,
                    }
                } else {
                    super::ScrollAxis::Vertical
                };
                let null_if_max = |v: u32| if v == SCROLL_NULL_U32 { None } else { Some(v) };
                Some(ScrollSemantic {
                    scroll_y, content_h, viewport_h, viewport_w,
                    first_visible: null_if_max(fv),
                    last_visible:  null_if_max(lv),
                    item_count:    null_if_max(ic),
                    axis,
                })
            } else {
                None
            };
            tree.nodes.push(SemanticNode { parent, role, x, y, w, h, id, label, value, scroll });
        }
        Some(tree)
    }
}

fn write_str(out: &mut Vec<u8>, s: &str) {
    // Cap at 64 KiB so a runaway label can't overflow the u16 length
    // prefix. Realistic widget labels are well under that; the cap is
    // a guard against fuzz cases, not a real product constraint.
    let bytes = s.as_bytes();
    let len = bytes.len().min(u16::MAX as usize) as u16;
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(&bytes[..len as usize]);
}

fn read_str(bytes: &[u8], p: &mut usize) -> Option<String> {
    if bytes.len() < *p + 2 { return None; }
    let len = u16::from_le_bytes(bytes[*p..*p + 2].try_into().ok()?) as usize;
    *p += 2;
    if bytes.len() < *p + len { return None; }
    let s = core::str::from_utf8(&bytes[*p..*p + len]).ok()?.to_string();
    *p += len;
    Some(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_tree_round_trips() {
        let t = SemanticTree::new();
        let bytes = t.encode();
        let back = SemanticTree::decode(&bytes).expect("decode");
        assert_eq!(t, back);
        assert_eq!(bytes, vec![3, 0, 0, 0, 0]);
    }

    #[test]
    fn single_root_button_round_trips() {
        let mut t = SemanticTree::new();
        t.push(SemanticNode {
            parent: None, role: SemanticRole::Button,
            x: 10.0, y: 20.0, w: 100.0, h: 40.0,
            id: Some("ok".into()), label: Some("OK".into()), value: None,
            scroll: None,
        });
        let bytes = t.encode();
        let back = SemanticTree::decode(&bytes).expect("decode");
        assert_eq!(t, back);
    }

    #[test]
    fn nested_tree_round_trips() {
        let mut t = SemanticTree::new();
        let root = t.push(SemanticNode {
            parent: None, role: SemanticRole::Group,
            x: 0.0, y: 0.0, w: 800.0, h: 600.0,
            id: None, label: None, value: None, scroll: None,
        });
        t.push(SemanticNode {
            parent: Some(root), role: SemanticRole::Text,
            x: 16.0, y: 16.0, w: 200.0, h: 24.0,
            id: None, label: Some("Hello".into()), value: None, scroll: None,
        });
        t.push(SemanticNode {
            parent: Some(root), role: SemanticRole::Button,
            x: 16.0, y: 60.0, w: 80.0, h: 36.0,
            id: Some("go".into()), label: Some("Go".into()), value: None,
            scroll: None,
        });
        let bytes = t.encode();
        let back = SemanticTree::decode(&bytes).expect("decode");
        assert_eq!(t, back);
        assert_eq!(back.nodes.len(), 3);
        assert_eq!(back.nodes[0].parent, None);
        assert_eq!(back.nodes[1].parent, Some(0));
        assert_eq!(back.nodes[2].parent, Some(0));
    }

    #[test]
    fn scroll_node_round_trips_with_metadata() {
        let mut t = SemanticTree::new();
        t.push(SemanticNode {
            parent: None, role: SemanticRole::Scroll,
            x: 0.0, y: 0.0, w: 200.0, h: 400.0,
            id: Some("feed".into()), label: None, value: None,
            scroll: Some(ScrollSemantic {
                scroll_y: 312.5, content_h: 8400.0,
                viewport_h: 400.0, viewport_w: 200.0,
                first_visible: Some(8),
                last_visible:  Some(17),
                item_count:    Some(80),
                axis: super::super::ScrollAxis::Vertical,
            }),
        });
        let bytes = t.encode();
        let back = SemanticTree::decode(&bytes).expect("decode");
        assert_eq!(t, back);
    }

    #[test]
    fn scroll_node_with_null_index_fields() {
        // Plain Scroll (non-LazyColumn) has null first/last/count.
        let mut t = SemanticTree::new();
        t.push(SemanticNode {
            parent: None, role: SemanticRole::Scroll,
            x: 0.0, y: 0.0, w: 200.0, h: 400.0,
            id: Some("body".into()), label: None, value: None,
            scroll: Some(ScrollSemantic {
                scroll_y: 50.0, content_h: 1000.0,
                viewport_h: 400.0, viewport_w: 200.0,
                first_visible: None, last_visible: None, item_count: None,
                axis: super::super::ScrollAxis::Vertical,
            }),
        });
        let bytes = t.encode();
        let back = SemanticTree::decode(&bytes).expect("decode");
        assert_eq!(t, back);
        let s = back.nodes[0].scroll.unwrap();
        assert_eq!(s.first_visible, None);
        assert_eq!(s.last_visible, None);
        assert_eq!(s.item_count, None);
    }

    #[test]
    fn horizontal_scroll_round_trips_with_axis_byte() {
        // v3 round-trip with axis=Horizontal — the new byte must
        // survive encode/decode unchanged.
        let mut t = SemanticTree::new();
        t.push(SemanticNode {
            parent: None, role: SemanticRole::Scroll,
            x: 0.0, y: 0.0, w: 400.0, h: 200.0,
            id: Some("carousel".into()), label: None, value: None,
            scroll: Some(ScrollSemantic {
                scroll_y: 800.0, content_h: 4800.0,
                viewport_h: 400.0, viewport_w: 200.0,
                first_visible: Some(2), last_visible: Some(4),
                item_count: Some(12),
                axis: super::super::ScrollAxis::Horizontal,
            }),
        });
        let bytes = t.encode();
        let back = SemanticTree::decode(&bytes).expect("decode");
        assert_eq!(t, back);
        assert_eq!(back.nodes[0].scroll.unwrap().axis,
            super::super::ScrollAxis::Horizontal);
    }

    #[test]
    fn v2_bytes_decode_with_vertical_axis_default() {
        // Hand-craft a v2 (28-byte payload, no axis byte) blob and
        // assert the v2-aware decoder reads it back with axis =
        // Vertical. This is the back-compat path — v2 emitters
        // remain on the wire during a rolling upgrade.
        let mut bytes = Vec::<u8>::new();
        bytes.push(2); // version 2
        bytes.extend_from_slice(&1u32.to_le_bytes()); // count
        // node 0: parent=-1, role=Scroll, bounds, flags=has_id+has_scroll
        bytes.extend_from_slice(&(-1i32).to_le_bytes());
        bytes.push(SemanticRole::Scroll as u8);
        bytes.extend_from_slice(&0f32.to_le_bytes());
        bytes.extend_from_slice(&0f32.to_le_bytes());
        bytes.extend_from_slice(&200f32.to_le_bytes());
        bytes.extend_from_slice(&400f32.to_le_bytes());
        bytes.push(0b1001); // has_id | has_scroll
        bytes.extend_from_slice(&4u16.to_le_bytes()); bytes.extend_from_slice(b"feed");
        // 28-byte scroll payload (no axis byte).
        bytes.extend_from_slice(&100f32.to_le_bytes());
        bytes.extend_from_slice(&1000f32.to_le_bytes());
        bytes.extend_from_slice(&400f32.to_le_bytes());
        bytes.extend_from_slice(&200f32.to_le_bytes());
        bytes.extend_from_slice(&SCROLL_NULL_U32.to_le_bytes());
        bytes.extend_from_slice(&SCROLL_NULL_U32.to_le_bytes());
        bytes.extend_from_slice(&SCROLL_NULL_U32.to_le_bytes());
        let back = SemanticTree::decode(&bytes).expect("v2 decode");
        let s = back.nodes[0].scroll.expect("scroll present");
        assert_eq!(s.axis, super::super::ScrollAxis::Vertical);
        assert_eq!(s.scroll_y, 100.0);
    }

    #[test]
    fn truncated_blob_returns_none() {
        // Partial header — count missing.
        assert!(SemanticTree::decode(&[3, 0, 0]).is_none());
        // Header claims one node but the body is empty.
        assert!(SemanticTree::decode(&[3, 1, 0, 0, 0]).is_none());
    }

    #[test]
    fn unknown_role_returns_none() {
        // version=3, count=1, parent=-1, role=99, ...
        let mut bytes = vec![3u8, 1, 0, 0, 0];
        bytes.extend_from_slice(&(-1i32).to_le_bytes());
        bytes.push(99);  // bad role
        bytes.extend_from_slice(&0f32.to_le_bytes());
        bytes.extend_from_slice(&0f32.to_le_bytes());
        bytes.extend_from_slice(&0f32.to_le_bytes());
        bytes.extend_from_slice(&0f32.to_le_bytes());
        bytes.push(0);
        assert!(SemanticTree::decode(&bytes).is_none());
    }

    #[test]
    fn truncated_scroll_payload_returns_none() {
        // version=3 + 1 node + scroll flag set but no 29-byte payload.
        let mut bytes = vec![3u8, 1, 0, 0, 0];
        bytes.extend_from_slice(&(-1i32).to_le_bytes());
        bytes.push(6); // Scroll role
        bytes.extend_from_slice(&0f32.to_le_bytes());
        bytes.extend_from_slice(&0f32.to_le_bytes());
        bytes.extend_from_slice(&0f32.to_le_bytes());
        bytes.extend_from_slice(&0f32.to_le_bytes());
        bytes.push(0b1000);  // flag says scroll payload follows…
        // …but it doesn't.
        assert!(SemanticTree::decode(&bytes).is_none());
    }

    #[test]
    fn v3_scroll_payload_missing_axis_byte_returns_none() {
        // v3 + scroll flag + 28 of the 29 payload bytes — the
        // trailing axis byte is missing, decoder must reject.
        let mut bytes = vec![3u8, 1, 0, 0, 0];
        bytes.extend_from_slice(&(-1i32).to_le_bytes());
        bytes.push(SemanticRole::Scroll as u8);
        bytes.extend_from_slice(&0f32.to_le_bytes());
        bytes.extend_from_slice(&0f32.to_le_bytes());
        bytes.extend_from_slice(&200f32.to_le_bytes());
        bytes.extend_from_slice(&400f32.to_le_bytes());
        bytes.push(0b1000);
        // 28 bytes of scroll payload.
        bytes.extend_from_slice(&0f32.to_le_bytes());
        bytes.extend_from_slice(&0f32.to_le_bytes());
        bytes.extend_from_slice(&0f32.to_le_bytes());
        bytes.extend_from_slice(&0f32.to_le_bytes());
        bytes.extend_from_slice(&SCROLL_NULL_U32.to_le_bytes());
        bytes.extend_from_slice(&SCROLL_NULL_U32.to_le_bytes());
        bytes.extend_from_slice(&SCROLL_NULL_U32.to_le_bytes());
        // …no axis byte.
        assert!(SemanticTree::decode(&bytes).is_none());
    }

    #[test]
    fn v3_invalid_axis_byte_returns_none() {
        // v3 round-trip but axis byte = 99 (not 0/1).
        let mut bytes = vec![3u8, 1, 0, 0, 0];
        bytes.extend_from_slice(&(-1i32).to_le_bytes());
        bytes.push(SemanticRole::Scroll as u8);
        bytes.extend_from_slice(&0f32.to_le_bytes());
        bytes.extend_from_slice(&0f32.to_le_bytes());
        bytes.extend_from_slice(&200f32.to_le_bytes());
        bytes.extend_from_slice(&400f32.to_le_bytes());
        bytes.push(0b1000);
        bytes.extend_from_slice(&0f32.to_le_bytes());
        bytes.extend_from_slice(&0f32.to_le_bytes());
        bytes.extend_from_slice(&0f32.to_le_bytes());
        bytes.extend_from_slice(&0f32.to_le_bytes());
        bytes.extend_from_slice(&SCROLL_NULL_U32.to_le_bytes());
        bytes.extend_from_slice(&SCROLL_NULL_U32.to_le_bytes());
        bytes.extend_from_slice(&SCROLL_NULL_U32.to_le_bytes());
        bytes.push(99); // invalid axis byte
        assert!(SemanticTree::decode(&bytes).is_none());
    }

    #[test]
    fn role_decorative_classification() {
        assert!(SemanticRole::Rect.is_decorative());
        assert!(SemanticRole::Spacer.is_decorative());
        assert!(!SemanticRole::Button.is_decorative());
        assert!(!SemanticRole::Text.is_decorative());
        assert!(!SemanticRole::Group.is_decorative());
        assert!(!SemanticRole::Image.is_decorative());
    }
}
