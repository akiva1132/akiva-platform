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
pub mod spring;
pub mod gesture;

pub use spring::{AnimatedValue, Spring};
pub use gesture::{
    DoubleTapDetector,
    DOUBLE_TAP_MAX_INTERVAL_MS, DOUBLE_TAP_MAX_MOVE_PX, TAP_SLOP_PX,
};

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

/// Which axis a scroll region scrolls along. Stage 2c shipped vertical
/// only; Stage 2d added horizontal as a runtime-selected variant
/// (compose `Orientation` shape). Default `Vertical` preserves
/// pre-2d behaviour for every existing call site.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ScrollAxis {
    /// y-axis main, x-axis cross — `LazyColumn`, `Ui::scroll(...)`.
    #[default]
    Vertical,
    /// x-axis main, y-axis cross — `LazyRow`, `Ui::scroll_h(...)`.
    Horizontal,
}

/// Snap behaviour after a fling settles. Compose
/// `HorizontalPager` shape — three modes covering the common
/// "stops at a grid line" gestures.
///
/// Snap is item-/page-aware: `Item` rounds scroll position to the
/// nearest item stride (item size + gap), `Page` rounds to the
/// nearest viewport-page. `None` leaves the scroll wherever the
/// fling settles (pre-2d behaviour).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SnapMode {
    /// No snap. Scroll lands wherever the fling settles. Default.
    #[default]
    None,
    /// Snap to nearest item edge after settle. Stride = item
    /// main-axis size + gap, picked up from
    /// `ScrollCell::item_stride` each present pass.
    Item,
    /// Snap to nearest page edge after settle. Page size = the
    /// viewport's main-axis size.
    Page,
}

/// Duration of one snap animation, in ms. Compose `HorizontalPager`
/// defaults to ~300 ms; iOS `UIPageView` snaps in ~250 ms. We split
/// the difference at 250 — feels snappy on touch + isn't too quick
/// for the eye to track.
pub(crate) const SNAP_DURATION_MS: f32 = 250.0;

/// Pixel tolerance for "already at the snap target" — below this,
/// we don't kick off a snap animation. Avoids a 1-frame
/// micro-animation when the fling happened to settle on the grid.
pub(crate) const SNAP_THRESHOLD: f32 = 0.5;

/// Maximum displacement past an edge during an overscroll bounce,
/// in physical pixels. A fling that wants to carry further than
/// this gets pinned and its velocity zeroed so the spring restoring
/// force can pull from a known finite offset. Matches Android
/// `EdgeEffect`'s default visible distance (~64 dp).
pub(crate) const OVERSCROLL_MAX_PX: f32 = 64.0;

/// Spring config used by the overscroll bounce. Soft-stiffness +
/// critical damping → a single visible overshoot (the content
/// briefly hangs past the edge), then a smooth return. We keep
/// the epsilon larger than the default (0.5 px) because the
/// settled state is exactly on the edge — anything smaller would
/// just be sub-pixel jitter.
pub(crate) const OVERSCROLL_SPRING: spring::Spring = spring::Spring {
    stiffness: 150.0,
    damping_ratio: 1.0,
    epsilon: 0.5,
};

