//! Spring animations — the third animation primitive alongside
//! fling (`Stage 2c.4`, exponential decay) and snap (`Stage 2d.3`,
//! cubic-out ease).
//!
//! ## Why springs
//!
//! Fling is a one-shot decay; snap is a fixed-duration ease. Neither
//! captures the "release-and-let-it-fall-back" feel of a physically
//! suspended object — the canonical gesture for:
//!   - overscroll bounce at the end of a list,
//!   - drag-and-release-to-origin (chips, sliders, sheets),
//!   - small UI affordances like button-press scale flicker.
//!
//! Compose models this with `SpringAnimationSpec(stiffness,
//! dampingRatio)`; iOS with `UISpringTimingParameters`. Both solve
//! the underlying ODE in closed form; we use semi-implicit Euler
//! because:
//!   - one tick = three multiplies and two adds (no `exp`/`sin`),
//!   - the maths is the same across critical / over / underdamped
//!     regimes — no branching by damping ratio,
//!   - the dt-clamp already in place from Stage 2c.4 (max 4 frames
//!     per tick) keeps the integrator stable for every stiffness in
//!     the preset range.
//!
//! ## Equation of motion
//!
//! For position `x`, velocity `v`, target `xt`, stiffness `k`, and
//! damping ratio `ζ` (zeta), the spring has natural angular frequency
//! `ω0 = √k` and damping coefficient `c = 2 * ζ * ω0`. Each tick:
//!
//! ```text
//!   a = -k * (x - xt) - c * v
//!   v += a * dt
//!   x += v * dt
//! ```
//!
//! `ζ = 1` is critical damping (single overshoot then settle —
//! actually no overshoot under Euler, just decay), `ζ > 1` is over-
//! damped (slow settle), `ζ < 1` is underdamped (visible bounce).
//!
//! ## Units
//!
//! `stiffness` is dimensionally `1/s²` (per-second-squared); `dt` is
//! in *seconds* inside the math but the public API takes `dt_ms`
//! to match the rest of the SDK. We divide by 1000 internally.
//! Velocity is in `value-units-per-second` (so a position in `px`
//! gives velocity in `px/s`).

/// Spring physics configuration. Construct via the presets
/// (`Spring::MEDIUM`, …) or a struct literal for custom feel.
///
/// `stiffness` controls how fast the spring pulls toward target;
/// `damping_ratio` controls how much it overshoots before settling.
/// `epsilon` is the per-axis tolerance — when both `|x - target|`
/// and `|v|` fall below it, [`Spring::settled`] returns true.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Spring {
    /// Stiffness, `1/s²`. Higher = snappier. Default-grade values:
    /// 200 (low), 700 (medium), 1500 (high), 10000 (very high).
    pub stiffness: f32,
    /// Damping ratio (`ζ`). `1.0` = critical (no overshoot under
    /// Euler), `< 1.0` = underdamped (bounces past target),
    /// `> 1.0` = overdamped (slow approach). Useful range:
    /// `0.2 .. 1.5`.
    pub damping_ratio: f32,
    /// Settling tolerance. The spring is "settled" when both
    /// `|value - target|` and `|velocity|` fall below this. Keep
    /// epsilon small relative to your scale (0.01 px for screen
    /// coords; 0.001 for normalized 0..1 values).
    pub epsilon: f32,
}

impl Default for Spring {
    fn default() -> Self { Self::MEDIUM }
}

impl Spring {
    /// Critical-damped, low stiffness (~1.5 s settle from rest).
    /// Slow / gentle return — useful for sheets and panels.
    pub const LOW: Spring = Spring {
        stiffness: 200.0, damping_ratio: 1.0, epsilon: 0.01,
    };
    /// Critical-damped, medium stiffness (~0.8 s settle). The
    /// default for app-owned animations and the spring used by
    /// scroll overscroll bounce.
    pub const MEDIUM: Spring = Spring {
        stiffness: 700.0, damping_ratio: 1.0, epsilon: 0.01,
    };
    /// Critical-damped, high stiffness (~0.4 s settle). Use for
    /// button-press flicker and other fast affordances.
    pub const HIGH: Spring = Spring {
        stiffness: 1500.0, damping_ratio: 1.0, epsilon: 0.01,
    };
    /// Critical-damped, very high stiffness (~0.15 s settle).
    /// Approaches a step jump; useful only when the animation is
    /// meant to be barely perceptible.
    pub const VERY_HIGH: Spring = Spring {
        stiffness: 10000.0, damping_ratio: 1.0, epsilon: 0.01,
    };
    /// Bouncy preset — moderate stiffness, low damping. Single
    /// visible overshoot of ~15 % then settle. Good default for
    /// drag-and-release demos.
    pub const BOUNCY: Spring = Spring {
        stiffness: 400.0, damping_ratio: 0.5, epsilon: 0.01,
    };

