//! Storage abstraction for the framework.
//!
//! The framework needs persistent state (installed packages, activity saved
//! state, resource caches) but cannot depend on `kernel::storage` because
//! the dependency arrow points the other way — the kernel is built on top
//! of the framework, not vice versa. This trait is the seam: the kernel
//! wraps its real HAL-backed KV store in an impl and hands a `&mut dyn
//! Storage` to framework methods that need persistence.
//!
//! Trait methods mirror `kernel::storage`: flat bytewise KV, no iteration,
//! no transactions. The framework applies its own key prefixes (`sys/`,
//! `packages/{id}/...`) to avoid colliding with app-scoped storage.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StorageError {
    /// Key has no value. Distinct from `Io` so callers can treat
    /// "first-boot, nothing installed yet" as a happy path.
    NotFound,
    /// Key was empty or otherwise malformed. Caller bug, not IO.
    BadKey,
    /// Underlying store failed (OPFS write error, quota, etc.).
    Io,
}

/// Synchronous key/value store the framework can read and write through.
///
/// Implementations must be single-threaded and may assume exclusive access
/// for the duration of any call — the Akiva runtime is single-threaded
/// inside the leader worker, and the framework never calls storage from a
/// reentrant context.
pub trait Storage {
    fn read(&self, key: &str) -> Result<Vec<u8>, StorageError>;
    fn write(&mut self, key: &str, value: &[u8]) -> Result<(), StorageError>;
    fn delete(&mut self, key: &str) -> Result<(), StorageError>;
}

/// In-memory `Storage` impl. Used for unit tests and — once we wire it —
/// as a fallback when the HAL can't open OPFS.
#[cfg(any(test, feature = "memory-storage"))]
pub struct MemoryStorage {
    map: std::collections::HashMap<String, Vec<u8>>,
}

#[cfg(any(test, feature = "memory-storage"))]
impl MemoryStorage {
    pub fn new() -> Self { Self { map: std::collections::HashMap::new() } }
}

#[cfg(any(test, feature = "memory-storage"))]
impl Default for MemoryStorage {
    fn default() -> Self { Self::new() }
}

#[cfg(any(test, feature = "memory-storage"))]
impl Storage for MemoryStorage {
    fn read(&self, key: &str) -> Result<Vec<u8>, StorageError> {
        if key.is_empty() { return Err(StorageError::BadKey); }
        self.map.get(key).cloned().ok_or(StorageError::NotFound)
    }
    fn write(&mut self, key: &str, value: &[u8]) -> Result<(), StorageError> {
        if key.is_empty() { return Err(StorageError::BadKey); }
        self.map.insert(key.to_string(), value.to_vec());
        Ok(())
    }
    fn delete(&mut self, key: &str) -> Result<(), StorageError> {
        if key.is_empty() { return Err(StorageError::BadKey); }
        self.map.remove(key);
        Ok(())
    }
}
