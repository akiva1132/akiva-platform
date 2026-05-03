//! Stone 13.2 — System dialog overlays.
//!
//! When an app calls `ctx.show_dialog(...)`, the SDK fires
//! `IO::SHOW_DIALOG` and parks the future. The host routes the request
//! into [`DialogManager::open`] here, which:
//!
//! 1. Allocates a `WindowLayer::Overlay` surface owned by the calling
//!    activity (so destroying the activity also tears down any open
//!    dialog automatically — see
//!    [`Framework::destroy_surfaces_for_owner`]).
//! 2. Focuses that surface so [`Framework::dispatch_pointer`] hit-tests
//!    the dialog instead of forwarding pointer events to the activity.
//! 3. Records the title / body / button labels + `future_id` so the
//!    framework can paint the dialog every frame and resolve the
//!    future when the user taps.
//!
//! The host (kernel) drains [`DialogManager::take_resolutions`] once
//! per frame and posts `host_op_done` for each resolved dialog,
//! unblocking the app worker's `DialogFuture`.
//!
//! # Wire format mirror
//!
//! `kind` byte in the resolution payload:
//! - `0` — `DialogResult::Button(btn_idx)` (mirror of `dialog::KIND_BUTTON`)
//! - `1` — `DialogResult::Dismissed`        (mirror of `dialog::KIND_DISMISSED`)

use crate::activity_manager::ActivityId;
use crate::compositor::PaintOp;
use crate::window_manager::{Rect, SurfaceId};

/// Mirror of `akiva_sdk::dialog::KIND_BUTTON`.
pub const KIND_BUTTON: u8 = 0;
/// Mirror of `akiva_sdk::dialog::KIND_DISMISSED`.
pub const KIND_DISMISSED: u8 = 1;

/// One open dialog. Lives in `DialogManager.dialogs` from the moment
/// `open` succeeds until the user taps. On resolution the entry moves
/// to `resolutions` for the host to drain.
#[derive(Debug, Clone)]
pub struct DialogState {
    pub handle: u32,
    pub future_id: u64,
    pub owner: ActivityId,
    pub surface: SurfaceId,
    pub title: String,
    pub body: String,
    pub buttons: Vec<String>,
    /// Last-laid-out dialog box rect in display-pixel space. Refreshed
    /// every render pass; pointer hit-testing reads it back.
    pub box_rect: Rect,
    /// Last-laid-out per-button rects (same length as `buttons`). Empty
    /// before the first render.
    pub button_rects: Vec<Rect>,
}

/// One resolved dialog awaiting host pickup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DialogResolution {
    pub handle: u32,
    pub future_id: u64,
    /// `KIND_BUTTON` / `KIND_DISMISSED`.
    pub kind: u8,
    /// Button index when `kind == KIND_BUTTON`; ignored otherwise.
    pub button_idx: u8,
}

#[derive(Debug, Default)]
pub struct DialogManager {
    /// Open dialogs keyed by handle. Per Stone 13.2 a single activity
    /// can hold at most one open dialog — opening a second on the same
    /// handle resolves the prior one as Dismissed first.
    dialogs: std::collections::HashMap<u32, DialogState>,
    /// Resolved dialogs the host hasn't drained yet.
    resolutions: Vec<DialogResolution>,
}

impl DialogManager {
    pub fn new() -> Self { Self::default() }

    /// True iff `handle` currently has a dialog open. Used by the
    /// framework's pointer dispatch to decide whether to hit-test or
    /// forward to the activity.
    pub fn is_open(&self, handle: u32) -> bool {
        self.dialogs.contains_key(&handle)
    }

    /// All currently-open dialogs (read-only). Host uses this to
    /// render the dialog box on every frame.
    pub fn open_dialogs(&self) -> impl Iterator<Item = &DialogState> {
        self.dialogs.values()
    }

    /// Mutable iter — used by the framework's render pass to refresh
    /// `box_rect` / `button_rects` after layout.
    pub fn open_dialogs_mut(&mut self) -> impl Iterator<Item = &mut DialogState> {
        self.dialogs.values_mut()
    }

    /// Borrow one dialog by handle.
    pub fn get(&self, handle: u32) -> Option<&DialogState> {
        self.dialogs.get(&handle)
    }

    /// Mutable borrow of one dialog. Render path uses this to write
    /// back the laid-out rects.
    pub fn get_mut(&mut self, handle: u32) -> Option<&mut DialogState> {
        self.dialogs.get_mut(&handle)
    }

