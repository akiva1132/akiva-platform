//! App-side runtime: handle table + ABI-v0 glue.
//!
//! Hidden implementation behind the `akiva_app!` macro. App authors never
//! call into this module directly — the macro generates `#[no_mangle]`
//! `extern "C"` exports that funnel through `on_*` helpers here, which
//! then dispatch to the user's `Activity` impl.
//!
//! Stone 11.2 — every dispatch awaits the [`Activity`] future via
//! [`block_on_ready`]. The wire shape stays identical to ABI v0
//! (host calls sync `extern "C"` exports); the asynchrony is purely
//! inside the wasm. Stone 11.3 swaps `block_on_ready` for the embassy
//! executor when host imports start returning real `Pending`.
//!
//! One wasm process can host multiple activity handles (the host creates a
//! fresh one per framework-side `WasmActivity`), so everything is keyed by
//! a `u32` handle. The process holds a single `HashMap<u32, Slot>` in
//! `static mut` — safe because wasm in a worker is single-threaded and the
//! host calls our exports serially.
//!
//! The paint buffer is stored in the slot so the pointer returned by
//! `akiva_app_render` remains valid until the next render for that
//! handle — exactly the lifetime rule in [docs/abi.md §2.2].

#![doc(hidden)]

use std::alloc::{alloc as std_alloc, dealloc as std_dealloc, Layout};
use std::collections::HashMap;

use crate::activity::{
    block_on_ready, Activity, Context, KeyEvent, KeyPhase, Metrics, PointerEvent, PointerPhase,
    SavedState,
};
use crate::intent::Intent;
use crate::result::ActivityResult;

// Host-provided imports. Matches `AppRegistry._buildAppImports` in
// runtime/web/app_registry.js. Gated on wasm32 so non-wasm builds
// (cargo test) link without needing to provide the symbols — the
// corresponding helper functions below fall back to in-process
// approximations when the host isn't available.
#[cfg(target_arch = "wasm32")]
extern "C" {
    pub fn akiva_host_log(level: u32, ptr: *const u8, len: u32);
    pub fn akiva_host_text_width(ptr: *const u8, len: u32, size: f32) -> f32;
    /// Stone 9.3 — ask the platform to tear this activity down. Handle
    /// disambiguates when the package has multiple activities. No return;
    /// the framework picks up the signal via its `hal_app_drain_finish`
    /// HAL bridge at the end of the callback. See docs/abi.md §2.3.
    pub fn akiva_host_finish(handle: u32);
    /// Stone 9.4 — ask the platform to launch another activity. The
    /// payload is an encoded `Intent` (see `Intent::encode`). Same
    /// drain-at-callback-end semantics as `akiva_host_finish`; the
    /// framework calls its `hal_app_drain_intent` HAL bridge to read
    /// back the blob. See docs/abi.md §2.3.
    pub fn akiva_host_start_activity(handle: u32, ptr: *const u8, len: u32);
    /// Stone 9.5 — launch another activity and keep the door open for a
    /// result. `request_code` is an app-chosen u32 that identifies the
    /// call site; the framework threads it back through
    /// `akiva_app_activity_result` so the caller can disambiguate. Same
    /// drain-at-callback-end flow as `akiva_host_start_activity`.
    pub fn akiva_host_start_activity_for_result(
        handle: u32, ptr: *const u8, len: u32, request_code: u32,
    );
    /// Stone 9.5 — stash the bytes this activity wants to return to its
    /// caller. Only meaningful when the activity was launched via
    /// `start_activity_for_result`. Repeatable (last call wins). The
    /// framework drains via `hal_app_drain_set_result` when this activity
    /// is about to be destroyed.
    pub fn akiva_host_set_result(handle: u32, ptr: *const u8, len: u32);
    /// Stone 11.0a — enumerate installed packages. Two-phase buffer: if
    /// the supplied `out_cap` is too small, the host writes nothing and
    /// returns the required size so the caller can realloc and retry.
    /// A zero return means "no packages" (or the grant is missing — the
    /// deny-stub answers identically by design). Gated by
    /// `akiva.permission.QUERY_PACKAGES`. See [`crate::pkgs`] for the
    /// safe wrapper.
    pub fn akiva_host_query_packages(out_ptr: *mut u8, out_cap: u32) -> u32;
}

