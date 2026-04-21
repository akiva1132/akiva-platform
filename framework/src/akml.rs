//! AKML parser + inflater.
//!
//! Akiva Markup Language is the declarative layout language documented in
//! docs/akml.md. Structurally similar to Android XML layouts but uses
//! a compact text syntax closer to JSX. The parser produces a widget
//! tree that the `widgets` module renders.
//!
//! Phase 1a: types only. Parser lands in Phase 2.

use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct AkmlNode {
    pub tag: String,
    pub attrs: HashMap<String, AkmlValue>,
    pub children: Vec<AkmlNode>,
}

#[derive(Debug, Clone)]
pub enum AkmlValue {
    Literal(String),
    ResourceRef { kind: String, name: String }, // @string/foo, @dimen/bar
    Dimen { value: f32, unit: DimenUnit },
    Color(u32),
    Bool(bool),
}

#[derive(Debug, Clone, Copy)]
pub enum DimenUnit {
    Dp,   // density-independent pixel
    Sp,   // scale-independent pixel (text)
    Px,   // raw pixels (discouraged)
    Percent,
    WrapContent,
    MatchParent,
}

#[derive(Debug)]
pub enum ParseError {
    UnexpectedToken { line: u32, col: u32, found: String },
    UnclosedTag(String),
    InvalidAttribute { tag: String, attr: String, reason: String },
    BadResourceRef(String),
}

pub fn parse(_source: &str) -> Result<AkmlNode, ParseError> {
    // Stub: Phase 2.
    Err(ParseError::UnexpectedToken { line: 0, col: 0, found: "<parser not implemented>".into() })
}
