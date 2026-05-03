//! Package manager.
//!
//! Owns the persistent registry of installed apps. Each package lives in
//! the kernel KV store under these keys:
//!
//! ```text
//! sys/packages/index                       → LF-separated list of package IDs
//! packages/{id}/manifest.toml              → parsed manifest bytes
//! packages/{id}/app.wasm                   → app wasm module bytes
//! packages/{id}/res/values/strings.toml    → @string/ resource pairs (optional)
//! packages/{id}/res/values/colors.toml     → @color/  resource pairs (optional)
//! ```
//!
//! At boot, [`PackageManager::load_installed`] rebuilds the in-memory map
//! from the index. A manifest that fails to parse is logged and skipped —
//! one bad package must not brick the whole boot. [`install`] is the
//! inverse: it parses + validates before it touches storage, so a failure
//! leaves the store untouched.
//!
//! The app wasm bytes are *not* held in memory after install/boot — the
//! runtime loader reads them on demand via [`read_wasm`]. That keeps the
//! resident set small for installations with many apps.
//!
//! Resource files are absent-by-default: an app that declares no strings
//! or no colors is legal, the loader returns an empty table for those
//! lookups. Corrupt resource files get reported but never block the
//! package from launching — the app just shows `⟨missing:foo⟩` and
//! magenta where the refs would have resolved.

use std::collections::HashMap;

use crate::activity_manager::LaunchMode;
use crate::manifest::{self, Manifest, ManifestError, RUNTIME_ABI_VERSION};
use crate::permissions::PermissionController;
use crate::resources::{ResourceError, ResourceTable};
use crate::storage::{Storage, StorageError};

/// A snapshot of one installed app. The fields that apps actually launch
/// against (id, activities, launch modes) are duplicated from `manifest`
/// so lookup paths don't have to re-traverse the TOML-sourced tree.
#[derive(Debug, Clone)]
pub struct PackageRecord {
    pub id: String,
    pub name: String,
    pub version: String,
    pub version_code: u64,
    pub entry_class: String,
    pub exported_classes: Vec<String>,
    pub manifest: Manifest,
}

impl PackageRecord {
    fn from_manifest(m: Manifest) -> Self {
        let entry = m.entry_activity().class.clone();
        let exported = m.activities.iter()
            .filter(|a| a.exported)
            .map(|a| a.class.clone())
            .collect();
        Self {
            id: m.app.id.clone(),
            name: m.app.name.clone(),
            version: m.app.version.clone(),
            version_code: m.app.version_code,
            entry_class: entry,
            exported_classes: exported,
            manifest: m,
        }
    }

    /// Launch mode declared for a class in this package, if any.
    pub fn launch_mode_of(&self, class: &str) -> Option<LaunchMode> {
        self.manifest.activities.iter()
            .find(|a| a.class == class)
            .map(|a| a.launch_mode)
    }

    /// Drawable name from `manifest.app.icon` with the `@drawable/`
    /// prefix stripped. Returns `""` for packages without an icon.
    /// Used by the package-list snapshot encoder so the Shell can
    /// render foreign-package launcher icons without re-parsing
    /// resource refs.
    pub fn icon_drawable_name(&self) -> &str {
        match self.manifest.app.icon.as_deref() {
            Some(s) if s.starts_with("@drawable/") => &s["@drawable/".len()..],
            _ => "",
        }
    }
}

#[derive(Debug)]
pub enum InstallError {
    Manifest(ManifestError),
    AbiMismatch { required: String, runtime: String },
    /// Package with this id is already installed and the offered
    /// `version_code` is lower than what's on record. Matches Android's
    /// posture of never silently downgrading.
    DowngradeRejected { id: String, installed: u64, offered: u64 },
    /// Package with this id is already installed and the offered
    /// `version_code` matches exactly. Distinct from downgrade so the
    /// caller can tell "stale archive" from "forgot to bump the code".
    /// Upgrades (strictly greater `version_code`) succeed silently.
    SameVersionRejected { id: String, version_code: u64 },
    /// The manifest's `permissions.required` list names a permission
    /// the system hasn't declared. Catches typos at install time rather
    /// than letting the app silently link without the capability it
    /// asked for.
    UnknownPermission { package_id: String, name: String },
    Storage(StorageError),
}

impl core::fmt::Display for InstallError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            InstallError::Manifest(e) => write!(f, "{e}"),
            InstallError::AbiMismatch { required, runtime } => {
                write!(f, "ABI mismatch: package needs >= {required}, runtime is {runtime}")
            }
            InstallError::DowngradeRejected { id, installed, offered } => {
                write!(f, "downgrade rejected for {id}: installed version_code={installed}, offered={offered}")
            }
            InstallError::SameVersionRejected { id, version_code } => {
                write!(f, "same version already installed for {id}: version_code={version_code} (bump version_code to upgrade)")
            }
            InstallError::UnknownPermission { package_id, name } => {
                write!(f, "unknown permission '{name}' required by {package_id} — not declared by the system")
            }
            InstallError::Storage(e) => write!(f, "storage error: {e:?}"),
        }
    }
}

impl From<ManifestError> for InstallError {
    fn from(e: ManifestError) -> Self { InstallError::Manifest(e) }
}
impl From<StorageError> for InstallError {
    fn from(e: StorageError) -> Self { InstallError::Storage(e) }
}

const KEY_INDEX: &str = "sys/packages/index";

fn key_manifest(id: &str) -> String { format!("packages/{id}/manifest.toml") }
fn key_wasm(id: &str) -> String { format!("packages/{id}/app.wasm") }
fn key_values_strings(id: &str) -> String { format!("packages/{id}/res/values/strings.toml") }
fn key_values_colors(id: &str) -> String { format!("packages/{id}/res/values/colors.toml") }
fn key_values_night_strings(id: &str) -> String {
    format!("packages/{id}/res/values-night/strings.toml")
}
fn key_values_night_colors(id: &str) -> String {
    format!("packages/{id}/res/values-night/colors.toml")
}

