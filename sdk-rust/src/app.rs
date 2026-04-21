//! App entry-point trait.
//!
//! Every Akiva app implements `App`. The `#[akiva::app]` proc-macro (lives
//! in a separate `akiva-sdk-macros` crate, Phase 2) generates the WIT
//! export glue so authors only write the `impl App`.

use crate::activity::ActivityContext;

pub trait App: 'static {
    fn on_create(&mut self, ctx: &mut AppContext) { let _ = ctx; }
    fn on_low_memory(&mut self, ctx: &mut AppContext) { let _ = ctx; }
    fn on_config_changed(&mut self, ctx: &mut AppContext) { let _ = ctx; }
    fn on_terminate(&mut self, ctx: &mut AppContext) { let _ = ctx; }
}

/// Context passed to app-level callbacks. Provides access to storage,
/// resources, and package-level info. Runtime-provided — apps never
/// construct one directly.
pub struct AppContext {
    pub(crate) _private: (),
}

impl AppContext {
    /// Spawn a new activity within this app. Returns the activity handle
    /// that the runtime hands back through `on_activity_create`.
    pub fn launch_activity(&mut self, _class: &str) -> ActivityContext {
        unimplemented!("launch_activity: Phase 2")
    }
}
