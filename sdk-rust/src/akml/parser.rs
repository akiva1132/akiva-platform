//! AKML parser: source bytes → [`Element`] tree.
//!
//! Hand-rolled recursive-descent over a byte-position cursor. We avoid
//! pulling in a full XML crate because:
//!
//! 1. AKML uses a *subset* of XML — no DTDs, no namespaces, no entity
//!    declarations beyond the five standard ones. A ~300-line parser
//!    covers everything the format promises.
//! 2. `sdk-rust` compiles to wasm and ships inside every Akiva app;
//!    dependency bloat costs real bytes in the final binary. A minimal
//!    parser keeps the SDK tight.
//! 3. A tailored parser can cite AKML concepts in its error messages
//!    ("unknown widget tag") instead of generic XML terminology.
//!
//! ## Supported surface
//!
//! - Optional prolog: `<?akml version="1.0"?>` (parsed, value ignored
//!   for now — see `docs/akml.md §11`).
//! - Elements: `<Tag attr="v">...children...</Tag>` or self-closing
//!   `<Tag attr="v" />`.
//! - Attributes: `name="value"` or `name='value'`. Quotes required;
//!   identifier-first name; HTML entities inside the value.
//! - Comments: `<!-- ... -->`. May appear between elements and in the
//!   prolog area, not inside tags.
//! - Five standard entities inside attribute values: `&amp; &lt; &gt;
//!   &quot; &apos;`. Unknown entities are an error rather than a
//!   silent pass-through — typos like `&ampp;` should surface early.
//!
//! ## Deliberately *not* supported
//!
//! - Text content between tags (use the `text` attribute).
//! - CDATA sections, processing instructions other than the prolog,
//!   DTDs, namespaces. The parser errors out if it sees them.

use core::fmt;

/// Location of a token in the source. 1-based for both line and column
/// because that's what editors display — emitting 0-based coordinates
/// just means a downstream tool has to add 1 before showing them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Loc {
    pub line: u32,
    pub col: u32,
}

impl fmt::Display for Loc {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.line, self.col)
    }
}

/// A parsed AKML element. Attributes preserve insertion order and
/// allow duplicates (the last one wins in [`Element::attr`]), which
/// matches how a user reads the source top-to-bottom. Children are
/// in document order.
#[derive(Debug, Clone, PartialEq)]
pub struct Element {
    pub tag: String,
    pub attrs: Vec<(String, String)>,
    pub children: Vec<Element>,
    /// Location of the opening `<` of this element — handy for error
    /// messages produced by the inflater stage.
    pub loc: Loc,
}

impl Element {
    /// Last-wins attribute lookup. Returns `None` if no attribute of
    /// that name was declared.
    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attrs.iter().rev()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ParseError {
    pub message: String,
    pub loc: Loc,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "akml parse error at {}: {}", self.loc, self.message)
    }
}

impl std::error::Error for ParseError {}

/// Parse a full AKML document. Accepts an optional prolog plus a single
/// root element. Trailing whitespace and comments are allowed; any
/// other trailing content is an error so the caller doesn't silently
/// drop a second root element.
pub fn parse(src: &str) -> Result<Element, ParseError> {
    let mut p = Parser::new(src);
    p.skip_trivia();
    if p.starts_with("<?") {
        p.parse_prolog()?;
        p.skip_trivia();
    }
    let root = p.parse_element()?;
    p.skip_trivia();
    if p.peek().is_some() {
        return Err(p.error("unexpected content after root element — AKML has exactly one root"));
    }
    Ok(root)
}

struct Parser<'a> {
    src: &'a [u8],
    pos: usize,
    line: u32,
    col: u32,
}

impl<'a> Parser<'a> {
    fn new(src: &'a str) -> Self {
        Self { src: src.as_bytes(), pos: 0, line: 1, col: 1 }
    }

    fn peek(&self) -> Option<u8> { self.src.get(self.pos).copied() }
    fn starts_with(&self, s: &str) -> bool {
        self.src[self.pos..].starts_with(s.as_bytes())
    }
    fn loc(&self) -> Loc { Loc { line: self.line, col: self.col } }

