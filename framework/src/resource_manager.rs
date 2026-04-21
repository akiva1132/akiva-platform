//! Resource manager.
//!
//! Resolves `@string/...`, `@drawable/...`, `@dimen/...`, `@color/...`
//! references against the currently active qualifier configuration
//! (locale, density, orientation, night mode). See docs/resources.md.

use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Qualifiers {
    pub locale: [u8; 4],       // BCP-47 prefix packed; MVP: "en" / "he"
    pub density: Density,
    pub orientation: Orientation,
    pub night: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Density {
    Mdpi,
    Hdpi,
    #[default]
    Xhdpi,
    Xxhdpi,
    Xxxhdpi,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Orientation {
    #[default]
    Any,
    Portrait,
    Landscape,
}

#[derive(Debug, Clone)]
pub enum ResourceValue {
    String(String),
    Dimen(f32),
    Color(u32),
    Drawable(DrawableRef),
}

#[derive(Debug, Clone)]
pub enum DrawableRef {
    Png { bytes: Vec<u8> },
    // Svg / jpg deferred to v0.2+ per docs/resources.md §8.
}

pub struct ResourceManager {
    qualifiers: Qualifiers,
    strings: HashMap<String, String>,
    dimens: HashMap<String, f32>,
    colors: HashMap<String, u32>,
    drawables: HashMap<String, DrawableRef>,
}

impl ResourceManager {
    pub fn new() -> Self {
        Self {
            qualifiers: Qualifiers::default(),
            strings: HashMap::new(),
            dimens: HashMap::new(),
            colors: HashMap::new(),
            drawables: HashMap::new(),
        }
    }

    pub fn set_qualifiers(&mut self, q: Qualifiers) {
        self.qualifiers = q;
        // Phase 2: reload the active resource set for the new qualifiers.
    }

    pub fn qualifiers(&self) -> Qualifiers { self.qualifiers }

    pub fn resolve_string(&self, key: &str) -> Option<&str> {
        self.strings.get(key).map(String::as_str)
    }
    pub fn resolve_dimen(&self, key: &str) -> Option<f32> {
        self.dimens.get(key).copied()
    }
    pub fn resolve_color(&self, key: &str) -> Option<u32> {
        self.colors.get(key).copied()
    }
    pub fn resolve_drawable(&self, key: &str) -> Option<&DrawableRef> {
        self.drawables.get(key)
    }
}
