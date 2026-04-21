//! App-side runtime: handle table + ABI-v0 glue.
//!
//! Hidden implementation behind the `akiva_app!` macro. App authors never
//! call into this module directly — the macro generates `#[no_mangle]`
//! `extern "C"` exports that funnel through `on_*` helpers here, which
//! then dispatch to the user's `Activity` impl.
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

use crate::activity::{Activity, Context, KeyEvent, KeyPhase, Metrics, PointerEvent, PointerPhase};

// Host-provided imports. Matches `AppRegistry._buildAppImports` in
// runtime/web/app_registry.js. Gated on wasm32 so non-wasm builds
// (cargo test) link without needing to provide the symbols — the
// corresponding helper functions below fall back to in-process
// approximations when the host isn't available.
#[cfg(target_arch = "wasm32")]
extern "C" {
    pub fn akiva_host_log(level: u32, ptr: *const u8, len: u32);
    pub fn akiva_host_text_width(ptr: *const u8, len: u32, size: f32) -> f32;
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
// They look up the slot, rebuild a `Context`, and invoke the trait method.

pub fn on_create<T: Activity + Default>(handle: u32, class_ptr: *const u8, class_len: u32) {
    let class = read_utf8(class_ptr, class_len);
    let mut activity: Box<dyn Activity> = Box::new(T::default());
    let mut ctx = Context {
        metrics: Metrics::default(),
        timestamp_ms: 0.0,
        pointer: None,
        class: class.as_str(),
    };
    activity.on_create(&mut ctx);
    slots().insert(handle, Slot {
        activity,
        paint_buf: Vec::with_capacity(INITIAL_PAINT_CAP),
        metrics: Metrics::default(),
        last_pointer: None,
    });
}

pub fn on_start(handle: u32)   { dispatch_lifecycle(handle, |a, c| a.on_start(c)); }
pub fn on_resume(handle: u32)  { dispatch_lifecycle(handle, |a, c| a.on_resume(c)); }
pub fn on_pause(handle: u32)   { dispatch_lifecycle(handle, |a, c| a.on_pause(c)); }
pub fn on_stop(handle: u32)    { dispatch_lifecycle(handle, |a, c| a.on_stop(c)); }

pub fn on_destroy(handle: u32) {
    if let Some(mut slot) = slots().remove(&handle) {
        let mut ctx = Context {
            metrics: slot.metrics,
            timestamp_ms: 0.0,
            pointer: slot.last_pointer,
            class: "",
        };
        slot.activity.on_destroy(&mut ctx);
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
    };
    slot.activity.on_pointer(&mut ctx, ev);
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
    };
    slot.activity.on_key(&mut ctx, ev);
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
    };
    slot.activity.on_resize(&mut ctx, metrics);
}

// ── Render ────────────────────────────────────────────────────────────

pub fn on_render(handle: u32, ts_ms: f64, out_len_ptr: *mut u32) -> *const u8 {
    let Some(slot) = slots().get_mut(&handle) else {
        unsafe { *out_len_ptr = 0; }
        return core::ptr::null();
    };
    slot.paint_buf.clear();
    let mut ctx = Context {
        metrics: slot.metrics,
        timestamp_ms: ts_ms,
        pointer: slot.last_pointer,
        class: "",
    };
    let mut canvas = crate::canvas::Canvas::new(&mut slot.paint_buf);
    slot.activity.on_render(&mut ctx, &mut canvas);
    let len = slot.paint_buf.len() as u32;
    let ptr = slot.paint_buf.as_ptr();
    unsafe { *out_len_ptr = len; }
    ptr
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