/// Storage key for the LF-separated list of permissions granted to a
/// package. Written on install, deleted on uninstall, read at boot so
/// grants persist across reboots.
///
/// Kept under `sys/` (not `packages/`) because these are system-side
/// grants — an app listing its own granted permissions without going
/// through the `PermissionController` would violate the capability
/// invariant.
fn key_permissions(id: &str) -> String { format!("sys/permissions/{id}") }

/// Storage key for an activity's persisted saved-state blob. Scoped to
/// `(package, class)` so two activity classes in the same app keep their
/// state independent, matching Android's onSaveInstanceState model.
pub fn key_saved_state(package_id: &str, class: &str) -> String {
    format!("packages/{package_id}/activities/{class}/saved_state.bin")
}

pub struct PackageManager {
    packages: HashMap<String, PackageRecord>,
}

impl PackageManager {
    pub fn new() -> Self { Self { packages: HashMap::new() } }

    /// Rebuild the in-memory registry from persistent storage. Safe to call
    /// multiple times; each call replaces the current set. Returns the
    /// number of packages that loaded cleanly. Per-package failures are
    /// reported via `on_error` but never abort the scan.
    pub fn load_installed(
        &mut self,
        storage: &dyn Storage,
        mut on_error: impl FnMut(&str, LoadError),
    ) -> usize {
        self.packages.clear();
        let index_bytes = match storage.read(KEY_INDEX) {
            Ok(b) => b,
            Err(StorageError::NotFound) => return 0, // first boot
            Err(e) => {
                on_error("<index>", LoadError::Storage(e));
                return 0;
            }
        };
        let Ok(index_str) = core::str::from_utf8(&index_bytes) else {
            on_error("<index>", LoadError::CorruptIndex);
            return 0;
        };
        let mut loaded = 0usize;
        for id in index_str.lines().filter(|l| !l.is_empty()) {
            match self.load_one(storage, id) {
                Ok(rec) => {
                    self.packages.insert(rec.id.clone(), rec);
                    loaded += 1;
                }
                Err(e) => on_error(id, e),
            }
        }
        loaded
    }

    fn load_one(&self, storage: &dyn Storage, id: &str) -> Result<PackageRecord, LoadError> {
        let bytes = storage.read(&key_manifest(id)).map_err(LoadError::Storage)?;
        let src = core::str::from_utf8(&bytes).map_err(|_| LoadError::CorruptManifest)?;
        let manifest = manifest::parse(src).map_err(LoadError::Manifest)?;
        if manifest.app.id != id {
            return Err(LoadError::IndexDesync {
                index_id: id.to_string(),
                manifest_id: manifest.app.id,
            });
        }
        Ok(PackageRecord::from_manifest(manifest))
    }

    /// Read every loaded package's `res/values/` files and parse them
    /// into [`ResourceTable`]s. Missing files are legal — an app with no
    /// strings or no colors gets an empty table on that side. A broken
    /// file is reported through `on_error` and the package still ends
    /// up with an empty table registered for it, so subsequent
    /// `@string/` / `@color/` lookups fail deterministically rather
    /// than blowing up mid-render.
    ///
    /// Returned in insertion order of the caller's choice — callers
    /// that want a stable order should sort by id. The framework's
    /// boot path doesn't care since it just feeds these into
    /// `ResourceManager::register`.
    pub fn read_resource_tables(
        &self,
        storage: &dyn Storage,
        mut on_error: impl FnMut(&str, ResourceLoadError),
    ) -> Vec<(String, ResourceTable)> {
        let mut out = Vec::with_capacity(self.packages.len());
        for id in self.packages.keys() {
            let strings = match read_optional_text(storage, &key_values_strings(id)) {
                Ok(v) => v,
                Err(e) => { on_error(id, e); None }
            };
            let colors = match read_optional_text(storage, &key_values_colors(id)) {
                Ok(v) => v,
                Err(e) => { on_error(id, e); None }
            };
            // Stone 11.6.9 #3 — load `values-night/` overlay alongside
            // the default. Missing files are fine; a parse error here
            // surfaces through `on_error` but the default still
            // succeeds, so the package boots in light mode without a
            // working dark mode rather than failing to boot entirely.
            let night_strings = match read_optional_text(storage, &key_values_night_strings(id)) {
                Ok(v) => v,
                Err(e) => { on_error(id, e); None }
            };
            let night_colors = match read_optional_text(storage, &key_values_night_colors(id)) {
                Ok(v) => v,
                Err(e) => { on_error(id, e); None }
            };
            let table = match ResourceTable::parse_with_night(
                strings.as_deref(), colors.as_deref(),
                night_strings.as_deref(), night_colors.as_deref(),
            ) {
                Ok(t) => t,
                Err(e) => {
                    on_error(id, ResourceLoadError::Parse(e));
                    ResourceTable::default()
                }
            };
            out.push((id.clone(), table));
        }
        out
    }

