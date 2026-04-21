//! Kernel memory.
//!
//! Phase 1a relies on the standard Rust allocator (dlmalloc on
//! wasm32-unknown-unknown). `host_alloc` / `host_dealloc` expose it to the
//! HAL so it can stage event buffers inside kernel memory without a
//! shared-memory dance.
//!
//! Phase 2+ introduces a per-process arena so wasm app allocations are
//! tracked and reclaimable on activity-destroy.

use std::alloc::{alloc, dealloc, Layout};

const HOST_BUFFER_ALIGN: usize = 16;

/// Allocate `size` bytes for a host-to-kernel transfer. Returns null on
/// failure — the HAL handles null by retrying after a GC pass.
///
/// # Safety
/// Caller must pair every `host_alloc(n)` with exactly one
/// `host_dealloc(ptr, n)`.
pub unsafe fn host_alloc(size: usize) -> *mut u8 {
    if size == 0 { return std::ptr::null_mut(); }
    let layout = match Layout::from_size_align(size, HOST_BUFFER_ALIGN) {
        Ok(l) => l,
        Err(_) => return std::ptr::null_mut(),
    };
    alloc(layout)
}

/// # Safety
/// See `host_alloc`.
pub unsafe fn host_dealloc(ptr: *mut u8, size: usize) {
    if ptr.is_null() || size == 0 { return; }
    let layout = match Layout::from_size_align(size, HOST_BUFFER_ALIGN) {
        Ok(l) => l,
        Err(_) => return,
    };
    dealloc(ptr, layout);
}
