//! Storage: key/value persistence layer.
//!
//! Backed by OPFS + SyncAccessHandle inside the worker (HAL side). The
//! kernel never touches files directly — the HAL does, and exposes a flat
//! KV interface. Keys are namespaced by the caller (`app.id` prefix) to
//! isolate apps from each other.

use crate::host;

#[derive(Debug)]
pub enum StorageError {
    NotFound,
    BadKey,
    Io,
}

pub fn read(key: &str) -> Result<Vec<u8>, StorageError> {
    if key.is_empty() { return Err(StorageError::BadKey); }
    let mut out_len: usize = 0;
    // SAFETY: pointers are valid for the call; HAL writes the returned
    // buffer length into `out_len`.
    let ptr = unsafe {
        host::hal_storage_read(key.as_ptr(), key.len(), &mut out_len as *mut usize)
    };
    if ptr.is_null() {
        return Err(StorageError::NotFound);
    }
    // SAFETY: HAL returns a kernel-allocated buffer of length `out_len`.
    let bytes = unsafe { core::slice::from_raw_parts(ptr, out_len) };
    let owned = bytes.to_vec();
    // SAFETY: the buffer came from host_alloc; release it now that we've copied.
    unsafe { crate::memory::host_dealloc(ptr, out_len) };
    Ok(owned)
}

pub fn write(key: &str, value: &[u8]) -> Result<(), StorageError> {
    if key.is_empty() { return Err(StorageError::BadKey); }
    // SAFETY: pointer/length pairs describe live slices.
    let rc = unsafe {
        host::hal_storage_write(key.as_ptr(), key.len(), value.as_ptr(), value.len())
    };
    match rc {
        0 => Ok(()),
        _ => Err(StorageError::Io),
    }
}

pub fn delete(key: &str) -> Result<(), StorageError> {
    if key.is_empty() { return Err(StorageError::BadKey); }
    // SAFETY: pointer/length describe a live slice.
    let rc = unsafe { host::hal_storage_delete(key.as_ptr(), key.len()) };
    match rc {
        0 => Ok(()),
        _ => Err(StorageError::Io),
    }
}
