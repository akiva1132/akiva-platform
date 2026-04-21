//! Package manager.
//!
//! Installs, indexes, and resolves apps. An installed package lives at
//! `apps/{id}/` with the layout from docs/manifest.md §7. The index is
//! rebuilt at boot from the manifest files on disk (OPFS in practice).

use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct PackageRecord {
    pub id: String,
    pub version: String,
    pub version_code: u64,
    pub entry_activity: String,
    pub min_abi_version: String,
    pub exported_activities: Vec<String>,
}

#[derive(Debug)]
pub enum InstallError {
    ManifestInvalid(String),
    AbiMismatch { required: String, have: String },
    DuplicateId(String),
    SignatureInvalid,
}

pub struct PackageManager {
    packages: HashMap<String, PackageRecord>,
}

impl PackageManager {
    pub fn new() -> Self {
        Self { packages: HashMap::new() }
    }

    pub fn install(&mut self, record: PackageRecord) -> Result<(), InstallError> {
        if self.packages.contains_key(&record.id) {
            return Err(InstallError::DuplicateId(record.id.clone()));
        }
        self.packages.insert(record.id.clone(), record);
        Ok(())
    }

    pub fn uninstall(&mut self, id: &str) -> bool {
        self.packages.remove(id).is_some()
    }

    pub fn get(&self, id: &str) -> Option<&PackageRecord> {
        self.packages.get(id)
    }

    pub fn list(&self) -> impl Iterator<Item = &PackageRecord> {
        self.packages.values()
    }
}
