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

// ── Kernel-side tuning knobs ───────────────────────────────────────────
//
// JS-side tunables (watchdog timeouts, heartbeat tick) live in
// `runtime/web/config.js`; this block is the Rust-side counterpart for
// values the kernel wasm reaches for. Each constant carries a `Why:`
// comment so the *reasoning* survives even if the value is later
// retuned.

/// P0 #3 — grace window between marking an activity `Trapped` and
/// recycling its `ActivityRecord` slot.
///
/// Why: 30s is long enough for a debugger pause or a tap-to-restart
/// confirm dialog to claim the record explicitly before GC takes it,
/// short enough that a crashed app doesn't sit in the table forever.
const TRAPPED_GRACE_MS: f64 = 30_000.0;

/// Initial buffer size for `WasmHost::poll_future` when fetching a
/// READY dispatch's bytes. The two-phase buffer protocol grows past
/// this on demand, so the value only matters for the common case.
///
/// Why: a typical app's per-frame paint stream sits a few KiB; 16 KiB
/// covers the common case in one host call. Anything that exceeds
/// (a `SavedState` blob, a complex render) takes a single retry. Too
/// small (e.g. 1 KiB) would force a retry every frame.
const POLL_RESULT_INITIAL_CAP: usize = 16 * 1024;

/// Initial buffer size for `hal_drain_trapped_packages`. Same two-
/// phase grow-on-overflow contract as `POLL_RESULT_INITIAL_CAP`.
///
/// Why: most frames the trap queue is empty (`needed == 0`) and the
/// buffer is unused; the bigger fallback only kicks in when many apps
/// trap at once. 256 covers ~10 typical package_ids before retry.
const TRAP_DRAIN_INITIAL_CAP: usize = 256;

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

    bump_boot_counter();
    let kstore: Box<dyn akiva_framework::storage::Storage> = Box::new(KernelStorage);
    let host: std::rc::Rc<dyn akiva_framework::wasm_activity::WasmHost> =
        std::rc::Rc::new(KernelWasmHost::new());
    let (mut fw, report) = framework::boot(kstore, host);
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
/// `hal_app_*` imports. The host side (worker.js) owns all per-app
/// state — wasm module, linear memory, paint scratch — keyed by the
/// returned handle. Stone 11.4c moved the wasm into a per-package
/// peer Worker, so every dispatch is async by construction: the JS
/// AppRegistry allocates a kernel-side `dispatch_id`, posts the
/// payload, returns the id, and we stash it as the future_id the
/// framework polls each frame via [`Self::poll_future`].
struct KernelWasmHost;

impl KernelWasmHost {
    fn new() -> Self { Self }

    /// Stone 11.4b/c — single-call helper. Hands the encoded payload
    /// + cmd_kind to the JS host. The caller is responsible for
    /// appending its per-cmd args after the `[handle: u32 LE]` prefix
    /// that every payload starts with. The return value is whatever
    /// JS allocated as the kernel-side `dispatch_id` — pass it
    /// directly to the framework as the future_id.
    fn dispatch(&self, cmd_kind: u32, payload: &[u8]) -> u64 {
        // SAFETY: payload is a borrow of caller-owned bytes; the host
        // copies them into app memory before returning. Ptr can be
        // null for empty payload — but we never emit empty payloads
        // (handle is always present), so just pass the slice as-is.
        unsafe {
            host::hal_app_dispatch(cmd_kind, payload.as_ptr(), payload.len() as u32)
        }
    }

    /// Stone 11.4c — fetch the bytes for a READY `future_id`. Two-
    /// phase buffer convention: start at `initial_cap`, retry once if
    /// the app produced more. The host evicts the entry on a fitting
    /// copy, so this is single-shot per id.
    fn fetch_result(&self, future_id: u64, initial_cap: usize) -> Vec<u8> {
        let mut buf = vec![0u8; initial_cap];
        let needed = unsafe {
            host::hal_app_get_result(future_id, buf.as_mut_ptr(), buf.len() as u32)
        } as usize;
        if needed == 0 {
            // Ack-only cmd: lifecycle, input, restore_state, etc. The
            // framework still expects a `Vec<u8>` from `poll_future` —
            // empty conveys "transition done, no payload".
            return Vec::new();
        }
        if needed > buf.len() {
            buf = vec![0u8; needed];
            let got = unsafe {
                host::hal_app_get_result(future_id, buf.as_mut_ptr(), buf.len() as u32)
            } as usize;
            buf.truncate(got.min(buf.len()));
        } else {
            buf.truncate(needed);
        }
        buf
    }
}

