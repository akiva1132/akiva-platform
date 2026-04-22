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

use super::bindings::Bindings;
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

/// Parse + inflate in one call with no bindings. The common case for
/// apps whose layout is entirely static.
pub fn inflate_str(ui: &mut Ui<'_, '_, '_>, src: &str) -> Result<NodeId, InflateError> {
    inflate_str_with(ui, src, &Bindings::default())
}

/// Parse + inflate, resolving any `{name}` attribute values through
/// `bindings`. Build a fresh `Bindings` per frame — it's cheap and
/// keeps the binding lifetimes local to `on_render`.
pub fn inflate_str_with(
    ui: &mut Ui<'_, '_, '_>, src: &str, bindings: &Bindings,
) -> Result<NodeId, InflateError> {
    let root = parser::parse(src)?;
    inflate_with(ui, &root, bindings)
}

/// Inflate a pre-parsed element tree onto `ui` with no bindings —
/// every `{name}` attribute value in `root` will error out as
/// unresolved. Prefer this when the source is known-static.
pub fn inflate(ui: &mut Ui<'_, '_, '_>, root: &Element) -> Result<NodeId, InflateError> {
    inflate_with(ui, root, &Bindings::default())
}

/// Inflate a pre-parsed element tree, resolving any `{name}`
/// references through `bindings`. Returns the id of the root widget.
/// Call `ui.present(root)` in the caller — keeping the two steps
/// separate so the caller can still mix imperative widgets above or
/// below the inflated sub-tree.
pub fn inflate_with(
    ui: &mut Ui<'_, '_, '_>, root: &Element, bindings: &Bindings,
) -> Result<NodeId, InflateError> {
    let id = match root.tag.as_str() {
        "VStack" | "HStack" => inflate_stack(ui, root, bindings)?,
        "Frame"   => inflate_frame(ui, root, bindings)?,
        "Grid"    => inflate_grid(ui, root, bindings)?,
        "Text"    => inflate_text(ui, root, bindings)?,
        "Button"  => inflate_button(ui, root, bindings)?,
        "Rect"    => inflate_rect(ui, root, bindings)?,
        "Spacer"  => inflate_spacer(ui, root, bindings)?,
        other => return Err(InflateError {
            message: format!("unknown widget tag <{other}>"),
            loc: root.loc,
        }),
    };
    Ok(id)
}

// ── Widget inflaters ─────────────────────────────────────────────────

fn inflate_stack(ui: &mut Ui<'_, '_, '_>, el: &Element, bindings: &Bindings) -> Result<NodeId, InflateError> {
    // Children first: the builder borrows `ui` mutably, so we gather
    // child ids before starting the stack builder chain.
    let mut child_ids = Vec::with_capacity(el.children.len());
    for child in &el.children {
        child_ids.push(inflate_with(ui, child, bindings)?);
    }
    let vertical = el.tag == "VStack";
    // Snapshot the attribute values before we hand `ui` to the builder —
    // after `.v_stack(...)` we hold a `StackBuilder` that borrows `ui`
    // mutably, and we can't also hand out immutable references to
    // `el.attrs` through the same `el` because attribute lookups don't
    // need `ui`; but parsing them below only needs &str, so doing the
    // parsing up front keeps the builder chain tidy.
    let gap      = attr_f32(el, bindings, "gap", "spacing")?.unwrap_or(0.0);
    let padding  = attr_padding(el, bindings)?.unwrap_or((0.0, 0.0, 0.0, 0.0));
    let main     = attr_main_axis(el)?;
    let cross    = attr_cross_axis(el)?;
    let flex     = attr_f32(el, bindings, "flex", "grow")?.unwrap_or(0.0);

    let mut b = if vertical { ui.v_stack(&child_ids) } else { ui.h_stack(&child_ids) };
    b = b.gap(gap).padding_trbl(padding.0, padding.1, padding.2, padding.3);
    if let Some(m) = main  { b = b.main(m); }
    if let Some(c) = cross { b = b.cross(c); }
    if flex > 0.0 { b = b.grow(flex); }
    Ok(b.build())
}