    /// Open a new dialog. If the activity already has one open, the
    /// existing dialog is resolved as `Dismissed` first (single-dialog
    /// invariant — Stone 13.2 explicitly scopes to one).
    ///
    /// `surface` must be a fresh `WindowLayer::Overlay` surface the
    /// caller allocated and owns. `DialogManager` only reads the id
    /// back when laying out paint ops or when the framework needs to
    /// destroy the overlay on resolution.
    pub fn open(
        &mut self,
        handle: u32,
        future_id: u64,
        owner: ActivityId,
        surface: SurfaceId,
        title: String,
        body: String,
        buttons: Vec<String>,
    ) -> &DialogState {
        // Single-dialog invariant: dismiss any prior dialog on this
        // handle. The host will pick up the Dismissed resolution next
        // frame and unblock the prior future.
        if let Some(prev) = self.dialogs.remove(&handle) {
            self.resolutions.push(DialogResolution {
                handle: prev.handle,
                future_id: prev.future_id,
                kind: KIND_DISMISSED,
                button_idx: 0,
            });
        }
        let state = DialogState {
            handle,
            future_id,
            owner,
            surface,
            title,
            body,
            buttons,
            box_rect: Rect::ZERO,
            button_rects: Vec::new(),
        };
        self.dialogs.insert(handle, state);
        // Safety: just inserted.
        self.dialogs.get(&handle).unwrap()
    }

    /// Resolve the dialog on `handle` with the given button index. The
    /// dialog is removed from the open set; a resolution record is
    /// pushed for the host to drain. No-op if no dialog is open.
    pub fn resolve_button(&mut self, handle: u32, button_idx: u8) -> bool {
        if let Some(s) = self.dialogs.remove(&handle) {
            self.resolutions.push(DialogResolution {
                handle: s.handle,
                future_id: s.future_id,
                kind: KIND_BUTTON,
                button_idx,
            });
            true
        } else {
            false
        }
    }

    /// Resolve as Dismissed. Used when the user taps outside the
    /// dialog box, or when the framework tears the activity down with
    /// a dialog still open (the SDK future then resolves cleanly
    /// rather than hanging).
    pub fn resolve_dismissed(&mut self, handle: u32) -> bool {
        if let Some(s) = self.dialogs.remove(&handle) {
            self.resolutions.push(DialogResolution {
                handle: s.handle,
                future_id: s.future_id,
                kind: KIND_DISMISSED,
                button_idx: 0,
            });
            true
        } else {
            false
        }
    }

    /// Owner of the dialog open on `handle`, if any. Used by the
    /// framework to figure out which activity the dialog hangs off
    /// (needed when destroying an activity to dismiss any in-flight
    /// dialog).
    pub fn dismiss_for_owner(&mut self, owner: ActivityId) -> Vec<u32> {
        let handles: Vec<u32> = self.dialogs.values()
            .filter(|d| d.owner == owner)
            .map(|d| d.handle)
            .collect();
        for h in &handles {
            self.resolve_dismissed(*h);
        }
        handles
    }

    /// Drain resolved dialogs the host hasn't yet picked up. Each
    /// frame, the kernel calls this and posts `host_op_done` for each
    /// to unblock the corresponding app worker.
    pub fn take_resolutions(&mut self) -> Vec<DialogResolution> {
        std::mem::take(&mut self.resolutions)
    }
}

// ── Layout ────────────────────────────────────────────────────────────

/// Result of laying out a dialog: the frame rect for the dialog box
/// and the per-button rects (same length and order as the dialog's
/// `buttons` vec).
#[derive(Debug, Clone)]
pub struct DialogLayout {
    pub box_rect: Rect,
    pub button_rects: Vec<Rect>,
}

