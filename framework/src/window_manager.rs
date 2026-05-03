//! Window manager — surface model (Stone 13.0).
//!
//! Arranges activity surfaces into displays. A *surface* is a logical
//! buffer an activity (or system overlay) paints into; the compositor
//! eventually flattens every visible surface in z-order and ships the
//! result to the host renderer.
//!
//! Pre-Stone-13.0 this was a thin stub with a single `Window` struct
//! per activity. The new model introduces:
//!
//! - [`WindowLayer`] — coarse z-band. Wallpaper at the bottom, App in
//!   the middle, Overlay above that (toasts, transient panels),
//!   System at the very top (pinned UI like notification center).
//! - [`SurfaceId`] — opaque u32 handle the rest of the framework
//!   passes around. Stable for the surface's lifetime; never reused
//!   after destroy.
//! - [`SurfaceRecord`] — owner / layer / bounds / visibility metadata.
//!   Per-surface paint commands live in the compositor (Stone 13.1
//!   teaches the compositor about per-surface buffers).
//!
//! Z-ordering: by `(layer, creation_seq)` ascending. Same layer ties
//! break on creation order, so a freshly-created Overlay sits above
//! older Overlays — the natural "last-pushed-on-top" behaviour for
//! transient UI. Cross-layer ordering ignores creation order entirely
//! (a brand-new Wallpaper still sits below an old App), matching how
//! Android's WindowToken layers work.
//!
//! Focus: a single surface holds keyboard/IME focus at any time.
//! Stored as a single `Option<SurfaceId>` rather than a stack — the
//! "focus stack" semantics live in the activity manager (task back
//! stack) where they belong; the window manager just tracks which
//! surface is the *current* recipient. Destroying the focused surface
//! clears focus to `None`; the framework decides what to focus next
//! (if anything) on the same frame.
//!
//! See docs/window-manager.md for the long-form design.

use std::collections::HashMap;

use crate::activity_manager::ActivityId;

/// Coarse z-band. Higher variants paint on top of lower. Repr is `u32`
/// so callers can transmute / serialise without churn; values are
/// spaced so we can wedge new bands in without renumbering existing
/// ones (e.g. `Drag = 250` would slot between Overlay and System if
/// drag-and-drop ever lands a dedicated layer).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u32)]
pub enum WindowLayer {
    /// Background fill — wallpaper, dynamic background. Single surface
    /// expected; multiple wallpaper surfaces all stack at the bottom
    /// in creation order.
    Wallpaper = 0,
    /// The dominant layer — every running activity's main surface
    /// goes here. Foreground vs. background-but-visible are
    /// distinguished by `visible`, not by sub-layer.
    App = 100,
    /// Transient panels above the active app: toasts, snackbars,
    /// drop-down menus, the keyboard. Auto-dismiss is the activity's
    /// responsibility; the window manager just paints them on top.
    Overlay = 200,
    /// Pinned UI that survives app switches: status bar, navigation
    /// bar, system dialogs that block input to everything below.
    /// Reserved for kernel/framework owners; user apps should live in
    /// `App`/`Overlay`.
    System = 300,
}

/// Opaque per-surface handle. `0` is the never-allocated sentinel —
/// `WindowManager` allocates from `1` upward. Stable across the
/// surface's lifetime; **not** reused after `destroy_surface` (the
/// counter is monotonic). Callers can compare with `==` and use as a
/// HashMap key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SurfaceId(pub u32);

impl SurfaceId {
    pub const NONE: SurfaceId = SurfaceId(0);
    /// Whether this is the never-allocated sentinel.
    pub fn is_none(self) -> bool { self.0 == 0 }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub const ZERO: Rect = Rect { x: 0.0, y: 0.0, w: 0.0, h: 0.0 };
}

/// Per-surface record. Owner is the activity that requested the
/// surface; bounds are in display-pixel space (the WindowManager
/// doesn't apply the density factor — that happens in the
/// compositor's per-surface paint pass in Stone 13.1).
#[derive(Debug, Clone)]
pub struct SurfaceRecord {
    pub id: SurfaceId,
    pub owner: ActivityId,
    pub layer: WindowLayer,
    pub bounds: Rect,
    pub visible: bool,
    /// Monotonic counter assigned at `create_surface` — used as the
    /// secondary sort key in `surfaces_in_z_order` so same-layer
    /// surfaces stack in creation order ("last on top").
    creation_seq: u64,
}

pub struct WindowManager {
    surfaces: HashMap<SurfaceId, SurfaceRecord>,
    /// Monotonic id allocator. Starts at `1` so `SurfaceId(0)` stays
    /// the "never-allocated" sentinel. Wraps to `1` after `u32::MAX`,
    /// which is a >40-billion-surface lifetime — practically infinite.
    next_id: u32,
    /// Monotonic creation counter — secondary z-order key.
    next_seq: u64,
    /// Currently-focused surface, or `None` if nothing has focus
    /// (boot, after destroy of the focused surface, before first
    /// `focus()` call).
    focused: Option<SurfaceId>,
}

