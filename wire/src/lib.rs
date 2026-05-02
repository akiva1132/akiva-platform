//! Shared wire-format codecs for Akiva.
//!
//! Stone 11.6.7 (#2) — collapses what used to be three independent
//! decoders (framework `wire/pkgs.rs`, SDK `pkgs.rs`, JS
//! `app_registry.js::_warmupWorkers`) into one canonical Rust source.
//! Both Rust crates that touch the wire format depend on this crate;
//! the JS side consults `pkgs.rs`'s schema-comment to stay in sync,
//! flagged at code-review time when the layout changes.
//!
//! Each module corresponds to one ABI primitive. Add a new module
//! here when a new wire format crosses the kernel/app boundary —
//! never duplicate the codec elsewhere. The whole point of this
//! crate is to make "where does this layout live?" a one-answer
//! question.

pub mod pkgs;