    /// Install or upgrade a package. Parses + validates the manifest,
    /// checks ABI compatibility, then atomically-ish writes manifest +
    /// wasm + updated index. "Atomically-ish" because we don't have
    /// transactions; a crash mid-write can leave the index out of sync
    /// with the per-package keys, but `load_installed` tolerates that
    /// (orphan keys are benign, orphan index entries are reported via
    /// `on_error`).
    ///
    /// Upgrade semantics: a second install for an id already on file is
    /// accepted only if the new `version_code` is strictly greater than
    /// the installed one. Equal and lower are rejected with distinct
    /// errors so the caller can tell "stale archive" from "forgot to
    /// bump." On MAJOR-version bumps (`app.version`), persisted
    /// activity `saved_state` blobs are wiped — a breaking version
    /// change is the developer saying "the old state may no longer be
    /// meaningful." Minor/patch bumps keep saved_state intact so an
    /// upgrade is seamless for the user.
    ///
    /// Resources (`res/values/*.toml`) are not rewritten here — the
    /// caller performing the install (CLI or dev bootstrap in
    /// worker.js) owns resource deployment, and writing fresh resource
    /// bytes alongside the manifest is enough for the next boot's
    /// `read_resource_tables` to pick them up.
    pub fn install(
        &mut self,
        storage: &mut dyn Storage,
        permissions: &mut PermissionController,
        manifest_toml: &str,
        wasm_bytes: &[u8],
    ) -> Result<String, InstallError> {
        let manifest = manifest::parse(manifest_toml)?;
        check_abi(&manifest.app.min_abi_version)?;

        let id = manifest.app.id.clone();
        let offered_code = manifest.app.version_code;

        // Stone 10.2 — validate every required permission is declared
        // by the system BEFORE we touch storage. Catching a typo'd
        // permission name after we've half-written the package leaves
        // the install in a partial state; doing the check first means
        // a bad manifest bounces cleanly.
        for name in &manifest.permissions.required {
            if !permissions.is_declared(name) {
                return Err(InstallError::UnknownPermission {
                    package_id: id.clone(),
                    name: name.clone(),
                });
            }
        }

        if let Some(existing) = self.packages.get(&id) {
            if offered_code < existing.version_code {
                return Err(InstallError::DowngradeRejected {
                    id,
                    installed: existing.version_code,
                    offered: offered_code,
                });
            }
            if offered_code == existing.version_code {
                return Err(InstallError::SameVersionRejected { id, version_code: offered_code });
            }
            // Strict upgrade. On a MAJOR-version change, sweep the
            // old manifest's activity saved_state keys so the freshly
            // upgraded app doesn't inherit blobs whose encoding it
            // may no longer understand. Minor/patch upgrades keep the
            // blobs — the ABI between them is the developer's promise.
            let same_major = major_of(&existing.manifest.app.version) == major_of(&manifest.app.version);
            if !same_major {
                for activity in &existing.manifest.activities {
                    // Ignore NotFound — best-effort, matches uninstall().
                    let _ = storage.delete(&key_saved_state(&id, &activity.class));
                }
            }

            storage.write(&key_manifest(&id), manifest_toml.as_bytes())?;
            storage.write(&key_wasm(&id), wasm_bytes)?;
            // Upgrade: revoke any grants from the previous version that
            // the new manifest no longer requests (app tightened its
            // permissions). Then grant whatever the new manifest asks
            // for, and overwrite the persisted grants file.
            let new_set: std::collections::HashSet<&str> =
                manifest.permissions.required.iter().map(String::as_str).collect();
            let old_grants: Vec<String> = permissions.granted_to(&id)
                .into_iter().map(String::from).collect();
            for old in old_grants {
                if !new_set.contains(old.as_str()) {
                    permissions.revoke(&id, &old);
                }
            }
            apply_grants(storage, permissions, &id, &manifest.permissions.required)?;

            // Index already contains this id — no update needed.
            self.packages.insert(id.clone(), PackageRecord::from_manifest(manifest));
            return Ok(id);
        }

        // First install for this id.
        storage.write(&key_manifest(&id), manifest_toml.as_bytes())?;
        storage.write(&key_wasm(&id), wasm_bytes)?;
        apply_grants(storage, permissions, &id, &manifest.permissions.required)?;

        let mut ids: Vec<String> = self.packages.keys().cloned().collect();
        ids.push(id.clone());
        ids.sort();
        storage.write(KEY_INDEX, ids.join("\n").as_bytes())?;

        self.packages.insert(id.clone(), PackageRecord::from_manifest(manifest));
        Ok(id)
    }

    /// Remove a package. Returns `true` if it was installed. Failures in
    /// the middle leave the store in a partially-uninstalled state, which
    /// the next `load_installed` heals.
    pub fn uninstall(
        &mut self,
        storage: &mut dyn Storage,
        permissions: &mut PermissionController,
        id: &str,
    ) -> bool {
        if self.packages.remove(id).is_none() {
            return false;
        }
        // Best-effort: missing key == already gone, not an error.
        let _ = storage.delete(&key_wasm(id));
        let _ = storage.delete(&key_manifest(id));
        let _ = storage.delete(&key_values_strings(id));
        let _ = storage.delete(&key_values_colors(id));
        let _ = storage.delete(&key_values_night_strings(id));
        let _ = storage.delete(&key_values_night_colors(id));
        let _ = storage.delete(&key_permissions(id));
        // Drop in-memory grants for this package (Stone 10.5 — keeps
        // `fw.permissions` from holding stale grants against a dead
        // package id that a future reinstall could inherit by mistake).
        let stale: Vec<String> = permissions.granted_to(id)
            .into_iter().map(String::from).collect();
        for p in stale {
            permissions.revoke(id, &p);
        }
        let mut ids: Vec<String> = self.packages.keys().cloned().collect();
        ids.sort();
        let _ = storage.write(KEY_INDEX, ids.join("\n").as_bytes());
        true
    }

    /// Rehydrate granted permissions into `controller` from the
    /// `sys/permissions/{id}` blobs written by `install`. Called once
    /// from `framework::boot` after `load_installed` finishes, so both
    /// in-memory package state and grant state reflect what's on disk
    /// before any activity is instantiated. Returns the total number
    /// of grants rehydrated; per-package failures are reported through
    /// `on_error` but never abort the scan (a corrupt grants file is
    /// worse than missing — better to boot without grants than to
    /// refuse to boot at all).
    pub fn load_granted_permissions(
        &self,
        storage: &dyn Storage,
        controller: &mut PermissionController,
        mut on_error: impl FnMut(&str, LoadError),
    ) -> usize {
        let mut count = 0usize;
        for id in self.packages.keys() {
            let bytes = match storage.read(&key_permissions(id)) {
                Ok(b) => b,
                Err(StorageError::NotFound) => continue, // app installed before 10.2, or no permissions
                Err(e) => { on_error(id, LoadError::Storage(e)); continue; }
            };
            let Ok(text) = core::str::from_utf8(&bytes) else {
                on_error(id, LoadError::CorruptManifest);
                continue;
            };
            for name in text.lines().filter(|l| !l.is_empty()) {
                // `grant` silently returns false if the system no longer
                // declares this permission — e.g. a future boot dropped
                // a permission from the catalog. The grants file is the
                // source of historical truth, not the override.
                if controller.grant(id, name) {
                    count += 1;
                }
            }
        }
        count
    }

