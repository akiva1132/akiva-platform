//! Gesture detectors — small state machines that consume
//! `PointerEvent` streams and surface higher-level intents.
//!
//! Stage 3.4 lands the first of these — `DoubleTapDetector`. The
//! shape is deliberately app-owned and pollable, mirroring the
//! `was_clicked` pattern already used for button taps. The app
//! feeds raw pointer events into the detector, checks the return
//! value, and reacts. No callbacks, no closures, no borrow
//! gymnastics with `Ui`.
//!
//! Why a detector (rather than `Ui::on_double_tap`)? The SDK's
//! widget toolkit doesn't expose callback-style listeners — every
//! interactive primitive uses the "post-`present` poll" shape (see
//! `Ui::was_clicked`). Adding a callback path *just* for double-tap
//! would split the model. The detector is the natural fit: the app
//! already owns its activity struct, so it can own one detector
//! per region it cares about (per-card photo area in Pixelfeed).

use crate::activity::{PointerEvent, PointerPhase};

/// Maximum time between two `Up` events for them to count as a
/// double-tap. 300 ms is the Android Material Design default and
/// matches Web's `dblclick` heuristics — short enough that two
/// separate single-taps aren't accidentally fused, long enough
/// that a deliberate human double-tap lands inside it.
pub const DOUBLE_TAP_MAX_INTERVAL_MS: f64 = 300.0;

/// Maximum distance (dp) between the two taps' down-points for
/// them to count as a double-tap. 32 dp covers a fingertip's
/// natural drift on the second tap; beyond that the gesture
/// becomes a quick drag and the user's intent is different.
pub const DOUBLE_TAP_MAX_MOVE_PX: f32 = 32.0;

/// Maximum distance (dp) a single tap can travel between Down and
/// Up before it's considered a drag rather than a tap. Filters out
/// "user pressed, dragged, released" sequences that should *not*
/// arm the next-tap candidate.
pub const TAP_SLOP_PX: f32 = 12.0;

/// Per-region double-tap detector. Each instance tracks one
/// continuous tap-pair candidate; apps that care about multiple
/// regions (one per Pixelfeed card's photo area) own one detector
/// per region and route pointer events to the right one based on
/// hit-test before calling [`Self::feed`].
///
/// Usage:
/// ```ignore
/// // In the app's `on_pointer`:
/// if photo_rect_contains(ev.x, ev.y) {
///     if let Some((x, y)) = self.dt.feed(ev, ctx.timestamp_ms) {
///         self.toggle_like();
///         self.show_heart_overlay(x, y);
///     }
/// } else {
///     self.dt.cancel();   // tap left the region — reset streak
/// }
/// ```
#[derive(Debug, Default, Clone, Copy)]
pub struct DoubleTapDetector {
    /// Timestamp of the prior tap's `Up`, or `0.0` if no candidate
    /// is pending. `0.0` is a safe sentinel because activity
    /// timestamps are milliseconds since activity creation and
    /// strictly grow monotonically — the very first feed reads
    /// some positive `now_ms`.
    last_up_ts: f64,
    /// Position of the prior tap's `Down` event. Used to enforce
    /// `DOUBLE_TAP_MAX_MOVE_PX` between taps.
    last_down_x: f32,
    last_down_y: f32,
    /// Position of the current in-flight tap's `Down` event, if
    /// any. `None` after `Up`/`Cancel`/end-of-stream.
    pending_down: Option<(f32, f32)>,
}

impl DoubleTapDetector {
    /// Fresh detector — no prior tap, no in-flight tap.
    pub fn new() -> Self { Self::default() }

    /// Feed one pointer event. Returns `Some((x, y))` on the `Up`
    /// that *completes* a double-tap, where `(x, y)` is the position
    /// of the completing Up. Returns `None` on every other event,
    /// including the first Up of a candidate pair (that one *primes*
    /// the streak; the second one *completes* it).
    ///
    /// `now_ms` should be `ctx.timestamp_ms`, the wall-clock
    /// milliseconds the framework hands to `on_render` /
    /// `on_pointer`. The detector reads it only on Up — Down and
    /// Move are timestamp-agnostic, so callers passing 0.0 for
    /// those events still get correct behaviour.
    pub fn feed(&mut self, ev: PointerEvent, now_ms: f64) -> Option<(f32, f32)> {
        match ev.phase {
            PointerPhase::Down => {
                // Begin a new tap candidate. Don't touch the prior
                // streak yet — it's the Up that decides if this is
                // a double-tap completion.
                self.pending_down = Some((ev.x, ev.y));
                None
            }
            PointerPhase::Move => {
                // A Move beyond the tap slop reclassifies the
                // gesture as a drag — kill the in-flight tap so
                // the upcoming Up can't complete a double-tap.
                if let Some((dx, dy)) = self.pending_down {
                    let dxp = ev.x - dx;
                    let dyp = ev.y - dy;
                    if (dxp * dxp + dyp * dyp).sqrt() > TAP_SLOP_PX {
                        self.pending_down = None;
                    }
                }
                None
            }
            PointerPhase::Up => {
                // Up without a paired Down (the Down was reclassified
                // as a drag, or no Down arrived) doesn't arm or
                // complete anything. Reset the streak just in case
                // we're mid-state — better to lose a streak than
                // fire a phantom double-tap.
                let Some((down_x, down_y)) = self.pending_down else {
                    self.last_up_ts = 0.0;
                    return None;
                };
                self.pending_down = None;
                // Is this the *second* tap of a pair?
                let interval = now_ms - self.last_up_ts;
                let prior_armed = self.last_up_ts > 0.0
                    && interval <= DOUBLE_TAP_MAX_INTERVAL_MS;
                let dx = down_x - self.last_down_x;
                let dy = down_y - self.last_down_y;
                let close = (dx * dx + dy * dy).sqrt() <= DOUBLE_TAP_MAX_MOVE_PX;
                if prior_armed && close {
                    // Complete — reset state so a third tap doesn't
                    // re-fire as another "second tap".
                    self.last_up_ts = 0.0;
                    return Some((ev.x, ev.y));
                }
                // Arm this tap as the candidate for the next pair.
                self.last_up_ts = now_ms;
                self.last_down_x = down_x;
                self.last_down_y = down_y;
                None
            }
            PointerPhase::Cancel => {
                // Pointer cancelled mid-gesture (OS gesture takeover,
                // touch left the surface). Reset everything.
                self.pending_down = None;
                self.last_up_ts = 0.0;
                None
            }
        }
    }

