//! Activity trait and lifecycle.
//!
//! Mirrors AOSP's Activity with the refinements documented in
//! docs/activity-model.md: three-layer state (UI / ViewModel / Persistent),
//! explicit `on_save_state` / `on_restore_state` hooks bounded at 64KB.

use std::collections::HashMap;

pub trait Activity: 'static {
    fn on_create(&mut self, ctx: &mut ActivityContext, saved: Option<&SavedState>) {
        let _ = (ctx, saved);
    }
    fn on_start(&mut self, ctx: &mut ActivityContext) { let _ = ctx; }
    fn on_resume(&mut self, ctx: &mut ActivityContext) { let _ = ctx; }
    fn on_pause(&mut self, ctx: &mut ActivityContext) { let _ = ctx; }
    fn on_stop(&mut self, ctx: &mut ActivityContext) { let _ = ctx; }
    fn on_destroy(&mut self, ctx: &mut ActivityContext) { let _ = ctx; }

    /// UI-state snapshot. Budget: 64 KiB. Use `StorageApi` for larger data.
    fn on_save_state(&self, out: &mut SavedState) { let _ = out; }
    fn on_restore_state(&mut self, saved: &SavedState) { let _ = saved; }

    /// Notified when a config change (orientation, locale, night mode)
    /// would otherwise trigger recreate, if the manifest opts in.
    fn on_config_changed(&mut self, ctx: &mut ActivityContext) { let _ = ctx; }
}

pub struct ActivityContext {
    pub(crate) _private: (),
}

#[derive(Debug, Default, Clone)]
pub struct SavedState {
    pub entries: HashMap<String, SavedValue>,
}

#[derive(Debug, Clone)]
pub enum SavedValue {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    String(String),
    Bytes(Vec<u8>),
}

impl SavedState {
    pub const MAX_BYTES: usize = 64 * 1024;

    pub fn put_int(&mut self, key: &str, v: i64) {
        self.entries.insert(key.into(), SavedValue::Int(v));
    }
    pub fn put_string(&mut self, key: &str, v: impl Into<String>) {
        self.entries.insert(key.into(), SavedValue::String(v.into()));
    }
    pub fn get_int(&self, key: &str) -> Option<i64> {
        match self.entries.get(key)? { SavedValue::Int(v) => Some(*v), _ => None }
    }
    pub fn get_string(&self, key: &str) -> Option<&str> {
        match self.entries.get(key)? { SavedValue::String(v) => Some(v), _ => None }
    }
}
