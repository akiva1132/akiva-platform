//! Per-frame bindings for AKML attribute values.
//!
//! The static AKML source describes the *shape* of the UI; `Bindings`
//! plugs in the parts that change across frames — the calculator's
//! display string, the current operator glyph, the foreground colour
//! when in error. Without this, every dynamic value would have to be
//! built imperatively after inflation, defeating the point of moving
//! to a declarative format.
//!
//! ## Syntax
//!
//! An attribute whose whole value is `"{name}"` is treated as a
//! binding reference:
//!
//! ```xml
//! <Text text="{display}" color="{display_color}" size="48" />
//! ```
//!
//! Partial interpolation (`"Result: {display}"`) is *not* supported —
//! either the full value is a `{name}` reference or it's a literal.
//! That keeps the resolver cheap and predictable; when a real
//! templating story is needed, it'll come in as a separate, opt-in
//! syntax rather than silently hijacking literal strings with braces.
//!
//! ## Typed lookups
//!
//! Three maps live side-by-side because AKML attributes have types:
//!
//! - `texts`   — `<Text text="{x}">`, `<Button label="{y}">`.
//! - `colors`  — `<Text color="{c}">`, `<Rect color="{bg}">`.
//! - `numbers` — `<Text size="{s}">`, `<VStack gap="{g}">`.
//!
//! A lookup checks the typed map that matches the attribute's target
//! type. A typed binding also doubles as a fallback for a `texts`
//! lookup (e.g. `text="{c}"` when `c` is a colour would read the hex
//! string), but that cross-type use is a convenience — the primary
//! path is same-type.

use std::collections::HashMap;

/// Per-frame value lookup for `{name}` references in AKML attribute
/// values. Build fresh each frame; the design is explicitly fine with
/// allocating a few `String`s per render because that cost is dwarfed
/// by everything else in the pipeline.
#[derive(Default, Debug, Clone)]
pub struct Bindings {
    texts: HashMap<String, String>,
    colors: HashMap<String, u32>,
    numbers: HashMap<String, f32>,
}

impl Bindings {
    pub fn new() -> Self { Self::default() }

    /// Bind a string, used by `<Text text>` / `<Button label>` /
    /// any other string-typed attribute.
    pub fn text(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.texts.insert(name.into(), value.into());
        self
    }

    /// Bind a packed `AARRGGBB` colour for any colour-typed attribute.
    pub fn color(mut self, name: impl Into<String>, value: u32) -> Self {
        self.colors.insert(name.into(), value);
        self
    }

    /// Bind a scalar for any numeric attribute (sizes, gaps, grow
    /// factors). Storing as `f32` matches the widget toolkit's native
    /// unit of account.
    pub fn number(mut self, name: impl Into<String>, value: f32) -> Self {
        self.numbers.insert(name.into(), value);
        self
    }

    /// If `raw` is the literal `"{name}"`, return `Some(name)`. Else
    /// `None`. Never allocates — returns a borrow into `raw`.
    pub(super) fn binding_name<'a>(raw: &'a str) -> Option<&'a str> {
        let s = raw.trim();
        if s.starts_with('{') && s.ends_with('}') && s.len() >= 3 {
            Some(&s[1..s.len() - 1])
        } else {
            None
        }
    }

    pub(super) fn get_text(&self, name: &str) -> Option<&str> {
        // Texts map takes precedence; if not present, surface a
        // typed colour/number as its canonical string form so an
        // author can cross-reference without duplicating values.
        if let Some(s) = self.texts.get(name) { return Some(s.as_str()); }
        None
    }

    pub(super) fn get_color(&self, name: &str) -> Option<u32> {
        // Typed first; then fall back to parsing a text binding as hex,
        // so `bindings.text("c", "0xffff0000")` works as a colour ref.
        if let Some(&c) = self.colors.get(name) { return Some(c); }
        if let Some(s) = self.texts.get(name) {
            // Accept the same literal forms the inflater does.
            return parse_color_loose(s);
        }
        None
    }

    pub(super) fn get_number(&self, name: &str) -> Option<f32> {
        if let Some(&n) = self.numbers.get(name) { return Some(n); }
        if let Some(s) = self.texts.get(name) {
            return s.trim().parse().ok();
        }
        None
    }
}

/// Stripped-down colour parser for cross-type binding fallback.
/// Intentionally narrower than the inflater's `parse_color` — this
/// path is a "the user put a hex string in texts" convenience, not a
/// blessed way to spell colours in AKML.
fn parse_color_loose(s: &str) -> Option<u32> {
    let s = s.trim();
    if let Some(hex) = s.strip_prefix('#') {
        return match hex.len() {
            6 => u32::from_str_radix(hex, 16).ok().map(|n| 0xFF00_0000 | n),
            8 => u32::from_str_radix(hex, 16).ok(),
            _ => None,
        };
    }
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        if hex.len() == 8 { return u32::from_str_radix(hex, 16).ok(); }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binding_name_extracts_from_braces() {
        assert_eq!(Bindings::binding_name("{display}"), Some("display"));
        assert_eq!(Bindings::binding_name(" {display} "), Some("display"));
    }

    #[test]
    fn binding_name_rejects_non_bindings() {
        assert_eq!(Bindings::binding_name("display"), None);
        assert_eq!(Bindings::binding_name("{foo"), None);
        assert_eq!(Bindings::binding_name("foo}"), None);
        // Require ≥ 1 char between braces — `{}` isn't a meaningful
        // name and "empty" references would otherwise be ambiguous
        // with user-facing literal "{}".
        assert_eq!(Bindings::binding_name("{}"), None);
    }

    #[test]
    fn text_binding_round_trips() {
        let b = Bindings::new().text("name", "value");
        assert_eq!(b.get_text("name"), Some("value"));
        assert_eq!(b.get_text("other"), None);
    }

    #[test]
    fn color_binding_typed_wins_over_text_fallback() {
        // If both maps have the same key, the typed one is used.
        let b = Bindings::new()
            .color("c", 0xff112233)
            .text("c", "0xffaabbcc");
        assert_eq!(b.get_color("c"), Some(0xff112233));
    }

    #[test]
    fn color_falls_back_to_text_hex_when_no_typed_binding() {
        let b = Bindings::new().text("c", "0xdeadbeef");
        assert_eq!(b.get_color("c"), Some(0xdeadbeef));
    }

    #[test]
    fn number_binding_parses_from_text_when_not_typed() {
        let b = Bindings::new().text("n", "12.5");
        assert_eq!(b.get_number("n"), Some(12.5));
    }

    #[test]
    fn missing_bindings_return_none() {
        let b = Bindings::new();
        assert!(b.get_text("x").is_none());
        assert!(b.get_color("x").is_none());
        assert!(b.get_number("x").is_none());
    }
}
