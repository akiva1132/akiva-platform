//! Storage facade for app code. Scoped to the calling package — the
//! runtime prefixes `{package_id}/` to every key.

#[derive(Debug)]
pub enum StorageError {
    NotFound,
    BadKey,
    Quota,
    Io,
}

pub fn read(_key: &str) -> Result<Vec<u8>, StorageError> {
    Err(StorageError::NotFound)
}

pub fn write(_key: &str, _value: &[u8]) -> Result<(), StorageError> {
    Err(StorageError::Io)
}

pub fn delete(_key: &str) -> Result<(), StorageError> {
    Err(StorageError::Io)
}