impl WindowManager {
    pub fn new() -> Self {
        Self {
            surfaces: HashMap::new(),
            next_id: 1,
            next_seq: 0,
            focused: None,
        }
    }

    /// Allocate a new surface. The id is stable for the surface's
    /// lifetime and never reused after `destroy_surface`. Initial
    /// state: `bounds = Rect::ZERO`, `visible = false` — caller is
    /// expected to follow up with `set_bounds` + `set_visible(true)`
    /// before the surface should appear on screen.
    pub fn create_surface(&mut self, owner: ActivityId, layer: WindowLayer) -> SurfaceId {
        let id = SurfaceId(self.next_id);
        self.next_id = self.next_id.checked_add(1).unwrap_or(1);
        let creation_seq = self.next_seq;
        self.next_seq = self.next_seq.wrapping_add(1);
        self.surfaces.insert(id, SurfaceRecord {
            id,
            owner,
            layer,
            bounds: Rect::ZERO,
            visible: false,
            creation_seq,
        });
        id
    }

    /// Drop a surface from the manager. Idempotent — destroying an
    /// unknown id is a silent no-op. If the destroyed surface was
    /// focused, focus is cleared to `None`; the framework picks up
    /// the slack by focusing whatever's appropriate (typically the
    /// new top of the foreground activity stack).
    pub fn destroy_surface(&mut self, id: SurfaceId) {
        if self.surfaces.remove(&id).is_some() && self.focused == Some(id) {
            self.focused = None;
        }
    }

    /// Update the surface's bounds rectangle. No-op for unknown ids.
    /// The compositor reads `bounds` on its next composition pass.
    pub fn set_bounds(&mut self, id: SurfaceId, rect: Rect) {
        if let Some(s) = self.surfaces.get_mut(&id) {
            s.bounds = rect;
        }
    }

    /// Toggle visibility. Hidden surfaces stay in z-order (their slot
    /// in `surfaces_in_z_order` is preserved) but the compositor
    /// skips them — same shape as Android's `View.GONE` / `INVISIBLE`
    /// distinction collapsed to a single bit. Activities that need
    /// "show me again next frame without losing my buffer" use
    /// `set_visible(false)` then `set_visible(true)`; activities
    /// that are tearing down call `destroy_surface`.
    pub fn set_visible(&mut self, id: SurfaceId, visible: bool) {
        if let Some(s) = self.surfaces.get_mut(&id) {
            s.visible = visible;
        }
    }

    /// Move focus to `id`. Returns the previously-focused surface (or
    /// `None` if focus was unset). No-op for unknown ids — focus
    /// stays where it was, and the return is `self.focused()` so the
    /// caller still gets a defined answer.
    pub fn focus(&mut self, id: SurfaceId) -> Option<SurfaceId> {
        if !self.surfaces.contains_key(&id) {
            return self.focused;
        }
        let prev = self.focused;
        self.focused = Some(id);
        prev
    }

    /// Currently-focused surface. `None` at boot, after the focused
    /// surface is destroyed, or before the first `focus()` call.
    pub fn focused(&self) -> Option<SurfaceId> {
        self.focused
    }

    /// Surfaces in bottom-to-top z-order. Sort key: `(layer,
    /// creation_seq)` ascending — Wallpaper first, then App, then
    /// Overlay, then System; within each layer, older surfaces
    /// before younger ones. Hidden surfaces are *not* filtered out —
    /// the compositor skips them when painting, but their slot in
    /// the z-stack stays consistent so a later `set_visible(true)`
    /// doesn't pop them above their peers.
    pub fn surfaces_in_z_order(&self) -> Vec<SurfaceId> {
        let mut v: Vec<&SurfaceRecord> = self.surfaces.values().collect();
        v.sort_by_key(|s| (s.layer, s.creation_seq));
        v.into_iter().map(|s| s.id).collect()
    }

    /// Lookup. `None` for unknown ids. Used by the compositor when it
    /// walks `surfaces_in_z_order` to fetch each surface's bounds +
    /// visibility flag.
    pub fn surface(&self, id: SurfaceId) -> Option<&SurfaceRecord> {
        self.surfaces.get(&id)
    }

    /// All surfaces a given activity owns. Used on activity destroy
    /// to clean up any surfaces the activity didn't explicitly tear
    /// down (the framework calls `destroy_surface` for each).
    pub fn surfaces_for_owner(&self, owner: ActivityId) -> Vec<SurfaceId> {
        self.surfaces.values()
            .filter(|s| s.owner == owner)
            .map(|s| s.id)
            .collect()
    }
}

impl Default for WindowManager {
    fn default() -> Self { Self::new() }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> WindowManager { WindowManager::new() }

