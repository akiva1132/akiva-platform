//! Calculator — Akiva's first real app.
//!
//! Phase 6 of the roadmap: exercises the Phase 3a widget toolkit (stacks,
//! grow, alignment, click-detection) on a workload more realistic than
//! hello-world. The state machine and the widget tree are intentionally
//! decoupled — the `Calculator` struct is pure Rust with no SDK types on
//! its boundaries, so the core logic can be unit-tested without a host.
//!
//! ## State machine
//!
//! Four fields carry the whole calculator state:
//!
//! - `display`   — the characters the user sees, also the parse source for
//!                 the "right-hand side" of the next operation.
//! - `acc`       — the running "left-hand side" that gets folded in as the
//!                 user chains operators.
//! - `pending`   — the operator waiting for an rhs to fold with.
//! - `just_computed` — set after `=` or a chained op, so the next digit
//!                 starts a fresh display rather than appending to a
//!                 post-computation number.
//!
//! Behaviour matches a standard pocket calculator (no operator precedence —
//! `2 + 3 × 4` evaluates to `20`, folding left-to-right). Errors
//! (divide-by-zero, non-finite results) latch until the user presses `C`.

use akiva_sdk::prelude::*;
use akiva_sdk::akml::{self, Bindings};
use akiva_sdk::res;

/// The whole UI, declared in AKML. Embedded via `include_str!` so the
/// layout ships *inside* the wasm binary — no layout loader, no I/O
/// path. When Phase 4 adds qualifier-aware layouts (`layout-land/`
/// etc.), this constant becomes a lookup against the resource manager,
/// but the source file (`res/layout/main.akml`) keeps its current
/// location.
///
/// Colours used by the layout live in [`res/values/colors.toml`],
/// referenced from AKML as `@color/name` and from this file as
/// `res::color("name")`. Keeping every palette entry there means the
/// two rendering paths (AKML-driven and imperative) can't drift.
const LAYOUT: &str = include_str!("../res/layout/main.akml");

// ── Calculator logic ──────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op { Add, Sub, Mul, Div }

impl Op {
    fn glyph(self) -> &'static str {
        match self { Op::Add => "+", Op::Sub => "−", Op::Mul => "×", Op::Div => "÷" }
    }
}

#[derive(Debug)]
pub struct Calculator {
    display: String,
    acc: f64,
    pending: Option<Op>,
    /// True right after `=` or a chained operator. The next digit press
    /// should start a new number rather than appending to `display`.
    just_computed: bool,
    /// Latched divide-by-zero / NaN / ∞. Every input except `C` is a
    /// no-op until cleared, so the user sees "Error" and knows the state
    /// is unsafe to continue from.
    error: bool,
}

impl Default for Calculator {
    fn default() -> Self { Self::new() }
}

impl Calculator {
    pub fn new() -> Self {
        Self {
            display: "0".into(),
            acc: 0.0,
            pending: None,
            just_computed: false,
            error: false,
        }
    }

    pub fn display(&self) -> &str { &self.display }
    pub fn is_error(&self) -> bool { self.error }
    pub fn pending_op(&self) -> Option<Op> { self.pending }

    pub fn clear(&mut self) {
        self.display = "0".into();
        self.acc = 0.0;
        self.pending = None;
        self.just_computed = false;
        self.error = false;
    }

    pub fn input_digit(&mut self, d: u8) {
        if self.error { return; }
        debug_assert!(d < 10);
        let ch = char::from(b'0' + d);
        if self.just_computed || self.display == "0" || self.display == "-0" {
            // Replace the placeholder '0' (keeping a leading '-' if the
            // user had already typed ± on the zero).
            let neg = self.display.starts_with('-');
            self.display = if neg { format!("-{ch}") } else { ch.to_string() };
            self.just_computed = false;
        } else if self.display.len() < MAX_INPUT_LEN {
            self.display.push(ch);
        }
        // Over-length: silently ignored — avoids the display overflowing
        // the surface and keeps parse latency bounded.
    }