    pub fn get(&self, id: &str) -> Option<&PackageRecord> { self.packages.get(id) }

    pub fn list(&self) -> impl Iterator<Item = &PackageRecord> { self.packages.values() }

    pub fn count(&self) -> usize { self.packages.len() }

    /// Read the wasm bytes for a package. Not cached — callers typically
    /// invoke this once during loader instantiation, then let the
    /// host-side registry hold the module.
    pub fn read_wasm(&self, storage: &dyn Storage, id: &str) -> Result<Vec<u8>, StorageError> {
        if !self.packages.contains_key(id) { return Err(StorageError::NotFound); }
        storage.read(&key_wasm(id))
    }
}

#[derive(Debug)]
pub enum LoadError {
    Storage(StorageError),
    Manifest(ManifestError),
    CorruptIndex,
    CorruptManifest,
    /// The index said `id` is installed but the manifest at
    /// `packages/{id}/...` declares a different package id.
    IndexDesync { index_id: String, manifest_id: String },
}

/// Why reading a package's resource tables failed. Distinct from
/// [`LoadError`] so resource issues can be reported without polluting the
/// package-load error channel — a broken resource file shouldn't look
/// like a broken manifest to a human reading the boot log.
#[derive(Debug)]
pub enum ResourceLoadError {
    Storage(StorageError),
    /// File existed but wasn't valid UTF-8 — a rare case since our own
    /// tooling writes ASCII-safe TOML, but worth flagging explicitly
    /// instead of silently dropping to "parse error".
    NotUtf8,
    Parse(ResourceError),
}

impl core::fmt::Display for ResourceLoadError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            ResourceLoadError::Storage(e) => write!(f, "storage: {e:?}"),
            ResourceLoadError::NotUtf8 => write!(f, "resource file is not valid UTF-8"),
            ResourceLoadError::Parse(e) => write!(f, "{e}"),
        }
    }
}

/// Read a key that may or may not exist. Missing → `Ok(None)`; present
/// but invalid UTF-8 → `Err(NotUtf8)`; storage I/O failure → `Err(Storage)`.
fn read_optional_text(
    storage: &dyn Storage,
    key: &str,
) -> Result<Option<String>, ResourceLoadError> {
    match storage.read(key) {
        Ok(bytes) => String::from_utf8(bytes)
            .map(Some)
            .map_err(|_| ResourceLoadError::NotUtf8),
        Err(StorageError::NotFound) => Ok(None),
        Err(e) => Err(ResourceLoadError::Storage(e)),
    }
}

/// Grant every permission in `required` to `package_id` (they've
/// already been validated as declared by the caller) and persist the
/// resulting set to `sys/permissions/{id}`. Called from the
/// fresh-install and upgrade branches of [`PackageManager::install`],
/// and from `framework::boot`'s self-heal pass for packages that were
/// staged without going through the install path (dev-apps).
///
/// We write the **full** new set rather than appending, so an upgrade
/// that removed a permission leaves behind only what the new manifest
/// asks for. The order of lines is stable (iteration over the input
/// slice) to keep the blob diff-able.
pub(crate) fn apply_grants(
    storage: &mut dyn Storage,
    controller: &mut PermissionController,
    package_id: &str,
    required: &[String],
) -> Result<(), InstallError> {
    for name in required {
        // `grant` returns false only when the permission isn't declared,
        // which `install` already ruled out. Treat a false return as an
        // assertion failure rather than a silent no-op.
        debug_assert!(controller.is_declared(name),
            "apply_grants called with undeclared permission '{}'; install should reject earlier", name);
        controller.grant(package_id, name);
    }
    let blob = required.join("\n");
    if blob.is_empty() {
        // Empty blob still written so boot-time rehydration can tell
        // "installed with no permissions" from "permissions file
        // missing because this was a pre-10.2 install". Readers can
        // treat both the same way today, but the distinction may
        // matter once we add upgrade-time audit logging.
        storage.write(&key_permissions(package_id), &[])?;
    } else {
        storage.write(&key_permissions(package_id), blob.as_bytes())?;
    }
    Ok(())
}

/// Verify the package's `min_abi_version` is compatible with the runtime.
/// Same-major-only compat for now — we can relax to full semver ranges
/// once we have multiple ABI versions in the wild.
fn check_abi(required: &str) -> Result<(), InstallError> {
    let req = parse_triple(required);
    let have = parse_triple(RUNTIME_ABI_VERSION);
    match (req, have) {
        (Some(r), Some(h)) if h.0 == r.0 && (h.1, h.2) >= (r.1, r.2) => Ok(()),
        _ => Err(InstallError::AbiMismatch {
            required: required.into(),
            runtime: RUNTIME_ABI_VERSION.into(),
        }),
    }
}

/// Extract the MAJOR component of a semver-shaped `app.version`. Used
/// by `install` to decide whether an upgrade wipes activity
/// `saved_state`. The manifest validator has already asserted the
/// overall shape (`MAJOR.MINOR.PATCH[-pre]`), so we don't need to
/// reject garbage here — a string that makes it past parse and can't
/// yield a MAJOR returns `None` and is treated as a mismatch by the
/// caller, which is conservative (wipes state on ambiguity).
fn major_of(version: &str) -> Option<u32> {
    parse_triple(version).map(|(major, _, _)| major)
}

