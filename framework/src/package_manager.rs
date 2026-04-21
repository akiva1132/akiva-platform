//! Package manager.
//!
//! Owns the persistent registry of installed apps. Each package lives in
//! the kernel KV store under three keys:
//!
//! ```text
//! sys/packages/index         → LF-separated list of package IDs
//! packages/{id}/manifest.toml → parsed manifest bytes
//! packages/{id}/app.wasm      → app wasm module bytes
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

use std::collections::HashMap;

use crate::activity_manager::LaunchMode;
use crate::manifest::{self, Manifest, ManifestError, RUNTIME_ABI_VERSION};
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
    DuplicateId(String),
    Storage(StorageError),
}

impl core::fmt::Display for InstallError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            InstallError::Manifest(e) => write!(f, "{e}"),
            InstallError::AbiMismatch { required, runtime } => {
                write!(f, "ABI mismatch: package needs >= {required}, runtime is {runtime}")
            }
            InstallError::DuplicateId(id) => write!(f, "package already installed: {id}"),
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

    /// Install a package. Parses + validates the manifest, checks ABI
    /// compatibility, then atomically-ish writes manifest + wasm + updated
    /// index. "Atomically-ish" because we don't have transactions; a crash
    /// mid-write can leave the index out of sync with the per-package
    /// keys, but `load_installed` tolerates that (orphan keys are benign,
    /// orphan index entries are reported via `on_error`).
    pub fn install(
        &mut self,
        storage: &mut dyn Storage,
        manifest_toml: &str,
        wasm_bytes: &[u8],
    ) -> Result<String, InstallError> {
        let manifest = manifest::parse(manifest_toml)?;
        check_abi(&manifest.app.min_abi_version)?;

        let id = manifest.app.id.clone();
        if self.packages.contains_key(&id) {
            return Err(InstallError::DuplicateId(id));
        }

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
    fn duplicate_install_rejected() {
        let mut store = MemoryStorage::new();
        let mut pm = PackageManager::new();
        pm.install(&mut store, HELLO, b"x").unwrap();
        let err = pm.install(&mut store, HELLO, b"x").unwrap_err();
        assert!(matches!(err, InstallError::DuplicateId(_)));
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
}