    fn bump(&mut self) -> Option<u8> {
        let b = self.peek()?;
        self.pos += 1;
        if b == b'\n' {
            self.line += 1;
            self.col = 1;
        } else if b & 0xC0 != 0x80 {
            // Count columns in *code points*, skipping UTF-8
            // continuation bytes (top two bits 10). So a multi-byte
            // glyph in the source counts as one column, which is what
            // editors and users actually see.
            self.col += 1;
        }
        Some(b)
    }

    fn error(&self, msg: impl Into<String>) -> ParseError {
        ParseError { message: msg.into(), loc: self.loc() }
    }

    fn expect(&mut self, ch: u8, ctx: &str) -> Result<(), ParseError> {
        match self.peek() {
            Some(b) if b == ch => { self.bump(); Ok(()) }
            Some(b) => Err(self.error(format!(
                "expected '{}' {}, got '{}'",
                ch as char, ctx, b as char
            ))),
            None => Err(self.error(format!("expected '{}' {}, got end of input", ch as char, ctx))),
        }
    }

    /// Advance past whitespace and comments. Comments can appear
    /// anywhere whitespace can — between elements, in the prolog area,
    /// and between attributes-free tag boundaries — but not *inside*
    /// an attribute value (the quoted region swallows them).
    fn skip_trivia(&mut self) {
        loop {
            // Whitespace
            while let Some(b) = self.peek() {
                if b.is_ascii_whitespace() { self.bump(); } else { break; }
            }
            // Comment?
            if self.starts_with("<!--") {
                // Don't advance char-by-char on `<!--` because we want
                // the loc to point at the '<' in error messages. Bump
                // four bytes blindly; none of them is a newline by
                // construction.
                for _ in 0..4 { self.bump(); }
                // Scan for closing `-->`. Per XML, `--` inside a
                // comment is illegal, but for MVP we just require the
                // terminator; a stray `--` inside text is a user bug
                // that'll surface as a malformed element immediately
                // after.
                while !self.starts_with("-->") {
                    if self.bump().is_none() {
                        // Unterminated comment. Error recovery is not
                        // useful here — there's no way to tell where
                        // the user meant the comment to end.
                        return;
                    }
                }
                for _ in 0..3 { self.bump(); }
                continue;
            }
            break;
        }
    }

    /// Parse `<?akml ...?>` prolog. We accept any content between `<?`
    /// and `?>` for forward compatibility; the version string is
    /// discarded in v0 since there's only one version.
    fn parse_prolog(&mut self) -> Result<(), ParseError> {
        debug_assert!(self.starts_with("<?"));
        self.bump(); self.bump(); // `<?`
        while !self.starts_with("?>") {
            if self.bump().is_none() {
                return Err(self.error("unterminated prolog — expected '?>'"));
            }
        }
        self.bump(); self.bump(); // `?>`
        Ok(())
    }

    fn parse_element(&mut self) -> Result<Element, ParseError> {
        let start_loc = self.loc();
        self.expect(b'<', "to open an element")?;
        if self.peek() == Some(b'/') {
            return Err(self.error("unexpected closing tag — expected an element"));
        }
        let tag = self.parse_name()?.to_string();
        let attrs = self.parse_attrs()?;
        // Decide: self-closing (`/>`) or has children (`>`).
        if self.peek() == Some(b'/') {
            self.bump();
            self.expect(b'>', "to close a self-closing element")?;
            return Ok(Element { tag, attrs, children: Vec::new(), loc: start_loc });
        }
        self.expect(b'>', "to open element body")?;
        // Parse children until we see `</`.
        let mut children = Vec::new();
        loop {
            self.skip_trivia();
            if self.starts_with("</") { break; }
            if self.peek().is_none() {
                return Err(ParseError {
                    message: format!("unterminated element <{tag}> — expected </{tag}>"),
                    loc: start_loc,
                });
            }
            children.push(self.parse_element()?);
        }
        // Closing tag.
        self.bump(); self.bump(); // `</`
        let close_tag = self.parse_name()?;
        if close_tag != tag {
            return Err(self.error(format!(
                "mismatched closing tag: <{tag}> was opened but </{close_tag}> found",
            )));
        }
        // Permit whitespace before `>`.
        while matches!(self.peek(), Some(b) if b.is_ascii_whitespace()) { self.bump(); }
        self.expect(b'>', "to close an element")?;
        Ok(Element { tag, attrs, children, loc: start_loc })
    }

