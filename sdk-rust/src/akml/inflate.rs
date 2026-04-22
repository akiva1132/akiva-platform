//! AKML inflater: [`Element`] tree → live widget tree on a [`Ui`].
//!
//! Walks the parsed element tree in document order and issues the
//! matching calls on the builder API from `sdk-rust/src/ui/`. Every
//! attribute that lands here is a *literal* value — resource resolution
//! (`@string/*`, `@color/*`, `@style/*`, `@dimen/*`) belongs to a later
//! stage that runs *before* this one and rewrites the element tree.
//!
//! ## Error model
//!
//! [`InflateError`] wraps a short message plus the source [`Loc`] of the
//! element (or attribute) that triggered the failure. Errors come from:
//!
//! - unknown element tags (misspelled widget name),
//! - unknown attribute values (e.g. `cross="middle"`),
//! - malformed numbers / colours,
//! - missing required attributes (`<Button>` with no `id` or `label`).
//!
//! Unknown *attribute names* are silently ignored — that lets older
//! apps keep loading when newer AKML gains new attributes, matching
//! Android's forward-compat story. The trade-off is that a typo in an
//! attribute name is not caught here; the linter (future work) is a
//! better place for that check.

use core::fmt;

use super::parser::{self, Element, Loc};
use crate::ui::node::NodeId;
use crate::ui::{CrossAxis, MainAxis, Ui};

#[derive(Debug, Clone, PartialEq)]
pub struct InflateError {
    pub message: String,
    pub loc: Loc,
}

impl fmt::Display for InflateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "akml inflate error at {}: {}", self.loc, self.message)
    }
}

impl std::error::Error for InflateError {}

impl From<parser::ParseError> for InflateError {
    fn from(e: parser::ParseError) -> Self {
        InflateError { message: e.message, loc: e.loc }
    }
}

/// Parse + inflate in one call. The common case for apps that embed
/// their layout via `include_str!`.
pub fn inflate_str(ui: &mut Ui<'_, '_, '_>, src: &str) -> Result<NodeId, InflateError> {
    let root = parser::parse(src)?;
    inflate(ui, &root)
}

/// Inflate a pre-parsed element tree onto `ui`. Returns the id of the
/// root widget. Call `ui.present(root)` in the caller — keeping the
/// two steps separate so the caller can still mix in imperative
/// widgets above or below the inflated sub-tree.
pub fn inflate(ui: &mut Ui<'_, '_, '_>, root: &Element) -> Result<NodeId, InflateError> {
    let id = match root.tag.as_str() {
        "VStack" | "HStack" => inflate_stack(ui, root)?,
        "Text"    => inflate_text(ui, root)?,
        "Button"  => inflate_button(ui, root)?,
        "Rect"    => inflate_rect(ui, root)?,
        "Spacer"  => inflate_spacer(ui, root)?,
        other => return Err(InflateError {
            message: format!("unknown widget tag <{other}>"),
            loc: root.loc,
        }),
    };
    Ok(id)
}

// ── Widget inflaters ─────────────────────────────────────────────────

fn inflate_stack(ui: &mut Ui<'_, '_, '_>, el: &Element) -> Result<NodeId, InflateError> {
    // Children first: the builder borrows `ui` mutably, so we gather
    // child ids before starting the stack builder chain.
    let mut child_ids = Vec::with_capacity(el.children.len());
    for child in &el.children {
        child_ids.push(inflate(ui, child)?);
    }
    let vertical = el.tag == "VStack";
    // Snapshot the attribute values before we hand `ui` to the builder —
    // after `.v_stack(...)` we hold a `StackBuilder` that borrows `ui`
    // mutably, and we can't also hand out immutable references to
    // `el.attrs` through the same `el` because attribute lookups don't
    // need `ui`; but parsing them below only needs &str, so doing the
    // parsing up front keeps the builder chain tidy.
    let gap      = attr_f32(el, "gap", "spacing")?.unwrap_or(0.0);
    let padding  = attr_padding(el)?.unwrap_or((0.0, 0.0, 0.0, 0.0));
    let main     = attr_main_axis(el)?;
    let cross    = attr_cross_axis(el)?;
    let flex     = attr_f32(el, "flex", "grow")?.unwrap_or(0.0);

    let mut b = if vertical { ui.v_stack(&child_ids) } else { ui.h_stack(&child_ids) };
    b = b.gap(gap).padding_trbl(padding.0, padding.1, padding.2, padding.3);
    if let Some(m) = main  { b = b.main(m); }
    if let Some(c) = cross { b = b.cross(c); }
    if flex > 0.0 { b = b.grow(flex); }
    Ok(b.build())
}

