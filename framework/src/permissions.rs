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

// ── Capability → host-import mapping (Stone 10.3) ─────────────────────
//
// ARCHITECTURE.md §8 pins which `akiva_host_*` imports become available
// to a wasm app based on which capabilities the system granted it. The
// table below is the canonical source of truth — worker.js's
// per-package import map will consult `imports_for_grants()` (via a
// kernel bridge, Stone 10.3b) to decide which functions to provide
// when instantiating an app.
//
// Only *gated* imports are listed. Auto-granted imports (log,
// resource lookups, text measurement, finish, set_result) land in
// every app's import map unconditionally and aren't tracked here.

/// Imports every installed app gets regardless of grants. These are
/// the foundational functions the lifecycle / render / input loop
/// depend on; gating them would mean the app can't even draw.
pub const AUTO_IMPORTS: &[&str] = &[
    // Resource lookups stay legacy intentionally — they're per-package
    // readonly queries that don't fit the fire/io shape (no async, no
    // side effect, no handle context), and consolidating them under
    // op_kinds would just add encode/decode overhead for no benefit.
    "akiva_host_resource_color",
    "akiva_host_resource_string",
    "akiva_host_resource_has_color",
    "akiva_host_resource_has_string",
    // Stone 11.3 unified-ABI multiplexers. Capability gating moved
    // entirely into the dispatcher (one import serves many op_kinds —
    // a single permission can't gate the import surface). Mirror in
    // `runtime/web/app_registry.js`.
    "akiva_host_fire",
    "akiva_host_io_start",
    "akiva_host_io_poll",
];

/// Imports gated behind a specific permission. After Stone 11.3-final
/// every fire/io op runs through the unified-ABI multiplexers and
/// capability gating happens *inside* the dispatcher (per op_kind),
/// not at the import surface. So every entry here is empty today —
/// the table is preserved as a tripwire: a future per-op import that
/// doesn't fit the fire/io shape (e.g. a streaming surface) registers
/// here and the host's `_applyPermissionFilter` plus the JS-side
/// `CAPABILITY_IMPORTS` mirror pick it up automatically.
pub fn imports_for_capability(name: &str) -> &'static [&'static str] {
    match name {
        LAUNCH_ANY_APP  => &[],
        QUERY_PACKAGES  => &[],
        STORAGE_SANDBOX => &[],
        NOTIFICATIONS   => &[],
        NETWORK         => &[],
        CAMERA          => &[],
        _ => &[],
    }
}

/// Compute the full set of `akiva_host_*` imports to provide to
/// `package_id`'s wasm instance. Always includes [`AUTO_IMPORTS`];
/// adds the gated imports for every capability the controller has
/// granted. The returned slice is deterministic but unsorted — callers
/// that need a stable serialization (e.g. the kernel→JS bridge in
/// Stone 10.3b) should sort before emitting.
pub fn imports_for_grants(
    controller: &PermissionController,
    package_id: &str,
) -> Vec<&'static str> {
    let mut out: Vec<&'static str> = AUTO_IMPORTS.to_vec();
    for granted in controller.granted_to(package_id) {
        out.extend(imports_for_capability(granted));
    }
    out
}

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

    // ── Stone 10.3: capability → import mapping ───────────────────────

    #[test]
    fn imports_for_grants_always_includes_auto_set() {
        // An app with zero grants still gets the foundational imports.
        // Otherwise hello (which requests no permissions) couldn't
        // even boot.
        let mut c = PermissionController::new();
        declare_mvp_permissions(&mut c);
        let imports = imports_for_grants(&c, "test.pkg");
        for &name in AUTO_IMPORTS {
            assert!(imports.contains(&name),
                "auto import {name} must be in every app's set");
        }
    }

    #[test]
    fn every_capability_contributes_zero_imports_after_11_3_final() {
        // After Stone 11.3-final the start_activity pair, query_packages,
        // log, finish, set_result, and text_width are all gone from the
        // import surface — they live as op_kinds inside the unified ABI.
        // Capability gating happens per call inside the host's
        // `_dispatchFire` / `_dispatchIoStart`, not via the import map.
        // So granting *any* capability adds nothing to `imports_for_grants`
        // — the auto set is what every app sees, and the dispatcher
        // enforces gates from there.
        let mut c = PermissionController::new();
        declare_mvp_permissions(&mut c);
        let baseline = imports_for_grants(&c, "test.pkg");
        for &cap in &[LAUNCH_ANY_APP, QUERY_PACKAGES, STORAGE_SANDBOX,
                      NOTIFICATIONS, NETWORK, CAMERA] {
            c.grant("test.pkg", cap);
            let after = imports_for_grants(&c, "test.pkg");
            assert_eq!(baseline, after,
                "granting {cap} added imports — capability lists should be empty post-11.3");
        }
    }

    #[test]
    fn unknown_capability_contributes_no_imports() {
        // A typo or future-unreleased permission name must not silently
        // shadow the auto set. `imports_for_capability` returns the
        // empty slice for anything it doesn't recognise.
        assert!(imports_for_capability("akiva.permission.TYPO").is_empty());
        assert!(imports_for_capability("").is_empty());
    }

    #[test]
    fn unified_abi_trio_is_in_auto_imports() {
        // Stone 11.3b — the three multiplexers must be available to every
        // app regardless of grants. Capability gating moves into the
        // dispatcher (in `runtime/web/app_registry.js`) since a single
        // import serves many op_kinds. If this test fails, the JS side
        // and Rust side have drifted and an app will either fail to
        // instantiate (LinkError on a missing import) or silently lose
        // capability gating (an ungated dispatcher).
        for name in ["akiva_host_fire", "akiva_host_io_start", "akiva_host_io_poll"] {
            assert!(AUTO_IMPORTS.contains(&name),
                "{name} must be in AUTO_IMPORTS — Stone 11.3b unified ABI");
        }
    }

    #[test]
    fn storage_sandbox_reserved_slot_is_currently_empty() {
        // Reminder to whoever wires `akiva_host_storage_*` into the
        // app-side HAL: update `imports_for_capability` at the same
        // time, or this test starts looking like a lie. Not a hard
        // failure — the mapping is correct today (no storage imports
        // exist yet), but the assertion pins the current contract.
        assert!(imports_for_capability(STORAGE_SANDBOX).is_empty(),
            "STORAGE_SANDBOX still has zero imports — update this test when akiva_host_storage_* lands");
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