/// Request the framework finish this activity. Safe to call from any
/// lifecycle or input callback on wasm; a no-op on host builds so unit
/// tests compile without a real host import. Handle is the one the
/// framework passed to the current callback.
pub fn host_finish(handle: u32) {
    #[cfg(target_arch = "wasm32")]
    unsafe {
        akiva_host_finish(handle);
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = handle;
    }
}

/// Request the framework resolve and launch an activity matching the
/// supplied `Intent`. The encoded bytes must outlive this call (the host
/// copies them out synchronously before we return). On non-wasm this is
/// a no-op so tests link without a real host.
pub fn host_start_activity(handle: u32, intent: &Intent) {
    let bytes = intent.encode();
    #[cfg(target_arch = "wasm32")]
    unsafe {
        akiva_host_start_activity(handle, bytes.as_ptr(), bytes.len() as u32);
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = (handle, bytes);
    }
}

/// Request the framework launch `intent` and record that this activity
/// expects a result back with `request_code`. Same lifetime rules as
/// `host_start_activity` (host copies bytes synchronously).
pub fn host_start_activity_for_result(handle: u32, intent: &Intent, request_code: u32) {
    let bytes = intent.encode();
    #[cfg(target_arch = "wasm32")]
    unsafe {
        akiva_host_start_activity_for_result(
            handle, bytes.as_ptr(), bytes.len() as u32, request_code,
        );
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = (handle, bytes, request_code);
    }
}

/// Stash a result the framework will deliver back to whoever launched
/// this activity via `start_activity_for_result`. Callable multiple
/// times in the activity's lifetime; only the last call's bytes are
/// delivered. If this activity was not launched for a result, the
/// framework silently drops the stashed bytes.
pub fn host_set_result(handle: u32, result: &ActivityResult) {
    let bytes = result.encode();
    #[cfg(target_arch = "wasm32")]
    unsafe {
        akiva_host_set_result(handle, bytes.as_ptr(), bytes.len() as u32);
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = (handle, bytes);
    }
}

/// Measure a rendered UTF-8 string's width at `size` pixels. On wasm
/// this calls through to the host's `CanvasRenderingContext2D.measureText`
/// against the same font the renderer uses, so SDK layout agrees with
/// what actually lands on screen. On non-wasm builds (tests) this falls
/// back to the cheap `chars × size × 0.55` estimate — layout tests that
/// depend on exact numbers should pass in their own fake to avoid
/// coupling to this heuristic.
pub fn text_width(text: &str, size: f32) -> f32 {
    #[cfg(target_arch = "wasm32")]
    unsafe {
        akiva_host_text_width(text.as_ptr(), text.len() as u32, size)
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        text.chars().count() as f32 * size * 0.55
    }
}

/// Minimum paint-op capacity we pre-allocate per slot. Grows on demand;
/// 4 KiB is enough for a typical frame of a hello-world and avoids a
/// realloc on the first render.
const INITIAL_PAINT_CAP: usize = 4 * 1024;

// ── Global state ──────────────────────────────────────────────────────

struct Slot {
    activity: Box<dyn Activity>,
    paint_buf: Vec<u8>,
    /// Separate buffer for `on_save_state` output. Kept per-slot (like
    /// `paint_buf`) so the pointer we return to the host stays valid
    /// until the next `akiva_app_save_state` for this handle — same
    /// lifetime rule as render, and it spares us from juggling a
    /// shared scratch space across handles.
    save_buf: Vec<u8>,
    metrics: Metrics,
    last_pointer: Option<PointerEvent>,
}

/// Registry keyed by handle. `Option` so we can lazy-init on first use
/// (`const` ctor for `HashMap` isn't stable).
static mut SLOTS: Option<HashMap<u32, Slot>> = None;