fn parse_triple(s: &str) -> Option<(u32, u32, u32)> {
    let core = s.split_once('-').map(|(c, _)| c).unwrap_or(s);
    let mut it = core.split('.');
    let a = it.next()?.parse().ok()?;
    let b = it.next()?.parse().ok()?;
    let c = it.next()?.parse().ok()?;
    if it.next().is_some() { return None; }
    Some((a, b, c))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::MemoryStorage;

    const HELLO: &str = r#"
[app]
id = "com.akiva.hello"
name = "Hello"
version = "0.1.0"
version_code = 1
min_abi_version = "0.1.0"
[runtime]
wasm = "hello.wasm"
[[activities]]
id = "main"
class = "HelloMain"
entry = true
"#;

    const WORLD: &str = r#"
[app]
id = "com.akiva.world"
name = "World"
version = "0.1.0"
version_code = 1
min_abi_version = "0.1.0"
[runtime]
wasm = "world.wasm"
[[activities]]
id = "main"
entry = true
"#;

    #[test]
    fn install_then_list() {
        let mut store = MemoryStorage::new();
        let mut pm = PackageManager::new();
        let mut pc = PermissionController::new();
        crate::permissions::declare_mvp_permissions(&mut pc);
        pm.install(&mut store, &mut pc, HELLO, b"\0asm\x01\x00\x00\x00").unwrap();
        assert_eq!(pm.count(), 1);
        let rec = pm.get("com.akiva.hello").unwrap();
        assert_eq!(rec.entry_class, "HelloMain");
    }

    #[test]
    fn install_is_persistent() {
        let mut store = MemoryStorage::new();
        let mut pm = PackageManager::new();
        let mut pc = PermissionController::new();
        crate::permissions::declare_mvp_permissions(&mut pc);
        pm.install(&mut store, &mut pc, HELLO, b"wasm-bytes").unwrap();
        pm.install(&mut store, &mut pc, WORLD, b"wasm-bytes").unwrap();
        drop(pm);

        // Fresh manager, same storage — should rehydrate both.
        let mut pm2 = PackageManager::new();
        let mut pc = PermissionController::new();
        crate::permissions::declare_mvp_permissions(&mut pc);
        let loaded = pm2.load_installed(&store, |id, e| panic!("load error for {id}: {e:?}"));
        assert_eq!(loaded, 2);
        assert!(pm2.get("com.akiva.hello").is_some());
        assert!(pm2.get("com.akiva.world").is_some());
    }

    #[test]
    fn same_version_install_rejected() {
        // Same id + same version_code is the "forgot to bump" case.
        // Distinct error so a CLI can suggest the fix.
        let mut store = MemoryStorage::new();
        let mut pm = PackageManager::new();
        let mut pc = PermissionController::new();
        crate::permissions::declare_mvp_permissions(&mut pc);
        pm.install(&mut store, &mut pc, HELLO, b"x").unwrap();
        let err = pm.install(&mut store, &mut pc, HELLO, b"x").unwrap_err();
        assert!(matches!(
            err,
            InstallError::SameVersionRejected { version_code: 1, .. }
        ));
    }

    #[test]
    fn downgrade_rejected() {
        // Installing version_code=1 after version_code=2 is blocked.
        let mut store = MemoryStorage::new();
        let mut pm = PackageManager::new();
        let mut pc = PermissionController::new();
        crate::permissions::declare_mvp_permissions(&mut pc);
        let v2 = HELLO.replace("version_code = 1", "version_code = 2");
        pm.install(&mut store, &mut pc, &v2, b"new").unwrap();
        let err = pm.install(&mut store, &mut pc, HELLO, b"old").unwrap_err();
        assert!(matches!(
            err,
            InstallError::DowngradeRejected { installed: 2, offered: 1, .. }
        ));
        // Storage still carries the v2 wasm — downgrade must not have
        // overwritten anything.
        assert_eq!(store.read(&key_wasm("com.akiva.hello")).unwrap(), b"new");
    }

    #[test]
    fn upgrade_replaces_wasm_and_record() {
        // Minor bump: new wasm lands, old wasm is gone, record
        // reflects the new version_code.
        let mut store = MemoryStorage::new();
        let mut pm = PackageManager::new();
        let mut pc = PermissionController::new();
        crate::permissions::declare_mvp_permissions(&mut pc);
        pm.install(&mut store, &mut pc, HELLO, b"v1-wasm").unwrap();
        // Only bump `version`, not `min_abi_version` — a naive string
        // replace would match both and trip AbiMismatch.
        let v2 = HELLO
            .replace("version_code = 1", "version_code = 2")
            .replacen("version = \"0.1.0\"", "version = \"0.2.0\"", 1);
        pm.install(&mut store, &mut pc, &v2, b"v2-wasm").unwrap();

        assert_eq!(pm.count(), 1);
        assert_eq!(pm.get("com.akiva.hello").unwrap().version_code, 2);
        assert_eq!(store.read(&key_wasm("com.akiva.hello")).unwrap(), b"v2-wasm");
    }

    #[test]
    fn upgrade_minor_preserves_saved_state() {
        // Installing 0.1.0 → 0.2.0 (same major) must keep the blob
        // an activity persisted via onSaveInstanceState.
        let mut store = MemoryStorage::new();
        let mut pm = PackageManager::new();
        let mut pc = PermissionController::new();
        crate::permissions::declare_mvp_permissions(&mut pc);
        pm.install(&mut store, &mut pc, HELLO, b"v1-wasm").unwrap();
        let state_key = key_saved_state("com.akiva.hello", "HelloMain");
        store.write(&state_key, b"persisted blob").unwrap();

        // Only bump `version`, not `min_abi_version` — a naive string
        // replace would match both and trip AbiMismatch.
        let v2 = HELLO
            .replace("version_code = 1", "version_code = 2")
            .replacen("version = \"0.1.0\"", "version = \"0.2.0\"", 1);
        pm.install(&mut store, &mut pc, &v2, b"v2-wasm").unwrap();
        assert_eq!(store.read(&state_key).unwrap(), b"persisted blob");
    }

    #[test]
    fn upgrade_major_clears_saved_state() {
        // 0.1.0 → 1.0.0 is a breaking version bump. Any state blob
        // persisted for an activity on the old major must be wiped so
        // the new code doesn't decode incompatible bytes.
        let mut store = MemoryStorage::new();
        let mut pm = PackageManager::new();
        let mut pc = PermissionController::new();
        crate::permissions::declare_mvp_permissions(&mut pc);
        pm.install(&mut store, &mut pc, HELLO, b"v0-wasm").unwrap();
        let state_key = key_saved_state("com.akiva.hello", "HelloMain");
        store.write(&state_key, b"legacy blob").unwrap();

        let v1 = HELLO
            .replace("version_code = 1", "version_code = 2")
            .replacen("version = \"0.1.0\"", "version = \"1.0.0\"", 1);
        pm.install(&mut store, &mut pc, &v1, b"v1-wasm").unwrap();
        assert!(matches!(store.read(&state_key), Err(StorageError::NotFound)));
    }

    #[test]
    fn uninstall_removes_all_traces() {
        let mut store = MemoryStorage::new();
        let mut pm = PackageManager::new();
        let mut pc = PermissionController::new();
        crate::permissions::declare_mvp_permissions(&mut pc);
        pm.install(&mut store, &mut pc, HELLO, b"wasm").unwrap();
        assert!(pm.uninstall(&mut store, &mut pc, "com.akiva.hello"));

        let mut pm2 = PackageManager::new();
        let mut pc = PermissionController::new();
        crate::permissions::declare_mvp_permissions(&mut pc);
        pm2.load_installed(&store, |id, e| panic!("load error for {id}: {e:?}"));
        assert_eq!(pm2.count(), 0);
        // The wasm key should be gone too.
        assert!(matches!(store.read(&key_wasm("com.akiva.hello")), Err(StorageError::NotFound)));
    }

    #[test]
    fn first_boot_no_index() {
        let store = MemoryStorage::new();
        let mut pm = PackageManager::new();
        let mut pc = PermissionController::new();
        crate::permissions::declare_mvp_permissions(&mut pc);
        let loaded = pm.load_installed(&store, |_, _| panic!("unexpected error"));
        assert_eq!(loaded, 0);
    }

    #[test]
    fn corrupted_manifest_is_reported_not_fatal() {
        let mut store = MemoryStorage::new();
        let mut pm = PackageManager::new();
        let mut pc = PermissionController::new();
        crate::permissions::declare_mvp_permissions(&mut pc);
        pm.install(&mut store, &mut pc, HELLO, b"x").unwrap();
        // Deliberately overwrite one manifest with garbage.
        store.write(&key_manifest("com.akiva.hello"), b"not valid toml =====").unwrap();

        let mut pm2 = PackageManager::new();
        let mut pc = PermissionController::new();
        crate::permissions::declare_mvp_permissions(&mut pc);
        let mut errors = Vec::new();
        pm2.load_installed(&store, |id, e| errors.push((id.to_string(), format!("{e:?}"))));
        assert_eq!(pm2.count(), 0);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].0, "com.akiva.hello");
    }

    #[test]
    fn index_desync_is_reported() {
        let mut store = MemoryStorage::new();
        // Write an index pointing at a package that doesn't exist.
        store.write(KEY_INDEX, b"com.akiva.ghost").unwrap();
        let mut pm = PackageManager::new();
        let mut pc = PermissionController::new();
        crate::permissions::declare_mvp_permissions(&mut pc);
        let mut errors = Vec::new();
        pm.load_installed(&store, |id, e| errors.push((id.to_string(), format!("{e:?}"))));
        assert_eq!(pm.count(), 0);
        assert_eq!(errors.len(), 1);
    }

    #[test]
    fn abi_mismatch_rejected() {
        let future = r#"
[app]
id = "com.akiva.future"
name = "Future"
version = "0.1.0"
version_code = 1
min_abi_version = "1.0.0"
[runtime]
wasm = "x.wasm"
[[activities]]
id = "main"
entry = true
"#;
        let mut store = MemoryStorage::new();
        let mut pm = PackageManager::new();
        let mut pc = PermissionController::new();
        crate::permissions::declare_mvp_permissions(&mut pc);
        let err = pm.install(&mut store, &mut pc, future, b"x").unwrap_err();
        assert!(matches!(err, InstallError::AbiMismatch { .. }));
    }

    #[test]
    fn read_wasm_roundtrip() {
        let mut store = MemoryStorage::new();
        let mut pm = PackageManager::new();
        let mut pc = PermissionController::new();
        crate::permissions::declare_mvp_permissions(&mut pc);
        pm.install(&mut store, &mut pc, HELLO, b"\0asm\x01").unwrap();
        let bytes = pm.read_wasm(&store, "com.akiva.hello").unwrap();
        assert_eq!(bytes, b"\0asm\x01");
    }

    #[test]
    fn resource_tables_read_populated_and_absent_packages() {
        let mut store = MemoryStorage::new();
        let mut pm = PackageManager::new();
        let mut pc = PermissionController::new();
        crate::permissions::declare_mvp_permissions(&mut pc);
        pm.install(&mut store, &mut pc, HELLO, b"x").unwrap();
        pm.install(&mut store, &mut pc, WORLD, b"x").unwrap();

        // Only HELLO gets resources; WORLD has no values/ at all.
        store.write(
            "packages/com.akiva.hello/res/values/strings.toml",
            br#"greeting = "hi""#,
        ).unwrap();
        store.write(
            "packages/com.akiva.hello/res/values/colors.toml",
            br##"bg = "#ff101418""##,
        ).unwrap();

        let mut errors = Vec::new();
        let tables = pm.read_resource_tables(&store, |id, e| {
            errors.push((id.to_string(), format!("{e}")))
        });
        assert!(errors.is_empty(), "unexpected errors: {errors:?}");
        assert_eq!(tables.len(), 2);

        let by_id: std::collections::HashMap<_, _> = tables.into_iter().collect();
        let cfg = crate::resources::DeviceConfig::default();
        let hello = &by_id["com.akiva.hello"];
        assert_eq!(hello.string("greeting", &cfg), Some("hi"));
        assert_eq!(hello.color("bg", &cfg), Some(0xff101418));

        let world = &by_id["com.akiva.world"];
        assert!(world.string("greeting", &cfg).is_none());
        assert!(world.color("bg", &cfg).is_none());
    }

    #[test]
    fn resource_tables_report_corrupt_files_but_still_emit_empty_table() {
        let mut store = MemoryStorage::new();
        let mut pm = PackageManager::new();
        let mut pc = PermissionController::new();
        crate::permissions::declare_mvp_permissions(&mut pc);
        pm.install(&mut store, &mut pc, HELLO, b"x").unwrap();
        store.write(
            "packages/com.akiva.hello/res/values/colors.toml",
            b"bg = \"not-a-colour\"",
        ).unwrap();

        let mut errors = Vec::new();
        let tables = pm.read_resource_tables(&store, |id, e| {
            errors.push((id.to_string(), format!("{e}")))
        });
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].0, "com.akiva.hello");
        // Still registered — just with empty contents, so @color/bg lookups
        // cleanly return "not found" rather than randomly crashing later.
        assert_eq!(tables.len(), 1);
        let cfg = crate::resources::DeviceConfig::default();
        assert!(tables[0].1.color("bg", &cfg).is_none());
    }

    #[test]
    fn uninstall_removes_resource_keys() {
        let mut store = MemoryStorage::new();
        let mut pm = PackageManager::new();
        let mut pc = PermissionController::new();
        crate::permissions::declare_mvp_permissions(&mut pc);
        pm.install(&mut store, &mut pc, HELLO, b"x").unwrap();
        store.write(
            "packages/com.akiva.hello/res/values/strings.toml",
            br#"x = "y""#,
        ).unwrap();
        // Stone 11.6.9 #3 — values-night/ overlay must also disappear
        // on uninstall so a future reinstall of a different package at
        // the same id can't inherit stale dark-mode colours.
        store.write(
            "packages/com.akiva.hello/res/values-night/colors.toml",
            br##"bg = "#ff111111""##,
        ).unwrap();
        pm.uninstall(&mut store, &mut pc, "com.akiva.hello");
        assert!(matches!(
            store.read("packages/com.akiva.hello/res/values/strings.toml"),
            Err(StorageError::NotFound),
        ));
        assert!(matches!(
            store.read("packages/com.akiva.hello/res/values-night/colors.toml"),
            Err(StorageError::NotFound),
        ));
    }

    #[test]
    fn resource_tables_load_values_night_overlay() {
        // Stone 11.6.9 #3 — `values-night/` overrides `values/` when
        // the device is in night mode. Lookups under DeviceConfig
        // { night: true } see the overlay; under default they don't.
        let mut store = MemoryStorage::new();
        let mut pm = PackageManager::new();
        let mut pc = PermissionController::new();
        crate::permissions::declare_mvp_permissions(&mut pc);
        pm.install(&mut store, &mut pc, HELLO, b"x").unwrap();

        store.write(
            "packages/com.akiva.hello/res/values/colors.toml",
            br##"bg = "#ffffffff""##,
        ).unwrap();
        store.write(
            "packages/com.akiva.hello/res/values-night/colors.toml",
            br##"bg = "#ff111111""##,
        ).unwrap();

        let tables = pm.read_resource_tables(&store, |_, _| {});
        let table = &tables[0].1;
        let day = crate::resources::DeviceConfig { night: false };
        let nite = crate::resources::DeviceConfig { night: true };
        assert_eq!(table.color("bg", &day),  Some(0xffffffff));
        assert_eq!(table.color("bg", &nite), Some(0xff111111));
    }

    // ── Stone 10.2: install-time grants + persistence ───────────────────

    const HELLO_WITH_NETWORK: &str = r#"
