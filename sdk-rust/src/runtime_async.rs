//! App-side runtime: handle table + ABI-v0 glue (async path).
//!
//! Stone 11.2 parallel to [`crate::runtime`]. Same wire shape — same
//! `extern "C"` exports the macro generates, same slot semantics — but
//! every dispatch awaits the [`AsyncActivity`] future via
//! [`block_on_ready`]. Hidden behind the `akiva_async_app!` macro.
//!
//! While the sync and async paths coexist (during 11.2 migration of the
//! 4 in-tree apps), both modules stand on their own and ship their own
//! static slot table. The unused module's table stays empty — wasm
//! tree-shaking can't eliminate it because the macro pulls the
//! corresponding helpers in via path. We accept the small bss cost
//! while the migration is in flight.

#![doc(hidden)]

use std::collections::HashMap;

use crate::activity::{Context, KeyEvent, KeyPhase, Metrics, PointerEvent, PointerPhase, SavedState};
use crate::activity_async::{block_on_ready, AsyncActivity};
use crate::intent::Intent;
use crate::result::ActivityResult;

/// Pre-allocated paint buffer per slot — matches [`crate::runtime`] so
/// the wire-level lifetime contract for `akiva_app_render` is identical
/// (pointer stays valid until the next render for the same handle).
const INITIAL_PAINT_CAP: usize = 4 * 1024;

struct Slot {
    activity: Box<dyn AsyncActivity>,
    paint_buf: Vec<u8>,
    save_buf: Vec<u8>,
    metrics: Metrics,
    last_pointer: Option<PointerEvent>,
}

static mut SLOTS: Option<HashMap<u32, Slot>> = None;

#[inline]
fn slots() -> &'static mut HashMap<u32, Slot> {
    // SAFETY: wasm in a worker is single-threaded; host exports are
    // invoked serially. Same justification as crate::runtime::slots().
    unsafe {
        let ptr = core::ptr::addr_of_mut!(SLOTS);
        if (*ptr).is_none() {
            *ptr = Some(HashMap::new());
        }
        (*ptr).as_mut().unwrap()
    }
}

// ── Lifecycle entry points ────────────────────────────────────────────

pub fn on_create<T: AsyncActivity + Default>(handle: u32, class_ptr: *const u8, class_len: u32) {
    let class = read_utf8(class_ptr, class_len);
    let mut activity: Box<dyn AsyncActivity> = Box::new(T::default());
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
    f: impl FnOnce(&mut dyn AsyncActivity, &mut Context<'_>),
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
        // SAFETY: same caller contract as crate::runtime::on_render — host
        // passes a writable u32 cell on each call.
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

pub fn on_new_intent(handle: u32, bytes_ptr: *const u8, bytes_len: u32) {
    let Some(slot) = slots().get_mut(&handle) else { return; };
    if bytes_ptr.is_null() || bytes_len == 0 { return; }
    // SAFETY: host guarantees [bytes_ptr, bytes_ptr + bytes_len) is a
    // valid readable region in app memory.
    let slice = unsafe { core::slice::from_raw_parts(bytes_ptr, bytes_len as usize) };
    let intent = match Intent::decode(slice) {
        Ok(i) => i,
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
        Err(_) => {}
    }
}

// ── Wire decoders ─────────────────────────────────────────────────────

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
    // SAFETY: same caller contract as crate::runtime::read_utf8.
    let slice = unsafe { core::slice::from_raw_parts(ptr, len as usize) };
    String::from_utf8_lossy(slice).into_owned()
}
