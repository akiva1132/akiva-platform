//! App loader.
//!
//! Loading an app in MVP means: fetch `app.wasm` + `manifest.toml` via the
//! HAL, validate the manifest against the schema in docs/manifest.md, then
//! hand the bytes to the framework's PackageManager. The kernel itself does
//! not instantiate the app wasm — that belongs to the framework's
//! ActivityManager, which owns the component-model instance and wires up
//! the WIT imports declared in wit/akiva.wit.
//!
//! Phase 1a: loader is stubbed. Phase 2 wires it up to PackageManager.

#[derive(Debug)]
pub struct AppPackage {
    pub manifest_toml: String,
    pub wasm_bytes: Vec<u8>,
    pub resources: Vec<(String, Vec<u8>)>,
}

#[derive(Debug)]
pub enum LoadError {
    NotFound,
    ManifestInvalid,
    WasmInvalid,
    Io,
}

/// Stub. Will delegate to `hal_storage_read` for builtin apps, then HTTP
/// fetch (over postMessage → main thread fetch) for installed packages.
pub fn load(app_id: &str) -> Result<AppPackage, LoadError> {
    let _ = app_id;
    Err(LoadError::NotFound)
}