/// Stone 11.4b — map [`framework::wasm_activity::LifecycleTransition`]
/// to its `cmd::*` op kind. The two enums are deliberately off by one
/// (transition starts at 0, cmd_kind at 0x01) so the `0` sentinel for
/// future_ids stays unambiguous; spelling the mapping explicitly
/// keeps it greppable.
fn cmd_for_lifecycle(t: akiva_framework::wasm_activity::LifecycleTransition) -> u32 {
    use akiva_framework::wasm_activity::LifecycleTransition::*;
    use akiva_framework::wire::op_kinds::cmd;
    match t {
        Create  => cmd::CREATE,
        Start   => cmd::START,
        Resume  => cmd::RESUME,
        Pause   => cmd::PAUSE,
        Stop    => cmd::STOP,
        Destroy => cmd::DESTROY,
    }
}

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
    ) -> u64 {
        // Stone 11.4b — every lifecycle transition is just `[handle]`.
        // CREATE looks identical here; the JS host augments the
        // payload with the slot's class string before forwarding to
        // the worker's `akiva_app_dispatch`.
        let payload = handle.get().to_le_bytes();
        self.dispatch(cmd_for_lifecycle(t), &payload)
    }

    fn pointer(
        &self,
        handle: akiva_framework::wasm_activity::AppHandle,
        ev: fw_act::PointerEvent,
        ts_ms: f64,
    ) -> u64 {
        // [handle: u32, x:f32, y:f32, button:u32, phase:u32, ts:f64] = 28 bytes
        let mut p = [0u8; 28];
        p[0..4].copy_from_slice(&handle.get().to_le_bytes());
        p[4..8].copy_from_slice(&ev.x.to_le_bytes());
        p[8..12].copy_from_slice(&ev.y.to_le_bytes());
        p[12..16].copy_from_slice(&(ev.button as u32).to_le_bytes());
        p[16..20].copy_from_slice(
            &akiva_framework::wasm_activity::pointer_phase_wire(ev.phase).to_le_bytes(),
        );
        p[20..28].copy_from_slice(&ts_ms.to_le_bytes());
        self.dispatch(akiva_framework::wire::op_kinds::cmd::POINTER, &p)
    }

    fn key(
        &self,
        handle: akiva_framework::wasm_activity::AppHandle,
        ev: fw_act::KeyEvent,
        ts_ms: f64,
    ) -> u64 {
        // [handle: u32, code:u32, modifiers:u32, phase:u32, ts:f64] = 24 bytes
        let mut p = [0u8; 24];
        p[0..4].copy_from_slice(&handle.get().to_le_bytes());
        p[4..8].copy_from_slice(&ev.code.to_le_bytes());
        p[8..12].copy_from_slice(&(ev.modifiers as u32).to_le_bytes());
        p[12..16].copy_from_slice(
            &akiva_framework::wasm_activity::key_phase_wire(ev.phase).to_le_bytes(),
        );
        p[16..24].copy_from_slice(&ts_ms.to_le_bytes());
        self.dispatch(akiva_framework::wire::op_kinds::cmd::KEY, &p)
    }

    fn resize(
        &self,
        handle: akiva_framework::wasm_activity::AppHandle,
        m: fw_act::SurfaceMetrics,
    ) -> u64 {
        // [handle: u32, width:u32, height:u32, density:f32] = 16 bytes
        let mut p = [0u8; 16];
        p[0..4].copy_from_slice(&handle.get().to_le_bytes());
        p[4..8].copy_from_slice(&m.width_px.to_le_bytes());
        p[8..12].copy_from_slice(&m.height_px.to_le_bytes());
        p[12..16].copy_from_slice(&m.density.to_le_bytes());
        self.dispatch(akiva_framework::wire::op_kinds::cmd::RESIZE, &p)
    }

    fn render(
        &self,
        handle: akiva_framework::wasm_activity::AppHandle,
        ts_ms: f64,
    ) -> u64 {
        // [handle: u32, ts:f64] = 12 bytes
        let mut p = [0u8; 12];
        p[0..4].copy_from_slice(&handle.get().to_le_bytes());
        p[4..12].copy_from_slice(&ts_ms.to_le_bytes());
        self.dispatch(akiva_framework::wire::op_kinds::cmd::RENDER, &p)
    }

    fn save_state(&self, handle: akiva_framework::wasm_activity::AppHandle) -> u64 {
        let payload = handle.get().to_le_bytes();
        self.dispatch(akiva_framework::wire::op_kinds::cmd::SAVE_STATE, &payload)
    }

    fn restore_state(
        &self,
        handle: akiva_framework::wasm_activity::AppHandle,
        bytes: &[u8],
    ) -> u64 {
        if bytes.is_empty() { return 0; }
        // [handle: u32, state: bytes...]
        let mut payload = Vec::with_capacity(4 + bytes.len());
        payload.extend_from_slice(&handle.get().to_le_bytes());
        payload.extend_from_slice(bytes);
        self.dispatch(
            akiva_framework::wire::op_kinds::cmd::RESTORE_STATE,
            &payload,
        )
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
    ) -> u64 {
        if bytes.is_empty() { return 0; }
        // [handle: u32, intent: bytes...]
        let mut payload = Vec::with_capacity(4 + bytes.len());
        payload.extend_from_slice(&handle.get().to_le_bytes());
        payload.extend_from_slice(bytes);
        let _ = self.dispatch(
            akiva_framework::wire::op_kinds::cmd::NEW_INTENT,
            &payload,
        );
        0
    }

    // ── Stone 9.5 ─────────────────────────────────────────────────────────

    fn drain_start_for_result(
        &self,
        handle: akiva_framework::wasm_activity::AppHandle,
    ) -> Option<(Vec<u8>, u32)> {
        // Same two-phase buffer dance as `drain_intent` — the payload is
        // `Intent::encode` bytes only; the `request_code` comes back via
        // `rc_out`. 512-byte initial guess covers the common explicit
        // intent plus a small action string.
        let mut buf = vec![0u8; 512];
        let mut rc_out: u32 = 0;
        let needed = unsafe {
            host::hal_app_drain_start_for_result(
                handle.get(), buf.as_mut_ptr(), buf.len() as u32, &mut rc_out,
            )
        } as usize;
        if needed == 0 {
            return None;
        }
        if needed > buf.len() {
            buf = vec![0u8; needed];
            let got = unsafe {
                host::hal_app_drain_start_for_result(
                    handle.get(), buf.as_mut_ptr(), buf.len() as u32, &mut rc_out,
                )
            } as usize;
            if got == 0 {
                log::warn(&format!(
                    "app {}: drain_start_for_result lost payload on retry",
                    handle.get()
                ));
                return None;
            }
            buf.truncate(got.min(buf.len()));
        } else {
            buf.truncate(needed);
        }
        Some((buf, rc_out))
    }

    fn drain_set_result(
        &self,
        handle: akiva_framework::wasm_activity::AppHandle,
    ) -> Option<Vec<u8>> {
        // Two-phase drain. 128 bytes is a generous first guess — a
        // typical result is a code + a short number/string.
        let mut buf = vec![0u8; 128];
        let needed = unsafe {
            host::hal_app_drain_set_result(handle.get(), buf.as_mut_ptr(), buf.len() as u32)
        } as usize;
        if needed == 0 {
            return None;
        }
        if needed > buf.len() {
            buf = vec![0u8; needed];
            let got = unsafe {
                host::hal_app_drain_set_result(handle.get(), buf.as_mut_ptr(), buf.len() as u32)
            } as usize;
            if got == 0 {
                log::warn(&format!(
                    "app {}: drain_set_result lost payload on retry",
                    handle.get()
                ));
                return None;
            }
            buf.truncate(got.min(buf.len()));
        } else {
            buf.truncate(needed);
        }
        Some(buf)
    }

    fn activity_result(
        &self,
        handle: akiva_framework::wasm_activity::AppHandle,
        request_code: u32,
        bytes: &[u8],
    ) -> u64 {
        if bytes.is_empty() { return 0; }
        // [handle: u32, request_code: u32, result: bytes...]
        let mut payload = Vec::with_capacity(8 + bytes.len());
        payload.extend_from_slice(&handle.get().to_le_bytes());
        payload.extend_from_slice(&request_code.to_le_bytes());
        payload.extend_from_slice(bytes);
        let _ = self.dispatch(
            akiva_framework::wire::op_kinds::cmd::ACTIVITY_RESULT,
            &payload,
        );
        0
    }

    // ── Stone 10.3b ───────────────────────────────────────────────────────

    fn set_grants(&self, package_id: &str, grants: &[&str]) {
        // Serialise as LF-separated UTF-8; matches the on-disk format
        // used by `PackageManager::install` in `sys/permissions/{id}`.
        // Empty slice → empty blob, which the host reads as "no grants"
        // (the app still gets the auto import set).
        let blob = grants.join("\n");
        let pid = package_id.as_bytes();
        let bytes = blob.as_bytes();
        unsafe {
            host::hal_host_set_grants(
                pid.as_ptr(), pid.len(),
                bytes.as_ptr(), bytes.len(),
            );
        }
    }

    // ── Stone 11.0a ───────────────────────────────────────────────────────

    fn set_packages(&self, bytes: &[u8]) {
        // Pass the framework's already-encoded blob straight through.
        // The host doesn't decode it — it serves the same bytes back to
        // any app that calls `akiva_host_query_packages`.
        unsafe {
            host::hal_host_set_packages(bytes.as_ptr(), bytes.len());
        }
    }

    fn poll_future(&self, future_id: u64) -> Option<Result<Vec<u8>, String>> {
        // Stone 11.4c: every dispatch is async. We poll the JS-side
        // future cache via `hal_app_poll_future`; READY means the
        // worker has replied and the bytes are stashed. Empty result
        // bytes are normal for ack-only cmds (lifecycle/input/etc) —
        // the framework treats them identically to a render with an
        // empty paint stream (a successful no-op transition).
        let status = unsafe { host::hal_app_poll_future(future_id) };
        match status {
            // STATUS::PENDING — worker hasn't replied yet; framework
            // re-polls next frame.
            0 => None,
            // STATUS::READY — fetch + return.
            1 => {
                // Common per-frame paint stream sits within the
                // POLL_RESULT_INITIAL_CAP window; the retry branch in
                // fetch_result handles SavedState and oversized paint.
                // Lifecycle/input cmds resolve to 0 bytes which
                // `fetch_result` returns as an empty Vec.
                let bytes = self.fetch_result(future_id, POLL_RESULT_INITIAL_CAP);
                Some(Ok(bytes))
            }
            // STATUS::ERROR — worker reported a trap or rejected the
            // dispatch (unknown handle on its side, etc.).
            _ => Some(Err(format!(
                "KernelWasmHost: dispatch {future_id:#x} reported error"
            ))),
        }
    }
}

