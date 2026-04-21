//! Akiva kernel.
//!
//! Runs as a single wasm module inside a Dedicated Worker held by the
//! leader tab. Owns the global state of the OS: process table, scheduler,
//! event queue, storage, and display list. The framework layer (activity
//! manager, package manager, etc.) is linked in as a library and runs in
//! the same address space.
//!
//! The JS HAL calls the exports in this module (`boot`, `frame`,
//! `on_event`, `shutdown`); the kernel calls back into the HAL via the
//! imports declared in `host.rs`.
//!
//! Per-frame flow:
//!   1. `akiva_frame(ts)` is called by the worker's rAF loop.
//!   2. All queued events are drained and dispatched via `handle_event`,
//!      which routes them through the framework to the active activity.
//!   3. Scheduler runs ready tasks up to the frame budget.
//!   4. Framework renders the frame; the resulting `FrameCommands` buffer
//!      is flushed to the HAL through `DisplayManager::flush`.

pub mod display_manager;
pub mod event_queue;
pub mod host;
pub mod log;
pub mod memory;
pub mod panic;
pub mod scheduler;
pub mod storage;
pub mod wasm_loader;

use core::sync::atomic::{AtomicBool, Ordering};

use akiva_framework::activity as fw_act;
use akiva_framework::{self as framework, Framework};

use crate::display_manager::DisplayManager;
use crate::event_queue::{Event, EventQueue, KeyPhase, PointerPhase};
use crate::scheduler::Scheduler;

/// Global kernel state. There is exactly one of these per OS instance.
/// Lives in a `static mut` because wasm is single-threaded within a worker
/// and all entry points are synchronous — no data races are possible.
struct Kernel {
    scheduler: Scheduler,
    events: EventQueue,
    display: DisplayManager,
    /// Lazy-initialised in `akiva_boot` because `Framework` allocates and
    /// therefore cannot be `const`-constructed.
    framework: Option<Framework>,
    frame_count: u64,
    last_frame_ms: f64,
}

impl Kernel {
    const fn new() -> Self {
        Self {
            scheduler: Scheduler::new(),
            events: EventQueue::new(),
            display: DisplayManager::new(),
            framework: None,
            frame_count: 0,
            last_frame_ms: 0.0,
        }
    }

    fn handle_event(&mut self, ev: Event) {
        let ts = self.last_frame_ms;
        match ev {
            Event::Pointer { x, y, button, phase } => {
                if let Some(fw) = self.framework.as_mut() {
                    fw.dispatch_pointer(
                        fw_act::PointerEvent { x, y, button, phase: map_pointer_phase(phase) },
                        ts,
                    );
                }
            }
            Event::Key { code, modifiers, phase } => {
                if let Some(fw) = self.framework.as_mut() {
                    fw.dispatch_key(
                        fw_act::KeyEvent { code, modifiers, phase: map_key_phase(phase) },
                        ts,
                    );
                }
            }
            Event::Resize { width_px, height_px, density } => {
                self.display.on_resize(width_px, height_px, density);
                if let Some(fw) = self.framework.as_mut() {
                    fw.set_metrics(fw_act::SurfaceMetrics { width_px, height_px, density });
                }
            }
            Event::Visibility { visible } => {
                log::debug(&format!("kernel: visibility={}", visible));
            }
        }
    }
}

fn map_pointer_phase(p: PointerPhase) -> fw_act::PointerPhase {
    match p {
        PointerPhase::Down => fw_act::PointerPhase::Down,
        PointerPhase::Move => fw_act::PointerPhase::Move,
        PointerPhase::Up => fw_act::PointerPhase::Up,
        PointerPhase::Cancel => fw_act::PointerPhase::Cancel,
    }
}

fn map_key_phase(p: KeyPhase) -> fw_act::KeyPhase {
    match p {
        KeyPhase::Down => fw_act::KeyPhase::Down,
        KeyPhase::Repeat => fw_act::KeyPhase::Repeat,
        KeyPhase::Up => fw_act::KeyPhase::Up,
    }
}

static BOOTED: AtomicBool = AtomicBool::new(false);
static mut KERNEL: Kernel = Kernel::new();

/// Grab a mutable reference to the kernel singleton.
///
/// Safety: wasm in a worker is single-threaded; entry points are mutually
/// exclusive because the host calls them serially. We rely on that
/// invariant rather than a RefCell to avoid the runtime check.
#[inline(always)]
fn kernel() -> &'static mut Kernel {
    // SAFETY: see item docs.
    unsafe { &mut *core::ptr::addr_of_mut!(KERNEL) }
}

// ── Exports: called by the JS HAL ─────────────────────────────────────

/// Initialise the kernel. Called once after the worker has loaded the wasm
/// and wired up imports. Must complete before any `frame` call.
#[no_mangle]
pub extern "C" fn akiva_boot() {
    if BOOTED.swap(true, Ordering::SeqCst) {
        log::warn("akiva_boot called twice; ignoring");
        return;
    }
    panic::install();
    log::info("kernel: boot");
    let k = kernel();
    k.scheduler.init();
    k.display.init();

    let boot_count = bump_boot_counter();
    let mut kstore = KernelStorage;
    let (mut fw, report) = framework::boot(boot_count, &mut kstore);
    log::info(&format!("packages: {} loaded", report.packages_loaded));
    for (id, err) in &report.package_errors {
        log::warn(&format!("package '{id}' failed to load: {err}"));
    }
    let m = k.display.metrics();
    fw.set_metrics(fw_act::SurfaceMetrics {
        width_px: m.width_px,
        height_px: m.height_px,
        density: m.density,
    });
    k.framework = Some(fw);
}

