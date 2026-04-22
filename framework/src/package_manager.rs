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
            let table = match ResourceTable::parse(strings.as_deref(), colors.as_deref()) {
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
        manifest_toml: &str,
        wasm_bytes: &[u8],
    ) -> Result<String, InstallError> {
        let manifest = manifest::parse(manifest_toml)?;
        check_abi(&manifest.app.min_abi_version)?;

        let id = manifest.app.id.clone();
        let offered_code = manifest.app.version_code;

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
            // Index already contains this id — no update needed.
            self.packages.insert(id.clone(), PackageRecord::from_manifest(manifest));
            return Ok(id);
        }

        // First install for this id.
        storage.write(&key_manifest(&id), manifest_toml.as_bytes())?;
        storage.write(&key_wasm(&id), wasm_bytes)?;

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
    pub fn uninstall(&mut self, storage: &mut dyn Storage, id: &str) -> bool {
        if self.packages.remove(id).is_none() {
            return false;
        }
        // Best-effort: missing key == already gone, not an error.
        let _ = storage.delete(&key_wasm(id));
        let _ = storage.delete(&key_manifest(id));
        let _ = storage.delete(&key_values_strings(id));
        let _ = storage.delete(&key_values_colors(id));
        let mut ids: Vec<String> = self.packages.keys().cloned().collect();
        ids.sort();
        let _ = storage.write(KEY_INDEX, ids.join("\n").as_bytes());
        true
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
        pm.install(&mut store, HELLO, b"\0asm\x01\x00\x00\x00").unwrap();
        assert_eq!(pm.count(), 1);
        let rec = pm.get("com.akiva.hello").unwrap();
        assert_eq!(rec.entry_class, "HelloMain");
    }

    #[test]
    fn install_is_persistent() {
        let mut store = MemoryStorage::new();
        let mut pm = PackageManager::new();
        pm.install(&mut store, HELLO, b"wasm-bytes").unwrap();
        pm.install(&mut store, WORLD, b"wasm-bytes").unwrap();
        drop(pm);

        // Fresh manager, same storage — should rehydrate both.
        let mut pm2 = PackageManager::new();
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
        pm.install(&mut store, HELLO, b"x").unwrap();
        let err = pm.install(&mut store, HELLO, b"x").unwrap_err();
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
        let v2 = HELLO.replace("version_code = 1", "version_code = 2");
        pm.install(&mut store, &v2, b"new").unwrap();
        let err = pm.install(&mut store, HELLO, b"old").unwrap_err();
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
        pm.install(&mut store, HELLO, b"v1-wasm").unwrap();
        // Only bump `version`, not `min_abi_version` — a naive string
        // replace would match both and trip AbiMismatch.
        let v2 = HELLO
            .replace("version_code = 1", "version_code = 2")
            .replacen("version = \"0.1.0\"", "version = \"0.2.0\"", 1);
        pm.install(&mut store, &v2, b"v2-wasm").unwrap();

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
        pm.install(&mut store, HELLO, b"v1-wasm").unwrap();
        let state_key = key_saved_state("com.akiva.hello", "HelloMain");
        store.write(&state_key, b"persisted blob").unwrap();

        // Only bump `version`, not `min_abi_version` — a naive string
        // replace would match both and trip AbiMismatch.
        let v2 = HELLO
            .replace("version_code = 1", "version_code = 2")
            .replacen("version = \"0.1.0\"", "version = \"0.2.0\"", 1);
        pm.install(&mut store, &v2, b"v2-wasm").unwrap();
        assert_eq!(store.read(&state_key).unwrap(), b"persisted blob");
    }

    #[test]
    fn upgrade_major_clears_saved_state() {
        // 0.1.0 → 1.0.0 is a breaking version bump. Any state blob
        // persisted for an activity on the old major must be wiped so
        // the new code doesn't decode incompatible bytes.
        let mut store = MemoryStorage::new();
        let mut pm = PackageManager::new();
        pm.install(&mut store, HELLO, b"v0-wasm").unwrap();
        let state_key = key_saved_state("com.akiva.hello", "HelloMain");
        store.write(&state_key, b"legacy blob").unwrap();

        let v1 = HELLO
            .replace("version_code = 1", "version_code = 2")
            .replacen("version = \"0.1.0\"", "version = \"1.0.0\"", 1);
        pm.install(&mut store, &v1, b"v1-wasm").unwrap();
        assert!(matches!(store.read(&state_key), Err(StorageError::NotFound)));
    }

    #[test]
    fn uninstall_removes_all_traces() {
        let mut store = MemoryStorage::new();
        let mut pm = PackageManager::new();
        pm.install(&mut store, HELLO, b"wasm").unwrap();
        assert!(pm.uninstall(&mut store, "com.akiva.hello"));

        let mut pm2 = PackageManager::new();
        pm2.load_installed(&store, |id, e| panic!("load error for {id}: {e:?}"));
        assert_eq!(pm2.count(), 0);
        // The wasm key should be gone too.
        assert!(matches!(store.read(&key_wasm("com.akiva.hello")), Err(StorageError::NotFound)));
    }

    #[test]
    fn first_boot_no_index() {
        let store = MemoryStorage::new();
        let mut pm = PackageManager::new();
        let loaded = pm.load_installed(&store, |_, _| panic!("unexpected error"));
        assert_eq!(loaded, 0);
    }

    #[test]
    fn corrupted_manifest_is_reported_not_fatal() {
        let mut store = MemoryStorage::new();
        let mut pm = PackageManager::new();
        pm.install(&mut store, HELLO, b"x").unwrap();
        // Deliberately overwrite one manifest with garbage.
        store.write(&key_manifest("com.akiva.hello"), b"not valid toml =====").unwrap();

        let mut pm2 = PackageManager::new();
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
        let err = pm.install(&mut store, future, b"x").unwrap_err();
        assert!(matches!(err, InstallError::AbiMismatch { .. }));
    }

    #[test]
    fn read_wasm_roundtrip() {
        let mut store = MemoryStorage::new();
        let mut pm = PackageManager::new();
        pm.install(&mut store, HELLO, b"\0asm\x01").unwrap();
        let bytes = pm.read_wasm(&store, "com.akiva.hello").unwrap();
        assert_eq!(bytes, b"\0asm\x01");
    }

    #[test]
    fn resource_tables_read_populated_and_absent_packages() {
        let mut store = MemoryStorage::new();
        let mut pm = PackageManager::new();
        pm.install(&mut store, HELLO, b"x").unwrap();
        pm.install(&mut store, WORLD, b"x").unwrap();

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
        let hello = &by_id["com.akiva.hello"];
        assert_eq!(hello.string("greeting"), Some("hi"));
        assert_eq!(hello.color("bg"), Some(0xff101418));

        let world = &by_id["com.akiva.world"];
        assert!(world.string("greeting").is_none());
        assert!(world.color("bg").is_none());
    }

    #[test]
    fn resource_tables_report_corrupt_files_but_still_emit_empty_table() {
        let mut store = MemoryStorage::new();
        let mut pm = PackageManager::new();
        pm.install(&mut store, HELLO, b"x").unwrap();
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
        assert!(tables[0].1.color("bg").is_none());
    }

    #[test]
    fn uninstall_removes_resource_keys() {
        let mut store = MemoryStorage::new();
        let mut pm = PackageManager::new();
        pm.install(&mut store, HELLO, b"x").unwrap();
        store.write(
            "packages/com.akiva.hello/res/values/strings.toml",
            br#"x = "y""#,
        ).unwrap();
        pm.uninstall(&mut store, "com.akiva.hello");
        assert!(matches!(
            store.read("packages/com.akiva.hello/res/values/strings.toml"),
            Err(StorageError::NotFound),
        ));
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