/// Exercises the storage HAL end-to-end: read a u32 counter under
/// `sys/boot_count`, increment it, write it back. Stone 11.0c — the
/// returned value used to feed `framework::boot` so the in-framework
/// Shell could display "boot #N". With Shell now an installed wasm
/// app, the value is just a kernel-side log line — visible breadcrumb
/// that storage I/O survived a reboot, no longer threaded into UI.
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

    // Stone 11.5 — pull any package_ids that trapped since the last
    // frame and tell the framework to mark every activity in each
    // bad package as Trapped. Done before event dispatch so a click
    // on a trapped activity doesn't try to forward into a dead worker.
    if let Some(fw) = k.framework.as_mut() {
        drain_trapped_packages_into(fw, timestamp_ms);
    }

    // P0 #1 — drain any save_state futures the worker has answered
    // since last frame. Persists the bytes (or deletes the slot on
    // empty save). Empty queue is the common case so the call is
    // effectively free outside of finish-bursts.
    if let Some(fw) = k.framework.as_mut() {
        fw.drive_pending_saves();
    }

    // P0 #3 — recycle Trapped records older than the grace window.
    // The instance + record drop without firing destroy callbacks
    // (the worker is dead). 30s gives a debugger time to attach and
    // a future tap-to-restart UI a chance to flip Trapped → Destroyed
    // explicitly before reap; bounded leak in the meantime.
    if let Some(fw) = k.framework.as_mut() {
        let n = fw.reap_trapped(timestamp_ms, TRAPPED_GRACE_MS);
        if n > 0 {
            log::info(&format!("reaped {n} trapped activity record(s)"));
        }
    }

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

