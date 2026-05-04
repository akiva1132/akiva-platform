//! Stone 13.3 — Activity transitions.
//!
//! Activity push / pop animations mean the dominant "feel" of the OS:
//! Android's left-to-right shove, iOS's interactive swipe-back, web's
//! fade. A jump-cut between activities reads as broken.
//!
//! ## Model
//!
//! Each running surface gets an optional [`Transition`]. The framework
//! ticks every active transition once per frame and asks the
//! compositor to apply the resulting `(translate_x, alpha)` pair. When
//! a transition resolves, the framework either keeps the surface
//! (push completed → resting at translate_x=0) or destroys it (pop
//! completed → outgoing surface gone).
//!
//! ## Wire-format note
//!
//! The DoD constrains us to existing paint opcodes — no new ABI for
//! 13.3. The compositor wraps each surface's ops with
//! `PushTransform { dx, 0, 1.0 } … PopTransform` to apply the
//! `translate_x`. Alpha is applied by walking the ops once before
//! flatten and multiplying the alpha byte of every colour-bearing op
//! (FillRect, FillRoundRect, etc.); that's an O(ops) pass per
//! transition frame, in steady state ≤200 ops × 4 surfaces. No
//! per-frame allocation past the existing Vec.

use std::collections::HashMap;

use crate::window_manager::SurfaceId;

/// What kind of transition is running on a surface. Each variant
/// carries its own duration so push (250ms ergonomic) and pop (200ms
/// snappier-back) can pace independently.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Transition {
    /// Slide in from the right. `t = 0` → fully off-screen at +width;
    /// `t = 1` → resting at `dx = 0`. Used on activity push.
    SlideFromRight { duration_ms: u32 },
    /// Slide out to the left. `t = 0` → resting at `dx = 0`; `t = 1`
    /// → fully off-screen at `-width`. Used on activity pop (paired
    /// with `FadeOut` for the outgoing surface).
    SlideToLeft { duration_ms: u32 },
    /// Slide in from the left. `t = 0` → fully off-screen at
    /// `-width`; `t = 1` → resting at `dx = 0`. Used to give pop a
    /// "reveal-from-left" feel on the incoming surface when capture-
    /// and-replay of the outgoing isn't available (Stone 13.3 ships
    /// without surface continuity for pop).
    SlideFromLeft { duration_ms: u32 },
    /// Fade in from alpha 0 to alpha 1. Linear curve.
    FadeIn { duration_ms: u32 },
    /// Fade out from alpha 1 to alpha 0. Linear curve. Often paired
    /// with `SlideToLeft` on pop so the outgoing surface both moves
    /// and dissolves; the AnimationController stores them as two
    /// independent entries on the same surface.
    FadeOut { duration_ms: u32 },
}

impl Transition {
    pub fn duration_ms(&self) -> u32 {
        match self {
            Transition::SlideFromRight { duration_ms }
            | Transition::SlideToLeft { duration_ms }
            | Transition::SlideFromLeft { duration_ms }
            | Transition::FadeIn { duration_ms }
            | Transition::FadeOut { duration_ms } => *duration_ms,
        }
    }
}

/// Per-surface animation state. Multiple transitions can run on the
/// same surface simultaneously (slide-out + fade-out on pop).
#[derive(Debug, Clone)]
struct ActiveTransition {
    transition: Transition,
    /// Wall-clock time the transition started (frame ts). The
    /// AnimationController ticks each entry against the current frame
    /// ts to derive `t = (now - start) / duration`.
    started_at: f64,
}

/// Result of a single tick: what the compositor should apply to the
/// surface this frame, plus whether the transition is complete.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SurfaceProps {
    /// Horizontal pixel offset to translate the surface by. `0.0` is
    /// resting; positive is to the right.
    pub translate_x: f32,
    /// Alpha multiplier in [0, 1]. `1.0` is fully opaque.
    pub alpha: f32,
}

impl SurfaceProps {
    pub const REST: SurfaceProps = SurfaceProps { translate_x: 0.0, alpha: 1.0 };

    pub fn is_rest(&self) -> bool {
        self.translate_x == 0.0 && self.alpha == 1.0
    }
}

#[derive(Debug, Default)]
pub struct AnimationController {
    /// One vec of transitions per animating surface. Empty entries
    /// are pruned eagerly so iteration stays cheap.
    active: HashMap<SurfaceId, Vec<ActiveTransition>>,
    /// Surfaces whose transitions all resolved this frame, queued
    /// for the framework to consume. The framework's pop-completion
    /// handler reads this and tears down the outgoing surface.
    completed: Vec<(SurfaceId, Transition)>,
}

impl AnimationController {
    pub fn new() -> Self { Self::default() }

    /// Begin a fresh transition on `surface`. Multiple transitions
    /// can stack on the same surface (slide + fade on pop). `now_ms`
    /// is the current frame timestamp; subsequent `tick` calls are
    /// relative to it.
    pub fn start(&mut self, surface: SurfaceId, transition: Transition, now_ms: f64) {
        self.active.entry(surface).or_default().push(ActiveTransition {
            transition, started_at: now_ms,
        });
    }

