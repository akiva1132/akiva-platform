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

    /// `true` iff `permission` was declared by the system. Used by
    /// `PackageManager::install` to reject a manifest that requests a
    /// permission the system has never heard of — catching typos at
    /// install time rather than at instantiation.
    pub fn is_declared(&self, permission: &str) -> bool {
        self.declarations.contains_key(permission)
    }

    /// Snapshot of every declared permission. Order is not guaranteed.
    /// Used for diagnostics and the Shell's upcoming permissions UI.
    pub fn declarations(&self) -> impl Iterator<Item = &PermissionDecl> {
        self.declarations.values()
    }
}

// ── MVP permission catalog ───────────────────────────────────────────────
//
// Canonical names of the six permissions the system declares at boot,
// per ARCHITECTURE.md §8. Apps reference them by string; these constants
// exist so framework code paths (install grant, capability→import
// mapping in Stone 10.3) can refer to the same identifier the manifest
// uses without risking a typo.

pub const STORAGE_SANDBOX: &str = "akiva.permission.STORAGE_SANDBOX";
pub const QUERY_PACKAGES:  &str = "akiva.permission.QUERY_PACKAGES";
pub const LAUNCH_ANY_APP:  &str = "akiva.permission.LAUNCH_ANY_APP";
pub const NOTIFICATIONS:   &str = "akiva.permission.NOTIFICATIONS";
pub const NETWORK:         &str = "akiva.permission.NETWORK";
pub const CAMERA:          &str = "akiva.permission.CAMERA";

/// Populate `controller` with the MVP permission set. Called once from
/// `framework::boot` before any package rehydration — `install` later
/// relies on every required permission being declared here, otherwise
/// it rejects the install with `InstallError::UnknownPermission`.
///
/// All six land at `ProtectionLevel::Normal` in v0.2: auto-granted at
/// install time with no user prompt. Upgrading specific permissions to
/// `Dangerous` (runtime prompt) or `System` (first-party only) is a
/// post-MVP concern — the data model already supports it.
pub fn declare_mvp_permissions(controller: &mut PermissionController) {
    let set = [
        (STORAGE_SANDBOX, "Per-app persistent storage sandbox."),
        (QUERY_PACKAGES,  "Enumerate other installed packages (Shell-level capability)."),
        (LAUNCH_ANY_APP,  "Launch activities in other packages (Shell-level capability)."),
        (NOTIFICATIONS,   "Post notifications to the system tray."),
        (NETWORK,         "Make outbound network requests."),
        (CAMERA,          "Capture images from a camera device."),
    ];
    for (name, description) in set {
        controller.declare(PermissionDecl {
            name: name.to_string(),
            description: description.to_string(),
            protection: ProtectionLevel::Normal,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn declare_mvp_permissions_registers_all_six() {
        let mut c = PermissionController::new();
        declare_mvp_permissions(&mut c);
        for name in [
            STORAGE_SANDBOX, QUERY_PACKAGES, LAUNCH_ANY_APP,
            NOTIFICATIONS, NETWORK, CAMERA,
        ] {
            assert!(c.is_declared(name), "{name} must be declared after boot-time registration");
        }
        assert_eq!(c.declarations().count(), 6,
            "MVP must declare exactly six permissions; adding one requires a roadmap entry");
    }

    #[test]
    fn mvp_permissions_are_all_normal_protection() {
        // v0.2 deliberately auto-grants every requested permission at
        // install time (no app store → no prompt UX). Upgrading any of
        // these to `Dangerous` breaks that invariant and the install
        // path has to learn to route through a prompt — so this test
        // fires a tripwire before that regression ships.
        let mut c = PermissionController::new();
        declare_mvp_permissions(&mut c);
        for decl in c.declarations() {
            assert_eq!(decl.protection, ProtectionLevel::Normal,
                "{} was upgraded beyond Normal — install-time auto-grant path needs a prompt step", decl.name);
        }
    }

    #[test]
    fn grant_rejects_undeclared_permission() {
        let c = &mut PermissionController::new();
        assert!(!c.grant("test.pkg", "akiva.permission.MADE_UP"),
            "grant must return false for a permission the system never declared");
        assert!(!c.check("test.pkg", "akiva.permission.MADE_UP"));
    }

    #[test]
    fn grant_then_check_roundtrips() {
        let mut c = PermissionController::new();
        declare_mvp_permissions(&mut c);
        assert!(c.grant("test.pkg", NETWORK));
        assert!(c.check("test.pkg", NETWORK));
        assert!(!c.check("test.pkg", CAMERA),
            "granting NETWORK must not imply CAMERA");
        // granted_to only returns what was actually granted.
        let granted: Vec<&str> = c.granted_to("test.pkg");
        assert_eq!(granted, vec![NETWORK]);
    }

    #[test]
    fn revoke_removes_a_grant_without_touching_others() {
        let mut c = PermissionController::new();
        declare_mvp_permissions(&mut c);
        c.grant("test.pkg", NETWORK);
        c.grant("test.pkg", CAMERA);
        c.revoke("test.pkg", NETWORK);
        assert!(!c.check("test.pkg", NETWORK));
        assert!(c.check("test.pkg", CAMERA),
            "revoking one permission must not drop the others");
    }
}
