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
    /// Lazy-initialised in `akiva_boot` because Stone 13.4 turned the
    /// internals into a `HashMap<DisplayId, Display>` which can't be
    /// const-constructed; the `display()` accessor returns a mutable
    /// reference that panics if accessed pre-boot (a bug).
    display: Option<DisplayManager>,
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
            display: None,
            framework: None,
            frame_count: 0,
            last_frame_ms: 0.0,
        }
    }

    /// Borrow the DisplayManager, panicking if `akiva_boot` hasn't
    /// been called. Every entry point either runs after boot or
    /// returns early before it would touch the display.
    fn display(&mut self) -> &mut DisplayManager {
        self.display.as_mut().expect("display accessed before akiva_boot")
    }

    fn handle_event(&mut self, ev: Event) {
        let ts = self.last_frame_ms;
        match ev {
            Event::Pointer { x, y, button, phase, display_id } => {
                if let Some(fw) = self.framework.as_mut() {
                    fw.dispatch_pointer_on_display(
                        fw_act::PointerEvent { x, y, button, phase: map_pointer_phase(phase) },
                        ts,
                        display_id,
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
            Event::Resize { width_px, height_px, density, display_id } => {
                // Stone 13.4 — resize is per-display. The DisplayManager
                // tracks one set of metrics per id; the framework only
                // needs `set_metrics` for the primary (the sole render
                // path's "global" metrics — for follower displays the
                // per-display loop in `akiva_frame` re-pushes metrics
                // before calling `render_frame_for_display`).
                self.display().on_resize(display_id, width_px, height_px, density);
                if display_id == display_manager::PRIMARY_DISPLAY {
                    if let Some(fw) = self.framework.as_mut() {
                        fw.set_metrics(fw_act::SurfaceMetrics { width_px, height_px, density });
                    }
                }
            }
            Event::Visibility { visible } => {
                log::debug(&format!("kernel: visibility={}", visible));
            }
            Event::TextInput { text } => {
                if let Some(fw) = self.framework.as_mut() {
                    fw.dispatch_text_input(&text, ts);
                }
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
    // Stone 13.4 — DisplayManager moved off `const fn new` (HashMap
    // can't be const-constructed), so we lazy-init here. Subsequent
    // boots are guarded by the BOOTED swap above.
    k.display = Some(DisplayManager::new());
    k.display().init();

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
    // Stone 11.6.7 #5 — push per-package watchdog overrides from
    // each package's `manifest.runtime.watchdog_timeout_ms`. Absent
    // = host default; this is one HAL call per package per boot,
    // not per frame, so the cost is bounded by install count.
    for rec in fw.packages.list() {
        if let Some(ms) = rec.manifest.runtime.watchdog_timeout_ms {
            unsafe {
                host::hal_app_set_watchdog_timeout_ms(rec.id.as_ptr(), rec.id.len(), ms);
            }
        }
    }
    let m = k.display().metrics();
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

    fn text_input(
        &self,
        handle: akiva_framework::wasm_activity::AppHandle,
        text: &str,
        ts_ms: f64,
    ) -> u64 {
        // [handle: u32 | text_len: u16 | text: utf8... | ts: f64]
        let bytes = text.as_bytes();
        // Cap text at 64 KiB so a runaway IME chunk can't overflow the
        // u16 length prefix or the cross-worker message budget.
        if bytes.len() > u16::MAX as usize {
            log::warn(&format!(
                "text_input: dropping chunk over {} bytes ({} bytes)",
                u16::MAX, bytes.len(),
            ));
            return 0;
        }
        let mut p = Vec::with_capacity(4 + 2 + bytes.len() + 8);
        p.extend_from_slice(&handle.get().to_le_bytes());
        p.extend_from_slice(&(bytes.len() as u16).to_le_bytes());
        p.extend_from_slice(bytes);
        p.extend_from_slice(&ts_ms.to_le_bytes());
        self.dispatch(akiva_framework::wire::op_kinds::cmd::TEXT_INPUT, &p)
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

    // Phase 12.1 + 12.2 — sample each installed package's memory
    // (byteLength) and cumulative CPU time, forward both to the
    // framework. The host caches the last pong-reported values per
    // process so this loop is two HAL calls per package per frame
    // (cheap; 5–10 packages typical). Phase 12.3 (LMK) ranks on
    // these; Phase 12.5 enforces manifest budgets on memory.
    if let Some(fw) = k.framework.as_mut() {
        sample_app_resources_into(fw, timestamp_ms);
    }

    // Phase 12.5 — manifest enforcement. Runs BEFORE the LMK so a
    // package that's over its own declared `max_memory_mb` budget gets
    // killed regardless of system pressure. The contract is one-sided:
    // the manifest declared a limit, and we hold the app to it. The
    // LMK below is system-wide pressure response; this is per-app
    // contract enforcement.
    if let Some(fw) = k.framework.as_mut() {
        enforce_memory_budgets_into(fw, timestamp_ms);
    }

    // Phase 12.3 — Low-Memory Killer. Runs after the resource sample
    // (so accounting is fresh) and before event dispatch (so a click
    // doesn't land on an activity that's about to be evicted). Ordered
    // list of victims is honoured: kill the highest-impact one first,
    // re-run on the next frame if pressure is still real.
    if let Some(fw) = k.framework.as_mut() {
        drive_lmk_into(fw, timestamp_ms);
    }

    // Drain all queued events and dispatch.
    while let Some(ev) = k.events.pop() {
        k.handle_event(ev);
    }

    k.scheduler.run_ready(timestamp_ms);

    // P1 #3 — drain async signals (finish / start_activity / set_result)
    // the foreground activity raised since its last callback. Done as a
    // dedicated frame phase so render stays purely "produce paint
    // commands". Pre-P1 #3 this drain piggybacked at the top of render;
    // moving it out keeps the contract clean and catches signals on
    // frames that don't repaint.
    if let Some(fw) = k.framework.as_mut() {
        fw.drive_pending_signals(timestamp_ms);
    }

    // Stone 13.4 — animation tick once per kernel frame, BEFORE the
    // per-display loop. Without this guard, a multi-display
    // configuration would advance transitions N times per frame.
    if let Some(fw) = k.framework.as_mut() {
        fw.tick_animations(timestamp_ms);
    }

    // Render through the framework, then flush the resulting command
    // buffer to the HAL. The split keeps HAL calls confined to the kernel
    // crate so the framework stays wasm-target-agnostic.
    //
    // Stone 13.4 — iterate every live display, render the task assigned
    // to that display, and flush per display. The primary path
    // (DisplayId 0) routes through immediate-mode HAL imports as
    // before; follower displays serialise their ops via
    // `hal_post_to_display` so the JS host can BroadcastChannel the
    // bytes to the claiming tab.
    // Stone 13.4 — disjoint borrows of `k.framework` and `k.display`
    // through individual field references so the per-display loop
    // doesn't trip the borrow checker (both are `Option<_>` fields on
    // the same Kernel struct; method-style `k.display()` borrows the
    // whole `k`).
    if let (Some(fw), Some(disp)) = (k.framework.as_mut(), k.display.as_mut()) {
        let display_ids = disp.ids();
        if display_ids.len() <= 1 {
            // Single-display fast path: render once to the primary
            // display. Matches the pre-13.4 behaviour exactly when no
            // follower has claimed a display.
            let frame = fw.render_frame(timestamp_ms);
            disp.flush(display_manager::PRIMARY_DISPLAY, frame);
        } else {
            for display_id in display_ids {
                // Push per-display metrics into the framework so
                // layout reads the right size for this pass. The
                // primary's metrics already match (Resize events flow
                // through `set_metrics`); follower displays carry
                // their own.
                if let Some(m) = disp.metrics_of(display_id) {
                    fw.set_metrics(fw_act::SurfaceMetrics {
                        width_px: m.width_px,
                        height_px: m.height_px,
                        density: m.density,
                    });
                }
                let frame = fw.render_frame_for_display(timestamp_ms, display_id);
                disp.flush(display_id, frame);
            }
        }
    }

    // Stone 13.2 — drain any dialog resolutions the user produced this
    // frame and hand them back to the host so it can post host_op_done
    // to the right app worker. Empty queue is the common case.
    if let Some(fw) = k.framework.as_mut() {
        for r in fw.drain_dialog_resolutions() {
            unsafe {
                host::hal_dialog_complete(
                    r.handle,
                    r.future_id,
                    r.kind as u32,
                    r.button_idx as u32,
                );
            }
        }
    }
}

/// Phase 12.1 + 12.2 — host → framework resource sampler. Walks
/// every installed package, asks the host for its current memory
/// (`WebAssembly.Memory.buffer.byteLength`) and cumulative CPU time
/// (ms spent in wasm-export brackets), forwards both readings into
/// [`Framework::record_app_memory`] and [`Framework::record_app_cpu`]
/// respectively.
///
/// Cheap: two HAL calls per package per frame (5–10 packages typical).
/// The host's readings are updated lazily via the heartbeat pong, so
/// this loop is essentially a pair of copies from the host's
/// per-process snapshot into the framework's accumulators. `frame_ts`
/// is passed into the CPU side so `CpuBudget` can derive the
/// recent-window rate; the memory side ignores it.
fn sample_app_resources_into(fw: &mut akiva_framework::Framework, frame_ts: f64) {
    let ids: Vec<String> = fw.packages.list().map(|rec| rec.id.clone()).collect();
    for id in ids {
        let bytes = unsafe {
            host::hal_app_memory_current(id.as_ptr(), id.len())
        };
        fw.record_app_memory(&id, bytes);
        let cpu_ms = unsafe {
            host::hal_app_cpu_ms(id.as_ptr(), id.len())
        };
        fw.record_app_cpu(&id, cpu_ms, frame_ts);
    }
}

/// Phase 12.3 — Low-Memory Killer driver. Asks the framework who to
/// evict, terminates each victim's worker via `hal_evict_package`, and
/// runs framework-side cleanup (`Framework::evict_package` — marks
/// every activity Trapped, forgets memory + cpu accounting).
///
/// Pure no-op below the policy threshold — `drive_lmk` returns an
/// empty vec and the loop never enters. `now_ms` is forwarded to the
/// framework so the resulting `Trapped` records get stamped for the
/// P0 #3 reap pass.
///
/// Each `worker.terminate()` is independent — a failed HAL call (host
/// returned 0) does not block the framework cleanup. The intent is
/// that the package is gone either way; if the worker was already
/// dead (race vs the watchdog), framework-side cleanup is still useful
/// because it recovers memory accounting.
/// Phase 12.5 — kill any package whose current memory exceeds its
/// manifest-declared `runtime.max_memory_mb`. Same `hal_evict_package`
/// + `Framework::evict_package` cleanup as the LMK kill path; logged
/// with a distinct prefix so post-mortems can tell budget violation
/// from pressure eviction.
///
/// Cheap when no app is over budget — `over_budget_packages` short-
/// circuits to an empty Vec without reading any storage.
fn enforce_memory_budgets_into(fw: &mut akiva_framework::Framework, now_ms: f64) {
    let violators = fw.over_budget_packages();
    for pkg in violators {
        let report = fw.memory_usage(&pkg);
        log::error(&format!(
            "memory budget exceeded — terminating {} (current={}MB, limit={}MB)",
            pkg,
            report.map(|r| r.current_bytes  / (1024 * 1024)).unwrap_or(0),
            report.map(|r| r.limit_bytes    / (1024 * 1024)).unwrap_or(0),
        ));
        let bytes = pkg.as_bytes();
        let _terminated = unsafe { host::hal_evict_package(bytes.as_ptr(), bytes.len()) };
        fw.evict_package(&pkg, now_ms);
    }
}

fn drive_lmk_into(fw: &mut akiva_framework::Framework, now_ms: f64) {
    use akiva_framework::lmk::{Action, KillReason};
    let victims = fw.drive_lmk(KillReason::SystemPressure);
    if victims.is_empty() {
        return;
    }
    for victim in &victims {
        log::info(&format!(
            "lmk: {} {} (priority={:?}, current={}MB, reason={:?})",
            match victim.action { Action::Suspend => "suspending", Action::Kill => "killing" },
            victim.package_id,
            victim.priority,
            victim.current_bytes / (1024 * 1024),
            victim.reason,
        ));
        let pid = victim.package_id.as_bytes();
        match victim.action {
            Action::Suspend => {
                // Stone 12.4b — soft eviction. Drive save_state +
                // pause/stop callbacks while the worker is still alive
                // so the bytes can land, mark Suspended, drop the
                // activity instance, then terminate the worker.
                fw.suspend_package(&victim.package_id, now_ms);
                let _terminated = unsafe { host::hal_evict_package(pid.as_ptr(), pid.len()) };
            }
            Action::Kill => {
                // Hard eviction — worker.terminate first, then mark the
                // framework records Trapped + forget accounting.
                let _terminated = unsafe { host::hal_evict_package(pid.as_ptr(), pid.len()) };
                fw.evict_package(&victim.package_id, now_ms);
            }
        }
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
    if let Some(d) = k.display.as_mut() { d.shutdown(); }
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
/// Stone 11.6.9 #3 — host-driven device-config change. Today the
/// only dimension is the dark-mode `night` bit; the body of this
/// export will fan out to additional flags as `values-he/`,
/// `values-land/`, etc. land. The host calls this whenever the
/// browser's `prefers-color-scheme` resolves to a new value (boot,
/// or a runtime change observed via `matchMedia`).
///
/// `night != 0` selects the `values-night/` overlay for subsequent
/// `@string/` / `@color/` lookups; the call is idempotent on the
/// current state, so duplicate matchMedia notifications are cheap.
/// Pre-boot calls are silently dropped — `akiva_boot` will see the
/// host's then-current value through its own initialiser path.
#[no_mangle]
pub extern "C" fn akiva_config_change(night: i32) {
    let k = kernel();
    let Some(fw) = k.framework.as_mut() else {
        return;
    };
    let _changed = fw.resources.set_night_mode(night != 0);
    // No request-redraw call yet — every running activity already
    // re-renders next frame because the kernel's rAF tick polls all
    // dirty slots, and `set_resources` (the JS-side companion) flushes
    // the new colour map to each app worker. If a future
    // background-only activity caches a resolved colour, it will need
    // its own invalidation hook here.
}

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

/// Stone 13.4 — allocate a fresh display for a follower tab. The JS
/// host calls this when a tab claims a display (`request_display` →
/// session forwards to worker → kernel calls back). Returns the
/// freshly-minted `DisplayId`; the host records the
/// displayId↔tabId mapping for paint stream routing via
/// `hal_post_to_display`.
///
/// `density_q16` is `density * 65536` rounded to a u32 — wasm i32
/// args don't carry float values directly without f32→i32 reinterpret
/// boilerplate on the JS side, so we use a fixed-point encoding for
/// the one value that needs sub-integer precision.
#[no_mangle]
pub extern "C" fn akiva_alloc_display(
    width_px: u32, height_px: u32, density_q16: u32,
) -> u32 {
    let density = (density_q16 as f32) / 65536.0;
    let metrics = display_manager::SurfaceMetrics { width_px, height_px, density };
    kernel().display().alloc_display(metrics)
}

/// Stone 13.4 — release a previously-allocated display when its
/// claiming tab closes (or gracefully relinquishes). No-op for
/// `PRIMARY_DISPLAY` and unknown ids. Returns 1 on a successful
/// removal, 0 otherwise.
#[no_mangle]
pub extern "C" fn akiva_release_display(display_id: u32) -> u32 {
    if kernel().display().release_display(display_id) { 1 } else { 0 }
}

/// Stone 13.4 — assign a task to a display. The kernel reads the
/// foreground activity of the named task when rendering that
/// display's frame each tick. `task_id == 0` is the wildcard "leave
/// at default" sentinel.
#[no_mangle]
pub extern "C" fn akiva_assign_task_to_display(
    task_id: u64, display_id: u32,
) -> u32 {
    let Some(fw) = kernel().framework.as_mut() else { return 0; };
    if fw.assign_task_to_display(task_id, display_id) { 1 } else { 0 }
}

/// Stone 13.4 — launch a fresh activity on its own task bound to the
/// named display. The host calls this after `akiva_alloc_display` so a
/// follower tab gets an independent `(package_id, class)` instance
/// rather than mirroring the primary. Returns the freshly-allocated
/// `task_id` on success, `0` on failure (unknown activity, lifecycle
/// shortcut to Destroyed, no framework yet).
///
/// The system foreground stack stays untouched — the new activity
/// renders only on `display_id` for as long as that display is
/// alive.
///
/// # Safety
/// `package_id_*` and `class_*` are valid UTF-8 byte slices the host
/// owns for the duration of the call.
#[no_mangle]
pub unsafe extern "C" fn akiva_launch_to_display(
    package_id_ptr: *const u8, package_id_len: usize,
    class_ptr: *const u8, class_len: usize,
    display_id: u32,
) -> u64 {
    let pkg = if package_id_ptr.is_null() || package_id_len == 0 {
        return 0;
    } else {
        match core::str::from_utf8(unsafe {
            core::slice::from_raw_parts(package_id_ptr, package_id_len)
        }) {
            Ok(s) => s,
            Err(_) => return 0,
        }
    };
    let class = if class_ptr.is_null() || class_len == 0 {
        return 0;
    } else {
        match core::str::from_utf8(unsafe {
            core::slice::from_raw_parts(class_ptr, class_len)
        }) {
            Ok(s) => s,
            Err(_) => return 0,
        }
    };
    let k = kernel();
    let ts = k.last_frame_ms;
    let Some(fw) = k.framework.as_mut() else { return 0; };
    fw.launch_to_display(pkg, class, display_id, ts).unwrap_or(0)
}

/// Stone 13.2 — open a system dialog overlay on behalf of an app
/// worker that fired `IO::SHOW_DIALOG`. The host decodes the wire
/// payload, allocates a kernel buffer via `akiva_alloc`, copies the
/// args in, and calls this. Returns:
/// - `0` — opened successfully; the framework will paint the dialog
///   from the next frame on, and `hal_dialog_complete` fires when the
///   user resolves it.
/// - `1` — bad payload (truncated args, mismatched lengths).
/// - `2` — no foreground activity to attach the dialog to.
///
/// On error the host should resolve the originating worker's future
/// immediately with `Dismissed` so the app's `.await` doesn't hang.
///
/// # Safety
/// `args_ptr..args_ptr+args_len` must be a kernel-owned byte buffer
/// (typically from `akiva_alloc`); the host owns dealloc and must
/// pair this call with one `akiva_dealloc(args_ptr, args_len)`.
#[no_mangle]
pub unsafe extern "C" fn akiva_open_dialog(
    handle: u32,
    future_id: u64,
    args_ptr: *const u8,
    args_len: usize,
) -> u32 {
    let bytes = if args_ptr.is_null() || args_len == 0 {
        return 1;
    } else {
        unsafe { core::slice::from_raw_parts(args_ptr, args_len) }
    };
    let (title, body, buttons) = match decode_dialog_args(bytes) {
        Some(t) => t,
        None => {
            log::warn("akiva_open_dialog: malformed args");
            return 1;
        }
    };
    let k = kernel();
    let Some(fw) = k.framework.as_mut() else { return 2; };
    match fw.open_dialog(handle, future_id, title, body, buttons) {
        Ok(()) => 0,
        Err(e) => {
            log::warn(&format!("akiva_open_dialog: {e}"));
            2
        }
    }
}

/// Decode the `IO::SHOW_DIALOG` wire format the SDK encodes in
/// `akiva_sdk::dialog::encode_args`. The host strips the leading
/// `handle` u32 before calling in (it's already passed as a function
/// argument here, not embedded in the slice).
///
/// Layout:
/// ```text
/// [title_len: u16 LE | title: utf8...
///  | body_len: u16 LE | body: utf8...
///  | btn_count: u8 | (btn_len: u16 LE | btn: utf8...)*]
/// ```
fn decode_dialog_args(bytes: &[u8]) -> Option<(String, String, Vec<String>)> {
    let mut p = 0usize;
    let read_u16 = |p: &mut usize, b: &[u8]| -> Option<u16> {
        if *p + 2 > b.len() { return None; }
        let v = u16::from_le_bytes([b[*p], b[*p + 1]]);
        *p += 2;
        Some(v)
    };
    let read_str = |p: &mut usize, b: &[u8]| -> Option<String> {
        let len = read_u16(p, b)? as usize;
        if *p + len > b.len() { return None; }
        let s = core::str::from_utf8(&b[*p..*p + len]).ok()?.to_string();
        *p += len;
        Some(s)
    };
    let title = read_str(&mut p, bytes)?;
    let body = read_str(&mut p, bytes)?;
    if p >= bytes.len() { return None; }
    let count = bytes[p] as usize;
    p += 1;
    let mut buttons = Vec::with_capacity(count);
    for _ in 0..count {
        buttons.push(read_str(&mut p, bytes)?);
    }
    Some((title, body, buttons))
}
