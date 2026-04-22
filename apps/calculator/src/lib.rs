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

// ── Palette ───────────────────────────────────────────────────────────

const COL_BG:          u32 = 0xff15172a;
const COL_DISPLAY_FG:  u32 = 0xfff8faff;
const COL_ERROR_FG:    u32 = 0xffff7b7b;
const COL_HINT_FG:     u32 = 0xff7a8398;

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

    fn on_render(&mut self, ctx: &mut Context<'_>, c: &mut Canvas<'_>) {
        c.clear(COL_BG);
        let mut ui = Ui::new(&mut self.ui, ctx, c);

        // ── Display ──
        // Right-aligned big number. We put it inside an HStack with
        // main=End + cross=Center so the text hugs the right edge
        // regardless of how wide the surface is.
        let hint = self.calc.pending_op().map(Op::glyph).unwrap_or(" ");
        let hint_node = ui.text(hint).size(22.0).color(COL_HINT_FG).build();
        let display_node = ui.text(self.calc.display())
            .size(48.0)
            .color(if self.calc.is_error() { COL_ERROR_FG } else { COL_DISPLAY_FG })
            .build();
        let display_row = ui.h_stack(&[hint_node, display_node])
            .gap(12.0)
            .main(MainAxis::End)
            .cross(CrossAxis::Center)
            .padding_trbl(12.0, 16.0, 12.0, 16.0)
            .build();

        // ── Keypad ──
        //
        // 5 rows × 4 columns. Every button uses grow=1 so they all share
        // the available space equally; every row uses grow=1 so each row
        // gets the same height. Pressing any cell rebuilds the tree and
        // re-lays-out — the toolkit is immediate-mode, so nothing to
        // diff across frames.
        let row1 = keypad_row(&mut ui, &[("clr", "C"), ("sign", "±"), ("pct", "%"), ("div", "÷")]);
        let row2 = keypad_row(&mut ui, &[("7", "7"), ("8", "8"), ("9", "9"), ("mul", "×")]);
        let row3 = keypad_row(&mut ui, &[("4", "4"), ("5", "5"), ("6", "6"), ("sub", "−")]);
        let row4 = keypad_row(&mut ui, &[("1", "1"), ("2", "2"), ("3", "3"), ("add", "+")]);
        let row5 = keypad_row(&mut ui, &[("0", "0"), ("dot", "."), ("back", "⌫"), ("eq", "=")]);

        let keypad = ui.v_stack(&[row1, row2, row3, row4, row5])
            .gap(8.0)
            .cross(CrossAxis::Stretch)
            .grow(1.0)
            .build();

        let root = ui.v_stack(&[display_row, keypad])
            .gap(8.0)
            .padding(16.0)
            .cross(CrossAxis::Stretch)
            .build();

        ui.present(root);

        // ── Dispatch clicks ──
        for d in 0u8..=9 {
            let label = (b'0' + d) as char;
            let mut buf = [0u8; 4];
            let id = label.encode_utf8(&mut buf);
            if ui.was_clicked(id) {
                self.calc.input_digit(d);
                log_debug!("digit {} → {}", d, self.calc.display());
            }
        }
        if ui.was_clicked("dot")  { self.calc.input_dot(); }
        if ui.was_clicked("add")  { self.calc.apply_op(Op::Add); }
        if ui.was_clicked("sub")  { self.calc.apply_op(Op::Sub); }
        if ui.was_clicked("mul")  { self.calc.apply_op(Op::Mul); }
        if ui.was_clicked("div")  { self.calc.apply_op(Op::Div); }
        if ui.was_clicked("eq")   { self.calc.equals(); log_debug!("= → {}", self.calc.display()); }
        if ui.was_clicked("clr")  { self.calc.clear(); }
        if ui.was_clicked("sign") { self.calc.toggle_sign(); }
        if ui.was_clicked("pct")  { self.calc.percent(); }
        if ui.was_clicked("back") { self.calc.backspace(); }
    }
}

/// Build one keypad row: four grow=1 buttons inside a cross=Stretch
/// HStack that itself grows so all five rows split the keypad height
/// evenly. Buttons carry stable string ids so `was_clicked` can
/// correlate a tap back to the action regardless of label rendering.
fn keypad_row(ui: &mut Ui<'_, '_, '_>, cells: &[(&str, &str)]) -> akiva_sdk::ui::node::NodeId {
    let ids: Vec<_> = cells.iter().map(|(id, label)| {
        let b = ui.button(*id, *label);
        ui.grow(b, 1.0);
        b
    }).collect();
    ui.h_stack(&ids)
        .gap(8.0)
        .cross(CrossAxis::Stretch)
        .grow(1.0)
        .build()
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