[app]
id = "com.akiva.hello"
name = "Hello"
version = "0.1.0"
version_code = 1
min_abi_version = "0.1.0"
[runtime]
wasm = "hello.wasm"
[permissions]
required = ["akiva.permission.NETWORK", "akiva.permission.STORAGE_SANDBOX"]
[[activities]]
id = "main"
class = "HelloMain"
entry = true
"#;

    const HELLO_WITH_UNKNOWN: &str = r#"
[app]
id = "com.akiva.hello"
name = "Hello"
version = "0.1.0"
version_code = 1
min_abi_version = "0.1.0"
[runtime]
wasm = "hello.wasm"
[permissions]
required = ["akiva.permission.MADE_UP"]
[[activities]]
id = "main"
class = "HelloMain"
entry = true
"#;

    #[test]
    fn install_grants_required_permissions() {
        // A manifest's `permissions.required` becomes live grants in
        // the controller post-install. The persistence blob holds the
        // same list for boot-time rehydration.
        let mut store = MemoryStorage::new();
        let mut pm = PackageManager::new();
        let mut pc = PermissionController::new();
        crate::permissions::declare_mvp_permissions(&mut pc);

        pm.install(&mut store, &mut pc, HELLO_WITH_NETWORK, b"x").unwrap();

        assert!(pc.check("com.akiva.hello", crate::permissions::NETWORK));
        assert!(pc.check("com.akiva.hello", crate::permissions::STORAGE_SANDBOX));
        assert!(!pc.check("com.akiva.hello", crate::permissions::CAMERA),
            "install must only grant what the manifest requests");

        let blob = store.read("sys/permissions/com.akiva.hello").unwrap();
        let text = std::str::from_utf8(&blob).unwrap();
        // Order preserved from the manifest list.
        assert_eq!(text,
            "akiva.permission.NETWORK\nakiva.permission.STORAGE_SANDBOX");
    }

    #[test]
    fn install_rejects_unknown_permission_before_touching_storage() {
        // A typo in `permissions.required` must fail the install
        // before any package bytes are written — otherwise a CLI would
        // have to unwind a half-committed install to clean up.
        let mut store = MemoryStorage::new();
        let mut pm = PackageManager::new();
        let mut pc = PermissionController::new();
        crate::permissions::declare_mvp_permissions(&mut pc);

        let err = pm.install(&mut store, &mut pc, HELLO_WITH_UNKNOWN, b"x").unwrap_err();
        assert!(matches!(err,
            InstallError::UnknownPermission { name, .. }
                if name == "akiva.permission.MADE_UP"));

        // Storage must be completely clean — no manifest, no wasm, no
        // index, no permissions file.
        assert!(matches!(store.read("packages/com.akiva.hello/manifest.toml"),
            Err(StorageError::NotFound)));
        assert!(matches!(store.read("packages/com.akiva.hello/app.wasm"),
            Err(StorageError::NotFound)));
        assert!(matches!(store.read("sys/packages/index"),
            Err(StorageError::NotFound)));
        assert!(matches!(store.read("sys/permissions/com.akiva.hello"),
            Err(StorageError::NotFound)));
    }

    #[test]
    fn grants_rehydrate_from_storage_on_boot() {
        // Install in one PackageManager+PermissionController pair, then
        // drop them and rebuild from storage — grants must come back.
        let mut store = MemoryStorage::new();
        let mut pm = PackageManager::new();
        let mut pc = PermissionController::new();
        crate::permissions::declare_mvp_permissions(&mut pc);
        pm.install(&mut store, &mut pc, HELLO_WITH_NETWORK, b"x").unwrap();
        drop(pm);
        drop(pc);

        // Fresh boot.
        let mut pm2 = PackageManager::new();
        let mut pc2 = PermissionController::new();
        crate::permissions::declare_mvp_permissions(&mut pc2);
        pm2.load_installed(&store, |id, e| panic!("load error for {id}: {e:?}"));
        let count = pm2.load_granted_permissions(&store, &mut pc2,
            |id, e| panic!("grants error for {id}: {e:?}"));
        assert_eq!(count, 2);
        assert!(pc2.check("com.akiva.hello", crate::permissions::NETWORK));
        assert!(pc2.check("com.akiva.hello", crate::permissions::STORAGE_SANDBOX));
    }

    #[test]
    fn upgrade_revokes_permissions_removed_from_new_manifest() {
        // v1 asks for NETWORK + STORAGE_SANDBOX; v2 only asks for
        // NETWORK. After upgrade, STORAGE_SANDBOX must be revoked so
        // the app can't use a capability it no longer declares.
        let mut store = MemoryStorage::new();
        let mut pm = PackageManager::new();
        let mut pc = PermissionController::new();
        crate::permissions::declare_mvp_permissions(&mut pc);
        pm.install(&mut store, &mut pc, HELLO_WITH_NETWORK, b"v1").unwrap();
        assert!(pc.check("com.akiva.hello", crate::permissions::STORAGE_SANDBOX));

        let v2 = HELLO_WITH_NETWORK
            .replace("version_code = 1", "version_code = 2")
            .replacen("version = \"0.1.0\"", "version = \"0.2.0\"", 1)
            .replace(
                r#"required = ["akiva.permission.NETWORK", "akiva.permission.STORAGE_SANDBOX"]"#,
                r#"required = ["akiva.permission.NETWORK"]"#,
            );
        pm.install(&mut store, &mut pc, &v2, b"v2").unwrap();

        assert!(pc.check("com.akiva.hello", crate::permissions::NETWORK),
            "kept permission must remain granted");
        assert!(!pc.check("com.akiva.hello", crate::permissions::STORAGE_SANDBOX),
            "permission dropped from manifest must be revoked on upgrade");
        // Persisted blob reflects the tightened set.
        let blob = store.read("sys/permissions/com.akiva.hello").unwrap();
        assert_eq!(std::str::from_utf8(&blob).unwrap(), "akiva.permission.NETWORK");
    }

    #[test]
    fn uninstall_deletes_permissions_file_and_revokes_grants() {
        // Stone 10.5 tripwire: uninstall cleans up both the persisted
        // blob and the in-memory grants so a reinstall under the same
        // id starts fresh.
        let mut store = MemoryStorage::new();
        let mut pm = PackageManager::new();
        let mut pc = PermissionController::new();
        crate::permissions::declare_mvp_permissions(&mut pc);
        pm.install(&mut store, &mut pc, HELLO_WITH_NETWORK, b"x").unwrap();
        assert!(pc.check("com.akiva.hello", crate::permissions::NETWORK));

        assert!(pm.uninstall(&mut store, &mut pc, "com.akiva.hello"));
        assert!(!pc.check("com.akiva.hello", crate::permissions::NETWORK),
            "uninstall must drop in-memory grants");
        assert!(matches!(store.read("sys/permissions/com.akiva.hello"),
            Err(StorageError::NotFound)),
            "uninstall must delete the persisted permissions blob");
    }

    #[test]
    fn install_with_no_permissions_writes_empty_blob() {
        // The HELLO fixture has no [permissions] section. Install still
        // writes an (empty) blob so rehydration can distinguish "no
        // permissions requested" from "pre-10.2 install".
        let mut store = MemoryStorage::new();
        let mut pm = PackageManager::new();
        let mut pc = PermissionController::new();
        crate::permissions::declare_mvp_permissions(&mut pc);
        pm.install(&mut store, &mut pc, HELLO, b"x").unwrap();
        let blob = store.read("sys/permissions/com.akiva.hello").unwrap();
        assert!(blob.is_empty());
        assert!(pc.granted_to("com.akiva.hello").is_empty());
    }

    #[test]
    fn saved_state_key_scopes_by_package_and_class() {
        assert_eq!(
            key_saved_state("com.akiva.calc", "CalcMain"),
            "packages/com.akiva.calc/activities/CalcMain/saved_state.bin",
        );
        // Two classes in the same package stay distinct — matches
        // Android's per-activity saved-instance contract.
        assert_ne!(
            key_saved_state("com.akiva.app", "Main"),
            key_saved_state("com.akiva.app", "Settings"),
        );
    }
}