/// Lay out a dialog inside `screen` (display-pixel space). Centred
/// horizontally; vertically centred minus a 10% offset so the box sits
/// slightly above the centre line (Material default — stops the box
/// from feeling like it's drifting downward against tall screens).
///
/// Width: ~80% of screen width, capped at 480dp; min 240dp. Height:
/// title + body + buttons, plus 16dp padding on each edge.
///
/// `density` scales the layout: a 16dp gutter on a 2× screen is 32px.
pub fn layout_dialog(
    screen: Rect,
    density: f32,
    _title: &str,
    body: &str,
    buttons: &[String],
) -> DialogLayout {
    let dp = density.max(0.5);
    let pad = 20.0 * dp;
    let title_h = 28.0 * dp;
    let body_h = body_layout_height(body, density);
    let btn_h = 44.0 * dp;
    let btn_gap = 12.0 * dp;
    let btn_row_h = if buttons.is_empty() { 0.0 } else { btn_h };

    let width = (screen.w * 0.8).clamp(240.0 * dp, 480.0 * dp);
    let height = pad + title_h + 8.0 * dp + body_h + pad + btn_row_h + pad;

    let x = screen.x + (screen.w - width) * 0.5;
    let y = screen.y + (screen.h - height) * 0.5 - screen.h * 0.05;
    let box_rect = Rect { x, y, w: width, h: height };

    let mut button_rects = Vec::with_capacity(buttons.len());
    if !buttons.is_empty() {
        let n = buttons.len() as f32;
        let total_gap = btn_gap * (n - 1.0);
        let btn_w = (width - 2.0 * pad - total_gap) / n;
        let btn_y = y + height - pad - btn_h;
        for i in 0..buttons.len() {
            let btn_x = x + pad + (btn_w + btn_gap) * (i as f32);
            button_rects.push(Rect { x: btn_x, y: btn_y, w: btn_w, h: btn_h });
        }
    }
    DialogLayout { box_rect, button_rects }
}

/// Estimate a body's drawn height. Splits on `'\n'` for explicit line
/// breaks; doesn't word-wrap (Stone 13.2 doesn't ship a text shaper —
/// the SDK treats the body as a single label). Consumers that want
/// wrapping today pass pre-wrapped strings.
fn body_layout_height(body: &str, density: f32) -> f32 {
    let lines = body.split('\n').count().max(1);
    (lines as f32) * 22.0 * density.max(0.5)
}

/// Render the dialog into `dst` (an Overlay surface's `FrameCommands`).
/// The first op is a full-screen dim layer; the dialog box paints on
/// top. Caller must `clear()` the surface buffer first or this will
/// stack on prior ops.
pub fn paint_dialog(
    dst: &mut crate::compositor::FrameCommands,
    state: &DialogState,
    screen: Rect,
    density: f32,
    layout: &DialogLayout,
) {
    // Full-screen dim. ~0.4 alpha matches Material standard ("scrim").
    // Using straight rgba so the renderer doesn't have to know about
    // surface alpha multipliers (Stone 13.3 introduces a richer alpha
    // path; this keeps 13.2 self-contained).
    dst.push(PaintOp::FillRect {
        x: screen.x, y: screen.y, w: screen.w, h: screen.h,
        rgba: 0x66000000,  // alpha 0x66 ≈ 0.4
    });

    // Box background — Material surface elevation 24 ≈ #2a2a2e.
    let dp = density.max(0.5);
    let radius = 16.0 * dp;
    dst.push(PaintOp::FillRoundRect {
        x: layout.box_rect.x, y: layout.box_rect.y,
        w: layout.box_rect.w, h: layout.box_rect.h,
        radius,
        rgba: 0xff2a2a2e,
    });

    // Title — left-aligned inside the box, padded.
    let pad = 20.0 * dp;
    let title_size = 22.0 * dp;
    dst.push(PaintOp::DrawText {
        x: layout.box_rect.x + pad,
        y: layout.box_rect.y + pad + title_size * 0.85,
        size: title_size,
        rgba: 0xfff8f8fa,
        text: state.title.clone(),
    });

    // Body — wrapped on existing newlines (Stone 13.2 doesn't
    // word-wrap; bodies are short by convention).
    let body_size = 16.0 * dp;
    let mut body_y = layout.box_rect.y + pad + 28.0 * dp + 16.0 * dp;
    for line in state.body.split('\n') {
        dst.push(PaintOp::DrawText {
            x: layout.box_rect.x + pad,
            y: body_y + body_size * 0.85,
            size: body_size,
            rgba: 0xffd8d8dc,
            text: line.to_string(),
        });
        body_y += 22.0 * dp;
    }

    // Buttons — pill-shaped. The last button is the primary (filled);
    // the rest are secondary (outline). Matches Material guidance for
    // dialog button order.
    let primary_idx = state.buttons.len().saturating_sub(1);
    for (i, label) in state.buttons.iter().enumerate() {
        let r = layout.button_rects[i];
        let is_primary = i == primary_idx;
        let bg = if is_primary { 0xff3d7eff } else { 0xff3a3a3e };
        dst.push(PaintOp::FillRoundRect {
            x: r.x, y: r.y, w: r.w, h: r.h, radius: r.h * 0.5, rgba: bg,
        });
        // Centred-ish label. Without a real text shaper we estimate
        // glyph width as 0.55 × size (matches sdk_runtime fallback).
        let label_size = 16.0 * dp;
        let estimated_w = label.chars().count() as f32 * label_size * 0.55;
        let lx = r.x + (r.w - estimated_w) * 0.5;
        let ly = r.y + (r.h + label_size) * 0.5 - 4.0 * dp;
        dst.push(PaintOp::DrawText {
            x: lx, y: ly, size: label_size,
            rgba: 0xffffffff, text: label.clone(),
        });
    }
}