    /// One semi-implicit Euler step. Returns `(new_value,
    /// new_velocity)`. `dt_ms` is the wall-clock delta since the
    /// last tick; values up to ~67 ms (4 frames at 60fps) are
    /// stable for every preset.
    ///
    /// `dt_ms <= 0` returns the input unchanged — saves the caller
    /// from a branch when its frame timer happens to fire twice
    /// at the same `ts_ms`.
    pub fn tick(&self, value: f32, velocity: f32, target: f32, dt_ms: f32) -> (f32, f32) {
        if dt_ms <= 0.0 {
            return (value, velocity);
        }
        // Convert ms → s. Clamp to 4 frames-equivalent (≈ 67 ms);
        // the same guard the fling/snap path uses against a
        // tab-resume monster-dt.
        let dt = (dt_ms / 1000.0).min(4.0 / 60.0);
        let k = self.stiffness.max(0.0);
        let omega0 = k.sqrt();
        let c = 2.0 * self.damping_ratio.max(0.0) * omega0;
        // Sub-step the integrator so `k * sub_dt² + c * sub_dt < 1`
        // — the stability bound for semi-implicit Euler with both
        // a restoring force and damping. Solving the quadratic
        // `k*x² + c*x - 1 = 0` gives the max stable sub_dt. Without
        // this, Spring::VERY_HIGH (k = 10000) diverges at 60fps and
        // definitely at the 4-frame clamp.
        //
        // For k = 10 000, ζ = 1 → sub_dt ≈ 4.1 ms (4 substeps per
        // frame). For k = 700 (MEDIUM) → sub_dt ≈ 19 ms (1 substep).
        // Cost: one extra mul/add per substep; worst case 8 per
        // frame, well below the noise floor.
        let target_sub_dt = if k > 0.0 {
            let disc = c * c + 4.0 * k;
            (-c + disc.sqrt()) / (2.0 * k)
        } else {
            dt
        };
        let n_substeps = ((dt / target_sub_dt).ceil() as i32).max(1) as i32;
        let sub_dt = dt / n_substeps as f32;
        let mut x = value;
        let mut v = velocity;
        for _ in 0..n_substeps {
            // Semi-implicit Euler: velocity first (using current
            // position), then position from the new velocity.
            let acc = -k * (x - target) - c * v;
            v += acc * sub_dt;
            x += v * sub_dt;
        }
        (x, v)
    }

    /// True when the spring has settled within `epsilon` of `target`
    /// AND its velocity magnitude is also below `epsilon`. Both
    /// checks matter: a fast-moving spring passing through `target`
    /// is *not* settled.
    pub fn settled(&self, value: f32, velocity: f32, target: f32) -> bool {
        (value - target).abs() <= self.epsilon
            && velocity.abs() <= self.epsilon
    }
}

/// App-owned animated `f32`. Carry one per animated property in
/// your `Activity`'s state struct; tick it once per frame
/// (`on_render` is the easy place — the wall-clock `dt_ms` is
/// available via `ctx.timestamp_ms`).
///
/// ```ignore
/// let mut bounce = AnimatedValue::new(0.0);
/// bounce.spring_to(1.0, Spring::BOUNCY);
/// // …each frame:
/// bounce.tick(16.667);
/// let x = bounce.value();
/// ```
///
/// State persists across rebuilds (it lives in your struct, not
/// `Ui` — same pattern as `ScrollCell` for scroll regions).
#[derive(Debug, Clone, Copy)]
pub struct AnimatedValue<T> {
    value: T,
    velocity: T,
    target: T,
    spring: Spring,
}

impl AnimatedValue<f32> {
    /// Construct at rest at `initial` with no active animation.
    /// `target = initial` so `is_settled()` returns true straight
    /// away; the first `spring_to(...)` call starts the motion.
    pub fn new(initial: f32) -> Self {
        Self {
            value: initial, velocity: 0.0,
            target: initial, spring: Spring::MEDIUM,
        }
    }

    /// Jump to `value` and zero velocity. Cancels any in-flight
    /// spring. Use for "snap to initial state" between
    /// presentation modes.
    pub fn set_immediately(&mut self, value: f32) {
        self.value = value;
        self.velocity = 0.0;
        self.target = value;
    }