fn inflate_text(ui: &mut Ui<'_, '_, '_>, el: &Element) -> Result<NodeId, InflateError> {
    let text = el.attr("text").unwrap_or("");
    let size = attr_f32(el, "textSize", "size")?.unwrap_or(14.0);
    let color = attr_color(el, "textColor", "color")?.unwrap_or(0xfff0f4fa);
    let flex = attr_f32(el, "flex", "grow")?.unwrap_or(0.0);
    let mut b = ui.text(text).size(size).color(color);
    if flex > 0.0 { b = b.grow(flex); }
    Ok(b.build())
}

fn inflate_button(ui: &mut Ui<'_, '_, '_>, el: &Element) -> Result<NodeId, InflateError> {
    // `id` is required for now because click correlation goes through
    // `was_clicked(id)` — a button with no id would be tappable but
    // never observable, which is almost certainly not what the author
    // meant.
    let id = el.attr("id").ok_or_else(|| InflateError {
        message: "<Button> requires an `id` attribute for click correlation".into(),
        loc: el.loc,
    })?;
    let label = el.attr("label").or_else(|| el.attr("text")).unwrap_or(id);
    let flex = attr_f32(el, "flex", "grow")?.unwrap_or(0.0);
    let node = ui.button(id, label);
    if flex > 0.0 { ui.grow(node, flex); }
    Ok(node)
}

fn inflate_rect(ui: &mut Ui<'_, '_, '_>, el: &Element) -> Result<NodeId, InflateError> {
    // A Rect without a color is just invisible padding; require one to
    // surface obvious mistakes.
    let color = attr_color(el, "background", "color")?
        .ok_or_else(|| InflateError {
            message: "<Rect> requires a `color` (or `background`) attribute".into(),
            loc: el.loc,
        })?;
    let w = attr_f32(el, "minWidth",  "min_width")?.unwrap_or(0.0);
    let h = attr_f32(el, "minHeight", "min_height")?.unwrap_or(0.0);
    let flex = attr_f32(el, "flex", "grow")?.unwrap_or(0.0);
    let mut b = ui.rect(color).size(w, h);
    if flex > 0.0 { b = b.grow(flex); }
    Ok(b.build())
}

fn inflate_spacer(ui: &mut Ui<'_, '_, '_>, el: &Element) -> Result<NodeId, InflateError> {
    let w = attr_f32(el, "minWidth",  "min_width")?.unwrap_or(0.0);
    let h = attr_f32(el, "minHeight", "min_height")?.unwrap_or(0.0);
    let flex = attr_f32(el, "flex", "grow")?.unwrap_or(0.0);
    let mut b = ui.spacer().size(w, h);
    if flex > 0.0 { b = b.grow(flex); }
    Ok(b.build())
}

// ── Attribute parsers ────────────────────────────────────────────────

/// Try up to two attribute names. Lets authors use either the
/// Android-ish camelCase (`textSize`) or the Rust-ish snake_case (`size`,
/// `grow`) without either feeling wrong. Last-wins across the two names
/// is too surprising — instead, the first listed name that exists
/// decides, and the other is ignored if present.
fn attr_f32(el: &Element, a: &str, b: &str) -> Result<Option<f32>, InflateError> {
    for name in [a, b] {
        if let Some(raw) = el.attr(name) {
            return Ok(Some(parse_f32(raw, el.loc, name)?));
        }
    }
    Ok(None)
}

fn attr_color(el: &Element, a: &str, b: &str) -> Result<Option<u32>, InflateError> {
    for name in [a, b] {
        if let Some(raw) = el.attr(name) {
            return Ok(Some(parse_color(raw, el.loc, name)?));
        }
    }
    Ok(None)
}