fn inflate_frame(ui: &mut Ui<'_, '_, '_>, el: &Element, bindings: &Bindings) -> Result<NodeId, InflateError> {
    let mut child_ids = Vec::with_capacity(el.children.len());
    for child in &el.children {
        child_ids.push(inflate_with(ui, child, bindings)?);
    }
    let padding = attr_padding(el, bindings)?.unwrap_or((0.0, 0.0, 0.0, 0.0));
    let flex = attr_f32(el, bindings, "flex", "grow")?.unwrap_or(0.0);
    let mut b = ui.frame(&child_ids)
        .padding_trbl(padding.0, padding.1, padding.2, padding.3);
    if flex > 0.0 { b = b.grow(flex); }
    Ok(b.build())
}

fn inflate_grid(ui: &mut Ui<'_, '_, '_>, el: &Element, bindings: &Bindings) -> Result<NodeId, InflateError> {
    let mut child_ids = Vec::with_capacity(el.children.len());
    for child in &el.children {
        child_ids.push(inflate_with(ui, child, bindings)?);
    }
    // `cols` and `rows` are required — a grid with neither is
    // indistinguishable from a Frame, and guessing one from the other
    // obscures intent more than it helps. A missing value is a layout
    // bug, not a silent default.
    let cols = attr_u32(el, bindings, "cols", "columns")?.ok_or_else(|| InflateError {
        message: "<Grid> requires a `cols` attribute".into(),
        loc: el.loc,
    })?;
    let rows = attr_u32(el, bindings, "rows", "rowCount")?.ok_or_else(|| InflateError {
        message: "<Grid> requires a `rows` attribute".into(),
        loc: el.loc,
    })?;
    // `gap` is uniform; per-axis `gapX` / `gapY` override it when an
    // author wants asymmetric spacing (denser rows of compact chips,
    // e.g.). Defaults to 0 on whichever axis wasn't overridden.
    let gap_all = attr_f32(el, bindings, "gap", "spacing")?.unwrap_or(0.0);
    let gap_x = attr_f32(el, bindings, "gapX", "colGap")?.unwrap_or(gap_all);
    let gap_y = attr_f32(el, bindings, "gapY", "rowGap")?.unwrap_or(gap_all);
    let padding = attr_padding(el, bindings)?.unwrap_or((0.0, 0.0, 0.0, 0.0));
    let flex = attr_f32(el, bindings, "flex", "grow")?.unwrap_or(0.0);

    let mut b = ui.grid(&child_ids, cols, rows)
        .gap_xy(gap_x, gap_y)
        .padding_trbl(padding.0, padding.1, padding.2, padding.3);
    if flex > 0.0 { b = b.grow(flex); }
    Ok(b.build())
}

fn inflate_text(ui: &mut Ui<'_, '_, '_>, el: &Element, bindings: &Bindings) -> Result<NodeId, InflateError> {
    let text = attr_text(el, bindings, "text")?.unwrap_or_default();
    let size = attr_f32(el, bindings, "textSize", "size")?.unwrap_or(14.0);
    let color = attr_color(el, bindings, "textColor", "color")?.unwrap_or(0xfff0f4fa);
    let flex = attr_f32(el, bindings, "flex", "grow")?.unwrap_or(0.0);
    let mut b = ui.text(text).size(size).color(color);
    if flex > 0.0 { b = b.grow(flex); }
    Ok(b.build())
}

fn inflate_button(ui: &mut Ui<'_, '_, '_>, el: &Element, bindings: &Bindings) -> Result<NodeId, InflateError> {
    // `id` is required for now because click correlation goes through
    // `was_clicked(id)` — a button with no id would be tappable but
    // never observable, which is almost certainly not what the author
    // meant. The id itself is *not* bindable: a `{name}` id would break
    // click correlation (the click site writes a literal id).
    let id = el.attr("id").ok_or_else(|| InflateError {
        message: "<Button> requires an `id` attribute for click correlation".into(),
        loc: el.loc,
    })?;
    let label = attr_text(el, bindings, "label")?
        .or(attr_text(el, bindings, "text")?)
        .unwrap_or_else(|| id.to_string());
    let flex = attr_f32(el, bindings, "flex", "grow")?.unwrap_or(0.0);
    // `onClick` is AKML's spec spelling for the semantic action name;
    // `action` is the shorter Rust-builder equivalent. Pick whichever
    // the author used — either attaches the same `action` on the node.
    let action = attr_text(el, bindings, "onClick")?
        .or(attr_text(el, bindings, "action")?);
    let node = ui.button(id, label);
    if flex > 0.0 { ui.grow(node, flex); }
    if let Some(a) = action { ui.action(node, a); }
    Ok(node)
}