#[inline]
fn slots() -> &'static mut HashMap<u32, Slot> {
    // SAFETY: wasm in a worker is single-threaded; host exports are
    // invoked serially. No concurrent access possible.
    unsafe {
        let ptr = core::ptr::addr_of_mut!(SLOTS);
        if (*ptr).is_none() {
            *ptr = Some(HashMap::new());
        }
        (*ptr).as_mut().unwrap()
    }
}

// ── Memory ABI (host ↔ app transfers) ─────────────────────────────────

/// `akiva_app_alloc` implementation. The host calls this when it needs to
/// place bytes into app memory (e.g. the class name during `on_create`).
/// Alignment of 1 is fine: the only readers are `DataView` / typed-array
/// copies on the JS side which don't require alignment.
pub fn alloc(size: u32) -> *mut u8 {
    if size == 0 { return core::ptr::null_mut(); }
    let layout = match Layout::from_size_align(size as usize, 1) {
        Ok(l) => l,
        Err(_) => return core::ptr::null_mut(),
    };
    // SAFETY: layout has non-zero size; allocator returns a valid pointer
    // or null on OOM. Caller pairs every alloc with an exact-sized dealloc.
    unsafe { std_alloc(layout) }
}

/// Companion to `alloc`. Size must match the one passed to `alloc`.
pub fn dealloc(ptr: *mut u8, size: u32) {
    if ptr.is_null() || size == 0 { return; }
    let layout = match Layout::from_size_align(size as usize, 1) {
        Ok(l) => l,
        Err(_) => return,
    };
    // SAFETY: caller guarantees `ptr` came from `alloc(size)`.
    unsafe { std_dealloc(ptr, layout); }
}

/// Version string the host may read to gate compat checks. Null-terminated
/// so the host can read until `\0` without tracking a length.
pub const ABI_VERSION: &[u8] = b"0.1.0\0";

// ── Lifecycle entry points ────────────────────────────────────────────
//
// Each accepts the raw wire types the host passes through `hal_app_*`.
// They look up the slot, rebuild a `Context`, and `block_on_ready` the
// async trait method.

pub fn on_create<T: Activity + Default>(handle: u32, class_ptr: *const u8, class_len: u32) {
    let class = read_utf8(class_ptr, class_len);
    let mut activity: Box<dyn Activity> = Box::new(T::default());
    let mut ctx = Context {
        metrics: Metrics::default(),
        timestamp_ms: 0.0,
        pointer: None,
        class: class.as_str(),
        handle,
    };
    block_on_ready(activity.on_create(&mut ctx));
    slots().insert(handle, Slot {
        activity,
        paint_buf: Vec::with_capacity(INITIAL_PAINT_CAP),
        save_buf: Vec::new(),
        metrics: Metrics::default(),
        last_pointer: None,
    });
}

pub fn on_start(handle: u32) {
    dispatch_lifecycle(handle, |a, c| block_on_ready(a.on_start(c)));
}
pub fn on_resume(handle: u32) {
    dispatch_lifecycle(handle, |a, c| block_on_ready(a.on_resume(c)));
}
pub fn on_pause(handle: u32) {
    dispatch_lifecycle(handle, |a, c| block_on_ready(a.on_pause(c)));
}
pub fn on_stop(handle: u32) {
    dispatch_lifecycle(handle, |a, c| block_on_ready(a.on_stop(c)));
}

pub fn on_destroy(handle: u32) {
    if let Some(mut slot) = slots().remove(&handle) {
        let mut ctx = Context {
            metrics: slot.metrics,
            timestamp_ms: 0.0,
            pointer: slot.last_pointer,
            class: "",
            handle,
        };
        block_on_ready(slot.activity.on_destroy(&mut ctx));
    }
}