/// Adapter that lets the framework read the kernel's OPFS-backed KV
/// store through its abstract `Storage` trait. Zero-sized: the backing
/// module is addressed via free functions, so there's no per-instance
/// state to carry.
struct KernelStorage;

impl akiva_framework::storage::Storage for KernelStorage {
    fn read(&self, key: &str) -> Result<Vec<u8>, akiva_framework::storage::StorageError> {
        storage::read(key).map_err(map_storage_err)
    }
    fn write(
        &mut self,
        key: &str,
        value: &[u8],
    ) -> Result<(), akiva_framework::storage::StorageError> {
        storage::write(key, value).map_err(map_storage_err)
    }
    fn delete(&mut self, key: &str) -> Result<(), akiva_framework::storage::StorageError> {
        storage::delete(key).map_err(map_storage_err)
    }
}

fn map_storage_err(e: storage::StorageError) -> akiva_framework::storage::StorageError {
    use akiva_framework::storage::StorageError as FwErr;
    match e {
        storage::StorageError::NotFound => FwErr::NotFound,
        storage::StorageError::BadKey => FwErr::BadKey,
        storage::StorageError::Io => FwErr::Io,
    }
}

/// Exercises the storage HAL end-to-end: read a u32 counter under
/// `sys/boot_count`, increment it, write it back. The returned value is
/// the post-increment count, fed into `framework::boot` so the Shell can
/// display it.
fn bump_boot_counter() -> u32 {
    const KEY: &str = "sys/boot_count";
    let prev = match storage::read(KEY) {
        Ok(bytes) if bytes.len() == 4 => {
            u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
        }
        Ok(_) => 0,
        Err(storage::StorageError::NotFound) => 0,
        Err(e) => {
            log::warn(&format!("boot_count read failed: {:?}", e));
            return 0;
        }
    };
    let next = prev.wrapping_add(1);
    match storage::write(KEY, &next.to_le_bytes()) {
        Ok(()) => log::info(&format!("kernel: boot #{} (prev={})", next, prev)),
        Err(e) => log::warn(&format!("boot_count write failed: {:?}", e)),
    }
    next
}

/// Main loop tick. Called by the host on every rAF (or fixed cadence when
/// the tab is hidden). `timestamp_ms` is monotonic host time.
#[no_mangle]
pub extern "C" fn akiva_frame(timestamp_ms: f64) {
    let k = kernel();
    k.frame_count = k.frame_count.wrapping_add(1);
    k.last_frame_ms = timestamp_ms;

    // Drain all queued events and dispatch.
    while let Some(ev) = k.events.pop() {
        k.handle_event(ev);
    }

    k.scheduler.run_ready(timestamp_ms);

    // Render through the framework, then flush the resulting command
    // buffer to the HAL. The split keeps HAL calls confined to the kernel
    // crate so the framework stays wasm-target-agnostic.
    if let Some(fw) = k.framework.as_mut() {
        let frame = fw.render_frame(timestamp_ms);
        k.display.flush(frame);
    }
}

/// Deliver an input/system event pushed by the HAL into a kernel-allocated
/// buffer. `kind` matches the `Event` tag; `ptr`/`len` point into kernel
/// memory at a buffer previously returned by `akiva_alloc`.
#[no_mangle]
pub extern "C" fn akiva_on_event(kind: u32, ptr: *mut u8, len: usize) {
    let bytes = if ptr.is_null() || len == 0 {
        &[][..]
    } else {
        // SAFETY: host guarantees ptr..ptr+len is a valid kernel alloc.
        unsafe { core::slice::from_raw_parts(ptr, len) }
    };
    match Event::decode(kind, bytes) {
        Some(ev) => kernel().events.push(ev),
        None => log::warn("on_event: malformed event discarded"),
    }
    // The event buffer is owned by the host; it will free it after return.
}

/// Teardown hook. Called when the leader is about to hand over or the tab
/// is closing. Kernel must flush any dirty state to persistent storage.
#[no_mangle]
pub extern "C" fn akiva_shutdown() {
    log::info("kernel: shutdown");
    let k = kernel();
    k.scheduler.shutdown();
    k.display.shutdown();
}

// ── Memory ABI: host<->guest transfers ────────────────────────────────

/// Allocate `size` bytes inside kernel memory and return a pointer the host
/// can write into. Host must call `akiva_dealloc` with the same size.
///
/// # Safety
/// Caller must pair every `akiva_alloc(n)` with exactly one
/// `akiva_dealloc(ptr, n)`.
#[no_mangle]
pub unsafe extern "C" fn akiva_alloc(size: usize) -> *mut u8 {
    memory::host_alloc(size)
}

/// Deallocate a buffer previously returned by `akiva_alloc`.
///
/// # Safety
/// See `akiva_alloc`.
#[no_mangle]
pub unsafe extern "C" fn akiva_dealloc(ptr: *mut u8, size: usize) {
    memory::host_dealloc(ptr, size)
}

/// Version string the host can read to verify ABI compatibility.
/// Points into a null-terminated static. Host reads as UTF-8 until the zero.
#[no_mangle]
pub extern "C" fn akiva_abi_version() -> *const u8 {
    concat!(env!("CARGO_PKG_VERSION"), "\0").as_ptr()
}
