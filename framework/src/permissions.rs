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
    "akiva_host_log",
    "akiva_host_text_width",
    "akiva_host_resource_color",
    "akiva_host_resource_string",
    "akiva_host_resource_has_color",
    "akiva_host_resource_has_string",
    "akiva_host_finish",
    "akiva_host_set_result",
    // Stone 11.3b — three multiplexers that subsume the per-op imports
    // above as 11.3c migrates call sites. They're auto-granted because
    // the capability check moves *inside* the dispatcher: a single
    // function can't be gated by a single permission since it serves
    // many op_kinds (some always-available, some gated). The dispatcher
    // in `runtime/web/app_registry.js` consults the same grants table
    // before running an op_kind that maps to a gated operation.
    "akiva_host_fire",
    "akiva_host_io_start",
    "akiva_host_io_poll",
];

/// Imports gated behind a specific permission. An app that imports one
/// of these without the matching grant must fail at instantiation
/// time — the host simply doesn't provide the function, producing a
/// wasm `LinkError`. Empty-list capabilities (NOTIFICATIONS, NETWORK,
/// CAMERA) exist in the catalog for future APIs; gating them now means
/// manifests can already declare them without a mapping regression
/// later.
pub fn imports_for_capability(name: &str) -> &'static [&'static str] {
    match name {
        LAUNCH_ANY_APP => &[
            "akiva_host_start_activity",
            "akiva_host_start_activity_for_result",
        ],
        // Stone 11.0a — QUERY_PACKAGES unlocks `akiva_host_query_packages`,
        // the two-phase reader for the framework's package-list snapshot
        // pushed at boot via `WasmHost::set_packages`. Without the grant
        // the JS deny-stub returns 0 (== "no packages") — same shape an
        // app sees on a freshly-booted system with nothing installed.
        QUERY_PACKAGES => &[
            "akiva_host_query_packages",
        ],
        // Storage HAL surface hasn't reached apps yet (framework-side
        // persistence goes through WasmHost::save/restore on the
        // kernel side, not app-visible imports). The mapping slot is
        // reserved for when `akiva_host_storage_*` lands.
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
    fn launch_any_app_adds_start_activity_imports() {
        let mut c = PermissionController::new();
        declare_mvp_permissions(&mut c);
        // Without the grant, start_activity is NOT in the import set —
        // an app that imports it will fail instantiation (Stone 10.3b
        // JS-side enforcement; the mapping here is the source of
        // truth it will consult).
        let before = imports_for_grants(&c, "test.shell");
        assert!(!before.contains(&"akiva_host_start_activity"));
        assert!(!before.contains(&"akiva_host_start_activity_for_result"));

        c.grant("test.shell", LAUNCH_ANY_APP);
        let after = imports_for_grants(&c, "test.shell");
        assert!(after.contains(&"akiva_host_start_activity"),
            "LAUNCH_ANY_APP must unlock start_activity");
        assert!(after.contains(&"akiva_host_start_activity_for_result"),
            "LAUNCH_ANY_APP must unlock start_activity_for_result");
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
    fn query_packages_adds_query_import() {
        // Stone 11.0a — QUERY_PACKAGES must unlock
        // `akiva_host_query_packages`, the two-phase reader for the
        // package-list snapshot. Mirrors the JS-side
        // `CAPABILITY_IMPORTS["akiva.permission.QUERY_PACKAGES"]`
        // entry; if the two diverge the JS deny-stub will mask a
        // real call (or expose a gated one), so this test fires the
        // tripwire on either side.
        let mut c = PermissionController::new();
        declare_mvp_permissions(&mut c);
        let before = imports_for_grants(&c, "test.shell");
        assert!(!before.contains(&"akiva_host_query_packages"));

        c.grant("test.shell", QUERY_PACKAGES);
        let after = imports_for_grants(&c, "test.shell");
        assert!(after.contains(&"akiva_host_query_packages"),
            "QUERY_PACKAGES must unlock akiva_host_query_packages");
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