    /// True if any transition is running on `surface`.
    pub fn is_animating(&self, surface: SurfaceId) -> bool {
        self.active.get(&surface).map(|v| !v.is_empty()).unwrap_or(false)
    }

    /// True if any surface is animating right now. Lets the kernel
    /// short-circuit the per-surface props pass on quiet frames.
    pub fn has_active(&self) -> bool {
        self.active.values().any(|v| !v.is_empty())
    }

    /// Compute the SurfaceProps to apply to `surface` for the frame
    /// at `now_ms`. Combines every active transition on that surface:
    /// slide and fade compose multiplicatively with REST. Returns
    /// `SurfaceProps::REST` when no transitions are active.
    ///
    /// `screen_width` is the display-pixel width — slide transitions
    /// scale by it (off-screen position is `±screen_width`).
    pub fn props_for(
        &self, surface: SurfaceId, now_ms: f64, screen_width: f32,
    ) -> SurfaceProps {
        let entries = match self.active.get(&surface) {
            Some(v) if !v.is_empty() => v,
            _ => return SurfaceProps::REST,
        };
        let mut props = SurfaceProps::REST;
        for entry in entries {
            let t = progress(entry, now_ms);
            apply_transition_to_props(entry.transition, t, screen_width, &mut props);
        }
        props
    }

    /// Tick every active transition forward to `now_ms`. Resolved
    /// transitions move to `completed`; surfaces with no remaining
    /// transitions are pruned from the active set.
    pub fn tick(&mut self, now_ms: f64) {
        let mut empties: Vec<SurfaceId> = Vec::new();
        for (surface, entries) in self.active.iter_mut() {
            entries.retain(|e| {
                if progress(e, now_ms) >= 1.0 {
                    self.completed.push((*surface, e.transition));
                    false
                } else {
                    true
                }
            });
            if entries.is_empty() {
                empties.push(*surface);
            }
        }
        for s in empties {
            self.active.remove(&s);
        }
    }

    /// Drain every transition that completed since the last call.
    /// The framework consumes this to advance pending pops (destroy
    /// the outgoing surface once its slide-out finished, etc.).
    pub fn take_completed(&mut self) -> Vec<(SurfaceId, Transition)> {
        std::mem::take(&mut self.completed)
    }

    /// Cancel every transition on `surface` immediately. Used when an
    /// activity is destroyed mid-transition — we don't want the
    /// transition completion handler to re-process a surface that's
    /// already gone.
    pub fn cancel(&mut self, surface: SurfaceId) {
        self.active.remove(&surface);
    }
}

/// Progress in `[0, 1]`. >1 means the transition is past its end and
/// should be considered complete this frame.
fn progress(entry: &ActiveTransition, now_ms: f64) -> f32 {
    let dur = entry.transition.duration_ms() as f64;
    if dur <= 0.0 { return 1.0; }
    ((now_ms - entry.started_at) / dur).clamp(0.0, 1.0) as f32
}

// Cubic ease-out: fast at start, decelerates to zero at end. Hides
// 60Hz strobing on dense content (vertical edges of buttons/text
// crossing pixel boundaries at constant velocity look stuttery) and
// matches the "settling" feel of native iOS/Android slides.
fn ease_out_cubic(t: f32) -> f32 {
    let inv = 1.0 - t;
    1.0 - inv * inv * inv
}

fn apply_transition_to_props(
    transition: Transition, t: f32, screen_width: f32, dst: &mut SurfaceProps,
) {
    match transition {
        Transition::SlideFromRight { .. } => {
            let e = ease_out_cubic(t);
            dst.translate_x += screen_width * (1.0 - e);
        }
        Transition::SlideToLeft { .. } => {
            let e = ease_out_cubic(t);
            dst.translate_x -= screen_width * e;
        }
        Transition::SlideFromLeft { .. } => {
            let e = ease_out_cubic(t);
            dst.translate_x -= screen_width * (1.0 - e);
        }
        Transition::FadeIn { .. } => {
            dst.alpha *= ease_out_cubic(t);
        }
        Transition::FadeOut { .. } => {
            dst.alpha *= 1.0 - ease_out_cubic(t);
        }
    }
}

