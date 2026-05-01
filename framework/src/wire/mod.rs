//! Wire encoders / decoders for the host↔app ABI.
//!
//! Stone 11.3 — every host-mediated operation gets a stable u32
//! `op_kind` (see [`op_kinds`]) and a binary payload encoded by a
//! sub-module here. The macro layout mirrors the WIT type definitions
//! we'll generate when the platform migrates to the Component Model in
//! v0.5+ (see [`docs/isolation-design.md §4.5`]); each `op_kind` will
//! become a WIT function and each encoder/decoder pair will become
//! generated bindings.
//!
//! Today's modules:
//!
//! - [`op_kinds`] — u32 constants and status codes.
//! - [`pkgs`] — encoder/decoder for the package-list snapshot the
//!   host pushes via `hal_host_set_packages` (Stone 11.0a).
//!
//! Future modules (added as Stone 11.3 progresses):
//!
//! - `log` — log-line args.
//! - `start_activity` — handle + encoded Intent.
//! - `storage` — get/set args + result blobs.
//! - `lifecycle` — lifecycle dispatch payloads (post-isolation).

pub mod op_kinds;
pub mod pkgs;
