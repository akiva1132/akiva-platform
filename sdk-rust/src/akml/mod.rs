//! AKML — Akiva Markup Language, a declarative UI description format.
//!
//! See `docs/akml.md` for the full surface. This module ships the
//! parsing + inflation path that an app running on the Phase 3a widget
//! toolkit can actually use today.
//!
//! ## MVP scope (and what's deferred)
//!
//! This first pass is deliberately narrow — `docs/akml.md` is the
//! long-term target, but we land it **stone by stone**:
//!
//! | area               | supported now                        | deferred |
//! |--------------------|--------------------------------------|----------|
//! | elements           | `VStack`, `HStack`, `Frame`, `Text`, `Button`, `Rect`, `Spacer` | `Grid`, `Image`, `Input` |
//! | attributes         | `id`, `gap`, `padding`, `main`, `cross`, `flex`, `size`, `color`, `text`, `label`, `min_width`, `min_height`, `onClick` | `margin`, `background`, `visible`, `textAlign`, `fontWeight`, `tag` |
//! | attribute values   | literal numbers, `#RGB` / `#RRGGBB` / `#AARRGGBB` / `0xAARRGGBB` colours, enum strings | `dp`/`sp`/`%` suffixes, `match_parent`/`wrap_content` |
//! | resources          | none — literals only                 | `@string/*`, `@color/*`, `@dimen/*`, `@style/*` |
//! | click correlation  | `id="eq"` → `ui.was_clicked("eq")`, or `onClick="equals"` → `ui.was_triggered("equals")` — pull-based, no closure registry | `tag="val"` payload threaded through the same hook |
//! | bindings           | none — the AKML is static for now    | `{name}` interpolation with per-frame substitutions |
//!
//! Every deferred row is a Phase-4 or later concern. This module will
//! grow to meet them in additive passes; nothing here is intended as
//! a permanent restriction.
//!
//! ## Usage
//!
//! ```ignore
//! use akiva_sdk::prelude::*;
//! use akiva_sdk::akml;
//!
//! const LAYOUT: &str = include_str!("../res/layout/main.akml");
//!
//! fn on_render(&mut self, ctx: &mut Context, c: &mut Canvas) {
//!     c.clear(0xff15172a);
//!     let mut ui = Ui::new(&mut self.ui, ctx, c);
//!     let root = akml::inflate_str(&mut ui, LAYOUT).expect("layout");
//!     ui.present(root);
//!     if ui.was_clicked("eq") { /* ... */ }
//! }
//! ```

pub mod bindings;
pub mod parser;
pub mod inflate;

pub use bindings::Bindings;
pub use parser::{parse, Element, Loc, ParseError};
pub use inflate::{inflate, inflate_str, inflate_with, inflate_str_with, InflateError};
