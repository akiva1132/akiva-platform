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
//!   2. All queued events are drained and dispatched via `handle_event`.
//!   3. Scheduler runs ready tasks up to the frame budget.
//!   4. Display manager renders the frame.

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

use crate::display_manager::DisplayManager;
use crate::event_queue::{Event, EventQueue, PointerPhase};
use crate::scheduler::Scheduler;

/// Global kernel state. There is exactly one of these per OS instance.
/// Lives in a `static mut` because wasm is single-threaded within a worker
/// and all entry points are synchronous — no data races are possible.
struct Kernel {
    scheduler: Scheduler,
    events: EventQueue,
    display: DisplayManager,
    frame_count: u64,
    last_frame_ms: f64,
}

impl Kernel {
    const fn new() -> Self {
        Self {
            scheduler: Scheduler::new(),
            events: EventQueue::new(),
            display: DisplayManager::new(),
            frame_count: 0,
            last_frame_ms: 0.0,
        }
    }

    fn handle_event(&mut self, ev: Event) {
        match ev {
            Event::Pointer { x, y, phase, .. } => match phase {
                PointerPhase::Down => self.display.set_pointer(x, y, true),
                PointerPhase::Move => {
                    // Preserve pressed flag across moves by only updating if
                    // the pointer is already active.
                    let pressed = self.display.pointer_pressed();
                    self.display.set_pointer(x, y, pressed);
                }
                PointerPhase::Up => self.display.set_pointer(x, y, false),
                PointerPhase::Cancel => self.display.pointer_leave(),
            },
            Event::Key { code, phase, .. } => {
                log::debug(&format!("kernel: key {:?} code={}", phase, code));
            }
            Event::Resize { width_px, height_px, density } => {
                self.display.on_resize(width_px, height_px, density);
                log::debug(&format!("kernel: resize {}x{} @ {}", width_px, height_px, density));
            }
            Event::Visibility { visible } => {
                log::debug(&format!("kernel: visibility={}", visible));
            }
        }
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
    k.display.render_frame(timestamp_ms);
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