    /// Begin (or retarget) a spring animation toward `target` with
    /// the given config. Velocity is preserved — a value moving
    /// at 50 px/s and then re-targeted from 100 → 200 keeps its
    /// 50 px/s so the user sees continuous motion, not a hitch.
    pub fn spring_to(&mut self, target: f32, spring: Spring) {
        self.target = target;
        self.spring = spring;
    }

    /// Advance the animation by `dt_ms`. Cheap no-op when already
    /// settled, so apps can call it unconditionally each frame.
    pub fn tick(&mut self, dt_ms: f32) {
        if self.is_settled() { return; }
        let (v, vel) = self.spring.tick(self.value, self.velocity, self.target, dt_ms);
        self.value = v;
        self.velocity = vel;
        // Snap exactly to target once below epsilon — avoids a
        // long asymptotic tail and stops `is_settled` from
        // flickering across frames.
        if self.spring.settled(self.value, self.velocity, self.target) {
            self.value = self.target;
            self.velocity = 0.0;
        }
    }

    /// Current value. Read each frame to drive layout / paint.
    pub fn value(&self) -> f32 { self.value }

    /// Current velocity. Useful for compositions where a child's
    /// motion depends on its parent's velocity (e.g. parallax).
    pub fn velocity(&self) -> f32 { self.velocity }

    /// Current target — the value the spring is heading toward.
    pub fn target(&self) -> f32 { self.target }

