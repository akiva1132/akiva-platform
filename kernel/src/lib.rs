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
    let kstore: Box<dyn akiva_framework::storage::Storage> = Box::new(KernelStorage);
    let host: std::rc::Rc<dyn akiva_framework::wasm_activity::WasmHost> =
        std::rc::Rc::new(KernelWasmHost);
    let (mut fw, report) = framework::boot(boot_count, kstore, host);
    log::info(&format!("packages: {} loaded", report.packages_loaded));
    for (id, err) in &report.package_errors {
        log::warn(&format!("package '{id}' failed to load: {err}"));
    }
    for (id, err) in &report.resource_errors {
        log::warn(&format!("package '{id}' resource file: {err}"));
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

/// Adapter that routes framework `WasmHost` calls to the matching
/// `hal_app_*` imports. Zero-sized: the host side (worker.js) owns all
/// per-instance state — wasm module, linear memory, paint scratch —
/// keyed by the returned handle.
struct KernelWasmHost;

impl akiva_framework::wasm_activity::WasmHost for KernelWasmHost {
    fn register(
        &self,
        package_id: &str,
        class: &str,
    ) -> Option<akiva_framework::wasm_activity::AppHandle> {
        let pid = package_id.as_bytes();
        let cls = class.as_bytes();
        // SAFETY: slices valid for their lengths, host reads only.
        let raw = unsafe {
            host::hal_app_register(pid.as_ptr(), pid.len(), cls.as_ptr(), cls.len())
        };
        akiva_framework::wasm_activity::AppHandle::new(raw)
    }

    fn unregister(&self, handle: akiva_framework::wasm_activity::AppHandle) {
        unsafe { host::hal_app_unregister(handle.get()) }
    }

    fn lifecycle(
        &self,
        handle: akiva_framework::wasm_activity::AppHandle,
        t: akiva_framework::wasm_activity::LifecycleTransition,
    ) {
        unsafe { host::hal_app_lifecycle(handle.get(), t as u32) }
    }

    fn pointer(
        &self,
        handle: akiva_framework::wasm_activity::AppHandle,
        ev: fw_act::PointerEvent,
        ts_ms: f64,
    ) {
        unsafe {
            host::hal_app_pointer(
                handle.get(),
                ev.x, ev.y,
                ev.button as u32,
                akiva_framework::wasm_activity::pointer_phase_wire(ev.phase),
                ts_ms,
            )
        }
    }

    fn key(
        &self,
        handle: akiva_framework::wasm_activity::AppHandle,
        ev: fw_act::KeyEvent,
        ts_ms: f64,
    ) {
        unsafe {
            host::hal_app_key(
                handle.get(),
                ev.code,
                ev.modifiers as u32,
                akiva_framework::wasm_activity::key_phase_wire(ev.phase),
                ts_ms,
            )
        }
    }

    fn resize(
        &self,
        handle: akiva_framework::wasm_activity::AppHandle,
        m: fw_act::SurfaceMetrics,
    ) {
        unsafe { host::hal_app_resize(handle.get(), m.width_px, m.height_px, m.density) }
    }

    fn render(
        &self,
        handle: akiva_framework::wasm_activity::AppHandle,
        ts_ms: f64,
        out: &mut akiva_framework::compositor::FrameCommands,
    ) {
        // 16 KiB is generous for per-frame paint ops; the retry branch
        // doubles up if an app ever needs more, at the cost of a second
        // render. Apps producing huge frames should use layered views
        // instead of fat single frames.
        let mut buf = vec![0u8; 16 * 1024];
        let needed = unsafe {
            host::hal_app_render(handle.get(), ts_ms, buf.as_mut_ptr(), buf.len() as u32)
        } as usize;
        if needed > buf.len() {
            log::warn(&format!(
                "app {}: paint buffer too small ({} > {}); retrying",
                handle.get(), needed, buf.len()
            ));
            buf = vec![0u8; needed];
            let got = unsafe {
                host::hal_app_render(handle.get(), ts_ms, buf.as_mut_ptr(), buf.len() as u32)
            } as usize;
            buf.truncate(got.min(buf.len()));
        } else {
            buf.truncate(needed);
        }
        if let Err(e) = akiva_framework::wasm_activity::decode_frame(&buf, out) {
            log::warn(&format!("app {}: paint decode failed: {:?}", handle.get(), e));
        }
    }

    fn save_state(&self, handle: akiva_framework::wasm_activity::AppHandle) -> Vec<u8> {
        // Start at the typical UI-state size. The framework caps the wire
        // size at `SavedState::MAX_BYTES` (64 KiB); if the app returns more
        // we truncate on the way to storage rather than allocating more.
        let mut buf = vec![0u8; 4 * 1024];
        let needed = unsafe {
            host::hal_app_save_state(handle.get(), buf.as_mut_ptr(), buf.len() as u32)
        } as usize;
        if needed > buf.len() {
            buf = vec![0u8; needed];
            let got = unsafe {
                host::hal_app_save_state(handle.get(), buf.as_mut_ptr(), buf.len() as u32)
            } as usize;
            buf.truncate(got.min(buf.len()));
        } else {
            buf.truncate(needed);
        }
        buf
    }

    fn restore_state(
        &self,
        handle: akiva_framework::wasm_activity::AppHandle,
        bytes: &[u8],
    ) {
        if bytes.is_empty() { return; }
        let rc = unsafe {
            host::hal_app_restore_state(handle.get(), bytes.as_ptr(), bytes.len() as u32)
        };
        if rc != 0 {
            log::warn(&format!(
                "app {}: restore_state returned {} (handle miss or host copy failed)",
                handle.get(), rc
            ));
        }
    }

    fn drain_finish(&self, handle: akiva_framework::wasm_activity::AppHandle) -> bool {
        unsafe { host::hal_app_drain_finish(handle.get()) != 0 }
    }

    fn drain_intent(
        &self,
        handle: akiva_framework::wasm_activity::AppHandle,
    ) -> Option<Vec<u8>> {
        // Two-phase buffer fetch — start at 512 bytes (covers the common
        // MAIN+LAUNCHER + target_package intent) and retry once with an
        // exact-sized buffer if the app produced more. An Intent blob is
        // hard-capped by `Intent::MAX_WIRE_BYTES` (16 KiB) on the
        // framework side, so a single retry suffices even for the
        // worst-case payload.
        let mut buf = vec![0u8; 512];
        let needed = unsafe {
            host::hal_app_drain_intent(handle.get(), buf.as_mut_ptr(), buf.len() as u32)
        } as usize;
        if needed == 0 {
            return None;
        }
        if needed > buf.len() {
            buf = vec![0u8; needed];
            let got = unsafe {
                host::hal_app_drain_intent(handle.get(), buf.as_mut_ptr(), buf.len() as u32)
            } as usize;
            if got == 0 {
                // The host drained the slot on the first call; a second
                // drain returns 0. This shouldn't happen — the host
                // contract is "if the first call returns `needed > cap`,
                // keep the blob armed". Treat as a host bug and drop.
                log::warn(&format!("app {}: drain_intent lost payload on retry", handle.get()));
                return None;
            }
            buf.truncate(got.min(buf.len()));
        } else {
            buf.truncate(needed);
        }
        Some(buf)
    }

    fn new_intent(
        &self,
        handle: akiva_framework::wasm_activity::AppHandle,
        bytes: &[u8],
    ) {
        if bytes.is_empty() { return; }
        let rc = unsafe {
            host::hal_app_new_intent(handle.get(), bytes.as_ptr(), bytes.len() as u32)
        };
        if rc != 0 {
            log::warn(&format!(
                "app {}: new_intent returned {} (handle miss or host copy failed)",
                handle.get(), rc
            ));
        }
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