fn dispatch_lifecycle(
    handle: u32,
    f: impl FnOnce(&mut dyn Activity, &mut Context<'_>),
) {
    let Some(slot) = slots().get_mut(&handle) else { return; };
    let mut ctx = Context {
        metrics: slot.metrics,
        timestamp_ms: 0.0,
        pointer: slot.last_pointer,
        class: "",
        handle,
    };
    f(slot.activity.as_mut(), &mut ctx);
}

// ── Input ─────────────────────────────────────────────────────────────

pub fn on_pointer(handle: u32, x: f32, y: f32, button: u32, phase_wire: u32, ts_ms: f64) {
    let Some(slot) = slots().get_mut(&handle) else { return; };
    let ev = PointerEvent {
        x, y,
        button: button as u8,
        phase: decode_pointer_phase(phase_wire),
    };
    slot.last_pointer = Some(ev);
    let mut ctx = Context {
        metrics: slot.metrics,
        timestamp_ms: ts_ms,
        pointer: Some(ev),
        class: "",
        handle,
    };
    block_on_ready(slot.activity.on_pointer(&mut ctx, ev));
}

pub fn on_key(handle: u32, code: u32, modifiers: u32, phase_wire: u32, ts_ms: f64) {
    let Some(slot) = slots().get_mut(&handle) else { return; };
    let ev = KeyEvent {
        code,
        modifiers: modifiers as u8,
        phase: decode_key_phase(phase_wire),
    };
    let mut ctx = Context {
        metrics: slot.metrics,
        timestamp_ms: ts_ms,
        pointer: slot.last_pointer,
        class: "",
        handle,
    };
    block_on_ready(slot.activity.on_key(&mut ctx, ev));
}

pub fn on_resize(handle: u32, width_px: u32, height_px: u32, density: f32) {
    let Some(slot) = slots().get_mut(&handle) else { return; };
    let metrics = Metrics { width_px, height_px, density };
    slot.metrics = metrics;
    let mut ctx = Context {
        metrics,
        timestamp_ms: 0.0,
        pointer: slot.last_pointer,
        class: "",
        handle,
    };
    block_on_ready(slot.activity.on_resize(&mut ctx, metrics));
}

// ── Render ────────────────────────────────────────────────────────────

pub fn on_render(handle: u32, ts_ms: f64, out_len_ptr: *mut u32) -> *const u8 {
    let Some(slot) = slots().get_mut(&handle) else {
        // SAFETY: host always passes a writable u32 cell on each call;
        // for handle-misses we write 0 and return null so the host
        // can short-circuit.
        unsafe { *out_len_ptr = 0; }
        return core::ptr::null();
    };
    slot.paint_buf.clear();
    let mut ctx = Context {
        metrics: slot.metrics,
        timestamp_ms: ts_ms,
        pointer: slot.last_pointer,
        class: "",
        handle,
    };
    let mut canvas = crate::canvas::Canvas::new(&mut slot.paint_buf);
    block_on_ready(slot.activity.on_render(&mut ctx, &mut canvas));
    let len = slot.paint_buf.len() as u32;
    let ptr = slot.paint_buf.as_ptr();
    unsafe { *out_len_ptr = len; }
    ptr
}

// ── Saved state ───────────────────────────────────────────────────────
//
// Symmetric with `on_render`: the app gives the host a pointer + length
// and guarantees the pointer stays valid until the next call to
// `akiva_app_save_state` for the same handle. The host copies the bytes
// into kernel memory, which in turn persists them through the framework.
//
// Save/restore are sync on the trait — see crate::activity::Activity for
// why.

pub fn on_save_state(handle: u32, out_len_ptr: *mut u32) -> *const u8 {
    let Some(slot) = slots().get_mut(&handle) else {
        unsafe { *out_len_ptr = 0; }
        return core::ptr::null();
    };
    let mut state = SavedState::default();
    slot.activity.on_save_state(&mut state);
    slot.save_buf = state.encode();
    let len = slot.save_buf.len() as u32;
    let ptr = slot.save_buf.as_ptr();
    unsafe { *out_len_ptr = len; }
    ptr
}

/// Deliver a re-issued intent to an already-existing activity. Called
/// from the `akiva_app_new_intent` export generated by the `akiva_app!`
/// macro. The host arranges bytes in app memory (via `akiva_app_alloc`
/// + copy) and frees them once the call returns, matching the
/// `akiva_app_restore_state` contract.
pub fn on_new_intent(handle: u32, bytes_ptr: *const u8, bytes_len: u32) {
    let Some(slot) = slots().get_mut(&handle) else { return; };
    if bytes_ptr.is_null() || bytes_len == 0 { return; }
    // SAFETY: host guarantees [bytes_ptr, bytes_ptr + bytes_len) is a
    // valid readable region it copied into app memory.
    let slice = unsafe { core::slice::from_raw_parts(bytes_ptr, bytes_len as usize) };
    let intent = match Intent::decode(slice) {
        Ok(i) => i,
        // Malformed blob — can't reach the app's handler meaningfully.
        // Silently drop rather than trap the whole app.
        Err(_) => return,
    };
    let mut ctx = Context {
        metrics: slot.metrics,
        timestamp_ms: 0.0,
        pointer: slot.last_pointer,
        class: "",
        handle,
    };
    block_on_ready(slot.activity.on_new_intent(&mut ctx, &intent));
}

/// Deliver an ActivityResult to the caller. Fired by the framework after
/// the callee (the one this activity launched via
/// `start_activity_for_result`) finishes. A malformed blob is treated
/// as "no result" — silently dropped, same policy as `on_new_intent` and
/// `on_restore_state`.
pub fn on_activity_result(
    handle: u32, request_code: u32, bytes_ptr: *const u8, bytes_len: u32,
) {
    let Some(slot) = slots().get_mut(&handle) else { return; };
    if bytes_ptr.is_null() || bytes_len == 0 { return; }
    let slice = unsafe { core::slice::from_raw_parts(bytes_ptr, bytes_len as usize) };
    let result = match ActivityResult::decode(slice) {
        Ok(r) => r,
        Err(_) => return,
    };
    let mut ctx = Context {
        metrics: slot.metrics,
        timestamp_ms: 0.0,
        pointer: slot.last_pointer,
        class: "",
        handle,
    };
    block_on_ready(slot.activity.on_activity_result(&mut ctx, request_code, &result));
}

pub fn on_restore_state(handle: u32, bytes_ptr: *const u8, bytes_len: u32) {
    let Some(slot) = slots().get_mut(&handle) else { return; };
    if bytes_ptr.is_null() || bytes_len == 0 { return; }
    let slice = unsafe { core::slice::from_raw_parts(bytes_ptr, bytes_len as usize) };
    match SavedState::decode(slice) {
        Ok(state) => slot.activity.on_restore_state(&state),
        // A malformed blob is effectively "no saved state" — the app
        // keeps its default-constructed state. Silently dropping is
        // deliberate: panicking here would trap the wasm and take the
        // whole app down on launch, which is worse than starting fresh.
        Err(_) => {}
    }
}

// ── Wire decoders ─────────────────────────────────────────────────────
//
// These match `pointer_phase_wire` / `key_phase_wire` in framework's
// `wasm_activity.rs`. Unknown values fall back to a safe default rather
// than panicking the whole app — the kernel can't easily recover a wasm
// that trapped.

fn decode_pointer_phase(w: u32) -> PointerPhase {
    match w {
        0 => PointerPhase::Down,
        1 => PointerPhase::Move,
        2 => PointerPhase::Up,
        _ => PointerPhase::Cancel,
    }
}

fn decode_key_phase(w: u32) -> KeyPhase {
    match w {
        0 => KeyPhase::Down,
        1 => KeyPhase::Repeat,
        _ => KeyPhase::Up,
    }
}

fn read_utf8(ptr: *const u8, len: u32) -> String {
    if ptr.is_null() || len == 0 { return String::new(); }
    // SAFETY: the host guarantees [ptr, ptr+len) is a valid region it
    // allocated via `akiva_app_alloc`; we only read. from_utf8_lossy
    // keeps us safe even if the host passed garbage.
    let slice = unsafe { core::slice::from_raw_parts(ptr, len as usize) };
    String::from_utf8_lossy(slice).into_owned()
}