    pub fn input_dot(&mut self) {
        if self.error { return; }
        if self.just_computed {
            self.display = "0.".into();
            self.just_computed = false;
            return;
        }
        if !self.display.contains('.') && self.display.len() < MAX_INPUT_LEN {
            self.display.push('.');
        }
    }

    pub fn backspace(&mut self) {
        if self.error { return; }
        if self.just_computed {
            // After a result, backspace is more useful as "clear the
            // computed value back to 0" than as "edit the shown number"
            // — editing a result is rarely what the user wants.
            self.display = "0".into();
            self.just_computed = false;
            return;
        }
        if self.display.len() > 1 && self.display != "-0" {
            self.display.pop();
            // "-" alone is not a valid display state.
            if self.display == "-" { self.display = "0".into(); }
        } else {
            self.display = "0".into();
        }
    }

    pub fn toggle_sign(&mut self) {
        if self.error { return; }
        if self.display == "0" { return; }  // don't produce "-0"
        if let Some(stripped) = self.display.strip_prefix('-') {
            self.display = stripped.to_string();
        } else {
            self.display.insert(0, '-');
        }
    }

    pub fn percent(&mut self) {
        if self.error { return; }
        let v = self.parse_display() / 100.0;
        self.display = format_number(v);
        // Percent is a terminal edit on the current display, not a fold
        // into the accumulator — so we keep `just_computed = false`
        // (next digit would still replace, matching iOS behaviour).
    }

    pub fn apply_op(&mut self, op: Op) {
        if self.error { return; }
        let rhs = self.parse_display();
        if let Some(prev) = self.pending.take() {
            if !self.just_computed {
                // Fold: acc = acc PREV rhs. The fold is skipped right
                // after =/op so "3 + + −" only keeps the last operator.
                match compute(self.acc, prev, rhs) {
                    Some(v) => { self.acc = v; self.display = format_number(v); }
                    None    => { self.error = true; self.display = "Error".into(); return; }
                }
            }
        } else {
            // First operator in a chain: seed the accumulator with the
            // current display.
            self.acc = rhs;
        }
        self.pending = Some(op);
        self.just_computed = true;
    }

    pub fn equals(&mut self) {
        if self.error { return; }
        let rhs = self.parse_display();
        if let Some(op) = self.pending.take() {
            match compute(self.acc, op, rhs) {
                Some(v) => { self.acc = v; self.display = format_number(v); }
                None    => { self.error = true; self.display = "Error".into(); return; }
            }
        } else {
            self.acc = rhs;
        }
        self.just_computed = true;
    }

    fn parse_display(&self) -> f64 {
        // `parse::<f64>` rejects a trailing dot; accept it as 123.0.
        let trimmed = self.display.trim_end_matches('.');
        trimmed.parse::<f64>().unwrap_or(0.0)
    }
}

fn compute(lhs: f64, op: Op, rhs: f64) -> Option<f64> {
    let v = match op {
        Op::Add => lhs + rhs,
        Op::Sub => lhs - rhs,
        Op::Mul => lhs * rhs,
        Op::Div => if rhs == 0.0 { return None; } else { lhs / rhs },
    };
    if v.is_finite() { Some(v) } else { None }
}

/// Longest input string we let the user build up. The display font is
/// sized for the surface, and beyond this many chars it starts clipping
/// the right edge; capping input gives consistent behaviour across
/// devices without the calculator needing to know its own pixel size.
const MAX_INPUT_LEN: usize = 16;

/// Stringify a computed `f64` so it re-parses cleanly and looks like a
/// number a human would read. We never print scientific notation — the
/// calculator isn't for that audience.
fn format_number(v: f64) -> String {
    if !v.is_finite() { return "Error".into(); }
    // Integer-valued within safe range: no decimal point.
    if v.fract() == 0.0 && v.abs() < 1e15 {
        return format!("{}", v as i64);
    }
    // Else: print with enough precision to recover the value, then trim
    // trailing zeros. `{:.10}` balances precision against overflowing
    // the display for common inputs.
    let s = format!("{v:.10}");
    let trimmed = s.trim_end_matches('0').trim_end_matches('.');
    trimmed.to_string()
}