fn inflate_rect(ui: &mut Ui<'_, '_, '_>, el: &Element, bindings: &Bindings) -> Result<NodeId, InflateError> {
    // A Rect without a color is just invisible padding; require one to
    // surface obvious mistakes.
    let color = attr_color(el, bindings, "background", "color")?
        .ok_or_else(|| InflateError {
            message: "<Rect> requires a `color` (or `background`) attribute".into(),
            loc: el.loc,
        })?;
    let w = attr_f32(el, bindings, "minWidth",  "min_width")?.unwrap_or(0.0);
    let h = attr_f32(el, bindings, "minHeight", "min_height")?.unwrap_or(0.0);
    let flex = attr_f32(el, bindings, "flex", "grow")?.unwrap_or(0.0);
    let mut b = ui.rect(color).size(w, h);
    if flex > 0.0 { b = b.grow(flex); }
    Ok(b.build())
}

fn inflate_spacer(ui: &mut Ui<'_, '_, '_>, el: &Element, bindings: &Bindings) -> Result<NodeId, InflateError> {
    let w = attr_f32(el, bindings, "minWidth",  "min_width")?.unwrap_or(0.0);
    let h = attr_f32(el, bindings, "minHeight", "min_height")?.unwrap_or(0.0);
    let flex = attr_f32(el, bindings, "flex", "grow")?.unwrap_or(0.0);
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
///
/// If the attribute's value is a `{name}` binding reference, the value
/// comes from `bindings`; a missing binding is an error. Else the value
/// is parsed as a literal number.
/// Integer variant of [`attr_f32`] for attributes that must be whole
/// numbers (grid cols / rows, maybe span counts later). Accepts a
/// `{binding}` reference only if the bound number has no fractional
/// part — otherwise falls out as a parse error with the offending name,
/// which is more helpful than silently truncating.
fn attr_u32(el: &Element, bindings: &Bindings, a: &str, b: &str) -> Result<Option<u32>, InflateError> {
    for name in [a, b] {
        if let Some(raw) = el.attr(name) {
            let n = if let Some(bname) = Bindings::binding_name(raw) {
                bindings.get_number(bname).ok_or_else(|| unresolved(el.loc, bname, name))?
            } else {
                parse_f32(raw, el.loc, name)?
            };
            if n < 0.0 || n.fract() != 0.0 {
                return Err(InflateError {
                    message: format!("attribute `{name}` expects a non-negative integer, got {n}"),
                    loc: el.loc,
                });
            }
            return Ok(Some(n as u32));
        }
    }
    Ok(None)
}

fn attr_f32(el: &Element, bindings: &Bindings, a: &str, b: &str) -> Result<Option<f32>, InflateError> {
    for name in [a, b] {
        if let Some(raw) = el.attr(name) {
            if let Some(bname) = Bindings::binding_name(raw) {
                return bindings.get_number(bname).map(Some).ok_or_else(|| unresolved(el.loc, bname, name));
            }
            return Ok(Some(parse_f32(raw, el.loc, name)?));
        }
    }
    Ok(None)
}

fn attr_color(el: &Element, bindings: &Bindings, a: &str, b: &str) -> Result<Option<u32>, InflateError> {
    for name in [a, b] {
        if let Some(raw) = el.attr(name) {
            // Ordering: {binding} (dynamic, per-frame) → @color/ref
            // (static per-package) → hex literal. Bindings win over
            // refs so an AKML author can override a resource-sourced
            // value for a single render pass if they want.
            if let Some(bname) = Bindings::binding_name(raw) {
                return bindings.get_color(bname).map(Some).ok_or_else(|| unresolved(el.loc, bname, name));
            }
            if let Some(refname) = resource_ref(raw, "@color/") {
                return Ok(Some(crate::res::color(refname)));
            }
            return Ok(Some(parse_color(raw, el.loc, name)?));
        }
    }
    Ok(None)
}

/// Resolve a single named attribute whose type is an arbitrary string
/// — `<Text text=...>`, `<Button label=...>`. Bindings preferred when
/// the value is `{name}`, `@string/` references second, literal text
/// last. Returns owned `String` because the bindings map owns its
/// strings, and callers pass the result to builder APIs that consume
/// `impl Into<String>` anyway.
fn attr_text(el: &Element, bindings: &Bindings, name: &str) -> Result<Option<String>, InflateError> {
    let raw = match el.attr(name) { Some(s) => s, None => return Ok(None) };
    if let Some(bname) = Bindings::binding_name(raw) {
        return bindings.get_text(bname)
            .map(|s| Some(s.to_string()))
            .ok_or_else(|| unresolved(el.loc, bname, name));
    }
    if let Some(refname) = resource_ref(raw, "@string/") {
        return Ok(Some(crate::res::string(refname)));
    }
    Ok(Some(raw.to_string()))
}

/// Detect an `@kind/name` resource reference and return the bare name
/// portion. `kind` must include the trailing slash (`"@color/"`,
/// `"@string/"`). Returns `None` if the value isn't a ref of this kind.
/// Names themselves are left to the host tables to validate — a typo
/// produces the visible placeholder rather than an inflater error,
/// since we want "works partially" over "refuses to load" during
/// iteration.
fn resource_ref<'a>(value: &'a str, kind: &str) -> Option<&'a str> {
    value.strip_prefix(kind).filter(|name| !name.is_empty())
}

