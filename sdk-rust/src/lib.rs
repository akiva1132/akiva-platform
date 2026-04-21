//! Akiva SDK for Rust apps.
//!
//! This crate is what app authors depend on. It wraps the generated WIT
//! bindings (see `wit/akiva.wit`) in an ergonomic Rust API: the `App`
//! trait, the `#[akiva::app]` proc-macro, typed intent builders, resource
//! accessors, and a logging facade. App code should never need to import
//! the raw bindings — the `bindings` feature exposes them for exceptional
//! cases only.

pub mod activity;
pub mod app;
pub mod intent;
pub mod log;
pub mod res;
pub mod storage;

pub use activity::{Activity, ActivityContext, SavedState};
pub use app::{App, AppContext};
pub use intent::{Intent, IntentBuilder};

// Re-export the handful of types app authors touch directly.
pub mod prelude {
    pub use crate::activity::{Activity, ActivityContext, SavedState};
    pub use crate::app::{App, AppContext};
    pub use crate::intent::{Intent, IntentBuilder};
    pub use crate::{log_debug, log_error, log_info, log_warn};
}