    /// Reset state without consuming an event. Apps call this when
    /// they detect that the gesture moved out of the region they
    /// care about (e.g. a Down inside the photo followed by an Up
    /// over the action row — that's not a double-tap-on-photo).
    pub fn cancel(&mut self) {
        self.pending_down = None;
        self.last_up_ts = 0.0;
    }

    /// True when a single tap has been registered and the detector
    /// is waiting for the second tap of a pair. Exposed for tests;
    /// app code rarely needs this.
    pub fn is_armed(&self) -> bool { self.last_up_ts > 0.0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn down(x: f32, y: f32) -> PointerEvent {
        PointerEvent { x, y, button: 0, phase: PointerPhase::Down }
    }
    fn up(x: f32, y: f32) -> PointerEvent {
        PointerEvent { x, y, button: 0, phase: PointerPhase::Up }
    }
    fn movev(x: f32, y: f32) -> PointerEvent {
        PointerEvent { x, y, button: 0, phase: PointerPhase::Move }
    }

    #[test]
    fn fresh_detector_is_not_armed_and_returns_none_on_any_event() {
        let mut d = DoubleTapDetector::new();
        assert!(!d.is_armed());
        assert_eq!(d.feed(down(10.0, 10.0), 0.0), None);
        assert_eq!(d.feed(movev(11.0, 11.0), 0.0), None);
    }

    #[test]
    fn single_tap_arms_but_does_not_fire() {
        let mut d = DoubleTapDetector::new();
        assert_eq!(d.feed(down(50.0, 50.0), 0.0), None);
        assert_eq!(d.feed(up(50.0, 50.0), 100.0), None);
        assert!(d.is_armed(), "after a tap the detector arms for the second tap");
    }

    #[test]
    fn second_tap_inside_window_completes_double_tap() {
        let mut d = DoubleTapDetector::new();
        let _ = d.feed(down(50.0, 50.0), 0.0);
        let _ = d.feed(up(50.0, 50.0), 100.0);
        let _ = d.feed(down(52.0, 53.0), 200.0);
        // Interval = 250 - 100 = 150 ms (within 300 ms).
        // Distance = sqrt(4+9) ≈ 3.6 px (within 32 px).
        let fired = d.feed(up(52.0, 53.0), 250.0);
        assert_eq!(fired, Some((52.0, 53.0)));
        assert!(!d.is_armed(), "after a fire the detector resets");
    }

    #[test]
    fn third_tap_after_double_does_not_re_fire_as_another_double() {
        let mut d = DoubleTapDetector::new();
        // First pair completes.
        let _ = d.feed(down(50.0, 50.0), 0.0);
        let _ = d.feed(up(50.0, 50.0), 100.0);
        let _ = d.feed(down(52.0, 53.0), 200.0);
        assert_eq!(d.feed(up(52.0, 53.0), 250.0), Some((52.0, 53.0)));
        // A third tap arrives shortly after — must NOT fire as a
        // "second tap" of a phantom pair (state was reset on
        // completion).
        let _ = d.feed(down(50.0, 50.0), 400.0);
        let fired = d.feed(up(50.0, 50.0), 500.0);
        assert_eq!(fired, None);
        assert!(d.is_armed());
    }

    #[test]
    fn second_tap_after_window_starts_a_fresh_pair() {
        let mut d = DoubleTapDetector::new();
        let _ = d.feed(down(50.0, 50.0), 0.0);
        let _ = d.feed(up(50.0, 50.0), 100.0);
        // 500 ms later — past the 300 ms window.
        let _ = d.feed(down(50.0, 50.0), 600.0);
        let fired = d.feed(up(50.0, 50.0), 700.0);
        assert_eq!(fired, None, "interval > 300 ms must not fire");
        assert!(d.is_armed(), "but the second tap re-arms for a fresh pair");
    }

    #[test]
    fn second_tap_too_far_away_starts_a_fresh_pair() {
        let mut d = DoubleTapDetector::new();
        let _ = d.feed(down(50.0, 50.0), 0.0);
        let _ = d.feed(up(50.0, 50.0), 100.0);
        // 30 px right + 30 px down ≈ 42 px (past the 32 px threshold).
        let _ = d.feed(down(80.0, 80.0), 200.0);
        let fired = d.feed(up(80.0, 80.0), 250.0);
        assert_eq!(fired, None);
        assert!(d.is_armed());
    }

    #[test]
    fn drag_within_tap_kills_the_pair_candidate() {
        let mut d = DoubleTapDetector::new();
        // First tap completes normally.
        let _ = d.feed(down(50.0, 50.0), 0.0);
        let _ = d.feed(up(50.0, 50.0), 100.0);
        // Second tap's Down lands close, but Move drags past slop.
        let _ = d.feed(down(50.0, 50.0), 200.0);
        let _ = d.feed(movev(80.0, 50.0), 210.0); // 30 px > 12 px slop
        let fired = d.feed(up(80.0, 50.0), 230.0);
        assert_eq!(fired, None, "an in-tap drag must not complete double-tap");
    }

    #[test]
    fn move_within_slop_does_not_kill_the_candidate() {
        let mut d = DoubleTapDetector::new();
        let _ = d.feed(down(50.0, 50.0), 0.0);
        let _ = d.feed(up(50.0, 50.0), 100.0);
        let _ = d.feed(down(50.0, 50.0), 200.0);
        // 6 px of drift is well within 12 px slop.
        let _ = d.feed(movev(56.0, 53.0), 210.0);
        let fired = d.feed(up(56.0, 53.0), 230.0);
        assert_eq!(fired, Some((56.0, 53.0)));
    }

    #[test]
    fn cancel_resets_the_detector() {
        let mut d = DoubleTapDetector::new();
        let _ = d.feed(down(50.0, 50.0), 0.0);
        let _ = d.feed(up(50.0, 50.0), 100.0);
        assert!(d.is_armed());
        let _ = d.feed(PointerEvent {
            x: 50.0, y: 50.0, button: 0, phase: PointerPhase::Cancel,
        }, 110.0);
        assert!(!d.is_armed(), "Cancel phase resets the armed state");
    }

    #[test]
    fn manual_cancel_resets_without_event() {
        let mut d = DoubleTapDetector::new();
        let _ = d.feed(down(50.0, 50.0), 0.0);
        let _ = d.feed(up(50.0, 50.0), 100.0);
        assert!(d.is_armed());
        d.cancel();
        assert!(!d.is_armed());
        // A subsequent tap arms fresh.
        let _ = d.feed(down(50.0, 50.0), 200.0);
        let fired = d.feed(up(50.0, 50.0), 250.0);
        assert_eq!(fired, None, "post-cancel the streak is gone — first tap only arms");
    }

    #[test]
    fn up_without_a_paired_down_does_not_fire_or_arm() {
        let mut d = DoubleTapDetector::new();
        // First tap completes (arms the detector).
        let _ = d.feed(down(50.0, 50.0), 0.0);
        let _ = d.feed(up(50.0, 50.0), 100.0);
        assert!(d.is_armed());
        // A bare Up with no Down (e.g. the Down was killed by a Move
        // outside slop, then Up arrives) must NOT consume the armed
        // streak — the streak quietly resets so it can't fire a
        // phantom double-tap on the next Up.
        let fired = d.feed(up(50.0, 50.0), 200.0);
        assert_eq!(fired, None);
        assert!(!d.is_armed(), "bare Up resets the streak — defensive against missed Downs");
    }

    #[test]
    fn boundary_interval_exactly_at_300_ms_still_fires() {
        let mut d = DoubleTapDetector::new();
        let _ = d.feed(down(50.0, 50.0), 0.0);
        let _ = d.feed(up(50.0, 50.0), 100.0);
        let _ = d.feed(down(50.0, 50.0), 200.0);
        // Interval = 400 - 100 = exactly 300 ms (boundary).
        let fired = d.feed(up(50.0, 50.0), 400.0);
        assert_eq!(fired, Some((50.0, 50.0)),
            "interval == 300 ms must still count as a double-tap (<=, not <)");
    }

    #[test]
    fn boundary_distance_exactly_at_32_px_still_fires() {
        let mut d = DoubleTapDetector::new();
        let _ = d.feed(down(0.0, 0.0), 0.0);
        let _ = d.feed(up(0.0, 0.0), 100.0);
        // Exactly 32 px right.
        let _ = d.feed(down(32.0, 0.0), 200.0);
        let fired = d.feed(up(32.0, 0.0), 250.0);
        assert_eq!(fired, Some((32.0, 0.0)),
            "distance == 32 px must still count (<=, not <)");
    }
}
