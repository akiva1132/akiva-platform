//! Window manager.
//!
//! Arranges activity surfaces into displays. An "activity window" is the
//! logical frame an activity renders into; a "display" is a physical
//! OffscreenCanvas owned by a tab. The window manager maps windows onto
//! displays and drives the compositor.
//!
//! See docs/window-manager.md for the design.

use std::collections::HashMap;

use crate::activity_manager::ActivityId;

pub type WindowId = u64;
pub type DisplayId = u32;

#[derive(Debug, Clone, Copy)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

#[derive(Debug)]
pub struct Window {
    pub id: WindowId,
    pub activity: ActivityId,
    pub display: DisplayId,
    pub bounds: Rect,
    pub z_order: u32,
    pub visible: bool,
}

pub struct WindowManager {
    windows: HashMap<WindowId, Window>,
    next_id: WindowId,
}

impl WindowManager {
    pub fn new() -> Self {
        Self { windows: HashMap::new(), next_id: 1 }
    }

    pub fn add_window(&mut self, activity: ActivityId, display: DisplayId, bounds: Rect) -> WindowId {
        let id = self.next_id;
        self.next_id += 1;
        self.windows.insert(id, Window {
            id,
            activity,
            display,
            bounds,
            z_order: 0,
            visible: true,
        });
        id
    }

    pub fn remove_window(&mut self, id: WindowId) -> bool {
        self.windows.remove(&id).is_some()
    }

    pub fn set_bounds(&mut self, id: WindowId, bounds: Rect) {
        if let Some(w) = self.windows.get_mut(&id) {
            w.bounds = bounds;
        }
    }

    pub fn z_ordered(&self, display: DisplayId) -> Vec<&Window> {
        let mut v: Vec<_> = self.windows.values()
            .filter(|w| w.display == display && w.visible)
            .collect();
        v.sort_by_key(|w| w.z_order);
        v
    }
}