fn attr_main_axis(el: &Element) -> Result<Option<MainAxis>, InflateError> {
    // The spec uses `justifyContent`; we also accept the shorter `main`
    // because apps written directly on the SDK builder already speak
    // that vocabulary.
    for name in ["justifyContent", "main"] {
        if let Some(raw) = el.attr(name) {
            let v = match raw {
                "start"         => MainAxis::Start,
                "center"        => MainAxis::Center,
                "end"           => MainAxis::End,
                "space_between" | "space-between" | "spaceBetween" => MainAxis::SpaceBetween,
                other => return Err(InflateError {
                    message: format!(
                        "unknown value '{other}' for {name} — expected start|center|end|space_between"
                    ),
                    loc: el.loc,
                }),
            };
            return Ok(Some(v));
        }
    }
    Ok(None)
}

fn attr_cross_axis(el: &Element) -> Result<Option<CrossAxis>, InflateError> {
    for name in ["alignItems", "cross"] {
        if let Some(raw) = el.attr(name) {
            let v = match raw {
                "start"   => CrossAxis::Start,
                "center"  => CrossAxis::Center,
                "end"     => CrossAxis::End,
                "stretch" => CrossAxis::Stretch,
                other => return Err(InflateError {
                    message: format!(
                        "unknown value '{other}' for {name} — expected start|center|end|stretch"
                    ),
                    loc: el.loc,
                }),
            };
            return Ok(Some(v));
        }
    }
    Ok(None)
}

/// Padding supports three shorthands so authors can pick whichever reads
/// best for the situation: `"16"` uniform, `"8 16"` vertical-horizontal,
/// `"4 8 4 8"` full TRBL.
fn attr_padding(el: &Element) -> Result<Option<(f32, f32, f32, f32)>, InflateError> {
    let raw = match el.attr("padding") { Some(s) => s, None => return Ok(None) };
    let mut parts = raw.split_ascii_whitespace();
    let a = parts.next().ok_or_else(|| InflateError {
        message: "empty `padding` value".into(), loc: el.loc,
    })?;
    let a = parse_f32(a, el.loc, "padding")?;
    let b = parts.next();
    let c = parts.next();
    let d = parts.next();
    if parts.next().is_some() {
        return Err(InflateError {
            message: "`padding` accepts 1, 2, or 4 values".into(), loc: el.loc,
        });
    }
    Ok(Some(match (b, c, d) {
        (None, _, _) => (a, a, a, a),
        (Some(b), None, _) => {
            let b = parse_f32(b, el.loc, "padding")?;
            (a, b, a, b)
        }
        (Some(b), Some(c), Some(d)) => {
            let b = parse_f32(b, el.loc, "padding")?;
            let c = parse_f32(c, el.loc, "padding")?;
            let d = parse_f32(d, el.loc, "padding")?;
            (a, b, c, d)
        }
        (Some(_), Some(_), None) => return Err(InflateError {
            message: "`padding` with 3 values is ambiguous — use 1, 2, or 4".into(),
            loc: el.loc,
        }),
    }))
}

/// Parse a numeric attribute. Strips trailing `dp`/`sp`/`px` suffixes —
/// density-independent resolution is a future-work item; for now all
/// three are treated as raw pixels. That's wrong on HDPI but keeps the
/// format forward-compatible: when resolution lands, the suffix will
/// gain meaning and the literal layouts keep working.
fn parse_f32(raw: &str, loc: Loc, name: &str) -> Result<f32, InflateError> {
    let trimmed = raw.trim();
    let n = trimmed.strip_suffix("dp")
        .or_else(|| trimmed.strip_suffix("sp"))
        .or_else(|| trimmed.strip_suffix("px"))
        .unwrap_or(trimmed);
    n.trim().parse::<f32>().map_err(|_| InflateError {
        message: format!("invalid numeric value '{raw}' for {name}"),
        loc,
    })
}

