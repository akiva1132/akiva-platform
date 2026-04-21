//! Resource accessor. Mirrors the framework's ResourceManager API through
//! the WIT bindings. Apps call `res::string("app_name")` → resolves against
//! the active qualifier config.

pub fn string(_key: &str) -> Option<String> { None }
pub fn dimen(_key: &str) -> Option<f32> { None }
pub fn color(_key: &str) -> Option<u32> { None }
