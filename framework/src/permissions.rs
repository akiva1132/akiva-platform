//! Permission controller.
//!
//! Per ARCHITECTURE.md §8, permissions in Akiva are enforced *at
//! instantiation time*, not at every call: the package manager decides
//! which WIT imports to wire up based on the manifest's granted
//! permissions. Runtime checks exist only for dangerous operations where
//! the grant can change during a session (camera, location — post-MVP).

use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProtectionLevel {
    Normal,     // Auto-granted at install time.
    Dangerous,  // Requires runtime prompt.
    Signature,  // Requires same signing key (post-MVP).
    System,     // Reserved for first-party apps.
}

#[derive(Debug, Clone)]
pub struct PermissionDecl {
    pub name: String,
    pub description: String,
    pub protection: ProtectionLevel,
}

pub struct PermissionController {
    declarations: HashMap<String, PermissionDecl>,
    grants: HashMap<String, HashSet<String>>, // package_id → granted permission names
}

impl PermissionController {
    pub fn new() -> Self {
        Self {
            declarations: HashMap::new(),
            grants: HashMap::new(),
        }
    }

    pub fn declare(&mut self, decl: PermissionDecl) {
        self.declarations.insert(decl.name.clone(), decl);
    }

    pub fn grant(&mut self, package_id: &str, permission: &str) -> bool {
        if !self.declarations.contains_key(permission) { return false; }
        self.grants
            .entry(package_id.to_string())
            .or_insert_with(HashSet::new)
            .insert(permission.to_string());
        true
    }

    pub fn revoke(&mut self, package_id: &str, permission: &str) {
        if let Some(set) = self.grants.get_mut(package_id) {
            set.remove(permission);
        }
    }

    pub fn check(&self, package_id: &str, permission: &str) -> bool {
        self.grants.get(package_id)
            .map(|set| set.contains(permission))
            .unwrap_or(false)
    }

    pub fn granted_to(&self, package_id: &str) -> Vec<&str> {
        self.grants.get(package_id)
            .map(|set| set.iter().map(String::as_str).collect())
            .unwrap_or_default()
    }
}