/// Persistent state for one scroll region. Lives in
/// [`UiState::scroll_states`] keyed by the caller-supplied
/// `ScrollBuilder::key`. Survives across frames so scroll position
/// doesn't reset on every rebuild — the same pattern as
/// [`UiState::hit_rects`], applied to scroll instead of clicks.
///
/// **Naming caveat for horizontal cells (Stage 2d, plan §3.2).** The
/// fields are named after the vertical case but mean "main-axis"
/// quantities for the cell's `axis`:
///   - `scroll_y` is the offset along the main axis (= y for
///     vertical, = x for horizontal).
///   - `content_h` is the content extent along the main axis (= total
///     height for vertical, total width for horizontal).
///   - `viewport_h` is the viewport size along the main axis.
///   - `viewport_w` is the viewport size along the cross axis.
/// The published wire (`ScrollSemantic`) keeps the same names; the
/// inspector exposes a separate `axis` field so callers that need to
/// be precise can read it.
#[derive(Debug, Clone, Copy, Default)]
pub struct ScrollCell {
    /// Which axis this cell scrolls along. Set by the present pass
    /// from the `Node::Scroll` / `Node::LazyColumn` / `Node::LazyRow`
    /// declaration; never flipped after creation (see plan §3.1).
    pub axis: ScrollAxis,
    /// Main-axis offset, in physical pixels. `0` = content top/left
    /// aligned with viewport top/left; positive values move content
    /// "up" (Vertical) or "left" (Horizontal) on screen.
    pub scroll_y: f32,
    /// Pixels-per-frame velocity for active flings. Set on
    /// pointer-up after a drag (Stage 2c.4); decays to 0 each tick.
    /// Programmatic `scroll_to` zeroes it.
    pub velocity: f32,
    /// Main-axis extent of the content (the scroll's "length"). For
    /// vertical: total height; for horizontal: total width. Updated
    /// every present pass; used to clamp `scroll_y` against
    /// `max = (content_h - viewport_h).max(0)`.
    pub content_h: f32,
    /// Cross-axis size of the viewport (width for vertical scrolls,
    /// height for horizontal scrolls).
    pub viewport_w: f32,
    /// Main-axis size of the viewport (height for vertical scrolls,
    /// width for horizontal scrolls).
    pub viewport_h: f32,
    /// Viewport rect in screen coordinates from the most recent
    /// present pass. Used by [`Ui::feed_wheel`] in Stage 2c.3 to
    /// hit-test wheel/pointer events against the topmost scroll
    /// region under the cursor.
    pub last_rect: Rect,
    /// Snap behaviour after a fling settles. Refreshed from the
    /// `LazyColumn::snap` / `LazyRow::snap` builder declaration
    /// each present pass; defaults to `SnapMode::None` for
    /// plain `Node::Scroll`. See [`SnapMode`].
    pub snap_mode: SnapMode,
    /// Item stride along the main axis (item size + gap). Used
    /// by `SnapMode::Item` to compute the nearest grid offset.
    /// Set each present pass by LazyColumn (item_h + gap) or
    /// LazyRow (item_w + gap). `0` (the default) disables item
    /// snap — `SnapMode::Item` falls back to no-op.
    pub item_stride: f32,
    /// Where the current snap animation started, in main-axis px.
    /// Only meaningful while `snap_target.is_some()`.
    pub snap_origin: f32,
    /// Active snap target. `Some` while a snap animation is in
    /// flight; cleared when the animation completes or the user
    /// takes back control (Down inside the region, wheel, or
    /// `scroll_to`).
    pub snap_target: Option<f32>,
    /// Milliseconds elapsed in the current snap. Advanced by
    /// `tick_animations`; lerp progress is
    /// `(elapsed / SNAP_DURATION_MS).clamp(0, 1)` with cubic-out
    /// easing on top.
    pub snap_elapsed_ms: f32,
    /// Marked `true` by `feed_pointer` on a drag-Up inside a
    /// snap-enabled cell — even a slow drag that didn't seed any
    /// fling velocity must still trigger the snap. Consumed by
    /// `tick_animations` when the snap kicks off, or cleared by
    /// any user-input interruption.
    pub snap_pending: bool,
    /// Stage 2e.3 — true while `scroll_y` is intentionally past one
    /// of the edges and the overscroll spring is pulling it back.
    /// Set when a fling tick crosses an edge with velocity; cleared
    /// when the spring settles or the user touches the region.
    /// `present()` honours this flag by relaxing its clamp range
    /// to `[-OVERSCROLL_MAX_PX, max + OVERSCROLL_MAX_PX]` while
    /// it's set, so the bounce isn't clipped back to the edge on
    /// the next frame.
    pub is_bouncing: bool,
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
    /// Frame timestamp of the most recent `present()`, in ms since
    /// activity start (the value the host passes to `Context`). Used
    /// to derive `dt_ms` for the fling-decay tick — see
    /// [`UiState::tick_animations`]. `0.0` before any present has
    /// run; the first frame then defaults to one 60fps frame.
    last_present_ts: f64,
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
/// inside; `started_at` is the (x, y) of the down event (for
/// slop thresholding); `last_main` is the *main-axis* projection of
/// the most recent Move (y for vertical scrolls, x for horizontal —
/// matches the cell's `axis`).
#[derive(Debug, Clone)]
pub(crate) struct DragGesture {
    pub(crate) key: String,
    /// Axis of the scroll region this drag started inside. Cached at
    /// Down time so Move events don't have to re-look-up the cell.
    pub(crate) axis: ScrollAxis,
    pub(crate) started_at: (f32, f32),
    /// Main-axis position of the most recent Move event (y if
    /// `axis == Vertical`, x otherwise). Per-frame deltas are
    /// `last_main - new_main` (positive = content scrolls forward).
    pub(crate) last_main: f32,
    pub(crate) is_dragging: bool,
}

/// Drag-detection threshold in dp. Pointer-down + move below this
/// distance still resolves as a tap (so a button inside a scroll
/// region keeps working); past it, the gesture switches to scroll-
/// drag and the original Down does *not* fire as a click.
/// Matches Android `ViewConfiguration.getScaledTouchSlop()` (8 dp on
/// most devices, 6 dp on a few); 6 is a good cross-platform median.
pub(crate) const DRAG_SLOP: f32 = 6.0;

/// Fling decay multiplier *per typical 60fps frame*. The actual
/// applied decay is `DECAY^(dt_ms / 16.667)` so a 30fps tick decays
/// twice as much as a 60fps one — keeps fling distance roughly
/// invariant to frame rate. 0.92 settles a 30 px/frame fling in ~25
/// frames (≈ 0.4 s at 60fps), which feels "snappy native" without
/// teleporting to rest.
pub(crate) const FLING_DECAY_PER_FRAME: f32 = 0.92;

/// Velocity below which we snap the fling to rest (0 px/frame). Pixel
/// motion this slow is below visual perception anyway; cutting it
/// short avoids dragging out a fling for 50+ extra frames just to
/// asymptote.
pub(crate) const FLING_STOP_VELOCITY: f32 = 0.5;

/// One 60fps frame in milliseconds. Used to dt-normalise fling decay
/// + position advance — see [`FLING_DECAY_PER_FRAME`].
pub(crate) const FRAME_MS: f32 = 1000.0 / 60.0;

/// Cubic-out easing on `[0, 1]`. Decelerating curve (steep at the
/// start, flat at the end) — matches Android's
/// `OvershootInterpolator`/`DecelerateInterpolator` family. Used
/// by snap animations so a card stops at its grid line without an
/// abrupt freeze.
pub(crate) fn cubic_out(p: f32) -> f32 {
    let clamped = p.clamp(0.0, 1.0);
    1.0 - (1.0 - clamped).powi(3)
}

/// Compute the snap target for a cell given its current
/// `scroll_y`, `snap_mode`, `item_stride`, and main-axis viewport.
/// Returns `None` if the cell has no snap or the snap math is
/// degenerate (stride/page ≤ 0). The result is clamped to
/// `[0, content_h - viewport_h]` so a snap can't try to scroll
/// past the end.
pub(crate) fn compute_snap_target(cell: &ScrollCell) -> Option<f32> {
    let max = (cell.content_h - cell.viewport_h).max(0.0);
    let raw = match cell.snap_mode {
        SnapMode::None => return None,
        SnapMode::Item => {
            if cell.item_stride <= 0.0 { return None; }
            (cell.scroll_y / cell.item_stride).round() * cell.item_stride
        }
        SnapMode::Page => {
            // Page = viewport along main axis (= cell.viewport_h
            // per the field-name convention; see ScrollCell docs).
            if cell.viewport_h <= 0.0 { return None; }
            (cell.scroll_y / cell.viewport_h).round() * cell.viewport_h
        }
    };
    Some(raw.clamp(0.0, max))
}

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
                    // Cache the axis at Down so Move events project
                    // the same way regardless of where the finger
                    // wanders — a near-horizontal swipe inside a
                    // vertical scroll stays a tap, not an axis steal.
                    let axis = self.scroll_states.get(&key)
                        .map(|c| c.axis).unwrap_or_default();
                    let last_main = match axis {
                        ScrollAxis::Vertical => ev.y,
                        ScrollAxis::Horizontal => ev.x,
                    };
                    // User is taking control back — cancel any
                    // in-flight snap or bounce so the touch wins
                    // over the animator. scroll_y is hard-clamped
                    // to [0, max] (no rubber-band during drag in
                    // this stage). snap_pending stays false until Up.
                    if let Some(cell) = self.scroll_states.get_mut(&key) {
                        cell.snap_target = None;
                        cell.snap_pending = false;
                        if cell.is_bouncing {
                            let max = (cell.content_h - cell.viewport_h).max(0.0);
                            cell.scroll_y = cell.scroll_y.clamp(0.0, max);
                            cell.velocity = 0.0;
                            cell.is_bouncing = false;
                        }
                    }
                    self.drag_gesture = Some(DragGesture {
                        key,
                        axis,
                        started_at: (ev.x, ev.y),
                        last_main,
                        is_dragging: false,
                    });
                } else {
                    self.pending_clicks.push((ev.x, ev.y));
                }
            }
            PointerPhase::Move => {
                if let Some(g) = self.drag_gesture.as_mut() {
                    // Project event position onto the main axis. Slop
                    // is measured along main only, so a sideways
                    // wobble inside a vertical scroll (and vice
                    // versa) stays a tap.
                    let (cur_main, started_main) = match g.axis {
                        ScrollAxis::Vertical =>
                            (ev.y, g.started_at.1),
                        ScrollAxis::Horizontal =>
                            (ev.x, g.started_at.0),
                    };
                    let total_main = (cur_main - started_main).abs();
                    if !g.is_dragging && total_main >= DRAG_SLOP {
                        g.is_dragging = true;
                    }
                    if g.is_dragging {
                        // Positive delta = finger moved against the
                        // axis = content scrolls forward (scroll_y
                        // increases). `last_main - cur_main` gives
                        // that sign for both axes.
                        let delta = g.last_main - cur_main;
                        // Capture before-update for borrow split.
                        let key = g.key.clone();
                        let cell = self.scroll_states.entry(key).or_default();
                        let max = (cell.content_h - cell.viewport_h).max(0.0);
                        cell.scroll_y = (cell.scroll_y + delta).clamp(0.0, max);
                        // EMA velocity in px/frame. Used by Stage
                        // 2c.4's fling animator at pointer-up. Half-
                        // life ≈ 1 sample with the 0.5 / 0.5 weights;
                        // bump the new-sample weight if drags feel
                        // sticky.
                        cell.velocity = 0.5 * cell.velocity + 0.5 * delta;
                    }
                    g.last_main = cur_main;
                }
            }
            PointerPhase::Up => {
                if let Some(g) = self.drag_gesture.take() {
                    if !g.is_dragging {
                        // Never crossed slop — promote to a tap so
                        // buttons inside the scroll region still
                        // work.
                        self.pending_clicks.push(g.started_at);
                    } else {
                        // Drag-end: cell.velocity already carries the
                        // EMA; 2c.4 picks it up to drive fling.
                        // If the cell has a snap mode, also flag
                        // snap_pending — slow drags that didn't
                        // seed velocity still snap on release.
                        if let Some(cell) = self.scroll_states.get_mut(&g.key) {
                            if cell.snap_mode != SnapMode::None {
                                cell.snap_pending = true;
                            }
                        }
                    }
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
    ///
    /// Per-axis routing: `ev.dy` looks for the deepest **vertical**
    /// scroll region containing the cursor; `ev.dx` looks for the
    /// deepest **horizontal** scroll region containing the cursor.
    /// The two axes are independent — a diagonal trackpad swipe
    /// routes each component to the closest matching region, and
    /// a pure-`dy` wheel inside a nested horizontal pager bubbles
    /// up past the pager to the enclosing vertical scroll (so a
    /// user hovering over a photo pager can still scroll the
    /// outer feed downward). Matches the native behaviour every
    /// desktop OS gives wheel events over nested scroll views.
    pub fn feed_wheel(&mut self, ev: super::activity::WheelEvent) {
        let Some((px, py)) = self.last_pointer else { return; };
        if ev.dy != 0.0 {
            if let Some(key) = self.scroll_region_at_axis(px, py, ScrollAxis::Vertical) {
                self.apply_wheel_delta(&key, ev.dy);
            }
        }
        if ev.dx != 0.0 {
            if let Some(key) = self.scroll_region_at_axis(px, py, ScrollAxis::Horizontal) {
                self.apply_wheel_delta(&key, ev.dx);
            }
        }
    }

    /// Apply a wheel delta to one scroll cell. Wheel input is an
    /// explicit user override, so any in-flight fling / snap /
    /// overscroll bounce gets cleared — the cell ends up at exactly
    /// `(scroll_y + delta).clamp(0, max)` with zero velocity.
    fn apply_wheel_delta(&mut self, key: &str, delta: f32) {
        let cell = self.scroll_states.entry(key.to_string()).or_default();
        let max = (cell.content_h - cell.viewport_h).max(0.0);
        // If the cell is in overscroll bounce, treat the bounce as
        // already resolved before applying the wheel delta — the
        // user's input shouldn't have to first "spend" pixels
        // cancelling an in-flight bounce.
        let base = cell.scroll_y.clamp(0.0, max);
        cell.scroll_y = (base + delta).clamp(0.0, max);
        cell.velocity = 0.0;
        cell.snap_target = None;
        cell.snap_pending = false;
        cell.is_bouncing = false;
    }

    /// Advance every active fling by `dt_ms`. `Ui::present` calls this
    /// once per frame; tests call it directly to step the simulation
    /// without spinning up a full Ui.
    ///
    /// Decay model — exponential, dt-normalised against a 60fps frame:
    /// ```text
    ///   frames_dt = (dt_ms / 16.667).clamp(0, 4)
    ///   velocity *= DECAY ^ frames_dt
    ///   scroll_y += velocity * frames_dt
    /// ```
    /// Hitting an edge clamps `scroll_y` and zeroes velocity — no
    /// rubber-band in this stage. Velocity below
    /// [`FLING_STOP_VELOCITY`] snaps to 0 so we don't drift on past
    /// the threshold of visual perception. The 4-frame clamp on
    /// `frames_dt` keeps a tab-resume (which can deliver a single
    /// `dt_ms = 500 ms`) from teleporting the scroll position.
    pub(crate) fn tick_animations(&mut self, dt_ms: f32) {
        if dt_ms <= 0.0 { return; }
        let frames_dt = (dt_ms / FRAME_MS).clamp(0.0, 4.0);
        for cell in self.scroll_states.values_mut() {
            let max = (cell.content_h - cell.viewport_h).max(0.0);
            // ── Phase 1a: bounce-back spring (overscroll) ─────────
            // Stage 2e.3 — when `is_bouncing`, the cell is past an
            // edge and a soft spring is pulling it back. This runs
            // before the fling path because the fling's exit
            // condition seeds the bounce (so we want the spring to
            // tick on the same frame the fling stops). When the
            // spring settles, we re-clamp scroll_y to the edge
            // exactly and clear the flag — the cell returns to
            // normal in-bounds physics.
            if cell.is_bouncing {
                let edge = if cell.scroll_y < 0.0 { 0.0 } else { max };
                // Safety cap: a giant fling can't carry the bounce
                // arbitrarily far. Clamp + zero outward velocity.
                let over = cell.scroll_y - edge;
                let capped = over.clamp(-OVERSCROLL_MAX_PX, OVERSCROLL_MAX_PX);
                if capped != over {
                    cell.scroll_y = edge + capped;
                    cell.velocity = 0.0;
                }
                let over_now = cell.scroll_y - edge;
                let (new_over, new_vel) = OVERSCROLL_SPRING.tick(
                    over_now, cell.velocity, 0.0, dt_ms,
                );
                cell.scroll_y = edge + new_over;
                cell.velocity = new_vel;
                if OVERSCROLL_SPRING.settled(new_over, new_vel, 0.0) {
                    cell.scroll_y = edge;
                    cell.velocity = 0.0;
                    cell.is_bouncing = false;
                }
                // Bounce phase consumed this tick — skip the fling
                // path. Snap is also skipped (no snap target gets
                // set while in bounce — overscroll always settles
                // at a page edge anyway, which IS a valid snap
                // target).
                continue;
            }
            // ── Phase 1b: fling decay ──────────────────────────────
            let had_velocity = cell.velocity != 0.0;
            if had_velocity {
                cell.velocity *= FLING_DECAY_PER_FRAME.powf(frames_dt);
                let new_y = cell.scroll_y + cell.velocity * frames_dt;
                if new_y < 0.0 || new_y > max {
                    // Crossed an edge with momentum. Hand off to
                    // the overscroll spring (next tick / this tick
                    // via `continue` if we re-enter). Cap the
                    // immediate overshoot so an absurd fling can't
                    // teleport into oblivion.
                    cell.scroll_y = new_y.clamp(
                        -OVERSCROLL_MAX_PX, max + OVERSCROLL_MAX_PX,
                    );
                    if cell.scroll_y != new_y {
                        // Pinned at cap — drop velocity so spring
                        // pulls back from a known offset.
                        cell.velocity = 0.0;
                    }
                    cell.is_bouncing = true;
                    continue;
                }
                cell.scroll_y = new_y;
                if cell.velocity.abs() < FLING_STOP_VELOCITY {
                    cell.velocity = 0.0;
                }
            }
            // ── Phase 2: snap entry ───────────────────────────────
            // Kick off a snap iff (a) the fling has fully settled
            // (velocity == 0) AND (b) the fling either just settled
            // this frame OR a drag-Up flagged snap_pending, AND
            // there's no snap already in flight, AND the cell has
            // a snap mode set. The `velocity == 0` gate means
            // snap_pending stays patiently true while the fling
            // continues — snap fires only after the glide stops.
            //
            // We don't auto-enter on long-idle cells — wheel events
            // explicitly do *not* set snap_pending, so the user
            // can wheel to a non-grid position and leave it.
            let just_settled = had_velocity && cell.velocity == 0.0;
            if cell.velocity == 0.0
                && (just_settled || cell.snap_pending)
                && cell.snap_target.is_none()
                && cell.snap_mode != SnapMode::None
            {
                cell.snap_pending = false;
                if let Some(target) = compute_snap_target(cell) {
                    if (target - cell.scroll_y).abs() > SNAP_THRESHOLD {
                        cell.snap_origin = cell.scroll_y;
                        cell.snap_target = Some(target);
                        cell.snap_elapsed_ms = 0.0;
                    }
                }
            }
            // ── Phase 3: snap advance ─────────────────────────────
            if let Some(target) = cell.snap_target {
                cell.snap_elapsed_ms += dt_ms;
                let progress = (cell.snap_elapsed_ms / SNAP_DURATION_MS).clamp(0.0, 1.0);
                let eased = cubic_out(progress);
                cell.scroll_y = cell.snap_origin + (target - cell.snap_origin) * eased;
                if progress >= 1.0 {
                    cell.scroll_y = target;
                    cell.snap_target = None;
                    cell.snap_elapsed_ms = 0.0;
                }
            }
        }
    }

    /// True iff any scroll cell is animating — fling in flight or
    /// snap glide active. Useful for activities (or the framework)
    /// that want to keep requesting frames while motion is on
    /// screen. Today the runtime renders every rAF regardless, so
    /// this is documentary — but it gives a future "render-on-
    /// demand" optimisation a clean predicate to gate on.
    pub fn has_active_animation(&self) -> bool {
        self.scroll_states.values().any(|c| {
            c.velocity != 0.0 || c.snap_target.is_some() || c.is_bouncing
        })
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

    /// Same as [`Self::scroll_region_at`] but only considers scroll
    /// regions whose axis matches `axis`. Lets `feed_wheel` route
    /// each wheel component to the right scroll: a pure-`dy` wheel
    /// inside a nested horizontal photo pager skips the pager and
    /// finds the enclosing vertical feed. Without this filter the
    /// inner pager would consume the event (smallest area wins) but
    /// then silently drop the `dy` component, freezing the outer
    /// scroll while the cursor was over any inner horizontal
    /// region.
    fn scroll_region_at_axis(
        &self, x: f32, y: f32, axis: ScrollAxis,
    ) -> Option<String> {
        let mut best: Option<(String, f32)> = None;
        for (key, cell) in &self.scroll_states {
            if cell.axis != axis { continue; }
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
    /// Frame timestamp from the host's `Context`. Threaded through so
    /// `present` can compute the dt for the fling-decay tick. `0.0`
    /// is a fine sentinel because activity timestamps are ms since
    /// activity start, so the very first frame is always > 0.
    ts_ms: f64,
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
            ts_ms: ctx.timestamp_ms,
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

    /// Single-child vertical scroll viewport. Chain `.key(...)` to
    /// address the region across frames (so the scroll position
    /// persists), then `.size(w, h)` or `.height(h)` if you want to
    /// lock the viewport to a specific size rather than inheriting
    /// from the slot. For horizontal scrolling, use [`Self::scroll_h`].
    pub fn scroll(&mut self, child: NodeId) -> node::ScrollBuilder<'_, 's, 'c, 'b> {
        node::ScrollBuilder::new(self, child)
    }

    /// Single-child horizontal scroll viewport — the carousel-style
    /// counterpart of [`Self::scroll`]. Same builder surface; the
    /// child measures against unlimited *width* (its intrinsic
    /// width becomes the scroll content extent) and paints with a
    /// `-scroll_y` x-offset (the field name lies for horizontal
    /// scrolls — see `ScrollCell` docs).
    pub fn scroll_h(&mut self, child: NodeId) -> node::ScrollBuilder<'_, 's, 'c, 'b> {
        node::ScrollBuilder::new_with_axis(self, child, ScrollAxis::Horizontal)
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
        // Programmatic scroll is exact — never snap or bounce on
        // top of a `scroll_to`. Callers that want a snap can
        // target an already-aligned offset.
        cell.snap_target = None;
        cell.snap_pending = false;
        cell.is_bouncing = false;
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

    /// Cached *main-axis* viewport size for a scroll key from the
    /// last present pass (viewport height for vertical lists,
    /// viewport width for horizontal lists). `LazyColumn` /
    /// `LazyRow` builders read this to compute their visible range
    /// on rebuilds — second frame onward uses the real measurement
    /// so virtualisation kicks in. Crate-private; apps query via
    /// [`Self::scroll_extent`] instead.
    pub(crate) fn scroll_cell_viewport_main(&self, key: &str) -> f32 {
        self.state.scroll_states.get(key).map(|c| c.viewport_h).unwrap_or(0.0)
    }

    /// Begin a [`LazyColumn`][node::Node::LazyColumn]. Terminate with
    /// `.items(|ui, i| ...)` — the closure is invoked once per
    /// visible index, in order. Uniform `item_h` is required; gap +
    /// padding are optional. See [`node::LazyColumnBuilder`].
    pub fn lazy_column(&mut self, count: u32) -> node::LazyColumnBuilder<'_, 's, 'c, 'b> {
        node::LazyColumnBuilder::new(self, count)
    }

    /// Begin a [`LazyRow`][node::Node::LazyRow] — the horizontal
    /// twin of [`Self::lazy_column`]. Uniform `item_w` is required.
    /// See [`node::LazyRowBuilder`].
    pub fn lazy_row(&mut self, count: u32) -> node::LazyRowBuilder<'_, 's, 'c, 'b> {
        node::LazyRowBuilder::new(self, count)
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

        // Advance any active fling for this frame. dt comes from
        // (this frame's ts) - (last present's ts); first frame
        // defaults to one 60fps frame so a fling started right
        // before the first present advances by something reasonable.
        let dt_ms = if self.state.last_present_ts > 0.0 {
            (self.ts_ms - self.state.last_present_ts) as f32
        } else {
            FRAME_MS
        };
        self.state.tick_animations(dt_ms);
        self.state.last_present_ts = self.ts_ms;

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

        // Refresh every Scroll / LazyColumn cell with this frame's
        // measured content/viewport sizes + viewport rect, then clamp
        // scroll_y against the fresh max. A node's first frame
        // initialises the cell at scroll_y=0 (the default) so the
        // very first paint shows the top of the content —
        // programmatic `scroll_to` called before any present() honours
        // the same default until the cell catches up.
        for (id, node) in self.nodes.iter().enumerate() {
            // Per-node side-info packed into a 5-tuple:
            //   (key, content_main, axis, snap_mode, item_stride)
            // `item_stride = item_main_size + gap` for lazy lists;
            // 0 for plain Scroll (which has no item granularity, so
            // SnapMode::Item degenerates to no-op there).
            let (key, content_main, axis, snap_mode, item_stride) = match node {
                Node::Scroll { child, key, axis, .. } => {
                    let (cw, ch) = sizes.get(child).copied().unwrap_or((0.0, 0.0));
                    // Main-axis extent of the content: child's
                    // height for vertical scrolls, width for
                    // horizontal scrolls.
                    let content = match axis {
                        ScrollAxis::Vertical => ch,
                        ScrollAxis::Horizontal => cw,
                    };
                    (key, content, *axis, SnapMode::None, 0.0)
                }
                Node::LazyColumn { key, count, item_h, gap, padding, snap, .. } => {
                    // Content height is `count * item_h + (count-1) *
                    // gap + padding_v`, regardless of which items are
                    // realised — virtualisation must not change the
                    // scrollable extent.
                    let n = *count as f32;
                    let ch = if *count == 0 {
                        padding.0 + padding.2
                    } else {
                        n * item_h + (n - 1.0) * gap + padding.0 + padding.2
                    };
                    (key, ch, ScrollAxis::Vertical, *snap, item_h + gap)
                }
                Node::LazyRow { key, count, item_w, gap, padding, snap, .. } => {
                    // Mirror of LazyColumn on the x-axis. Main-axis
                    // extent uses padding_left + padding_right.
                    let n = *count as f32;
                    let cw = if *count == 0 {
                        padding.3 + padding.1
                    } else {
                        n * item_w + (n - 1.0) * gap + padding.3 + padding.1
                    };
                    (key, cw, ScrollAxis::Horizontal, *snap, item_w + gap)
                }
                _ => continue,
            };
            let viewport_rect = rects.get(&id).copied().unwrap_or_default();
            let cell = self.state.scroll_states.entry(key.clone()).or_default();
            cell.axis = axis;
            cell.snap_mode = snap_mode;
            cell.item_stride = item_stride;
            cell.content_h = content_main;
            // viewport_w / viewport_h follow the convention in
            // `ScrollCell`: `viewport_h` = main-axis size,
            // `viewport_w` = cross-axis size. For vertical scrolls
            // those map straight onto rect.w / rect.h; for
            // horizontal they swap.
            match axis {
                ScrollAxis::Vertical => {
                    cell.viewport_w = viewport_rect.w;
                    cell.viewport_h = viewport_rect.h;
                }
                ScrollAxis::Horizontal => {
                    cell.viewport_w = viewport_rect.h;
                    cell.viewport_h = viewport_rect.w;
                }
            }
            cell.last_rect = viewport_rect;
            let max = (cell.content_h - cell.viewport_h).max(0.0);
            // During a bounce, the cell is intentionally past edge —
            // clamp only to the overscroll cap (safety net against
            // content shrink during paint). Otherwise clamp tight
            // so a content shrink doesn't strand scroll_y past the
            // new end.
            if cell.is_bouncing {
                let upper = max + OVERSCROLL_MAX_PX;
                let lower = -OVERSCROLL_MAX_PX;
                if cell.scroll_y > upper { cell.scroll_y = upper; cell.velocity = 0.0; }
                if cell.scroll_y < lower { cell.scroll_y = lower; cell.velocity = 0.0; }
            } else {
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
            let tree = build_semantic_tree(
                &self.nodes, root, &rects, &self.state.scroll_states,
            );
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
    scroll_states: &HashMap<String, ScrollCell>,
) -> semantics::SemanticTree {
    use semantics::{ScrollSemantic, SemanticNode, SemanticRole, SemanticTree};
    let mut tree = SemanticTree::new();
    fn visit(
        nodes: &[Node],
        rects: &HashMap<NodeId, Rect>,
        scroll_states: &HashMap<String, ScrollCell>,
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
            Node::Scroll { key, .. }
            | Node::LazyColumn { key, .. }
            | Node::LazyRow { key, .. } => {
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
        // Stage 2c.6 — pack scroll-region metadata into the semantic
        // node so the inspector can publish scroll_y, content_h, the
        // visible index range, etc. without an out-of-band channel.
        // Plain `Node::Scroll` has null first/last/count; only
        // virtualisers populate the index trio.
        let scroll = match node {
            Node::Scroll { key, .. } => {
                let cell = scroll_states.get(key).copied().unwrap_or_default();
                Some(ScrollSemantic {
                    scroll_y: cell.scroll_y,
                    content_h: cell.content_h,
                    viewport_h: cell.viewport_h,
                    viewport_w: cell.viewport_w,
                    first_visible: None,
                    last_visible: None,
                    item_count: None,
                    axis: cell.axis,
                })
            }
            Node::LazyColumn { key, count, realized, .. }
            | Node::LazyRow { key, count, realized, .. } => {
                let cell = scroll_states.get(key).copied().unwrap_or_default();
                let (first, last) = if let (Some((f, _)), Some((l, _))) =
                    (realized.first(), realized.last())
                {
                    (Some(*f), Some(*l))
                } else {
                    (None, None)
                };
                Some(ScrollSemantic {
                    scroll_y: cell.scroll_y,
                    content_h: cell.content_h,
                    viewport_h: cell.viewport_h,
                    viewport_w: cell.viewport_w,
                    first_visible: first,
                    last_visible: last,
                    item_count: Some(*count),
                    axis: cell.axis,
                })
            }
            _ => None,
        };
        let idx = tree.push(SemanticNode {
            parent, role,
            x: rect.x, y: rect.y, w: rect.w, h: rect.h,
            id, label, value, scroll,
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
                    visit(nodes, rects, scroll_states, tree, *c, Some(idx));
                }
            }
            Node::Scroll { child, .. } => {
                visit(nodes, rects, scroll_states, tree, *child, Some(idx));
            }
            Node::LazyColumn { realized, .. } | Node::LazyRow { realized, .. } => {
                for (_, c) in realized {
                    visit(nodes, rects, scroll_states, tree, *c, Some(idx));
                }
            }
            _ => {}
        }
    }
    visit(nodes, rects, scroll_states, &mut tree, root, None);
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
        seed_scroll_cell_axis(state, key, rect, content_h, scroll_y,
            ScrollAxis::Vertical);
    }

    /// Variant of `seed_scroll_cell` that lets the test pick the
    /// scroll axis. Vertical cells follow the pre-2d field
    /// convention (`viewport_w` = rect.w, `viewport_h` = rect.h);
    /// horizontal cells swap those (see `ScrollCell` docs).
    fn seed_scroll_cell_axis(
        state: &mut UiState, key: &str,
        rect: Rect, content_h: f32, scroll_y: f32, axis: ScrollAxis,
    ) {
        let (vw, vh) = match axis {
            ScrollAxis::Vertical => (rect.w, rect.h),
            ScrollAxis::Horizontal => (rect.h, rect.w),
        };
        state.scroll_states.insert(key.to_string(), ScrollCell {
            axis,
            scroll_y,
            velocity: 0.0,
            content_h,
            viewport_w: vw,
            viewport_h: vh,
            last_rect: rect,
            ..Default::default()
        });
    }

    fn wheel(dy: f32) -> crate::activity::WheelEvent {
        crate::activity::WheelEvent { dx: 0.0, dy }
    }

    fn wheel_xy(dx: f32, dy: f32) -> crate::activity::WheelEvent {
        crate::activity::WheelEvent { dx, dy }
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

    // ── Stage 2c.4: fling animation ──────────────────────────────────

    #[test]
    fn fling_decays_monotonically_over_time() {
        // Seed a strong upward fling; tick at 60fps and watch velocity
        // strictly decrease until it snaps to 0.
        let mut state = UiState::new();
        seed_scroll_cell(
            &mut state, "feed",
            Rect { x: 0.0, y: 0.0, w: 200.0, h: 400.0 },
            10_000.0, 100.0,
        );
        state.scroll_states.get_mut("feed").unwrap().velocity = 20.0;
        let mut prev = 20.0;
        for _ in 0..200 {
            state.tick_animations(FRAME_MS);
            let v = state.scroll_states["feed"].velocity;
            assert!(v <= prev + 1e-3,
                "velocity must be non-increasing each tick: prev={prev}, now={v}");
            prev = v;
            if v == 0.0 { break; }
        }
        assert_eq!(state.scroll_states["feed"].velocity, 0.0,
            "fling must come to rest within 200 frames");
    }

    #[test]
    fn fling_advances_scroll_position() {
        let mut state = UiState::new();
        seed_scroll_cell(
            &mut state, "feed",
            Rect { x: 0.0, y: 0.0, w: 200.0, h: 400.0 },
            10_000.0, 100.0,
        );
        state.scroll_states.get_mut("feed").unwrap().velocity = 15.0;
        let start = state.scroll_states["feed"].scroll_y;
        state.tick_animations(FRAME_MS);
        let after = state.scroll_states["feed"].scroll_y;
        assert!(after > start, "scroll_y must advance with positive velocity");
    }

    #[test]
    fn fling_enters_overscroll_bounce_at_bottom_edge() {
        // Pre-Stage 2e behaviour was: clamp scroll_y to max, zero
        // velocity. After 2e.3 a fling that crosses the edge enters
        // the overscroll bounce phase — scroll_y goes briefly past
        // max with `is_bouncing = true`, and a soft spring pulls
        // it back over subsequent frames.
        let mut state = UiState::new();
        seed_scroll_cell(
            &mut state, "feed",
            Rect { x: 0.0, y: 0.0, w: 200.0, h: 400.0 },
            500.0,        // content_h → max scroll = 100
            95.0,         // 5 px from the bottom edge
        );
        state.scroll_states.get_mut("feed").unwrap().velocity = 50.0;
        state.tick_animations(FRAME_MS);
        let cell = &state.scroll_states["feed"];
        assert!(cell.is_bouncing, "must enter bounce when fling crosses bottom edge");
        assert!(cell.scroll_y > 100.0,
            "scroll_y must be past max during bounce (got {})", cell.scroll_y);
        assert!(cell.scroll_y <= 100.0 + OVERSCROLL_MAX_PX,
            "scroll_y must stay within overscroll cap");
    }

    #[test]
    fn fling_enters_overscroll_bounce_at_top_edge() {
        // Mirror of the bottom-edge case for negative velocity at
        // the top.
        let mut state = UiState::new();
        seed_scroll_cell(
            &mut state, "feed",
            Rect { x: 0.0, y: 0.0, w: 200.0, h: 400.0 },
            500.0, 5.0,
        );
        state.scroll_states.get_mut("feed").unwrap().velocity = -50.0;
        state.tick_animations(FRAME_MS);
        let cell = &state.scroll_states["feed"];
        assert!(cell.is_bouncing);
        assert!(cell.scroll_y < 0.0,
            "scroll_y must be past top edge during bounce (got {})", cell.scroll_y);
        assert!(cell.scroll_y >= -OVERSCROLL_MAX_PX);
    }

    #[test]
    fn overscroll_bounce_settles_back_to_edge_in_finite_frames() {
        // After the fling enters bounce, the overscroll spring
        // pulls scroll_y exactly back to the edge. The spring is
        // critically damped at low stiffness, so settling takes
        // ~30–60 frames in worst case.
        let mut state = UiState::new();
        seed_scroll_cell(
            &mut state, "feed",
            Rect { x: 0.0, y: 0.0, w: 200.0, h: 400.0 },
            500.0, 95.0,
        );
        state.scroll_states.get_mut("feed").unwrap().velocity = 50.0;
        // Step until settled or budget exhausted (200 frames =
        // ~3.3 s of simulated time — comfortably past the spring's
        // settling time).
        let mut settled = false;
        for _ in 0..200 {
            state.tick_animations(FRAME_MS);
            if !state.scroll_states["feed"].is_bouncing {
                settled = true;
                break;
            }
        }
        assert!(settled, "overscroll bounce must settle within 200 frames");
        let cell = &state.scroll_states["feed"];
        assert_eq!(cell.scroll_y, 100.0, "must land exactly on the edge");
        assert_eq!(cell.velocity, 0.0);
        assert!(!cell.is_bouncing);
    }

    #[test]
    fn pointer_down_in_bouncing_cell_clamps_back_to_edge() {
        // Touch input wins over the animator — Down inside a
        // bouncing region clamps scroll_y to [0, max], zeros
        // velocity, and clears is_bouncing so the new gesture
        // starts from a clean state.
        let mut state = UiState::new();
        seed_scroll_cell(
            &mut state, "feed",
            Rect { x: 0.0, y: 0.0, w: 200.0, h: 400.0 },
            500.0, 95.0,
        );
        {
            let cell = state.scroll_states.get_mut("feed").unwrap();
            cell.scroll_y = 120.0;  // past edge
            cell.velocity = -10.0;
            cell.is_bouncing = true;
        }
        state.feed_pointer(pointer(100.0, 200.0, PointerPhase::Down));
        let cell = &state.scroll_states["feed"];
        assert_eq!(cell.scroll_y, 100.0, "must clamp to max");
        assert_eq!(cell.velocity, 0.0);
        assert!(!cell.is_bouncing);
    }

    #[test]
    fn wheel_during_bounce_clears_bounce_state() {
        let mut state = UiState::new();
        seed_scroll_cell(
            &mut state, "feed",
            Rect { x: 0.0, y: 0.0, w: 200.0, h: 400.0 },
            500.0, 95.0,
        );
        {
            let cell = state.scroll_states.get_mut("feed").unwrap();
            cell.scroll_y = 120.0;
            cell.velocity = -10.0;
            cell.is_bouncing = true;
        }
        // Position the pointer over the region so feed_wheel can
        // hit-test, then dispatch a small wheel delta.
        state.feed_pointer(pointer(100.0, 200.0, PointerPhase::Move));
        state.feed_wheel(wheel(-30.0));
        let cell = &state.scroll_states["feed"];
        assert!(!cell.is_bouncing, "wheel must clear bounce");
        assert_eq!(cell.velocity, 0.0);
        // The wheel delta (-30) applied from scroll_y=120 (which
        // got clamped to max=100 by the wheel's own clamp before
        // the delta), landing at 70.
        assert_eq!(cell.scroll_y, 70.0);
    }

    #[test]
    fn overscroll_caps_at_max_px() {
        // A massive fling can't carry the bounce arbitrarily far —
        // OVERSCROLL_MAX_PX is the safety pin.
        let mut state = UiState::new();
        seed_scroll_cell(
            &mut state, "feed",
            Rect { x: 0.0, y: 0.0, w: 200.0, h: 400.0 },
            500.0, 99.0,
        );
        state.scroll_states.get_mut("feed").unwrap().velocity = 10000.0;
        state.tick_animations(FRAME_MS);
        let cell = &state.scroll_states["feed"];
        assert!(cell.is_bouncing);
        assert!(cell.scroll_y <= 100.0 + OVERSCROLL_MAX_PX);
    }

    #[test]
    fn fling_dt_normalisation_30fps_matches_60fps_distance() {
        // 30fps means each tick is 2× as long, but velocity decays
        // and position advances proportionally — so total distance
        // travelled is invariant (modulo discretisation noise).
        fn run(dt_ms: f32, n: u32) -> f32 {
            let mut state = UiState::new();
            seed_scroll_cell(
                &mut state, "feed",
                Rect { x: 0.0, y: 0.0, w: 200.0, h: 4000.0 },
                10_000.0, 0.0,
            );
            state.scroll_states.get_mut("feed").unwrap().velocity = 30.0;
            for _ in 0..n { state.tick_animations(dt_ms); }
            state.scroll_states["feed"].scroll_y
        }
        let dist_60 = run(FRAME_MS, 60);
        let dist_30 = run(FRAME_MS * 2.0, 30);
        // Within 10% — discretisation noise + the snap-to-zero
        // threshold means it won't be exactly equal.
        let ratio = dist_30 / dist_60;
        assert!((0.9..=1.1).contains(&ratio),
            "30fps/60fps distance ratio = {ratio} (60: {dist_60}, 30: {dist_30})");
    }

    #[test]
    fn has_active_animation_reflects_velocity() {
        let mut state = UiState::new();
        assert!(!state.has_active_animation());
        seed_scroll_cell(
            &mut state, "feed",
            Rect { x: 0.0, y: 0.0, w: 200.0, h: 400.0 },
            10_000.0, 0.0,
        );
        assert!(!state.has_active_animation(), "velocity 0 doesn't count");
        state.scroll_states.get_mut("feed").unwrap().velocity = 5.0;
        assert!(state.has_active_animation());
    }

    #[test]
    fn tick_animations_with_zero_dt_is_noop() {
        let mut state = UiState::new();
        seed_scroll_cell(
            &mut state, "feed",
            Rect { x: 0.0, y: 0.0, w: 200.0, h: 400.0 },
            10_000.0, 100.0,
        );
        state.scroll_states.get_mut("feed").unwrap().velocity = 20.0;
        let before = state.scroll_states["feed"].clone();
        state.tick_animations(0.0);
        let after = state.scroll_states["feed"];
        assert_eq!(before.scroll_y, after.scroll_y);
        assert_eq!(before.velocity, after.velocity);
    }

    #[test]
    fn tick_animations_clamps_huge_dt_to_four_frames() {
        // A tab-resume can deliver a single dt = 500 ms or more.
        // Without a clamp the fling would teleport. We clamp to 4
        // frames worth of advance / decay.
        let mut state = UiState::new();
        seed_scroll_cell(
            &mut state, "feed",
            Rect { x: 0.0, y: 0.0, w: 200.0, h: 4000.0 },
            10_000.0, 0.0,
        );
        state.scroll_states.get_mut("feed").unwrap().velocity = 30.0;
        // dt = 60 frames worth
        state.tick_animations(FRAME_MS * 60.0);
        let cell = &state.scroll_states["feed"];
        // At most 4 frames of advance: 30 * 4 = 120 px upper bound
        // (with decay it's less). Anything close to "60 * 30 = 1800"
        // would be teleportation.
        assert!(cell.scroll_y < 200.0,
            "huge dt must be clamped — got scroll_y = {}", cell.scroll_y);
    }

    #[test]
    fn scroll_to_during_active_fling_zeros_velocity() {
        // scroll_to is "user intent overrides physics" — verified in
        // 2c.2, re-checked here in the fling context.
        let mut state = UiState::new();
        seed_scroll_cell(
            &mut state, "feed",
            Rect { x: 0.0, y: 0.0, w: 200.0, h: 400.0 },
            10_000.0, 100.0,
        );
        state.scroll_states.get_mut("feed").unwrap().velocity = 25.0;
        let c = ctx("t");
        let mut buf = Vec::new();
        let mut canvas = Canvas::new(&mut buf);
        let mut ui = Ui::new(&mut state, &c, &mut canvas);
        ui.scroll_to("feed", 500.0);
        assert_eq!(state.scroll_states["feed"].velocity, 0.0);
        assert_eq!(state.scroll_states["feed"].scroll_y, 500.0);
    }

    #[test]
    fn fling_advances_during_present_pass() {
        // Real-world: drag ends with a velocity; the next frame's
        // present() ticks the fling forward. We seed a scroll cell +
        // velocity, present twice at 16.67 ms apart, and observe
        // scroll_y has advanced.
        let mut state = UiState::new();
        // First frame establishes last_present_ts.
        {
            let c = Context {
                metrics: Metrics { width_px: 800, height_px: 600, density: 1.0 },
                timestamp_ms: 100.0, pointer: None, class: "t", handle: 0,
            };
            let mut buf = Vec::new();
            let mut canvas = Canvas::new(&mut buf);
            let mut ui = Ui::new(&mut state, &c, &mut canvas);
            let r = ui.rect(0xff00ffff).size(200.0, 4000.0).build();
            let s = ui.scroll(r).key("feed").size(200.0, 400.0).build();
            ui.present(s);
        }
        // Seed a fling.
        state.scroll_states.get_mut("feed").unwrap().velocity = 25.0;
        let scroll_y_before = state.scroll_states["feed"].scroll_y;
        // Second frame, 16.67ms later.
        {
            let c = Context {
                metrics: Metrics { width_px: 800, height_px: 600, density: 1.0 },
                timestamp_ms: 116.667, pointer: None, class: "t", handle: 0,
            };
            let mut buf = Vec::new();
            let mut canvas = Canvas::new(&mut buf);
            let mut ui = Ui::new(&mut state, &c, &mut canvas);
            let r = ui.rect(0xff00ffff).size(200.0, 4000.0).build();
            let s = ui.scroll(r).key("feed").size(200.0, 400.0).build();
            ui.present(s);
        }
        assert!(state.scroll_states["feed"].scroll_y > scroll_y_before,
            "fling must advance during present()");
    }

    // ── Stage 2c.5: Node::LazyColumn ─────────────────────────────────

    /// Build a LazyColumn with the given dimensions and capture the
    /// indices the closure was invoked for. Returns
    /// `(realized_indices, NodeId, UiState)`.
    fn build_lazy(
        viewport_h: f32, item_h: f32, count: u32,
        seeded_cell: Option<(f32, f32)>,  // (scroll_y, cached_viewport_h)
    ) -> (Vec<u32>, NodeId, UiState) {
        use std::cell::RefCell;
        let mut state = UiState::new();
        if let Some((scroll_y, cached_vh)) = seeded_cell {
            // Pre-seed so the cold-start path doesn't fire.
            state.scroll_states.insert("feed".to_string(), ScrollCell {
                scroll_y,
                content_h: count as f32 * item_h,
                viewport_w: 200.0,
                viewport_h: cached_vh,
                last_rect: Rect { x: 0.0, y: 0.0, w: 200.0, h: cached_vh },
                ..Default::default()
            });
        }
        let c = ctx("t");
        let mut buf = Vec::new();
        let mut canvas = Canvas::new(&mut buf);
        let mut ui = Ui::new(&mut state, &c, &mut canvas);
        let realized: RefCell<Vec<u32>> = RefCell::new(Vec::new());
        let lc = ui.lazy_column(count)
            .key("feed")
            .item_h(item_h)
            .size(200.0, viewport_h)
            .items(|ui, i| {
                realized.borrow_mut().push(i);
                ui.rect(0xff00ffff).size(200.0, item_h).build()
            });
        ui.present(lc);
        (realized.into_inner(), lc, state)
    }

    #[test]
    fn lazycolumn_first_frame_realises_up_to_cold_start_cap() {
        // No cached cell + no item_h hint → cold start fires the
        // first LAZY_COLD_START_CAP items.
        let (realized, _, _) = build_lazy(400.0, 0.0, 80, None);
        // item_h = 0 → stride = 0 → cold start path.
        assert_eq!(realized.len(), node::LAZY_COLD_START_CAP as usize);
        assert_eq!(realized.first(), Some(&0));
    }

    #[test]
    fn lazycolumn_realises_only_visible_plus_overscan_with_seeded_cell() {
        // Seed a cached cell so the second-frame virtualisation path
        // kicks in. viewport_h=400, item_h=80, count=80, scroll_y=0:
        // visible items 0..=5 (5*80=400). +overscan(2) → 0..=7 = 8 items.
        let (realized, _, _) = build_lazy(400.0, 80.0, 80, Some((0.0, 400.0)));
        assert!(realized.len() <= 8 + 2,
            "expected ≤ 10 realised items, got {} ({realized:?})", realized.len());
        assert_eq!(realized.first(), Some(&0));
    }

    #[test]
    fn lazycolumn_advances_visible_range_when_scrolled() {
        // scroll_y = 400 → 5 items past the top → first_visible = 5.
        let (realized, _, _) = build_lazy(400.0, 80.0, 80, Some((400.0, 400.0)));
        assert!(*realized.first().unwrap() >= 3,
            "first realised index should advance with scroll_y, got {}",
            realized.first().unwrap());
        assert!(*realized.last().unwrap() < 80,
            "last realised must stay inside count, got {}",
            realized.last().unwrap());
    }

    #[test]
    fn lazycolumn_clamps_realised_indices_to_count() {
        // Scroll way past the end — no items past `count` should be
        // built.
        let (realized, _, _) = build_lazy(400.0, 80.0, 10, Some((10_000.0, 400.0)));
        assert!(realized.iter().all(|&i| i < 10));
    }

    #[test]
    fn lazycolumn_content_h_eq_count_times_item_h_plus_gap_padding() {
        // 80 items × 80 dp + 79 × 8 gap + 16 + 16 padding =
        //   6400 + 632 + 32 = 7064.
        let mut state = UiState::new();
        let c = ctx("t");
        let mut buf = Vec::new();
        let mut canvas = Canvas::new(&mut buf);
        let mut ui = Ui::new(&mut state, &c, &mut canvas);
        let lc = ui.lazy_column(80)
            .key("feed").item_h(80.0).gap(8.0)
            .padding(16.0).size(200.0, 400.0)
            .items(|ui, _i| ui.rect(0xff00ffff).build());
        ui.present(lc);
        let (content_h, viewport_h) = ui.scroll_extent("feed");
        assert_eq!(content_h, 80.0 * 80.0 + 79.0 * 8.0 + 32.0);
        assert_eq!(viewport_h, 400.0);
    }

    #[test]
    fn lazycolumn_empty_count_realises_nothing() {
        let (realized, _, state) = build_lazy(400.0, 80.0, 0, Some((0.0, 400.0)));
        assert!(realized.is_empty());
        // content_h = padding only (none in this helper → 0).
        assert_eq!(state.scroll_states["feed"].content_h, 0.0);
    }

    #[test]
    fn lazycolumn_realised_count_independent_of_count() {
        // 10 items vs 10 000 items should realise about the same
        // number of items at scroll_y=0 — the whole point of
        // virtualisation.
        let (r_small, _, _) = build_lazy(400.0, 80.0, 10, Some((0.0, 400.0)));
        let (r_huge, _, _) = build_lazy(400.0, 80.0, 10_000, Some((0.0, 400.0)));
        // small is capped by count=10; huge is capped by viewport+overscan.
        assert!(r_huge.len() <= r_small.len().max(12),
            "huge realised ({}) must stay bounded vs viewport, not count",
            r_huge.len());
    }

    #[test]
    fn lazycolumn_publishes_scroll_role_with_key() {
        // Same semantic shape as Node::Scroll — both project to
        // SemanticRole::Scroll with their caller key as `id`.
        let mut state = UiState::new();
        // Need handle != 0 to test publish path? No — we test the
        // build_semantic_tree directly.
        let nodes = vec![
            Node::Rect { color: 0xff00ffff, min: (200.0, 80.0), radius: 0.0 },
            Node::LazyColumn {
                key: "feed".to_string(),
                count: 10, item_h: 80.0, gap: 0.0,
                padding: (0.0, 0.0, 0.0, 0.0),
                viewport_w: Some(200.0), viewport_h: Some(400.0),
                realized: vec![(0, 0)],
                snap: SnapMode::None,
            },
        ];
        let attrs: Vec<NodeAttr> = (0..nodes.len()).map(|_| NodeAttr::default()).collect();
        let sizes = layout::measure(&nodes, 1, (800.0, 600.0), 1.0,
            &layout::estimate_text_width);
        let mut rects: HashMap<NodeId, Rect> = HashMap::new();
        layout::layout(
            &nodes, &attrs, 1,
            Rect { x: 0.0, y: 0.0, w: 800.0, h: 600.0 },
            &sizes, &HashMap::new(), &mut rects,
        );
        let tree = build_semantic_tree(&nodes, 1, &rects, &HashMap::new());
        let root = &tree.nodes[0];
        assert_eq!(root.role, semantics::SemanticRole::Scroll);
        assert_eq!(root.id.as_deref(), Some("feed"));
        let _ = &mut state;
    }

    #[test]
    fn lazycolumn_items_placed_at_index_times_stride() {
        // Item i should sit at y = padding_top + i * (item_h + gap) -
        // scroll_y, within the viewport at (rect.x, rect.y).
        let nodes = vec![
            Node::Rect { color: 0xff00ffff, min: (200.0, 80.0), radius: 0.0 },
            Node::Rect { color: 0xff00ffff, min: (200.0, 80.0), radius: 0.0 },
            Node::LazyColumn {
                key: "feed".to_string(),
                count: 10, item_h: 80.0, gap: 8.0,
                padding: (16.0, 16.0, 16.0, 16.0),
                viewport_w: Some(200.0), viewport_h: Some(400.0),
                realized: vec![(0, 0), (1, 1)],
                snap: SnapMode::None,
            },
        ];
        let attrs: Vec<NodeAttr> = (0..nodes.len()).map(|_| NodeAttr::default()).collect();
        let sizes = layout::measure(&nodes, 2, (800.0, 600.0), 1.0,
            &layout::estimate_text_width);
        let mut rects: HashMap<NodeId, Rect> = HashMap::new();
        layout::layout(
            &nodes, &attrs, 2,
            Rect { x: 0.0, y: 0.0, w: 800.0, h: 600.0 },
            &sizes, &HashMap::new(), &mut rects,
        );
        // Item 0 at padding_top = 16, item 1 at 16 + 80 + 8 = 104.
        assert_eq!(rects[&0].y, 16.0);
        assert_eq!(rects[&1].y, 104.0);
        // Width = viewport_w - padding_h*2 = 200 - 32 = 168.
        assert_eq!(rects[&0].w, 168.0);
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
                axis: ScrollAxis::Vertical,
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
        let tree = build_semantic_tree(&nodes, 1, &rects, &HashMap::new());
        assert!(!tree.nodes.is_empty());
        let root = &tree.nodes[0];
        assert_eq!(root.role, semantics::SemanticRole::Scroll);
        assert_eq!(root.id.as_deref(), Some("feed"));
    }

    /// Stage 3.3 — `Frame::opacity(a)` with `a < 1.0` wraps the
    /// frame's children in a push_opacity / pop_opacity pair. The
    /// `OP_PUSH_OPACITY = 20` / `OP_POP_OPACITY = 21` byte tags are
    /// wire-stable (canvas.rs).
    #[test]
    fn frame_opacity_below_one_emits_push_pop_opacity_pair() {
        const OP_PUSH_OPACITY: u8 = 20;
        const OP_POP_OPACITY:  u8 = 21;
        let mut state = UiState::new();
        let c = ctx("t");
        let mut buf = Vec::new();
        {
            let mut canvas = Canvas::new(&mut buf);
            let mut ui = Ui::new(&mut state, &c, &mut canvas);
            let inner = ui.rect(0xff_22_44_66).size(40.0, 40.0).build();
            let f = ui.frame(&[inner]).opacity(0.5).build();
            ui.present(f);
        }
        assert!(buf.iter().any(|&b| b == OP_PUSH_OPACITY),
            "Frame::opacity(0.5) must emit push_opacity");
        assert!(buf.iter().any(|&b| b == OP_POP_OPACITY),
            "Frame::opacity(0.5) must emit a matching pop_opacity");
    }

    /// Stage 3.3 — `Frame::opacity(1.0)` (the default) is a paint-
    /// stream no-op: no push/pop pair, just the children. Saves two
    /// ops per Frame on the steady-state common path.
    #[test]
    fn frame_opacity_one_emits_no_opacity_pair() {
        const OP_PUSH_OPACITY: u8 = 20;
        const OP_POP_OPACITY:  u8 = 21;
        let mut state = UiState::new();
        let c = ctx("t");
        let mut buf = Vec::new();
        {
            let mut canvas = Canvas::new(&mut buf);
            let mut ui = Ui::new(&mut state, &c, &mut canvas);
            let inner = ui.rect(0xff_22_44_66).size(40.0, 40.0).build();
            let f = ui.frame(&[inner]).build();   // default opacity = 1.0
            ui.present(f);
        }
        assert!(!buf.iter().any(|&b| b == OP_PUSH_OPACITY),
            "Frame at opacity 1.0 must NOT emit push_opacity");
        assert!(!buf.iter().any(|&b| b == OP_POP_OPACITY),
            "Frame at opacity 1.0 must NOT emit pop_opacity");
    }

    /// Stage 3.3 — `opacity_av` reads the AnimatedValue at build
    /// time and threads its value into the Frame's opacity. A
    /// settled animated value at 1.0 takes the no-op fast path; a
    /// mid-flight one (e.g. fade-in still climbing from 0.0) emits
    /// the push/pop pair.
    #[test]
    fn frame_opacity_av_reads_animated_value() {
        use crate::ui::{AnimatedValue, Spring};
        const OP_PUSH_OPACITY: u8 = 20;
        let mut state = UiState::new();
        let c = ctx("t");
        let mut av = AnimatedValue::<f32>::new(0.0);
        av.spring_to(1.0, Spring::MEDIUM);  // not settled yet
        let mut buf = Vec::new();
        {
            let mut canvas = Canvas::new(&mut buf);
            let mut ui = Ui::new(&mut state, &c, &mut canvas);
            let inner = ui.rect(0xff_22_44_66).size(40.0, 40.0).build();
            let f = ui.frame(&[inner]).opacity_av(&av).build();
            ui.present(f);
        }
        assert!(buf.iter().any(|&b| b == OP_PUSH_OPACITY),
            "opacity_av(0.0-driven animation) must emit push_opacity");
    }

    /// Stage 3.3 — overshoot/undershoot values clamp to `[0, 1]`.
    /// A spring that overshoots past 1.0 doesn't break the wire
    /// stream; an undershoot below 0.0 still emits the pair (the
    /// renderer treats 0.0 as fully-transparent — children paint
    /// invisibly but the ops stream stays well-formed).
    #[test]
    fn frame_opacity_clamps_overshoot_to_one_no_op_path() {
        const OP_PUSH_OPACITY: u8 = 20;
        let mut state = UiState::new();
        let c = ctx("t");
        let mut buf = Vec::new();
        {
            let mut canvas = Canvas::new(&mut buf);
            let mut ui = Ui::new(&mut state, &c, &mut canvas);
            let inner = ui.rect(0xff_aa_bb_cc).size(20.0, 20.0).build();
            // 1.5 clamps to 1.0 → no-op fast path kicks in → push_opacity NOT emitted.
            let f = ui.frame(&[inner]).opacity(1.5).build();
            ui.present(f);
        }
        assert!(!buf.iter().any(|&b| b == OP_PUSH_OPACITY),
            "opacity(1.5) clamps to 1.0 and must take the no-op path");
    }

    #[test]
    fn frame_opacity_undershoot_clamps_to_zero_and_emits_pair() {
        const OP_PUSH_OPACITY: u8 = 20;
        const OP_POP_OPACITY:  u8 = 21;
        let mut state = UiState::new();
        let c = ctx("t");
        let mut buf = Vec::new();
        {
            let mut canvas = Canvas::new(&mut buf);
            let mut ui = Ui::new(&mut state, &c, &mut canvas);
            let inner = ui.rect(0xff_aa_bb_cc).size(20.0, 20.0).build();
            // -0.5 clamps to 0.0 → push/pop pair emitted (fully-
            // transparent children, but the stream is well-formed).
            let f = ui.frame(&[inner]).opacity(-0.5).build();
            ui.present(f);
        }
        assert!(buf.iter().any(|&b| b == OP_PUSH_OPACITY),
            "opacity(-0.5) clamps to 0.0 and must emit push_opacity");
        assert!(buf.iter().any(|&b| b == OP_POP_OPACITY),
            "opacity(-0.5) must emit matching pop_opacity");
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
                axis: ScrollAxis::Vertical,
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

    // ── Stage 2d.1: ScrollAxis routing ────────────────────────────────

    #[test]
    fn scroll_axis_default_is_vertical() {
        // ScrollCell + ScrollAxis defaults must round-trip to
        // Vertical so every pre-2d call site behaves identically.
        let c = ScrollCell::default();
        assert_eq!(c.axis, ScrollAxis::Vertical);
        let a = ScrollAxis::default();
        assert_eq!(a, ScrollAxis::Vertical);
    }

    #[test]
    fn horizontal_wheel_advances_horizontal_scroll() {
        // A horizontal cell consumes wheel `dx` and ignores `dy`.
        // Cross-axis components must drop on the floor — no axis
        // stealing (plan §3.6).
        let mut state = UiState::new();
        seed_scroll_cell_axis(
            &mut state, "row",
            Rect { x: 0.0, y: 0.0, w: 400.0, h: 200.0 },
            1000.0, 0.0, ScrollAxis::Horizontal,
        );
        state.feed_pointer(pointer(200.0, 100.0, PointerPhase::Move));
        // dx=120, dy=300 — only dx applies.
        state.feed_wheel(wheel_xy(120.0, 300.0));
        assert_eq!(state.scroll_states["row"].scroll_y, 120.0);
        // Reverse: dy alone on a horizontal cell is a no-op.
        state.feed_wheel(wheel_xy(0.0, 500.0));
        assert_eq!(state.scroll_states["row"].scroll_y, 120.0);
    }

    #[test]
    fn vertical_wheel_ignores_dx() {
        // Mirror of the above — a vertical cell consumes only dy.
        let mut state = UiState::new();
        seed_scroll_cell(
            &mut state, "col",
            Rect { x: 0.0, y: 0.0, w: 200.0, h: 400.0 },
            1000.0, 0.0,
        );
        state.feed_pointer(pointer(100.0, 200.0, PointerPhase::Move));
        state.feed_wheel(wheel_xy(400.0, 50.0));
        // Only the 50 dy lands.
        assert_eq!(state.scroll_states["col"].scroll_y, 50.0);
    }

    /// Regression — pre-fix, a wheel.y inside a nested horizontal
    /// scroll region was silently dropped because `scroll_region_at`
    /// picked the smallest-area region (the horizontal child) and
    /// `feed_wheel` projected ev to `cell.axis`, dropping dy. The
    /// fix routes each wheel component to the deepest scroll region
    /// of its own axis — so wheel.y over an inner horizontal pager
    /// bubbles up to the enclosing vertical scroll.
    #[test]
    fn vertical_wheel_over_nested_horizontal_scrolls_outer_vertical() {
        let mut state = UiState::new();
        // Outer feed: 800-dp-tall vertical scroll covering the
        // whole viewport.
        seed_scroll_cell(
            &mut state, "feed",
            Rect { x: 0.0, y: 0.0, w: 1280.0, h: 800.0 },
            5000.0, 0.0,
        );
        // Inner photo-pager: smaller-area horizontal scroll nested
        // inside the feed. Hit-test point (640, 400) sits inside
        // both — the pager's smaller area would have won under the
        // old behaviour.
        seed_scroll_cell_axis(
            &mut state, "pager",
            Rect { x: 440.0, y: 200.0, w: 400.0, h: 400.0 },
            2000.0, 0.0, ScrollAxis::Horizontal,
        );
        state.feed_pointer(pointer(640.0, 400.0, PointerPhase::Move));
        // Pure-dy wheel — must scroll the OUTER feed even though the
        // cursor sits inside the inner horizontal pager.
        state.feed_wheel(wheel_xy(0.0, 250.0));
        assert_eq!(state.scroll_states["feed"].scroll_y, 250.0,
            "wheel.y over a nested horizontal scroll must bubble up to the vertical parent");
        assert_eq!(state.scroll_states["pager"].scroll_y, 0.0,
            "the horizontal pager must NOT receive the dy delta");
    }

    /// Companion to the above — wheel.x over the same nested layout
    /// routes to the inner horizontal pager, not the outer vertical
    /// feed (the deepest horizontal scroll wins for the x axis).
    #[test]
    fn horizontal_wheel_over_nested_horizontal_scrolls_inner_horizontal() {
        let mut state = UiState::new();
        seed_scroll_cell(
            &mut state, "feed",
            Rect { x: 0.0, y: 0.0, w: 1280.0, h: 800.0 },
            5000.0, 0.0,
        );
        seed_scroll_cell_axis(
            &mut state, "pager",
            Rect { x: 440.0, y: 200.0, w: 400.0, h: 400.0 },
            2000.0, 0.0, ScrollAxis::Horizontal,
        );
        state.feed_pointer(pointer(640.0, 400.0, PointerPhase::Move));
        state.feed_wheel(wheel_xy(180.0, 0.0));
        assert_eq!(state.scroll_states["pager"].scroll_y, 180.0,
            "wheel.x routes to the deepest horizontal scroll");
        assert_eq!(state.scroll_states["feed"].scroll_y, 0.0,
            "the outer vertical feed must NOT receive the dx delta");
    }

    /// Diagonal trackpad swipe — `dx` and `dy` route independently
    /// to the right scroll on each axis, both axes advance in the
    /// same `feed_wheel` call.
    #[test]
    fn diagonal_wheel_routes_per_axis_independently() {
        let mut state = UiState::new();
        seed_scroll_cell(
            &mut state, "feed",
            Rect { x: 0.0, y: 0.0, w: 1280.0, h: 800.0 },
            5000.0, 0.0,
        );
        seed_scroll_cell_axis(
            &mut state, "pager",
            Rect { x: 440.0, y: 200.0, w: 400.0, h: 400.0 },
            2000.0, 0.0, ScrollAxis::Horizontal,
        );
        state.feed_pointer(pointer(640.0, 400.0, PointerPhase::Move));
        state.feed_wheel(wheel_xy(120.0, 75.0));
        assert_eq!(state.scroll_states["pager"].scroll_y, 120.0,
            "diagonal wheel: dx component goes to the pager");
        assert_eq!(state.scroll_states["feed"].scroll_y, 75.0,
            "diagonal wheel: dy component goes to the feed");
    }

    #[test]
    fn horizontal_drag_past_slop_advances_scroll_x() {
        // Pointer-drag along x inside a horizontal scroll updates
        // scroll_y (the "main-axis offset" field). Slop is measured
        // along the main axis only.
        let mut state = UiState::new();
        seed_scroll_cell_axis(
            &mut state, "row",
            Rect { x: 0.0, y: 0.0, w: 400.0, h: 200.0 },
            1000.0, 0.0, ScrollAxis::Horizontal,
        );
        // Down inside the row.
        state.feed_pointer(pointer(200.0, 100.0, PointerPhase::Down));
        // Drag 100 px left — well past slop. Three Moves so the
        // total delta lands.
        state.feed_pointer(pointer(170.0, 100.0, PointerPhase::Move));
        state.feed_pointer(pointer(140.0, 100.0, PointerPhase::Move));
        state.feed_pointer(pointer(100.0, 100.0, PointerPhase::Move));
        // last_main reset doesn't matter — final scroll_y is the
        // accumulated delta = 200 - 100 = 100.
        assert!(state.scroll_states["row"].scroll_y >= 90.0);
        state.feed_pointer(pointer(100.0, 100.0, PointerPhase::Up));
        // No tap got promoted (the gesture crossed slop).
        assert!(state.pending_clicks.is_empty());
    }

    #[test]
    fn cross_axis_drag_inside_horizontal_stays_tap() {
        // A 20px y-drag inside a horizontal scroll never crosses
        // main-axis slop, so it resolves as a tap at Up. (Sideways
        // wobble in a horizontal carousel must not eat clicks on
        // buttons under the finger.)
        let mut state = UiState::new();
        seed_scroll_cell_axis(
            &mut state, "row",
            Rect { x: 0.0, y: 0.0, w: 400.0, h: 200.0 },
            1000.0, 0.0, ScrollAxis::Horizontal,
        );
        state.feed_pointer(pointer(200.0, 100.0, PointerPhase::Down));
        // Drag 20 px down — well past slop on the *cross* axis,
        // zero main-axis component.
        state.feed_pointer(pointer(200.0, 120.0, PointerPhase::Move));
        state.feed_pointer(pointer(200.0, 130.0, PointerPhase::Move));
        state.feed_pointer(pointer(200.0, 120.0, PointerPhase::Up));
        // scroll_y unchanged (no main-axis movement) and the Up
        // promoted the gesture to a tap.
        assert_eq!(state.scroll_states["row"].scroll_y, 0.0);
        assert_eq!(state.pending_clicks.len(), 1);
    }

    #[test]
    fn horizontal_drag_seeds_horizontal_velocity_for_fling() {
        // A fast left-drag inside a horizontal scroll feeds the
        // velocity EMA so the 2c.4 fling animator picks it up at
        // Up time. The sign convention matches vertical: positive
        // delta = forward (scroll_y increases).
        let mut state = UiState::new();
        seed_scroll_cell_axis(
            &mut state, "row",
            Rect { x: 0.0, y: 0.0, w: 400.0, h: 200.0 },
            1000.0, 0.0, ScrollAxis::Horizontal,
        );
        state.feed_pointer(pointer(300.0, 100.0, PointerPhase::Down));
        // Fast swipe left: 4 Moves at 50 px each.
        for x in [250.0, 200.0, 150.0, 100.0] {
            state.feed_pointer(pointer(x, 100.0, PointerPhase::Move));
        }
        let v = state.scroll_states["row"].velocity;
        assert!(v > 30.0, "expected positive forward velocity, got {v}");
        state.feed_pointer(pointer(100.0, 100.0, PointerPhase::Up));
        // Velocity preserved at Up so tick_animations can fling.
        assert!(state.scroll_states["row"].velocity > 30.0);
    }

    #[test]
    fn horizontal_scroll_clamps_at_content_end() {
        // A horizontal cell with content_h=1000 viewport_h=400 has
        // max=600; trying to scroll past it pins to 600.
        let mut state = UiState::new();
        seed_scroll_cell_axis(
            &mut state, "row",
            Rect { x: 0.0, y: 0.0, w: 400.0, h: 200.0 },
            1000.0, 0.0, ScrollAxis::Horizontal,
        );
        state.feed_pointer(pointer(200.0, 100.0, PointerPhase::Move));
        state.feed_wheel(wheel_xy(2000.0, 0.0));
        assert_eq!(state.scroll_states["row"].scroll_y, 600.0);
        // Negative dx at scroll=0 pins to 0.
        state.scroll_states.get_mut("row").unwrap().scroll_y = 0.0;
        state.feed_wheel(wheel_xy(-2000.0, 0.0));
        assert_eq!(state.scroll_states["row"].scroll_y, 0.0);
    }

    #[test]
    fn ui_scroll_h_builds_horizontal_node() {
        // The Ui::scroll_h factory must mark the produced Node and
        // the persisted ScrollCell as horizontal. We check the cell
        // via has_hit_rect-style inspection (after present) — the
        // cell exists with axis=Horizontal.
        let mut state = UiState::new();
        let c = ctx("t");
        let mut buf = Vec::new();
        let mut canvas = Canvas::new(&mut buf);
        let mut ui = Ui::new(&mut state, &c, &mut canvas);
        let r = ui.rect(0xff112233).size(2000.0, 200.0).build();
        let s = ui.scroll_h(r).key("strip").size(400.0, 200.0).build();
        ui.present(s);
        // Cell registered, axis=Horizontal, content_h carries the
        // child's *width* (the lying field name — see ScrollCell
        // docstring). content=2000, viewport=400 → max=1600.
        let cell = state.scroll_states.get("strip").expect("cell present");
        assert_eq!(cell.axis, ScrollAxis::Horizontal);
        assert_eq!(cell.content_h, 2000.0);
        assert_eq!(cell.viewport_h, 400.0);
        assert_eq!(cell.viewport_w, 200.0);
    }

    #[test]
    fn horizontal_scroll_paints_child_with_negative_x_offset() {
        // After scroll_to(N), a horizontal Scroll places its child
        // at x = viewport.x - N. The vertical twin of this test
        // (`scrolled_offset_shifts_child_rect_upward`) covers the
        // y-axis case.
        let nodes = vec![
            Node::Rect { color: 0xff00ffff, min: (2000.0, 200.0), radius: 0.0 },
            Node::Scroll {
                child: 0, key: "strip".to_string(),
                viewport_w: Some(400.0), viewport_h: Some(200.0),
                axis: ScrollAxis::Horizontal,
            },
        ];
        let attrs: Vec<NodeAttr> = (0..nodes.len()).map(|_| NodeAttr::default()).collect();
        let sizes = layout::measure(&nodes, 1, (800.0, 600.0), 1.0,
            &layout::estimate_text_width);
        let mut scroll_offsets: HashMap<String, f32> = HashMap::new();
        scroll_offsets.insert("strip".to_string(), 250.0);
        let mut rects: HashMap<NodeId, Rect> = HashMap::new();
        layout::layout(
            &nodes, &attrs, 1,
            Rect { x: 0.0, y: 0.0, w: 800.0, h: 600.0 },
            &sizes, &scroll_offsets, &mut rects,
        );
        // Scroll viewport at x=0; child rect at x = 0 - 250.
        assert_eq!(rects[&1].x, 0.0);
        assert_eq!(rects[&0].x, -250.0);
        // Child width is the content extent (2000), not the viewport.
        assert_eq!(rects[&0].w, 2000.0);
        // y unchanged — horizontal scroll doesn't shift y.
        assert_eq!(rects[&0].y, 0.0);
    }

    // ── Stage 2d.2: Node::LazyRow ─────────────────────────────────────

    /// Mirror of `build_lazy` for horizontal lists. Captures the
    /// indices the closure ran on so virtualisation tests can assert
    /// the realised range.
    fn build_lazy_row(
        viewport_w: f32, item_w: f32, count: u32,
        seeded_cell: Option<(f32, f32)>,  // (scroll, cached_viewport_main)
    ) -> (Vec<u32>, NodeId, UiState) {
        use std::cell::RefCell;
        let mut state = UiState::new();
        if let Some((scroll, cached_vmain)) = seeded_cell {
            state.scroll_states.insert("row".to_string(), ScrollCell {
                axis: ScrollAxis::Horizontal,
                scroll_y: scroll,
                content_h: count as f32 * item_w,
                // Cross-axis size (viewport height for horizontal).
                viewport_w: 200.0,
                // Main-axis size (viewport width for horizontal).
                viewport_h: cached_vmain,
                last_rect: Rect {
                    x: 0.0, y: 0.0,
                    w: cached_vmain, h: 200.0,
                },
                ..Default::default()
            });
        }
        let c = ctx("t");
        let mut buf = Vec::new();
        let mut canvas = Canvas::new(&mut buf);
        let mut ui = Ui::new(&mut state, &c, &mut canvas);
        let realized: RefCell<Vec<u32>> = RefCell::new(Vec::new());
        let lr = ui.lazy_row(count)
            .key("row")
            .item_w(item_w)
            .size(viewport_w, 200.0)
            .items(|ui, i| {
                realized.borrow_mut().push(i);
                ui.rect(0xff00ffff).size(item_w, 200.0).build()
            });
        ui.present(lr);
        (realized.into_inner(), lr, state)
    }

    #[test]
    fn lazyrow_cold_start_realises_up_to_cap() {
        // item_w=0 forces cold start (stride=0). Capped at 30 items.
        let (realized, _, _) = build_lazy_row(400.0, 0.0, 80, None);
        assert_eq!(realized.len(), node::LAZY_COLD_START_CAP as usize);
        assert_eq!(realized.first(), Some(&0));
    }

    #[test]
    fn lazyrow_realises_only_visible_plus_overscan() {
        // viewport_w=400, item_w=80, count=80, scroll=0:
        // visible 0..=4 (5 items, stride=80) + overscan(2) → 0..=6 = 7.
        let (realized, _, _) = build_lazy_row(400.0, 80.0, 80, Some((0.0, 400.0)));
        assert!(realized.len() <= 10,
            "expected ≤ 10 realised items, got {} ({realized:?})", realized.len());
        assert_eq!(realized.first(), Some(&0));
    }

    #[test]
    fn lazyrow_advances_visible_range_when_scrolled() {
        // scroll = 400 (5 items past the leading edge) → first ≈ 5.
        let (realized, _, _) = build_lazy_row(400.0, 80.0, 80, Some((400.0, 400.0)));
        assert!(*realized.first().unwrap() >= 3,
            "first realised must advance with scroll, got {}",
            realized.first().unwrap());
        assert!(*realized.last().unwrap() < 80,
            "last must stay inside count, got {}",
            realized.last().unwrap());
    }

    #[test]
    fn lazyrow_content_extent_uses_count_times_item_w_plus_gap_padding() {
        // 12 items × 200 dp + 11 × 16 gap + 32 + 32 padding =
        //   2400 + 176 + 64 = 2640. content_h field is the main-axis
        // extent (width for horizontal).
        let mut state = UiState::new();
        let c = ctx("t");
        let mut buf = Vec::new();
        let mut canvas = Canvas::new(&mut buf);
        let mut ui = Ui::new(&mut state, &c, &mut canvas);
        let lr = ui.lazy_row(12)
            .key("strip").item_w(200.0).gap(16.0)
            .padding(32.0).size(400.0, 200.0)
            .items(|ui, _i| ui.rect(0xff00ffff).build());
        ui.present(lr);
        let cell = state.scroll_states.get("strip").expect("cell present");
        assert_eq!(cell.axis, ScrollAxis::Horizontal);
        assert_eq!(cell.content_h, 12.0 * 200.0 + 11.0 * 16.0 + 64.0);
        assert_eq!(cell.viewport_h, 400.0);
    }

    #[test]
    fn lazyrow_empty_count_realises_nothing() {
        let (realized, _, state) = build_lazy_row(400.0, 80.0, 0, Some((0.0, 400.0)));
        assert!(realized.is_empty());
        assert_eq!(state.scroll_states["row"].content_h, 0.0);
    }

    #[test]
    fn lazyrow_realises_independent_of_count() {
        // Same realised-count bounded check as LazyColumn — a 10k
        // row list shouldn't pay more than a 10-row one at scroll=0.
        let (r_small, _, _) = build_lazy_row(400.0, 80.0, 10, Some((0.0, 400.0)));
        let (r_huge, _, _) = build_lazy_row(400.0, 80.0, 10_000, Some((0.0, 400.0)));
        assert!(r_huge.len() <= r_small.len().max(12),
            "huge realised ({}) must stay bounded vs viewport", r_huge.len());
    }

    #[test]
    fn lazyrow_places_realised_items_at_index_times_stride_x() {
        // Pure layout test: realised items at item_w=200, gap=16 land
        // at x = padding.3 + idx*(216) − scroll.
        let nodes = vec![
            // 2 realised items (indices 0 and 1).
            Node::Rect { color: 0xff00ffff, min: (200.0, 200.0), radius: 0.0 },
            Node::Rect { color: 0xffff00ff, min: (200.0, 200.0), radius: 0.0 },
            Node::LazyRow {
                key: "strip".to_string(),
                count: 12, item_w: 200.0, gap: 16.0,
                padding: (0.0, 0.0, 0.0, 32.0),
                viewport_w: Some(400.0), viewport_h: Some(200.0),
                realized: vec![(0, 0), (1, 1)],
                snap: SnapMode::None,
            },
        ];
        let attrs: Vec<NodeAttr> = (0..nodes.len()).map(|_| NodeAttr::default()).collect();
        let sizes = layout::measure(&nodes, 2, (800.0, 600.0), 1.0,
            &layout::estimate_text_width);
        let mut rects: HashMap<NodeId, Rect> = HashMap::new();
        layout::layout(
            &nodes, &attrs, 2,
            Rect { x: 0.0, y: 0.0, w: 800.0, h: 600.0 },
            &sizes, &HashMap::new(), &mut rects,
        );
        // Item 0 at padding.3=32, item 1 at 32 + 200 + 16 = 248.
        assert_eq!(rects[&0].x, 32.0);
        assert_eq!(rects[&1].x, 248.0);
        // y at viewport.y=0, width = item_w=200.
        assert_eq!(rects[&0].y, 0.0);
        assert_eq!(rects[&0].w, 200.0);
    }

    #[test]
    fn lazyrow_publishes_scroll_role_with_horizontal_axis() {
        // The semantic projection emits SemanticRole::Scroll for
        // LazyRow with the caller key as id. The wire `axis` byte
        // lands in Stage 2d.4 (Inspector v3); this test guards the
        // role + key right now.
        let nodes = vec![
            Node::Rect { color: 0xff00ffff, min: (200.0, 200.0), radius: 0.0 },
            Node::LazyRow {
                key: "strip".to_string(),
                count: 1, item_w: 200.0, gap: 0.0,
                padding: (0.0, 0.0, 0.0, 0.0),
                viewport_w: Some(400.0), viewport_h: Some(200.0),
                realized: vec![(0, 0)],
                snap: SnapMode::None,
            },
        ];
        let attrs: Vec<NodeAttr> = (0..nodes.len()).map(|_| NodeAttr::default()).collect();
        let sizes = layout::measure(&nodes, 1, (800.0, 600.0), 1.0,
            &layout::estimate_text_width);
        let mut rects: HashMap<NodeId, Rect> = HashMap::new();
        layout::layout(
            &nodes, &attrs, 1,
            Rect { x: 0.0, y: 0.0, w: 800.0, h: 600.0 },
            &sizes, &HashMap::new(), &mut rects,
        );
        let tree = build_semantic_tree(&nodes, 1, &rects, &HashMap::new());
        let root = &tree.nodes[0];
        assert_eq!(root.role, semantics::SemanticRole::Scroll);
        assert_eq!(root.id.as_deref(), Some("strip"));
        // ScrollSemantic populated for LazyRow with item_count.
        let s = root.scroll.as_ref().expect("scroll meta present");
        assert_eq!(s.item_count, Some(1));
    }

    #[test]
    fn realise_window_zero_count_returns_empty() {
        let (s, e) = node::realise_window(0, 100.0, 400.0, 0.0, 0.0, 2);
        assert_eq!((s, e), (0, 0));
    }

    #[test]
    fn realise_window_cold_start_caps_at_constant() {
        // stride=0 forces cold-start; capped at LAZY_COLD_START_CAP.
        let (s, e) = node::realise_window(80, 0.0, 400.0, 0.0, 0.0, 2);
        assert_eq!(s, 0);
        assert_eq!(e, node::LAZY_COLD_START_CAP);
    }

    #[test]
    fn realise_window_scrolled_advances_first_index() {
        // stride 100, viewport 400, scroll 250 → first ≈ 2,
        // last ≈ 6. + overscan 2 → [0, 9).
        let (s, e) = node::realise_window(80, 100.0, 400.0, 250.0, 0.0, 2);
        assert_eq!(s, 0);
        assert_eq!(e, 9);
    }

    // ── Stage 2d.3: SnapMode + cubic-out snap animation ──────────────

    fn snap_cell(
        snap_mode: SnapMode, item_stride: f32, viewport_main: f32,
        content: f32, scroll: f32,
    ) -> ScrollCell {
        ScrollCell {
            axis: ScrollAxis::Horizontal,
            scroll_y: scroll,
            content_h: content,
            viewport_w: 200.0,
            viewport_h: viewport_main,
            snap_mode, item_stride,
            ..Default::default()
        }
    }

    #[test]
    fn cubic_out_is_monotonic_zero_to_one() {
        assert!((cubic_out(0.0) - 0.0).abs() < 1e-6);
        assert!((cubic_out(1.0) - 1.0).abs() < 1e-6);
        // Decelerating: 0.5 should land well past 0.5 on the curve.
        let half = cubic_out(0.5);
        assert!(half > 0.7, "cubic_out(0.5) = {half}, expected > 0.7");
        // Strictly monotonic over 10 samples.
        let mut last = -1.0;
        for i in 0..=10 {
            let v = cubic_out(i as f32 / 10.0);
            assert!(v > last, "cubic_out not monotonic at {i}");
            last = v;
        }
    }

    #[test]
    fn cubic_out_clamps_outside_unit_interval() {
        assert_eq!(cubic_out(-1.0), 0.0);
        assert_eq!(cubic_out(2.0), 1.0);
    }

    #[test]
    fn snap_target_item_rounds_to_nearest_stride() {
        // stride=100, scroll=180 → nearest is 200.
        let cell = snap_cell(SnapMode::Item, 100.0, 400.0, 2000.0, 180.0);
        let t = compute_snap_target(&cell).expect("Item snap target");
        assert_eq!(t, 200.0);
        // scroll=140 (under halfway) → 100.
        let cell = snap_cell(SnapMode::Item, 100.0, 400.0, 2000.0, 140.0);
        let t = compute_snap_target(&cell).expect("Item snap target");
        assert_eq!(t, 100.0);
    }

    #[test]
    fn snap_target_page_rounds_to_viewport_size() {
        // viewport=400, scroll=620 → nearest page is 800 (round half-up).
        let cell = snap_cell(SnapMode::Page, 0.0, 400.0, 4000.0, 620.0);
        let t = compute_snap_target(&cell).expect("Page snap target");
        assert_eq!(t, 800.0);
        // scroll=180 → 0.
        let cell = snap_cell(SnapMode::Page, 0.0, 400.0, 4000.0, 180.0);
        let t = compute_snap_target(&cell).expect("Page snap target");
        assert_eq!(t, 0.0);
    }

    #[test]
    fn snap_target_clamps_to_content_end() {
        // viewport=400, content=1000 → max = 600. Rounded 800 clamps to 600.
        let cell = snap_cell(SnapMode::Page, 0.0, 400.0, 1000.0, 620.0);
        let t = compute_snap_target(&cell).expect("Page snap target");
        assert_eq!(t, 600.0);
    }

    #[test]
    fn snap_target_none_returns_none() {
        let cell = snap_cell(SnapMode::None, 100.0, 400.0, 2000.0, 180.0);
        assert!(compute_snap_target(&cell).is_none());
    }

    #[test]
    fn snap_target_degenerate_stride_returns_none() {
        // SnapMode::Item with stride=0 must not divide by zero.
        let cell = snap_cell(SnapMode::Item, 0.0, 400.0, 2000.0, 180.0);
        assert!(compute_snap_target(&cell).is_none());
    }

    #[test]
    fn fling_settle_triggers_snap_when_mode_is_item() {
        // Velocity decays to 0, then snap kicks in to glide toward
        // the nearest item edge. After enough ticks, scroll_y
        // lands at the target.
        let mut state = UiState::new();
        state.scroll_states.insert("row".to_string(), ScrollCell {
            axis: ScrollAxis::Horizontal,
            scroll_y: 180.0, velocity: 0.5,
            content_h: 2000.0,
            viewport_w: 200.0, viewport_h: 400.0,
            snap_mode: SnapMode::Item, item_stride: 100.0,
            ..Default::default()
        });
        state.tick_animations(FRAME_MS);
        assert_eq!(state.scroll_states["row"].velocity, 0.0);
        assert!(state.scroll_states["row"].snap_target.is_some());
        let target = state.scroll_states["row"].snap_target.unwrap();
        assert_eq!(target, 200.0);
        for _ in 0..20 { state.tick_animations(FRAME_MS); }
        assert_eq!(state.scroll_states["row"].scroll_y, 200.0);
        assert!(state.scroll_states["row"].snap_target.is_none());
    }

    #[test]
    fn snap_does_not_fire_when_already_at_target() {
        let mut state = UiState::new();
        // scroll_y starts exactly at a grid line (200 = 2*100).
        // The 0.5 px/frame velocity decays to 0 within one tick
        // and advances scroll_y by < 0.5 px — within SNAP_THRESHOLD
        // of the target, so no snap kicks off.
        state.scroll_states.insert("row".to_string(), ScrollCell {
            axis: ScrollAxis::Horizontal,
            scroll_y: 200.0, velocity: 0.5,
            content_h: 2000.0,
            viewport_w: 200.0, viewport_h: 400.0,
            snap_mode: SnapMode::Item, item_stride: 100.0,
            ..Default::default()
        });
        state.tick_animations(FRAME_MS);
        assert_eq!(state.scroll_states["row"].velocity, 0.0);
        assert!(state.scroll_states["row"].snap_target.is_none(),
            "snap should not fire when scroll_y is already at the grid");
        // scroll_y drifted by the residual velocity (~0.46 px) but
        // stayed within SNAP_THRESHOLD of the grid line.
        let y = state.scroll_states["row"].scroll_y;
        assert!((y - 200.0).abs() < SNAP_THRESHOLD,
            "scroll_y={y} should stay within SNAP_THRESHOLD of 200");
    }

    #[test]
    fn drag_up_seeds_snap_pending_with_snap_mode_set() {
        let mut state = UiState::new();
        state.scroll_states.insert("row".to_string(), ScrollCell {
            axis: ScrollAxis::Horizontal,
            scroll_y: 0.0, velocity: 0.0,
            content_h: 2000.0,
            viewport_w: 200.0, viewport_h: 400.0,
            last_rect: Rect { x: 0.0, y: 0.0, w: 400.0, h: 200.0 },
            snap_mode: SnapMode::Page,
            ..Default::default()
        });
        state.feed_pointer(pointer(200.0, 100.0, PointerPhase::Down));
        for x in [180.0, 130.0, 80.0, 30.0] {
            state.feed_pointer(pointer(x, 100.0, PointerPhase::Move));
        }
        state.feed_pointer(pointer(30.0, 100.0, PointerPhase::Up));
        assert!(state.scroll_states["row"].snap_pending,
            "snap_pending must be set on Up in a snap-enabled cell");
        // Tick until both fling and snap finish (or sanity bail
        // after a generous frame budget). 4 s of simulated time
        // is far more than either phase can need.
        for _ in 0..240 {
            state.tick_animations(FRAME_MS);
            if !state.has_active_animation() { break; }
        }
        assert!(!state.has_active_animation(),
            "animations should finish within budget");
        let final_y = state.scroll_states["row"].scroll_y;
        // Final scroll_y must be at a page edge (0 / 400 / 800
        // / ...). Tolerance: nothing — snap completion sets
        // scroll_y = target exactly.
        let page = 400.0;
        let n = (final_y / page).round();
        assert!((final_y - n * page).abs() < 0.001,
            "snap should land on a page edge, got {final_y}");
        assert!(!state.scroll_states["row"].snap_pending);
    }

    #[test]
    fn pointer_down_cancels_active_snap() {
        let mut state = UiState::new();
        state.scroll_states.insert("row".to_string(), ScrollCell {
            axis: ScrollAxis::Horizontal,
            scroll_y: 100.0, velocity: 0.0,
            content_h: 2000.0,
            viewport_w: 200.0, viewport_h: 400.0,
            last_rect: Rect { x: 0.0, y: 0.0, w: 400.0, h: 200.0 },
            snap_mode: SnapMode::Page,
            snap_target: Some(0.0), snap_origin: 100.0, snap_elapsed_ms: 50.0,
            ..Default::default()
        });
        state.feed_pointer(pointer(200.0, 100.0, PointerPhase::Down));
        let cell = &state.scroll_states["row"];
        assert!(cell.snap_target.is_none());
        assert!(!cell.snap_pending);
    }

    #[test]
    fn wheel_cancels_active_snap() {
        let mut state = UiState::new();
        state.scroll_states.insert("row".to_string(), ScrollCell {
            axis: ScrollAxis::Horizontal,
            scroll_y: 100.0, velocity: 0.0,
            content_h: 2000.0,
            viewport_w: 200.0, viewport_h: 400.0,
            last_rect: Rect { x: 0.0, y: 0.0, w: 400.0, h: 200.0 },
            snap_mode: SnapMode::Page,
            snap_target: Some(0.0), snap_origin: 100.0, snap_elapsed_ms: 50.0,
            ..Default::default()
        });
        state.feed_pointer(pointer(200.0, 100.0, PointerPhase::Move));
        state.feed_wheel(wheel_xy(50.0, 0.0));
        assert!(state.scroll_states["row"].snap_target.is_none());
    }

    #[test]
    fn scroll_to_cancels_active_snap() {
        let mut state = UiState::new();
        state.scroll_states.insert("row".to_string(), ScrollCell {
            axis: ScrollAxis::Horizontal,
            scroll_y: 100.0, velocity: 0.0,
            content_h: 2000.0,
            viewport_w: 200.0, viewport_h: 400.0,
            snap_mode: SnapMode::Page,
            snap_target: Some(0.0), snap_origin: 100.0, snap_elapsed_ms: 50.0,
            ..Default::default()
        });
        let c = ctx("t");
        let mut buf = Vec::new();
        let mut canvas = Canvas::new(&mut buf);
        let mut ui = Ui::new(&mut state, &c, &mut canvas);
        ui.scroll_to("row", 50.0);
        assert_eq!(state.scroll_states["row"].scroll_y, 50.0);
        assert!(state.scroll_states["row"].snap_target.is_none());
    }

    #[test]
    fn has_active_animation_includes_snap() {
        let mut state = UiState::new();
        state.scroll_states.insert("row".to_string(), ScrollCell {
            snap_target: Some(400.0), snap_origin: 100.0, snap_elapsed_ms: 50.0,
            ..Default::default()
        });
        assert!(state.has_active_animation(),
            "Active snap counts as an active animation");
        state.scroll_states.get_mut("row").unwrap().snap_target = None;
        assert!(!state.has_active_animation());
    }

    #[test]
    fn lazy_row_snap_mode_threads_into_cell_via_present() {
        // `.snap(SnapMode::Page)` on the builder must land on the
        // cell after present so tick_animations sees it.
        let mut state = UiState::new();
        let c = ctx("t");
        let mut buf = Vec::new();
        let mut canvas = Canvas::new(&mut buf);
        let mut ui = Ui::new(&mut state, &c, &mut canvas);
        let lr = ui.lazy_row(12)
            .key("carousel").item_w(360.0).gap(16.0)
            .size(400.0, 200.0)
            .snap(SnapMode::Page)
            .items(|ui, _i| ui.rect(0xff00ffff).build());
        ui.present(lr);
        let cell = state.scroll_states.get("carousel").expect("cell present");
        assert_eq!(cell.snap_mode, SnapMode::Page);
        assert_eq!(cell.item_stride, 360.0 + 16.0);
    }
}