/// Stone 11.5 — host → framework bridge for the trapped-packages
/// queue. Two-phase buffer: peek at the size first, then realloc if
/// needed. The host evicts the entries on a fitting copy, so this is
/// always single-shot per frame.
///
/// `now_ms` is the frame timestamp; passed through to
/// `mark_package_trapped` so each `Trapped` record gets stamped with
/// the time it died for P0 #3's reap pass.
fn drain_trapped_packages_into(fw: &mut akiva_framework::Framework, now_ms: f64) {
    // Most frames the queue is empty (`needed == 0`); the bigger
    // buffer only kicks in when many apps trap simultaneously. See
    // TRAP_DRAIN_INITIAL_CAP at the top of this file for the rationale.
    let mut buf = [0u8; TRAP_DRAIN_INITIAL_CAP];
    let needed = unsafe {
        host::hal_drain_trapped_packages(buf.as_mut_ptr(), buf.len() as u32)
    } as usize;
    if needed == 0 {
        return;
    }
    let bytes: Vec<u8> = if needed > buf.len() {
        let mut big = vec![0u8; needed];
        let got = unsafe {
            host::hal_drain_trapped_packages(big.as_mut_ptr(), big.len() as u32)
        } as usize;
        big.truncate(got.min(big.len()));
        big
    } else {
        buf[..needed].to_vec()
    };
    let blob = match core::str::from_utf8(&bytes) {
        Ok(s) => s,
        Err(_) => {
            log::warn("drain_trapped_packages: non-utf8 blob from host, discarding");
            return;
        }
    };
    for pkg in blob.split('\n') {
        let pkg = pkg.trim();
        if pkg.is_empty() { continue; }
        let n = fw.mark_package_trapped(pkg, now_ms);
        if n > 0 {
            log::warn(&format!("package '{pkg}' trapped — {n} activity(s) marked Trapped"));
        }
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

/// P0 #2 — graceful-shutdown checkpoint. The HAL calls this when the
/// browser tab signals "user is leaving" (`visibilitychange` to
/// `hidden`, `pagehide`). The framework dispatches `save_state` at the
/// foreground activity; the bytes arrive over the worker boundary and
/// get persisted by the regular `drive_pending_saves` path. The
/// activity's lifecycle state is unchanged.
///
/// `timestamp_ms` is the same monotonic clock `akiva_frame` uses; we
/// pass it straight through so the future's `started_at_ms` is on the
/// same clock as the watchdog's deadline check.
///
/// Returns the number of save dispatches issued — 0 means nothing to
/// checkpoint (no foreground, already in flight, in-process activity).
/// The HAL logs the value for visibility but doesn't act on it.
#[no_mangle]
pub extern "C" fn akiva_checkpoint(timestamp_ms: f64) -> u32 {
    let k = kernel();
    let Some(fw) = k.framework.as_mut() else {
        return 0;
    };
    let n = fw.checkpoint(timestamp_ms) as u32;
    if n > 0 {
        log::debug(&format!("checkpoint: {n} save dispatched"));
    }
    n
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