/// Hit-test a pointer event against the dialog's last-laid-out rects.
/// Returns:
/// - `Some(idx)` if the click fell on button `idx`.
/// - `None` if the click was inside the box (a non-button area — eat
///   the event silently; don't dismiss).
/// - sentinel `usize::MAX` (via `HitTest::Outside`) for taps outside
///   the box (caller resolves Dismissed).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HitTest {
    Button(usize),
    InsideBoxNonButton,
    Outside,
}

pub fn hit_test(state: &DialogState, x: f32, y: f32) -> HitTest {
    for (i, r) in state.button_rects.iter().enumerate() {
        if x >= r.x && x < r.x + r.w && y >= r.y && y < r.y + r.h {
            return HitTest::Button(i);
        }
    }
    let b = state.box_rect;
    if x >= b.x && x < b.x + b.w && y >= b.y && y < b.y + b.h {
        HitTest::InsideBoxNonButton
    } else {
        HitTest::Outside
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen() -> Rect { Rect { x: 0.0, y: 0.0, w: 1080.0, h: 1920.0 } }

    fn open_one(mgr: &mut DialogManager) -> &DialogState {
        mgr.open(
            7, 100, 1u64, SurfaceId(42),
            "Clear?".into(),
            "This will reset".into(),
            vec!["Cancel".into(), "OK".into()],
        )
    }

    #[test]
    fn open_records_state_and_marks_handle_open() {
        let mut mgr = DialogManager::new();
        open_one(&mut mgr);
        assert!(mgr.is_open(7));
        let s = mgr.get(7).unwrap();
        assert_eq!(s.title, "Clear?");
        assert_eq!(s.buttons.len(), 2);
        assert_eq!(s.surface, SurfaceId(42));
    }

    #[test]
    fn opening_a_second_dialog_on_same_handle_dismisses_the_first() {
        let mut mgr = DialogManager::new();
        mgr.open(7, 100, 1u64, SurfaceId(42),
            "First".into(), "Body".into(), vec!["OK".into()]);
        mgr.open(7, 200, 1u64, SurfaceId(43),
            "Second".into(), "Body".into(), vec!["OK".into()]);
        let drained = mgr.take_resolutions();
        // First became Dismissed; second is still open.
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].future_id, 100);
        assert_eq!(drained[0].kind, KIND_DISMISSED);
        assert!(mgr.is_open(7));
        assert_eq!(mgr.get(7).unwrap().future_id, 200);
    }

    #[test]
    fn resolve_button_emits_button_kind_with_idx() {
        let mut mgr = DialogManager::new();
        open_one(&mut mgr);
        assert!(mgr.resolve_button(7, 1));
        assert!(!mgr.is_open(7));
        let drained = mgr.take_resolutions();
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].kind, KIND_BUTTON);
        assert_eq!(drained[0].button_idx, 1);
        assert_eq!(drained[0].future_id, 100);
    }

    #[test]
    fn resolve_dismissed_emits_dismissed_kind() {
        let mut mgr = DialogManager::new();
        open_one(&mut mgr);
        assert!(mgr.resolve_dismissed(7));
        let drained = mgr.take_resolutions();
        assert_eq!(drained[0].kind, KIND_DISMISSED);
    }

    #[test]
    fn resolve_unknown_handle_is_a_silent_noop() {
        let mut mgr = DialogManager::new();
        assert!(!mgr.resolve_button(7, 0));
        assert!(!mgr.resolve_dismissed(7));
        assert!(mgr.take_resolutions().is_empty());
    }

    #[test]
    fn dismiss_for_owner_resolves_every_dialog_owned() {
        let mut mgr = DialogManager::new();
        // Same owner, two handles
        mgr.open(7, 100, 1u64, SurfaceId(42), "a".into(), "b".into(), vec!["OK".into()]);
        mgr.open(8, 200, 1u64, SurfaceId(43), "a".into(), "b".into(), vec!["OK".into()]);
        // Different owner
        mgr.open(9, 300, 2u64, SurfaceId(44), "a".into(), "b".into(), vec!["OK".into()]);
        let dismissed = mgr.dismiss_for_owner(1u64);
        assert_eq!(dismissed.len(), 2);
        assert!(!mgr.is_open(7));
        assert!(!mgr.is_open(8));
        assert!(mgr.is_open(9));
    }

    #[test]
    fn take_resolutions_drains_and_clears() {
        let mut mgr = DialogManager::new();
        open_one(&mut mgr);
        mgr.resolve_button(7, 0);
        let first = mgr.take_resolutions();
        assert_eq!(first.len(), 1);
        let second = mgr.take_resolutions();
        assert!(second.is_empty(), "second drain should be empty");
    }

    #[test]
    fn layout_centres_box_within_screen() {
        let lay = layout_dialog(screen(), 1.0, "Title", "Body", &["OK".into()]);
        // Centred ± 5% upward shift
        let cx = lay.box_rect.x + lay.box_rect.w * 0.5;
        assert!((cx - 540.0).abs() < 1.0, "expected centred x, got {cx}");
        // Width within bounds
        assert!(lay.box_rect.w >= 240.0 && lay.box_rect.w <= 480.0);
    }

    #[test]
    fn layout_button_count_matches_buttons() {
        let lay = layout_dialog(screen(), 1.0, "T", "B",
            &["A".into(), "B".into(), "C".into()]);
        assert_eq!(lay.button_rects.len(), 3);
        // Buttons sit at the same y, fully inside the box
        for r in &lay.button_rects {
            assert!(r.y > lay.box_rect.y);
            assert!(r.y + r.h <= lay.box_rect.y + lay.box_rect.h);
        }
    }

    #[test]
    fn layout_zero_buttons_produces_empty_button_rects() {
        let lay = layout_dialog(screen(), 1.0, "T", "B", &[]);
        assert!(lay.button_rects.is_empty());
    }

    #[test]
    fn hit_test_button_reports_button_idx() {
        let mut state = DialogState {
            handle: 1, future_id: 1, owner: 1u64, surface: SurfaceId(1),
            title: "".into(), body: "".into(), buttons: vec!["A".into(), "B".into()],
            box_rect: Rect { x: 100.0, y: 100.0, w: 300.0, h: 200.0 },
            button_rects: vec![
                Rect { x: 110.0, y: 250.0, w: 80.0, h: 40.0 },
                Rect { x: 210.0, y: 250.0, w: 80.0, h: 40.0 },
            ],
        };
        // Tap inside button 0
        assert_eq!(hit_test(&state, 150.0, 270.0), HitTest::Button(0));
        // Tap inside button 1
        assert_eq!(hit_test(&state, 250.0, 270.0), HitTest::Button(1));
        // Inside box, between buttons (gap area)
        state.button_rects[0] = Rect { x: 110.0, y: 250.0, w: 50.0, h: 40.0 };
        assert_eq!(hit_test(&state, 180.0, 270.0), HitTest::InsideBoxNonButton);
        // Outside
        assert_eq!(hit_test(&state, 50.0, 50.0), HitTest::Outside);
        assert_eq!(hit_test(&state, 500.0, 500.0), HitTest::Outside);
    }

    #[test]
    fn paint_dialog_emits_dim_then_box_then_buttons() {
        let mut fc = crate::compositor::FrameCommands::new();
        let state = DialogState {
            handle: 1, future_id: 1, owner: 1u64, surface: SurfaceId(1),
            title: "Clear?".into(), body: "Reset".into(),
            buttons: vec!["No".into(), "Yes".into()],
            box_rect: Rect::ZERO,
            button_rects: Vec::new(),
        };
        let lay = layout_dialog(screen(), 1.0, &state.title, &state.body, &state.buttons);
        paint_dialog(&mut fc, &state, screen(), 1.0, &lay);
        // First op = dim (full-screen FillRect)
        assert!(matches!(fc.ops[0], PaintOp::FillRect { .. }));
        // Second op = box background (rounded rect)
        assert!(matches!(fc.ops[1], PaintOp::FillRoundRect { .. }));
        // Counts: 1 dim + 1 box + 1 title + 1 body line + 2 (button bg + label) per button
        assert_eq!(fc.ops.len(), 1 + 1 + 1 + 1 + 2 * 2);
    }
}