    /// True when value and velocity are both within `spring.epsilon`
    /// of rest at `target`. Apps can use this to skip the per-frame
    /// `tick` when nothing's moving.
    pub fn is_settled(&self) -> bool {
        self.spring.settled(self.value, self.velocity, self.target)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Drive a spring to settle (or fail) and return the number of
    /// 60fps ticks needed. Capped at 600 frames (10 s of simulated
    /// time) — anything that takes longer is a bug.
    fn ticks_to_settle(
        spring: Spring, mut value: f32, target: f32,
    ) -> Option<u32> {
        let mut velocity = 0.0;
        let dt_ms = 1000.0 / 60.0;
        for i in 0..600 {
            if spring.settled(value, velocity, target) { return Some(i); }
            let (v, vel) = spring.tick(value, velocity, target, dt_ms);
            value = v;
            velocity = vel;
        }
        None
    }

    #[test]
    fn spring_critical_damped_settles_without_overshoot() {
        // Critical damping (ζ = 1) under semi-implicit Euler never
        // overshoots the target.
        let spring = Spring::MEDIUM;
        let mut value = 0.0_f32;
        let mut velocity = 0.0_f32;
        let target = 1.0;
        let dt_ms = 1000.0 / 60.0;
        let mut max_value: f32 = value;
        for _ in 0..120 {
            let (v, vel) = spring.tick(value, velocity, target, dt_ms);
            value = v;
            velocity = vel;
            if value > max_value { max_value = value; }
        }
        // Under critical damping the value approaches target
        // monotonically from below — never exceeds it.
        assert!(max_value <= target + spring.epsilon,
            "critical-damped spring overshot: max={max_value}");
    }

    #[test]
    fn spring_settles_in_finite_time() {
        // All three default presets must converge inside the
        // 10-second sanity budget.
        for spring in [Spring::LOW, Spring::MEDIUM, Spring::HIGH, Spring::VERY_HIGH] {
            let n = ticks_to_settle(spring, 0.0, 1.0);
            assert!(n.is_some(), "spring {spring:?} failed to settle");
        }
    }

    #[test]
    fn spring_high_stiffness_settles_faster_than_low() {
        let low = ticks_to_settle(Spring::LOW, 0.0, 1.0).unwrap();
        let high = ticks_to_settle(Spring::HIGH, 0.0, 1.0).unwrap();
        assert!(high < low,
            "expected HIGH stiffness ({high} ticks) to be faster than LOW ({low})");
    }

    #[test]
    fn spring_bouncy_overshoots_target() {
        // Underdamped (ζ < 1) must visibly pass the target on its
        // way to settle.
        let spring = Spring::BOUNCY;
        let mut value = 0.0_f32;
        let mut velocity = 0.0_f32;
        let target = 1.0;
        let dt_ms = 1000.0 / 60.0;
        let mut max_value: f32 = value;
        for _ in 0..240 {
            let (v, vel) = spring.tick(value, velocity, target, dt_ms);
            value = v;
            velocity = vel;
            if value > max_value { max_value = value; }
        }
        assert!(max_value > target + 0.05,
            "bouncy spring failed to overshoot: max={max_value}");
    }

    #[test]
    fn spring_settled_requires_low_velocity() {
        // A spring passing exactly through target with non-zero
        // velocity is not settled — the velocity check matters.
        let spring = Spring::MEDIUM;
        assert!(!spring.settled(1.0, 5.0, 1.0));
        assert!(spring.settled(1.0, 0.0, 1.0));
        assert!(spring.settled(1.005, 0.005, 1.0));
    }

    #[test]
    fn spring_zero_dt_is_noop() {
        let spring = Spring::MEDIUM;
        let (v, vel) = spring.tick(0.5, 1.0, 1.0, 0.0);
        assert_eq!(v, 0.5);
        assert_eq!(vel, 1.0);
        let (v2, vel2) = spring.tick(0.5, 1.0, 1.0, -10.0);
        assert_eq!(v2, 0.5);
        assert_eq!(vel2, 1.0);
    }

    #[test]
    fn spring_huge_dt_clamps_to_four_frames() {
        // A tab-resume can deliver dt_ms = 500 ms. The clamp keeps
        // the integrator from blowing up.
        let spring = Spring::VERY_HIGH;
        let (v, vel) = spring.tick(0.0, 0.0, 1.0, 5000.0);
        // Under a 4-frame clamp dt = 67 ms and stiffness 10 000:
        // acc = -10000 * (-1) = +10000; v = 10000 * 0.067 = 670;
        // x = 670 * 0.067 ≈ 44.9. Without the clamp x would be
        // 10000 * 5 = 50000 — clearly out of bounds.
        assert!(v.is_finite() && v.abs() < 100.0,
            "huge dt produced unstable value: {v}");
        assert!(vel.is_finite());
    }

    // ── AnimatedValue<f32> ───────────────────────────────────────────

    #[test]
    fn animated_value_new_is_immediately_settled() {
        let av = AnimatedValue::new(0.0_f32);
        assert!(av.is_settled());
        assert_eq!(av.value(), 0.0);
        assert_eq!(av.velocity(), 0.0);
        assert_eq!(av.target(), 0.0);
    }

    #[test]
    fn animated_value_spring_to_advances_toward_target() {
        let mut av = AnimatedValue::new(0.0_f32);
        av.spring_to(1.0, Spring::MEDIUM);
        // Take a few ticks and observe motion toward target.
        let mut prev = av.value();
        for _ in 0..10 {
            av.tick(16.667);
            assert!(av.value() >= prev,
                "critical-damped spring must approach monotonically");
            prev = av.value();
        }
        // Run until settled.
        for _ in 0..200 {
            av.tick(16.667);
            if av.is_settled() { break; }
        }
        assert!(av.is_settled());
        assert!((av.value() - 1.0).abs() < 0.02);
    }

    #[test]
    fn animated_value_set_immediately_cancels_spring() {
        let mut av = AnimatedValue::new(0.0_f32);
        av.spring_to(1.0, Spring::MEDIUM);
        av.tick(16.667);
        // Not yet settled.
        assert!(!av.is_settled());
        av.set_immediately(0.5);
        assert!(av.is_settled());
        assert_eq!(av.value(), 0.5);
        assert_eq!(av.velocity(), 0.0);
        assert_eq!(av.target(), 0.5);
    }

    #[test]
    fn animated_value_retarget_preserves_velocity() {
        // Mid-flight retarget: velocity carries over so motion
        // stays continuous (the user shouldn't see a hitch).
        let mut av = AnimatedValue::new(0.0_f32);
        av.spring_to(1.0, Spring::MEDIUM);
        for _ in 0..5 { av.tick(16.667); }
        let v_before = av.velocity();
        assert!(v_before > 0.0, "spring should be moving forward");
        av.spring_to(2.0, Spring::MEDIUM);
        assert_eq!(av.velocity(), v_before,
            "retarget must preserve velocity");
        assert_eq!(av.target(), 2.0);
    }

    #[test]
    fn animated_value_tick_when_settled_is_noop() {
        let mut av = AnimatedValue::new(0.5_f32);
        // Settled at construction. Tick should be cheap and not move.
        for _ in 0..50 { av.tick(16.667); }
        assert_eq!(av.value(), 0.5);
        assert_eq!(av.velocity(), 0.0);
    }

    #[test]
    fn animated_value_snaps_exactly_to_target_at_settle() {
        // Once the spring's settled threshold is reached, value =
        // target exactly. Prevents an asymptotic 0.9999... tail.
        let mut av = AnimatedValue::new(0.0_f32);
        av.spring_to(1.0, Spring::HIGH);
        for _ in 0..200 {
            av.tick(16.667);
            if av.is_settled() { break; }
        }
        assert_eq!(av.value(), 1.0);
        assert_eq!(av.velocity(), 0.0);
    }
}