    #[test]
    fn create_surface_assigns_monotonic_ids_starting_at_one() {
        let mut wm = fixture();
        let a = wm.create_surface(1u64, WindowLayer::App);
        let b = wm.create_surface(1u64, WindowLayer::App);
        let c = wm.create_surface(2u64, WindowLayer::Overlay);
        assert_eq!(a, SurfaceId(1));
        assert_eq!(b, SurfaceId(2));
        assert_eq!(c, SurfaceId(3));
    }

    #[test]
    fn z_order_sorts_by_layer_then_creation() {
        let mut wm = fixture();
        // Create out of layer order to confirm the sort works.
        let app1 = wm.create_surface(1u64, WindowLayer::App);
        let wall = wm.create_surface(2u64, WindowLayer::Wallpaper);
        let app2 = wm.create_surface(3u64, WindowLayer::App);
        let sys  = wm.create_surface(4u64, WindowLayer::System);
        let over = wm.create_surface(5u64, WindowLayer::Overlay);
        let order = wm.surfaces_in_z_order();
        // Bottom-to-top: Wallpaper, App(creation=0), App(creation=2),
        // Overlay, System.
        assert_eq!(order, vec![wall, app1, app2, over, sys]);
    }

    #[test]
    fn set_visible_does_not_change_z_order() {
        let mut wm = fixture();
        let a = wm.create_surface(1u64, WindowLayer::App);
        let b = wm.create_surface(1u64, WindowLayer::App);
        wm.set_visible(a, false);
        // Hidden surface still occupies its slot.
        assert_eq!(wm.surfaces_in_z_order(), vec![a, b]);
        assert_eq!(wm.surface(a).map(|s| s.visible), Some(false));
    }

    #[test]
    fn focus_returns_previous_and_updates() {
        let mut wm = fixture();
        let a = wm.create_surface(1u64, WindowLayer::App);
        let b = wm.create_surface(2u64, WindowLayer::App);
        assert_eq!(wm.focused(), None);
        assert_eq!(wm.focus(a), None);
        assert_eq!(wm.focused(), Some(a));
        assert_eq!(wm.focus(b), Some(a));
        assert_eq!(wm.focused(), Some(b));
    }

    #[test]
    fn focus_unknown_id_is_a_silent_noop() {
        let mut wm = fixture();
        let a = wm.create_surface(1u64, WindowLayer::App);
        wm.focus(a);
        // Unknown id leaves focus where it was.
        let nope = SurfaceId(999);
        assert_eq!(wm.focus(nope), Some(a));
        assert_eq!(wm.focused(), Some(a));
    }

    #[test]
    fn destroy_surface_clears_focus_if_focused() {
        let mut wm = fixture();
        let a = wm.create_surface(1u64, WindowLayer::App);
        let b = wm.create_surface(1u64, WindowLayer::App);
        wm.focus(a);
        wm.destroy_surface(a);
        assert_eq!(wm.focused(), None);
        // b is still around.
        assert!(wm.surface(b).is_some());
        // a is gone.
        assert!(wm.surface(a).is_none());
    }

    #[test]
    fn destroy_surface_keeps_focus_when_destroyed_was_not_focused() {
        let mut wm = fixture();
        let a = wm.create_surface(1u64, WindowLayer::App);
        let b = wm.create_surface(1u64, WindowLayer::App);
        wm.focus(a);
        wm.destroy_surface(b);
        assert_eq!(wm.focused(), Some(a));
    }

    #[test]
    fn set_bounds_updates_record() {
        let mut wm = fixture();
        let a = wm.create_surface(1u64, WindowLayer::App);
        let r = Rect { x: 10.0, y: 20.0, w: 300.0, h: 400.0 };
        wm.set_bounds(a, r);
        assert_eq!(wm.surface(a).map(|s| s.bounds), Some(r));
    }

    #[test]
    fn surfaces_for_owner_filters_correctly() {
        let mut wm = fixture();
        let a1 = wm.create_surface(1u64, WindowLayer::App);
        let a2 = wm.create_surface(1u64, WindowLayer::Overlay);
        let _b = wm.create_surface(2u64, WindowLayer::App);
        let mut owned = wm.surfaces_for_owner(1u64);
        owned.sort();
        assert_eq!(owned, vec![a1, a2]);
    }

    #[test]
    fn destroy_unknown_surface_is_silent() {
        let mut wm = fixture();
        wm.destroy_surface(SurfaceId(999));
        // No panic, no state change.
        assert_eq!(wm.surfaces_in_z_order(), Vec::<SurfaceId>::new());
    }

    #[test]
    fn surface_id_none_sentinel_is_zero() {
        assert_eq!(SurfaceId::NONE, SurfaceId(0));
        assert!(SurfaceId::NONE.is_none());
        assert!(!SurfaceId(1).is_none());
    }
}