// ── Activity glue ──────────────────────────────────────────────────────

#[derive(Default)]
pub struct CalculatorMain {
    ui: UiState,
    calc: Calculator,
}

impl Activity for CalculatorMain {
    fn on_create(&mut self, ctx: &mut Context<'_>) {
        log_info!("CalculatorMain created ({}x{} @ {:.1}x)",
            ctx.metrics.width_px, ctx.metrics.height_px, ctx.metrics.density);
    }

    fn on_pointer(&mut self, _ctx: &mut Context<'_>, ev: PointerEvent) {
        self.ui.feed_pointer(ev);
    }

    /// Persist the arithmetic state machine across process kills. Only
    /// `Calculator` is saved — `UiState` is reconstructible from the
    /// next frame's input and would inflate the blob for nothing (hit
    /// rects etc. depend on the current layout, which re-measures from
    /// scratch anyway). Keys are short on purpose: the whole blob is
    /// well under 100 bytes for any reachable state, leaving headroom
    /// for future fields without approaching the 64 KiB cap.
    fn on_save_state(&self, state: &mut SavedState) {
        state.put_string("d", &self.calc.display);
        state.put_float("a", self.calc.acc);
        state.put_int("p", match self.calc.pending {
            None         => -1,
            Some(Op::Add) => 0,
            Some(Op::Sub) => 1,
            Some(Op::Mul) => 2,
            Some(Op::Div) => 3,
        });
        state.put_bool("j", self.calc.just_computed);
        state.put_bool("e", self.calc.error);
    }

    fn on_restore_state(&mut self, state: &SavedState) {
        if let Some(d) = state.get_string("d") { self.calc.display = d.to_string(); }
        if let Some(a) = state.get_float("a")  { self.calc.acc = a; }
        self.calc.pending = match state.get_int("p").unwrap_or(-1) {
            0 => Some(Op::Add),
            1 => Some(Op::Sub),
            2 => Some(Op::Mul),
            3 => Some(Op::Div),
            _ => None,
        };
        if let Some(j) = state.get_bool("j") { self.calc.just_computed = j; }
        if let Some(e) = state.get_bool("e") { self.calc.error = e; }
        log_info!("Calculator restored: display={:?} acc={} pending={:?}",
            self.calc.display, self.calc.acc, self.calc.pending);
    }