/// Parse a colour. Accepted forms:
///   - `#RGB`        — expanded to `#FFRRGGBB` (alpha opaque)
///   - `#RRGGBB`     — alpha opaque
///   - `#AARRGGBB`   — alpha explicit
///   - `0xAARRGGBB`  — same, Rust-ish prefix
/// The `0xAARRGGBB` form matches the u32 literals the widget toolkit
/// takes in imperative code, so the two worlds read identically.
fn parse_color(raw: &str, loc: Loc, name: &str) -> Result<u32, InflateError> {
    let s = raw.trim();
    let err = || InflateError {
        message: format!("invalid colour '{raw}' for {name} — accept #RGB, #RRGGBB, #AARRGGBB, 0xAARRGGBB"),
        loc,
    };
    if let Some(hex) = s.strip_prefix('#') {
        return match hex.len() {
            3 => {
                let n = u32::from_str_radix(hex, 16).map_err(|_| err())?;
                let r = (n >> 8) & 0xF; let g = (n >> 4) & 0xF; let b = n & 0xF;
                // Expand each nibble: "F" → "FF".
                let r = r | (r << 4);
                let g = g | (g << 4);
                let b = b | (b << 4);
                Ok(0xFF00_0000 | (r << 16) | (g << 8) | b)
            }
            6 => {
                let n = u32::from_str_radix(hex, 16).map_err(|_| err())?;
                Ok(0xFF00_0000 | n)
            }
            8 => u32::from_str_radix(hex, 16).map_err(|_| err()),
            _ => Err(err()),
        };
    }
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        if hex.len() != 8 { return Err(err()); }
        return u32::from_str_radix(hex, 16).map_err(|_| err());
    }
    Err(err())
}

// ── Tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::activity::{Context, Metrics};
    use crate::canvas::Canvas;
    use crate::ui::UiState;

    fn ctx() -> Context<'static> {
        Context {
            metrics: Metrics { width_px: 800, height_px: 600, density: 1.0 },
            timestamp_ms: 0.0,
            pointer: None,
            class: "t",
        }
    }

    /// Run `f` with a live Ui, returning whatever `f` returns. Builds
    /// the transient Canvas/UiState/Ui plumbing each call so individual
    /// tests don't have to thread that boilerplate.
    fn with_ui<R>(f: impl FnOnce(&mut Ui<'_, '_, '_>) -> R) -> R {
        let mut state = UiState::new();
        let c = ctx();
        let mut buf = Vec::new();
        let mut canvas = Canvas::new(&mut buf);
        let mut ui = Ui::new(&mut state, &c, &mut canvas);
        f(&mut ui)
    }

    // ── Parse+inflate: widget types ───────────────────────────────────

    #[test]
    fn inflates_single_self_closing_text() {
        with_ui(|ui| {
            let id = inflate_str(ui, r#"<Text text="hi" size="16" />"#).unwrap();
            assert_eq!(id, 0);  // first pushed node
        });
    }

    #[test]
    fn inflates_stack_with_children() {
        with_ui(|ui| {
            let src = r#"
                <VStack gap="8" padding="16">
                    <Text text="title" size="20" />
                    <Text text="body"  size="14" />
                </VStack>
            "#;
            let root = inflate_str(ui, src).unwrap();
            // Two text nodes pushed before the stack → stack id is 2.
            assert_eq!(root, 2);
        });
    }

    #[test]
    fn inflates_button_with_id() {
        with_ui(|ui| {
            let id = inflate_str(ui, r#"<Button id="go" label="Go" />"#).unwrap();
            // Render once so we can then check click correlation wires up.
            ui.present(id);
            assert!(ui.has_hit_rect("go"));
        });
    }

    #[test]
    fn button_falls_back_from_label_to_text_to_id() {
        with_ui(|ui| {
            // Authors coming from the Android spec use `text="…"` on
            // Buttons; we accept that, and fall back to the id if even
            // that's absent. Either way the click id is the same.
            let id = inflate_str(ui, r#"<Button id="done" text="Done!" />"#).unwrap();
            ui.present(id);
            assert!(ui.has_hit_rect("done"));
        });
    }

    // ── Attribute coverage ────────────────────────────────────────────

    #[test]
    fn flex_attribute_sets_grow() {
        with_ui(|ui| {
            // Smoke: no panic, tree builds, and we trust the layout
            // tests to validate grow arithmetic. Use the `0x` colour
            // form so the Rust raw-string doesn't collide with `#`.
            let src = r##"<VStack><Rect color="0xffff0000" flex="1" /></VStack>"##;
            let _ = inflate_str(ui, src).unwrap();
        });
    }

    #[test]
    fn main_and_cross_axis_accept_both_vocabularies() {
        with_ui(|ui| {
            // `justifyContent`/`alignItems` come from the AKML spec;
            // `main`/`cross` match the Rust builder API. Both must work.
            let spec_style = inflate_str(ui, r#"<HStack justifyContent="center" alignItems="stretch"><Text text="a" /></HStack>"#);
            let rust_style = inflate_str(ui, r#"<HStack main="center" cross="stretch"><Text text="a" /></HStack>"#);
            spec_style.unwrap(); rust_style.unwrap();
        });
    }

    // ── Error paths ────────────────────────────────────────────────────

    #[test]
    fn unknown_tag_errors_with_location() {
        with_ui(|ui| {
            let err = inflate_str(ui, "\n  <Wat />").expect_err("unknown tag");
            assert!(err.message.contains("unknown widget tag"), "msg: {}", err.message);
            assert_eq!(err.loc.line, 2);
        });
    }

    #[test]
    fn button_without_id_is_an_error() {
        with_ui(|ui| {
            let err = inflate_str(ui, r#"<Button label="Go" />"#).unwrap_err();
            assert!(err.message.contains("requires an `id`"), "msg: {}", err.message);
        });
    }

    #[test]
    fn rect_without_color_is_an_error() {
        with_ui(|ui| {
            let err = inflate_str(ui, r#"<Rect minWidth="10" />"#).unwrap_err();
            assert!(err.message.contains("requires a `color`"), "msg: {}", err.message);
        });
    }

    #[test]
    fn bad_main_axis_value_is_an_error() {
        with_ui(|ui| {
            let err = inflate_str(ui, r#"<VStack main="middle"><Text text="x" /></VStack>"#).unwrap_err();
            assert!(err.message.contains("middle"), "msg: {}", err.message);
        });
    }

    #[test]
    fn malformed_color_is_an_error() {
        with_ui(|ui| {
            let err = inflate_str(ui, r##"<Rect color="#zzz" />"##).unwrap_err();
            assert!(err.message.contains("invalid colour"), "msg: {}", err.message);
        });
    }

    // ── Direct helper checks ──────────────────────────────────────────

    #[test]
    fn color_accepts_three_nibble_shorthand() {
        let loc = Loc { line: 1, col: 1 };
        assert_eq!(parse_color("#f00", loc, "c").unwrap(), 0xFFFF0000);
    }

    #[test]
    fn color_accepts_six_nibble_hex() {
        let loc = Loc { line: 1, col: 1 };
        assert_eq!(parse_color("#12abCD", loc, "c").unwrap(), 0xFF12ABCD);
    }

    #[test]
    fn color_accepts_eight_nibble_with_alpha() {
        let loc = Loc { line: 1, col: 1 };
        assert_eq!(parse_color("#80ff0000", loc, "c").unwrap(), 0x80FF0000);
        assert_eq!(parse_color("0xDEADBEEF", loc, "c").unwrap(), 0xDEADBEEF);
    }

    #[test]
    fn padding_shorthand_expands_correctly() {
        fn pad(src: &str) -> (f32, f32, f32, f32) {
            // Wrap the attribute in a minimal element so we reuse the
            // real parser path; shorter than hand-building an Element.
            let el = parser::parse(&format!(r#"<A padding="{src}" />"#)).unwrap();
            attr_padding(&el).unwrap().unwrap()
        }
        assert_eq!(pad("16"),           (16.0, 16.0, 16.0, 16.0));
        assert_eq!(pad("8 16"),         (8.0,  16.0, 8.0,  16.0));
        assert_eq!(pad("4 8 12 16"),    (4.0,  8.0,  12.0, 16.0));
    }

    #[test]
    fn numeric_dp_suffix_is_stripped() {
        let loc = Loc { line: 1, col: 1 };
        assert_eq!(parse_f32("16dp", loc, "x").unwrap(), 16.0);
        assert_eq!(parse_f32("12.5sp", loc, "x").unwrap(), 12.5);
        assert_eq!(parse_f32(" 24 px ", loc, "x").unwrap(), 24.0);
    }
}