// Phase 15.3 — `apply_props` (op-list rewriter) deleted. Animations
// now flow through `Animations::props_for(surface)` straight to
// `compositor::SurfaceDrawOpts` on the CompositeSurface op, and the
// renderer applies translate/scale/globalAlpha around drawImage at
// the screen-composite step. The pre-15.3 op-list wrapper translated
// content INSIDE the per-Activity offscreen, which clipped slide
// animations the moment 15.1 introduced per-Activity surfaces.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compositor::PaintOp;

    fn s(id: u32) -> SurfaceId { SurfaceId(id) }

    #[test]
    fn rest_props_is_identity() {
        assert!(SurfaceProps::REST.is_rest());
    }

    #[test]
    fn slide_from_right_props_at_zero_is_full_width() {
        let mut ctrl = AnimationController::new();
        ctrl.start(s(1), Transition::SlideFromRight { duration_ms: 250 }, 1000.0);
        let p = ctrl.props_for(s(1), 1000.0, 1080.0);
        assert!((p.translate_x - 1080.0).abs() < 0.5);
        assert_eq!(p.alpha, 1.0);
    }

    #[test]
    fn slide_from_right_props_at_half_is_eased() {
        let mut ctrl = AnimationController::new();
        ctrl.start(s(1), Transition::SlideFromRight { duration_ms: 200 }, 1000.0);
        let p = ctrl.props_for(s(1), 1100.0, 1080.0);
        // t = 0.5, ease_out_cubic = 0.875 → translate_x = 1080 * 0.125 = 135
        assert!((p.translate_x - 135.0).abs() < 1.0);
    }

    #[test]
    fn slide_to_left_progresses_negative() {
        let mut ctrl = AnimationController::new();
        ctrl.start(s(1), Transition::SlideToLeft { duration_ms: 200 }, 1000.0);
        // t = 0.5, ease_out_cubic = 0.875 → translate_x = -1080 * 0.875 = -945
        let p = ctrl.props_for(s(1), 1100.0, 1080.0);
        assert!((p.translate_x + 945.0).abs() < 1.0);
    }

    #[test]
    fn fade_in_alpha_progresses() {
        let mut ctrl = AnimationController::new();
        ctrl.start(s(1), Transition::FadeIn { duration_ms: 200 }, 1000.0);
        // t=0 → alpha 0
        assert!((ctrl.props_for(s(1), 1000.0, 1080.0).alpha - 0.0).abs() < 0.01);
        // t=0.5, ease_out_cubic = 0.875 → alpha 0.875
        assert!((ctrl.props_for(s(1), 1100.0, 1080.0).alpha - 0.875).abs() < 0.01);
        // t=1 → alpha 1
        assert!((ctrl.props_for(s(1), 1200.0, 1080.0).alpha - 1.0).abs() < 0.01);
    }

    #[test]
    fn tick_resolves_finished_and_keeps_in_progress() {
        let mut ctrl = AnimationController::new();
        ctrl.start(s(1), Transition::SlideFromRight { duration_ms: 100 }, 1000.0);
        ctrl.start(s(2), Transition::SlideFromRight { duration_ms: 500 }, 1000.0);
        // After 200ms, surface 1 finished, surface 2 in progress.
        ctrl.tick(1200.0);
        assert!(!ctrl.is_animating(s(1)));
        assert!(ctrl.is_animating(s(2)));
        let done = ctrl.take_completed();
        assert_eq!(done.len(), 1);
        assert_eq!(done[0].0, s(1));
        assert_eq!(done[0].1, Transition::SlideFromRight { duration_ms: 100 });
    }

    #[test]
    fn concurrent_slide_and_fade_on_same_surface_compose() {
        // Pop transition: outgoing surface slides AND fades. Both run
        // for 200ms with ease-out cubic; at t=0.5, ease=0.875 →
        // translate_x = -1080 * 0.875 = -945, alpha = 1 - 0.875 = 0.125.
        let mut ctrl = AnimationController::new();
        ctrl.start(s(7), Transition::SlideToLeft { duration_ms: 200 }, 1000.0);
        ctrl.start(s(7), Transition::FadeOut { duration_ms: 200 }, 1000.0);
        let p = ctrl.props_for(s(7), 1100.0, 1080.0);
        assert!((p.translate_x + 945.0).abs() < 1.0);
        assert!((p.alpha - 0.125).abs() < 0.01);
    }

    #[test]
    fn cancel_clears_a_surface_immediately() {
        let mut ctrl = AnimationController::new();
        ctrl.start(s(1), Transition::SlideToLeft { duration_ms: 200 }, 1000.0);
        ctrl.cancel(s(1));
        assert!(!ctrl.is_animating(s(1)));
        // Subsequent tick has nothing to complete.
        ctrl.tick(2000.0);
        assert!(ctrl.take_completed().is_empty());
    }

    #[test]
    fn props_for_completed_transition_returns_rest_after_tick() {
        let mut ctrl = AnimationController::new();
        ctrl.start(s(1), Transition::SlideFromRight { duration_ms: 100 }, 1000.0);
        ctrl.tick(2000.0);  // Way past
        let p = ctrl.props_for(s(1), 2000.0, 1080.0);
        assert!(p.is_rest());
    }

    #[test]
    fn has_active_reflects_state() {
        let mut ctrl = AnimationController::new();
        assert!(!ctrl.has_active());
        ctrl.start(s(1), Transition::SlideFromRight { duration_ms: 100 }, 0.0);
        assert!(ctrl.has_active());
        ctrl.tick(1000.0);
        assert!(!ctrl.has_active());
    }

    #[test]
    fn zero_duration_resolves_on_first_tick() {
        let mut ctrl = AnimationController::new();
        ctrl.start(s(1), Transition::SlideFromRight { duration_ms: 0 }, 1000.0);
        ctrl.tick(1000.0);
        assert!(!ctrl.is_animating(s(1)));
        assert_eq!(ctrl.take_completed().len(), 1);
    }
}