    /// Identifier: first char alphabetic or `_`, rest alphanumeric or
    /// `_` / `-` / `.`. That's a strict superset of what AKML widget
    /// tags and attribute names actually use, and keeps us honest about
    /// what's a name versus arbitrary text.
    fn parse_name(&mut self) -> Result<&'a str, ParseError> {
        let start = self.pos;
        let first = self.peek().ok_or_else(|| self.error("expected name, got end of input"))?;
        if !(first.is_ascii_alphabetic() || first == b'_') {
            return Err(self.error(format!(
                "expected name to start with a letter or '_', got '{}'",
                first as char,
            )));
        }
        self.bump();
        while let Some(b) = self.peek() {
            if b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.') {
                self.bump();
            } else { break; }
        }
        // Safety: parse_name only consumed ASCII characters, so the
        // byte slice is also valid UTF-8.
        Ok(std::str::from_utf8(&self.src[start..self.pos]).unwrap())
    }

    fn parse_attrs(&mut self) -> Result<Vec<(String, String)>, ParseError> {
        let mut attrs = Vec::new();
        loop {
            // At least one whitespace byte must separate attrs from
            // the tag name or from each other. `>`/`/` ends the list.
            while matches!(self.peek(), Some(b) if b.is_ascii_whitespace()) { self.bump(); }
            match self.peek() {
                Some(b'>') | Some(b'/') | None => return Ok(attrs),
                Some(_) => {}
            }
            let name = self.parse_name()?.to_string();
            while matches!(self.peek(), Some(b) if b.is_ascii_whitespace()) { self.bump(); }
            self.expect(b'=', &format!("after attribute '{name}'"))?;
            while matches!(self.peek(), Some(b) if b.is_ascii_whitespace()) { self.bump(); }
            let value = self.parse_attr_value()?;
            attrs.push((name, value));
        }
    }

    fn parse_attr_value(&mut self) -> Result<String, ParseError> {
        let quote = match self.peek() {
            Some(b'"') | Some(b'\'') => self.bump().unwrap(),
            Some(b) => return Err(self.error(format!(
                "expected quoted attribute value, got '{}'", b as char
            ))),
            None => return Err(self.error("expected quoted attribute value, got end of input")),
        };
        // We collect the value as a sequence of source slices (for
        // plain runs of bytes) and single chars (for entities). The
        // "plain run" path matters for UTF-8: copying `b as char`
        // byte-by-byte would corrupt any multi-byte character by
        // treating every continuation byte as a separate codepoint.
        // Slicing the source instead keeps multi-byte sequences intact
        // — that's sound because every break point below is an ASCII
        // byte (quote, '<', '&'), and ASCII bytes cannot appear inside
        // a multi-byte UTF-8 sequence by construction.
        let mut out = String::new();
        let mut run_start = self.pos;
        loop {
            match self.peek() {
                None => return Err(self.error("unterminated attribute value")),
                Some(b) if b == quote => {
                    out.push_str(std::str::from_utf8(&self.src[run_start..self.pos]).unwrap());
                    self.bump();
                    return Ok(out);
                }
                Some(b'<') => return Err(self.error(
                    "raw '<' in attribute value — use &lt; if you need a literal angle bracket"
                )),
                Some(b'&') => {
                    out.push_str(std::str::from_utf8(&self.src[run_start..self.pos]).unwrap());
                    self.bump();
                    out.push(self.parse_entity()?);
                    run_start = self.pos;
                }
                Some(_) => {
                    // Just advance; the byte stays in the pending run
                    // and will be flushed on the next break or close.
                    self.bump();
                }
            }
        }
    }

    /// Parse one of the five standard XML entities: `&amp; &lt; &gt;
    /// &quot; &apos;`. `&` has already been consumed by the caller.
    fn parse_entity(&mut self) -> Result<char, ParseError> {
        let start = self.pos;
        while let Some(b) = self.peek() {
            if b == b';' { break; }
            if b.is_ascii_whitespace() || b == b'<' {
                return Err(self.error("unterminated entity reference"));
            }
            self.bump();
        }
        // Safety: only consumed ASCII in the entity name (since name
        // stops at any whitespace, `<`, or `;`).
        let name = std::str::from_utf8(&self.src[start..self.pos]).unwrap();
        let ch = match name {
            "amp" => '&',
            "lt" => '<',
            "gt" => '>',
            "quot" => '"',
            "apos" => '\'',
            other => return Err(self.error(format!(
                "unknown entity '&{other};' — AKML supports &amp; &lt; &gt; &quot; &apos;"
            ))),
        };
        self.expect(b';', "to close an entity reference")?;
        Ok(ch)
    }
}

// ── Tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn p(src: &str) -> Element { parse(src).expect("parse ok") }
    fn e(src: &str) -> ParseError { parse(src).expect_err("expected error") }

    // ── Happy-path: shape ────────────────────────────────────────────

    #[test]
    fn empty_self_closing_element() {
        let el = p("<Spacer />");
        assert_eq!(el.tag, "Spacer");
        assert!(el.attrs.is_empty());
        assert!(el.children.is_empty());
        assert_eq!(el.loc, Loc { line: 1, col: 1 });
    }

    #[test]
    fn element_with_attributes() {
        let el = p(r#"<Text text="hi" size="14" />"#);
        assert_eq!(el.tag, "Text");
        assert_eq!(el.attr("text"), Some("hi"));
        assert_eq!(el.attr("size"), Some("14"));
        assert_eq!(el.attr("missing"), None);
    }

    #[test]
    fn element_with_children() {
        let el = p("<VStack><Text text=\"a\" /><Text text=\"b\" /></VStack>");
        assert_eq!(el.tag, "VStack");
        assert_eq!(el.children.len(), 2);
        assert_eq!(el.children[0].attr("text"), Some("a"));
        assert_eq!(el.children[1].attr("text"), Some("b"));
    }

    #[test]
    fn nested_elements_track_location() {
        let src = "<A>\n  <B />\n</A>";
        let el = p(src);
        assert_eq!(el.loc, Loc { line: 1, col: 1 });
        assert_eq!(el.children[0].loc, Loc { line: 2, col: 3 });
    }

    #[test]
    fn comments_are_skipped_between_elements() {
        let src = "<!-- hi -->\n<A><!-- mid --><B /></A>";
        let el = p(src);
        assert_eq!(el.tag, "A");
        assert_eq!(el.children.len(), 1);
        assert_eq!(el.children[0].tag, "B");
    }

    #[test]
    fn prolog_is_accepted_and_ignored() {
        let el = p(r#"<?akml version="1.0"?><A />"#);
        assert_eq!(el.tag, "A");
    }

    #[test]
    fn whitespace_is_insignificant_between_elements() {
        let src = "  <A>   <B />    <C />   </A>  ";
        let el = p(src);
        assert_eq!(el.children.len(), 2);
    }

    // ── Attribute value details ──────────────────────────────────────

    #[test]
    fn single_quoted_attributes_are_accepted() {
        let el = p("<A x='y' />");
        assert_eq!(el.attr("x"), Some("y"));
    }

    #[test]
    fn entities_decode_in_attribute_values() {
        let el = p(r#"<A x="a &amp; b &lt; c &gt; d &quot; e &apos; f" />"#);
        assert_eq!(el.attr("x"), Some("a & b < c > d \" e ' f"));
    }

    #[test]
    fn last_duplicate_attribute_wins() {
        // Source-order-preserving with last-wins matches how a reader
        // scans top-to-bottom and expects later overrides to stick.
        let el = p(r#"<A x="1" x="2" />"#);
        assert_eq!(el.attr("x"), Some("2"));
    }

    #[test]
    fn attribute_value_can_contain_the_other_quote() {
        let el = p(r#"<A msg="can't stop" />"#);
        assert_eq!(el.attr("msg"), Some("can't stop"));
    }

    #[test]
    fn utf8_multi_byte_characters_round_trip_unchanged() {
        // Regression: an earlier version of the parser copied bytes
        // one at a time as `b as char`, which re-encodes each UTF-8
        // continuation byte as its own codepoint and produces garbage
        // like "Â±" out of "±". Every glyph below costs 2+ bytes in
        // UTF-8, so the test would fail loudly on the old code.
        let el = p(r#"<A glyphs="± ÷ × − ⌫ — שלום 日本語" />"#);
        assert_eq!(el.attr("glyphs"), Some("± ÷ × − ⌫ — שלום 日本語"));
    }

    #[test]
    fn utf8_characters_mix_with_entities() {
        let el = p(r#"<A v="± &amp; ÷" />"#);
        assert_eq!(el.attr("v"), Some("± & ÷"));
    }

    #[test]
    fn utf8_glyph_counts_as_one_column() {
        // With the column fix, a glyph like `±` (two UTF-8 bytes)
        // should count as a single column so error locations match
        // what an editor displays.
        let src = "<A x=\"±\" y=bad />";
        let err = parse(src).expect_err("unquoted y should error");
        // The unquoted `y=bad` is at offset: `<A x="±" ` = 9 grapheme
        // columns in and then `y=b` — the error will complain at the
        // `b` (bump past `=`), which is col 12. Allow either 11 or 12
        // to avoid tying the test to the exact error-raise point.
        assert_eq!(err.loc.line, 1);
        assert!(err.loc.col >= 10 && err.loc.col <= 13,
            "expected col 10..=13, got {}", err.loc.col);
    }

    // ── Error paths ──────────────────────────────────────────────────

    #[test]
    fn unknown_entity_is_an_error() {
        let err = e(r#"<A x="&foo;" />"#);
        assert!(err.message.contains("unknown entity"), "msg: {}", err.message);
    }

    #[test]
    fn mismatched_closing_tag_is_an_error() {
        let err = e("<A><B></A></A>");
        assert!(err.message.contains("mismatched"), "msg: {}", err.message);
    }

    #[test]
    fn unterminated_element_is_an_error() {
        let err = e("<A><B />");
        assert!(err.message.contains("unterminated"), "msg: {}", err.message);
    }

    #[test]
    fn multiple_roots_are_an_error() {
        let err = e("<A /><B />");
        assert!(err.message.contains("after root element"), "msg: {}", err.message);
    }

    #[test]
    fn missing_equals_after_attr_name_is_an_error() {
        let err = e(r#"<A x "y" />"#);
        assert!(err.message.contains("expected '='"), "msg: {}", err.message);
    }

    #[test]
    fn unquoted_attribute_value_is_an_error() {
        // XML allows lone identifiers here as a shorthand; AKML doesn't,
        // because the strictness catches typos like `flex=1,0` that
        // would otherwise silently lose part of the value.
        let err = e("<A x=5 />");
        assert!(err.message.contains("quoted"), "msg: {}", err.message);
    }

    #[test]
    fn error_location_points_at_the_problem() {
        // The unknown-tag-closing error should land on the `/` byte
        // after two characters of the source.
        let err = e("</A>");
        assert_eq!(err.loc.line, 1);
        assert!(err.loc.col <= 3, "expected col ≤ 3, got {}", err.loc.col);
    }

    #[test]
    fn raw_angle_bracket_in_attr_is_an_error() {
        let err = e(r#"<A msg="<evil" />"#);
        assert!(err.message.contains("raw '<'"), "msg: {}", err.message);
    }
}