    fn on_render(&mut self, ctx: &mut Context<'_>, c: &mut Canvas<'_>) {
        c.clear(res::color("bg_window"));

        // Dynamic values the layout's `{name}` references will consume.
        // Building a fresh Bindings per frame is intentional — it keeps
        // lifetimes local and the allocation cost is rounding error
        // next to paint-op serialization.
        //
        // The display colour is data-driven (flips on error), so it
        // goes through the per-frame bindings layer rather than an
        // `@color/` ref. `display_fg` / `display_fg_error` still live
        // in colors.toml as the source of truth; we just pick between
        // them here.
        let hint = self.calc.pending_op().map(Op::glyph).unwrap_or(" ");
        let display_color = if self.calc.is_error() {
            res::color("display_fg_error")
        } else {
            res::color("display_fg")
        };
        let bindings = Bindings::new()
            .text("hint", hint)
            .text("display", self.calc.display())
            .color("display_color", display_color);

        let mut ui = Ui::new(&mut self.ui, ctx, c);
        let root = akml::inflate_str_with(&mut ui, LAYOUT, &bindings)
            .expect("calculator layout is statically valid — inflate failure is a build bug");
        ui.present(root);

        // ── Dispatch clicks ──
        // Each `<Button>` in the AKML carries an `onClick="…"` naming a
        // semantic action; the toolkit resolves pointer-down events to
        // those action strings. The Activity only knows these action
        // names — not widget ids — so AKML can rename `id="eq"` to
        // `id="equal_btn"` or split one button into two without
        // touching any code here.
        // Digit actions are `digit_0` .. `digit_9`. Hard-coded table
        // beats a runtime `format!` (no alloc per frame) and beats an
        // unsafe `from_utf8_unchecked` roll-your-own (ten short strings
        // aren't worth bypassing std's checks).
        const DIGIT_ACTIONS: [&str; 10] = [
            "digit_0", "digit_1", "digit_2", "digit_3", "digit_4",
            "digit_5", "digit_6", "digit_7", "digit_8", "digit_9",
        ];
        for (d, action) in DIGIT_ACTIONS.iter().enumerate() {
            if ui.was_triggered(action) {
                self.calc.input_digit(d as u8);
                log_debug!("digit {} → {}", d, self.calc.display());
            }
        }
        if ui.was_triggered("input_dot")     { self.calc.input_dot(); }
        if ui.was_triggered("apply_add")     { self.calc.apply_op(Op::Add); }
        if ui.was_triggered("apply_sub")     { self.calc.apply_op(Op::Sub); }
        if ui.was_triggered("apply_mul")     { self.calc.apply_op(Op::Mul); }
        if ui.was_triggered("apply_div")     { self.calc.apply_op(Op::Div); }
        if ui.was_triggered("equals")        { self.calc.equals(); log_debug!("= → {}", self.calc.display()); }
        if ui.was_triggered("clear")         { self.calc.clear(); }
        if ui.was_triggered("toggle_sign")   { self.calc.toggle_sign(); }
        if ui.was_triggered("percent")       { self.calc.percent(); }
        if ui.was_triggered("backspace")     { self.calc.backspace(); }
    }
}

akiva_sdk::akiva_app!(CalculatorMain);

// ── Tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn digits(c: &mut Calculator, s: &str) {
        for ch in s.chars() {
            match ch {
                '0'..='9' => c.input_digit(ch.to_digit(10).unwrap() as u8),
                '.'       => c.input_dot(),
                _ => panic!("bad digit char: {ch}"),
            }
        }
    }

    #[test]
    fn new_calculator_shows_zero() {
        let c = Calculator::new();
        assert_eq!(c.display(), "0");
        assert_eq!(c.pending_op(), None);
        assert!(!c.is_error());
    }

    #[test]
    fn digits_append_to_display() {
        let mut c = Calculator::new();
        digits(&mut c, "123");
        assert_eq!(c.display(), "123");
    }

    #[test]
    fn first_digit_replaces_the_placeholder_zero() {
        let mut c = Calculator::new();
        c.input_digit(5);
        assert_eq!(c.display(), "5");
    }

    #[test]
    fn decimal_point_inserts_only_once() {
        let mut c = Calculator::new();
        digits(&mut c, "1.2");
        c.input_dot();
        assert_eq!(c.display(), "1.2");
    }

    #[test]
    fn dot_on_fresh_display_produces_leading_zero() {
        let mut c = Calculator::new();
        c.input_dot();
        assert_eq!(c.display(), "0.");
    }

    #[test]
    fn backspace_removes_last_char() {
        let mut c = Calculator::new();
        digits(&mut c, "123");
        c.backspace();
        assert_eq!(c.display(), "12");
    }

    #[test]
    fn backspace_on_single_digit_goes_to_zero() {
        let mut c = Calculator::new();
        c.input_digit(7);
        c.backspace();
        assert_eq!(c.display(), "0");
    }

    #[test]
    fn toggle_sign_adds_minus() {
        let mut c = Calculator::new();
        digits(&mut c, "42");
        c.toggle_sign();
        assert_eq!(c.display(), "-42");
        c.toggle_sign();
        assert_eq!(c.display(), "42");
    }

    #[test]
    fn toggle_sign_on_zero_is_noop() {
        let mut c = Calculator::new();
        c.toggle_sign();
        assert_eq!(c.display(), "0");  // no "-0"
    }

    #[test]
    fn simple_addition() {
        // 2 + 3 = 5
        let mut c = Calculator::new();
        c.input_digit(2);
        c.apply_op(Op::Add);
        c.input_digit(3);
        c.equals();
        assert_eq!(c.display(), "5");
    }

    #[test]
    fn chained_ops_fold_left_to_right() {
        // 2 + 3 × 4 = (2+3)×4 = 20, since no precedence.
        let mut c = Calculator::new();
        c.input_digit(2);
        c.apply_op(Op::Add);
        c.input_digit(3);
        c.apply_op(Op::Mul);
        c.input_digit(4);
        c.equals();
        assert_eq!(c.display(), "20");
    }

    #[test]
    fn subtraction_with_decimals() {
        // 5.5 - 2.25 = 3.25
        let mut c = Calculator::new();
        digits(&mut c, "5.5");
        c.apply_op(Op::Sub);
        digits(&mut c, "2.25");
        c.equals();
        assert_eq!(c.display(), "3.25");
    }

    #[test]
    fn division_by_zero_latches_error() {
        let mut c = Calculator::new();
        c.input_digit(5);
        c.apply_op(Op::Div);
        c.input_digit(0);
        c.equals();
        assert!(c.is_error());
        assert_eq!(c.display(), "Error");
        // Further input is ignored until clear.
        c.input_digit(7);
        assert_eq!(c.display(), "Error");
        c.clear();
        assert!(!c.is_error());
        assert_eq!(c.display(), "0");
    }

    #[test]
    fn changing_operator_overrides_the_pending_one() {
        // 8 + - 2 = should act like 8 - 2 = 6 (the "+" is replaced,
        // not folded in against a still-empty rhs).
        let mut c = Calculator::new();
        c.input_digit(8);
        c.apply_op(Op::Add);
        c.apply_op(Op::Sub);
        c.input_digit(2);
        c.equals();
        assert_eq!(c.display(), "6");
    }

    #[test]
    fn digit_after_equals_starts_fresh() {
        // 2 + 3 = 5; then typing 7 should show "7", not "57".
        let mut c = Calculator::new();
        c.input_digit(2); c.apply_op(Op::Add); c.input_digit(3); c.equals();
        assert_eq!(c.display(), "5");
        c.input_digit(7);
        assert_eq!(c.display(), "7");
    }

    #[test]
    fn percent_divides_current_display_by_100() {
        let mut c = Calculator::new();
        digits(&mut c, "50");
        c.percent();
        assert_eq!(c.display(), "0.5");
    }

    #[test]
    fn clear_wipes_every_field() {
        let mut c = Calculator::new();
        c.input_digit(4);
        c.apply_op(Op::Add);
        c.input_digit(9);
        c.clear();
        assert_eq!(c.display(), "0");
        assert_eq!(c.pending_op(), None);
        assert!(!c.is_error());
    }

    #[test]
    fn input_length_is_capped() {
        // We never want the display string to grow past MAX_INPUT_LEN —
        // once it hits the cap, additional digits are silently dropped.
        let mut c = Calculator::new();
        for _ in 0..MAX_INPUT_LEN + 5 { c.input_digit(9); }
        assert_eq!(c.display().len(), MAX_INPUT_LEN);
    }

    #[test]
    fn integer_result_has_no_trailing_decimal() {
        // Floating-point math naturally produces a tiny fractional part
        // for many integer-valued results; `format_number` must clean
        // that up so the user sees "5", not "5.0000000001".
        let mut c = Calculator::new();
        digits(&mut c, "2.5");
        c.apply_op(Op::Mul);
        digits(&mut c, "2");
        c.equals();
        assert_eq!(c.display(), "5");
    }
}