fn unresolved(loc: Loc, binding_name: &str, attr_name: &str) -> InflateError {
    InflateError {
        message: format!(
            "unresolved binding '{{{binding_name}}}' for attribute '{attr_name}' — no entry in Bindings"
        ),
        loc,
    }
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
/// `"4 8 4 8"` full TRBL. A `{name}` binding resolves to a scalar and
/// is applied uniformly — mixed shorthand inside a binding isn't
/// supported (the binding name must reference a single `f32`).
fn attr_padding(el: &Element, bindings: &Bindings) -> Result<Option<(f32, f32, f32, f32)>, InflateError> {
    let raw = match el.attr("padding") { Some(s) => s, None => return Ok(None) };
    if let Some(bname) = Bindings::binding_name(raw) {
        let v = bindings.get_number(bname).ok_or_else(|| unresolved(el.loc, bname, "padding"))?;
        return Ok(Some((v, v, v, v)));
    }
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
            handle: 0,
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

    #[test]
    fn inflates_frame_with_background_rect_and_content() {
        // Canonical Frame use: a background Rect under an HStack of text.
        // Smoke-checks that the tag dispatches, children inflate in
        // order, and the button id (if any) still wires up.
        with_ui(|ui| {
            let src = r##"
                <Frame padding="4">
                    <Rect color="0xff112233" />
                    <HStack padding="8 12" main="end" cross="center">
                        <Text text="hint" size="12" />
                        <Text text="display" size="24" />
                    </HStack>
                </Frame>
            "##;
            let root = inflate_str(ui, src).unwrap();
            ui.present(root);
            // 6 nodes in doc order: Rect, Text, Text, HStack, Frame.
            // Root id = 4 (0..=4 indices).
            assert_eq!(root, 4);
        });
    }

    #[test]
    fn button_onclick_attribute_attaches_semantic_action() {
        // Smoke: `onClick="name"` in AKML must propagate through the
        // inflater so `was_triggered("name")` fires on click. We can't
        // fake a click through the public API without a ctx+state
        // dance, so the assertion is: a non-panicking inflate plus a
        // registered hit rect on the button's id. The deeper click-→-
        // action round trip is pinned by the Ui-level tests in
        // ui/mod.rs — this test only guards the AKML wiring.
        with_ui(|ui| {
            let src = r#"<Button id="b" label="B" onClick="do_thing" />"#;
            let id = inflate_str(ui, src).unwrap();
            ui.present(id);
            assert!(ui.has_hit_rect("b"));
            // No click was injected, so `was_triggered` is still false
            // — but the action *is* on the node, ready to fire.
            assert!(!ui.was_triggered("do_thing"));
        });
    }

    #[test]
    fn button_accepts_action_alias_for_onclick() {
        // Rust-builder authors may write `action="…"` (matching the
        // `Ui::action` setter) instead of the spec's `onClick="…"`.
        // Both must reach the same attach point.
        with_ui(|ui| {
            let src = r#"<Button id="b" label="B" action="do_thing" />"#;
            let _ = inflate_str(ui, src).unwrap();
        });
    }

    #[test]
    fn frame_accepts_flex_attribute() {
        // Frame should grow in a parent stack just like any other
        // container builder.
        with_ui(|ui| {
            let src = r##"<VStack><Frame flex="1"><Rect color="0xffff0000" /></Frame></VStack>"##;
            let _ = inflate_str(ui, src).unwrap();
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
            attr_padding(&el, &Bindings::default()).unwrap().unwrap()
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

    // ── Bindings ───────────────────────────────────────────────────────

    #[test]
    fn text_binding_substitutes_attribute_value() {
        // Compile the tree twice with different bindings — only the
        // bindings change; the id stays the same, and `was_clicked`
        // still keys off the literal id.
        with_ui(|ui| {
            let bindings = Bindings::new().text("display", "hello world");
            let src = r#"<Text text="{display}" size="14" />"#;
            let root = inflate_str_with(ui, src, &bindings).unwrap();
            ui.present(root);
            // No direct assertion on text content (we don't expose the
            // rendered string); compile + render is the smoke test.
            let _ = root;
        });
    }

    #[test]
    fn color_binding_uses_typed_value() {
        with_ui(|ui| {
            let bindings = Bindings::new().color("bg", 0xff112233);
            let src = r##"<Rect color="{bg}" minWidth="10" minHeight="10" />"##;
            let _ = inflate_str_with(ui, src, &bindings).unwrap();
        });
    }

    #[test]
    fn number_binding_drives_size_and_gap() {
        with_ui(|ui| {
            let bindings = Bindings::new().number("big", 48.0).number("pad", 12.0);
            let src = r#"<VStack gap="{pad}" padding="{pad}">
                <Text text="title" size="{big}" />
            </VStack>"#;
            let _ = inflate_str_with(ui, src, &bindings).unwrap();
        });
    }

    #[test]
    fn unresolved_binding_is_a_clear_error() {
        with_ui(|ui| {
            // text="{name}" but the Bindings is empty.
            let err = inflate_str_with(
                ui, r#"<Text text="{missing}" />"#, &Bindings::default(),
            ).unwrap_err();
            assert!(err.message.contains("unresolved binding '{missing}'"), "msg: {}", err.message);
            assert!(err.message.contains("'text'"), "msg: {}", err.message);
        });
    }

    #[test]
    fn partial_binding_is_treated_as_literal() {
        // "{name}" must be the entire attribute value. "Prefix {name}"
        // is taken as a literal string — no surprise interpolation of
        // brace-wrapped substrings into the middle of a user message.
        with_ui(|ui| {
            let bindings = Bindings::new().text("display", "42");
            let src = r#"<Text text="value is {display}" />"#;
            // No error — the literal is used as-is. The rendered text
            // would show "value is {display}"; we don't assert the
            // rendered string, just that no binding error fires.
            let _ = inflate_str_with(ui, src, &bindings).unwrap();
        });
    }

    #[test]
    fn literal_attr_does_not_touch_bindings() {
        // Bindings is populated but the attr is literal — the binding
        // is ignored, the literal wins.
        with_ui(|ui| {
            let bindings = Bindings::new()
                .text("label", "SURPRISE");  // must not appear
            let src = r#"<Button id="go" label="Go" />"#;
            let id = inflate_str_with(ui, src, &bindings).unwrap();
            ui.present(id);
            assert!(ui.has_hit_rect("go"));
        });
    }

    #[test]
    fn inflate_without_bindings_errors_on_binding_reference() {
        // Using the short non-`_with` entry point with a `{name}`
        // attribute is a mistake we want to flag, not silently render
        // the literal "{name}" as text.
        with_ui(|ui| {
            let err = inflate_str(ui, r#"<Text text="{name}" />"#).unwrap_err();
            assert!(err.message.contains("unresolved"), "msg: {}", err.message);
        });
    }

    // ── Resource references ───────────────────────────────────────────

    #[test]
    fn color_ref_inflates_to_the_resource_sentinel_on_non_wasm() {
        // On non-wasm (cargo test), `res::color` returns MISSING_COLOR
        // because there's no host to query. The inflater still accepts
        // the attribute — it doesn't know or care whether the ref will
        // resolve at runtime; resolution is the host's business. A
        // missing ref on wasm would render the same magenta.
        with_ui(|ui| {
            let src = r#"<Rect color="@color/display_fg" />"#;
            let id = inflate_str(ui, src).unwrap();
            ui.present(id);
            // Success proves the @color/ prefix was recognised and the
            // literal parser wasn't called (which would have errored
            // on a non-hex value).
        });
    }

    #[test]
    fn string_ref_inflates_to_the_placeholder_on_non_wasm() {
        with_ui(|ui| {
            let src = r#"<Text text="@string/greeting" />"#;
            let id = inflate_str(ui, src).unwrap();
            // Just needs to inflate — there's no way to observe the
            // stored label text from this layer without reaching into
            // UiState internals, but passing means the prefix branch
            // was taken (a literal "@string/…" would parse as raw text
            // and still succeed, so this test is primarily a guard
            // against regressing the prefix logic).
            ui.present(id);
        });
    }

    #[test]
    fn literal_color_still_parses_when_value_is_not_a_ref() {
        // Regression guard: the @-prefix check must not swallow plain
        // hex values that happen to start with '#'.
        with_ui(|ui| {
            let src = r##"<Rect color="#ff15172a" />"##;
            let id = inflate_str(ui, src).unwrap();
            ui.present(id);
        });
    }

    #[test]
    fn empty_ref_name_falls_through_to_literal_parser() {
        // "@color/" with no name isn't a valid ref. We fall through to
        // the literal parser, which rejects it as malformed hex — the
        // point is to make sure we don't silently pass an empty name
        // to the host.
        with_ui(|ui| {
            let src = r#"<Rect color="@color/" />"#;
            let err = inflate_str(ui, src).unwrap_err();
            assert!(err.message.contains("color") || err.message.contains("hex"),
                "msg: {}", err.message);
        });
    }

    // ── Grid ─────────────────────────────────────────────────────────
    //
    // Grid gets its own test cluster because its attribute surface is
    // larger than any other container (`cols`, `rows`, `gap`, `gapX`,
    // `gapY`, `padding`, `flex`) and because the `cols`/`rows` parsing
    // goes through the new `attr_u32` helper.

    #[test]
    fn inflates_grid_with_cols_rows_gap() {
        with_ui(|ui| {
            let src = r##"
                <Grid cols="2" rows="2" gap="4">
                    <Rect color="#ff111111" />
                    <Rect color="#ff222222" />
                    <Rect color="#ff333333" />
                    <Rect color="#ff444444" />
                </Grid>
            "##;
            let root = inflate_str(ui, src).unwrap();
            // Four rects + the grid itself = id 4.
            assert_eq!(root, 4);
        });
    }

    #[test]
    fn grid_missing_cols_is_an_error() {
        with_ui(|ui| {
            let src = r##"<Grid rows="2"><Rect color="#ff000000" /></Grid>"##;
            let err = inflate_str(ui, src).unwrap_err();
            assert!(err.message.contains("cols"), "msg: {}", err.message);
        });
    }

    #[test]
    fn grid_missing_rows_is_an_error() {
        with_ui(|ui| {
            let src = r##"<Grid cols="2"><Rect color="#ff000000" /></Grid>"##;
            let err = inflate_str(ui, src).unwrap_err();
            assert!(err.message.contains("rows"), "msg: {}", err.message);
        });
    }

    #[test]
    fn grid_rejects_fractional_cols() {
        // Non-integer cols is always a typo — a grid with 2.5 columns
        // is undefined. Surface it as a parse error, not a silent
        // truncation.
        with_ui(|ui| {
            let src = r##"<Grid cols="2.5" rows="2"><Rect color="#ff000000" /></Grid>"##;
            let err = inflate_str(ui, src).unwrap_err();
            assert!(err.message.contains("integer"), "msg: {}", err.message);
        });
    }

    #[test]
    fn grid_accepts_camelcase_gapx_and_gapy_aliases() {
        // Asymmetric gap via per-axis attributes; ensure the uniform
        // `gap` default (0) doesn't override them.
        with_ui(|ui| {
            let src = r##"
                <Grid cols="2" rows="2" gapX="6" gapY="10">
                    <Rect color="#ff000000" />
                    <Rect color="#ff000000" />
                    <Rect color="#ff000000" />
                    <Rect color="#ff000000" />
                </Grid>
            "##;
            // Just needs to parse + inflate cleanly — the layout pass
            // elsewhere has the tests for what gap values actually
            // produce on the wire.
            assert!(inflate_str(ui, src).is_ok());
        });
    }

    #[test]
    fn grid_with_flex_grows_in_parent_stack() {
        // `flex` on a Grid should feed the SDK's grow attr so a VStack
        // parent hands the grid its leftover main-axis space — the
        // calculator-keypad pattern.
        with_ui(|ui| {
            let src = r##"
                <VStack>
                    <Text text="header" size="16" />
                    <Grid cols="2" rows="1" flex="1">
                        <Rect color="#ff000000" />
                        <Rect color="#ff000000" />
                    </Grid>
                </VStack>
            "##;
            assert!(inflate_str(ui, src).is_ok());
        });
    }
}
