//! Package-level `App` trait — forward-compat stub.
//!
//! Not driven by the v0 runtime (which only exposes per-activity
//! callbacks). Kept here so that when the Component Model path lands with
//! a real application-level lifecycle (`on_low_memory`, `on_config_changed`,
//! `on_terminate`), apps can start writing against it today and the
//! runtime will pick them up without source churn.

pub trait App: 'static {
    fn on_create(&mut self, ctx: &mut AppContext) { let _ = ctx; }
    fn on_low_memory(&mut self, ctx: &mut AppContext) { let _ = ctx; }
    fn on_config_changed(&mut self, ctx: &mut AppContext) { let _ = ctx; }
    fn on_terminate(&mut self, ctx: &mut AppContext) { let _ = ctx; }
}

/// Context passed to app-level callbacks. Empty in v0.
pub struct AppContext {
    pub(crate) _private: (),
}
