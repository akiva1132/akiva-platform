//! Wasm activity proxy.
//!
//! This is the bridge between the in-kernel [`Activity`] trait and app
//! wasm modules that the host (worker.js) instantiates alongside the
//! kernel. The kernel itself can't call into an app's wasm module — only
//! the host can — so every lifecycle/input/render call is forwarded
//! through a [`WasmHost`] abstraction whose concrete implementation in
//! the runtime calls `hal_app_*` imports. Tests inject a mock host.
//!
//! See [docs/abi.md §2.1](../../docs/abi.md) for the raw-export ABI that
//! apps speak to the host. This module only deals with the kernel⇄host
//! seam; the host⇄app seam lives entirely in JS.
//!
//! ## Paint-op wire format
//!
//! Apps can't hand a [`FrameCommands`] directly to the kernel — they live
//! in different wasm instances, different address spaces. Each frame the
//! app serialises its ops into a flat little-endian byte stream which the
//! host copies into kernel memory, and [`decode_frame`] rehydrates it
//! into the kernel's `FrameCommands`. The encoding is intentionally
//! trivial: a variant tag followed by packed primitives. Variable-length
//! strings are `u32` length + UTF-8.

use std::rc::Rc;
use core::num::NonZeroU32;

use crate::activity::{Activity, ActivityContext, KeyEvent, PointerEvent, PointerPhase, KeyPhase, SurfaceMetrics};
use crate::compositor::{FrameCommands, PaintOp};
use crate::intent::Intent;
use crate::result::ActivityResult;

// ── Types ────────────────────────────────────────────────────────────────

/// Handle the host issues for one app activity instance. `NonZeroU32` so
/// `Option<AppHandle>` is niche-packed to the same size as `u32` — the
/// host reserves `0` for "no such app".
pub type AppHandle = NonZeroU32;

/// Mirror of the kernel-side lifecycle transitions, but flattened to a
/// `u32` wire value so the extern call has a stable ABI and a mock host
/// doesn't need to know the framework's `LifecycleState` enum.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum LifecycleTransition {
    Create  = 0,
    Start   = 1,
    Resume  = 2,
    Pause   = 3,
    Stop    = 4,
    Destroy = 5,
}

/// Abstract handle the framework uses to drive a hosted app. Implemented
/// by `KernelWasmHost` (real, calls `hal_app_*`) and by test mocks.
///
/// All methods take `&self` because the real implementation is a ZST and
/// kernel wasm is single-threaded; mocks use interior mutability if they
/// need to record calls.
pub trait WasmHost {
    fn register(&self, package_id: &str, class: &str) -> Option<AppHandle>;
    fn unregister(&self, handle: AppHandle);

    fn lifecycle(&self, handle: AppHandle, transition: LifecycleTransition);
    fn pointer(&self, handle: AppHandle, ev: PointerEvent, ts_ms: f64);
    fn key(&self, handle: AppHandle, ev: KeyEvent, ts_ms: f64);
    fn resize(&self, handle: AppHandle, metrics: SurfaceMetrics);

    /// Ask the app to paint one frame. The host is responsible for
    /// fetching the app's encoded paint-op bytes and appending the
    /// decoded ops to `out`.
    fn render(&self, handle: AppHandle, ts_ms: f64, out: &mut FrameCommands);

    /// Ask the app to serialise its per-activity UI state for a possible
    /// process kill. Returned bytes are in the SDK's `SavedState` wire
    /// format — the framework treats them as opaque. An empty vec means
    /// "nothing to save" and the framework will skip the storage write.
    ///
    /// Default returns empty so in-process activities (e.g. Shell) can
    /// keep ignoring the hook.
    fn save_state(&self, handle: AppHandle) -> Vec<u8> {
        let _ = handle;
        Vec::new()
    }

    /// Hand previously-saved bytes back to the app. Called after
    /// `lifecycle(Create)` and before `lifecycle(Start)` so the app sees
    /// its restored state in `on_start`/`on_resume` just like a fresh
    /// launch. Empty bytes = "no saved state", skipped by the framework.
    ///
    /// Default is a no-op for the same reason as `save_state`.
    fn restore_state(&self, handle: AppHandle, bytes: &[u8]) {
        let _ = (handle, bytes);
    }

    /// Pop any "finish requested" signal raised by the app during the
    /// previous host→app call. The app sets it by calling the
    /// `akiva_host_finish` import from inside a lifecycle or input
    /// callback ([`docs/abi.md §2.3`](../../docs/abi.md)). The real host
    /// tracks one flag per handle and clears it on read; tests default
    /// to `false` unless they want to simulate app-initiated finish.
    ///
    /// Called by [`WasmActivity`] at the end of every method that carries
    /// an `ActivityContext`, so a set flag propagates into the framework
    /// dispatch exactly as if an in-kernel activity (e.g. Shell) had
    /// written `*ctx.finish_requested = true` directly.
    fn drain_finish(&self, handle: AppHandle) -> bool {
        let _ = handle;
        false
    }

    /// Pop any pending intent blob raised by the app via
    /// `akiva_host_start_activity`. Same single-shot semantic as
    /// `drain_finish` — the host clears its per-handle slot on read.
    /// The bytes are the SDK's encoded `Intent` (see
    /// `akiva_sdk::intent::Intent::encode`); `WasmActivity` decodes them
    /// into `framework::intent::Intent` and feeds them into
    /// `ctx.launch_requested`. Default `None` keeps in-process hosts
    /// opt-out. See [`docs/abi.md §2.3`](../../docs/abi.md).
    fn drain_intent(&self, handle: AppHandle) -> Option<Vec<u8>> {
        let _ = handle;
        None
    }

    /// Deliver an already-resolved intent to an existing activity handle
    /// — fires `akiva_app_new_intent(handle, ptr, len)` on the real
    /// host. Called when `ActivityManager::launch` picks a reuse path
    /// (SingleTop; and Phase 9.2's SingleTask/SingleInstance) instead
    /// of stamping out a fresh instance. Default no-op so in-kernel
    /// hosts (Shell) that don't expose a new-intent path stay quiet.
    fn new_intent(&self, handle: AppHandle, bytes: &[u8]) {
        let _ = (handle, bytes);
    }

    /// Stone 9.5 — pop any pending `(intent_bytes, request_code)` the app
    /// raised via `akiva_host_start_activity_for_result`. Same
    /// drain-at-callback-end semantic as `drain_intent`. `intent_bytes`
    /// is the SDK's encoded `Intent`; the caller decodes. Default `None`
    /// keeps in-kernel activities opt-out.
    fn drain_start_for_result(&self, handle: AppHandle) -> Option<(Vec<u8>, u32)> {
        let _ = handle;
        None
    }

    /// Stone 9.5 — pop any bytes the app stashed via
    /// `akiva_host_set_result`. Drained on the destroy-path of the
    /// callee (not every callback) — more like a pull than a signal.
    /// Bytes are the SDK's encoded [`ActivityResult`]. Default `None`.
    fn drain_set_result(&self, handle: AppHandle) -> Option<Vec<u8>> {
        let _ = handle;
        None
    }

    /// Stone 9.5 — deliver an `ActivityResult` to the caller activity.
    /// Fires `akiva_app_activity_result(handle, request_code, ptr, len)`
    /// on the real host. Default no-op for activities that don't launch
    /// for-result.
    fn activity_result(
        &self,
        handle: AppHandle,
        request_code: u32,
        bytes: &[u8],
    ) {
        let _ = (handle, request_code, bytes);
    }

    /// Stone 10.3b — push a package's granted-permission list to the
    /// host so it can filter the `akiva_host_*` import map at
    /// instantiation time (soft enforcement via stub replacement; see
    /// [`docs/abi.md §2.3.1`](../../docs/abi.md)).
    ///
    /// Called once per package from `framework::boot` after
    /// `PackageManager::load_granted_permissions`, before the first
    /// activity is launched — so the host has the grant list on hand
    /// the first time `hal_app_register` triggers `_instantiateProcess`.
    /// On upgrade via `PackageManager::install`, the framework issues a
    /// second call with the new full set; the host applies last-write-
    /// wins semantics.
    ///
    /// Default is a no-op so in-kernel hosts (Shell / test harnesses)
    /// and MockHost can ignore the push without breaking.
    fn set_grants(&self, package_id: &str, grants: &[&str]) {
        let _ = (package_id, grants);
    }
}

// ── Proxy activity ───────────────────────────────────────────────────────

/// Framework-side [`Activity`] that forwards every callback to a hosted
/// wasm app via a [`WasmHost`]. Owns the `AppHandle` for its lifetime and
/// releases it in `on_destroy`. Carries an `Rc<dyn WasmHost>` so multiple
/// activities in the same app package can share one host (e.g. Shell's
/// host instance and the foreground app's host instance may be different
/// objects, but two activities of the same package share).
pub struct WasmActivity {
    handle: AppHandle,
    host: Rc<dyn WasmHost>,
}

impl WasmActivity {
    pub fn new(handle: AppHandle, host: Rc<dyn WasmHost>) -> Self {
        Self { handle, host }
    }

    pub fn handle(&self) -> AppHandle { self.handle }

    /// Pop any app-raised post-callback signals for this handle and
    /// propagate them into the active dispatch's [`ActivityContext`].
    /// Called at the end of every callback that carries a ctx — a single
    /// choke point so a new lifecycle method can't silently skip it.
    /// `on_destroy` does NOT call this (destroy is already terminal;
    /// post-callback requests are moot there).
    ///
    /// A malformed `Intent` blob from the host is dropped silently — the
    /// wasm app is the only code that could have produced it, and
    /// panicking mid-dispatch would trap the whole app on what's almost
    /// certainly a version skew. The request surfaces as "start_activity
    /// did nothing" to the app, which is the same outcome as an
    /// unresolvable intent (`apply_post` drops those too).
    fn drain_post(&self, ctx: &mut ActivityContext<'_>) {
        if self.host.drain_finish(self.handle) {
            *ctx.finish_requested = true;
        }
        if let Some(bytes) = self.host.drain_intent(self.handle) {
            if let Ok(intent) = Intent::decode(&bytes) {
                *ctx.launch_requested = Some(intent);
            }
        }
        if let Some((bytes, request_code)) = self.host.drain_start_for_result(self.handle) {
            if let Ok(intent) = Intent::decode(&bytes) {
                *ctx.start_for_result_requested = Some((intent, request_code));
            }
        }
        if let Some(bytes) = self.host.drain_set_result(self.handle) {
            if let Ok(result) = ActivityResult::decode(&bytes) {
                *ctx.set_result_requested = Some(result);
            }
        }
    }
}

impl Activity for WasmActivity {
    fn on_create(&mut self, ctx: &mut ActivityContext<'_>) {
        self.host.lifecycle(self.handle, LifecycleTransition::Create);
        // Prime the app's slot with the live surface metrics right after
        // construction. The SDK's `on_create` builds the slot with
        // `Metrics::default()` (zeros) because the `lifecycle(Create)`
        // wire doesn't carry a surface size; without this follow-up
        // resize, the first `on_render` would run with zero metrics and
        // lay out the whole tree at the origin — manifesting as a
        // "tiny calculator in the top-left until the user resizes the
        // window". One extra round-trip here is worth a much saner
        // first frame.
        self.host.resize(self.handle, ctx.metrics);
        self.drain_post(ctx);
    }
    fn on_start(&mut self, ctx: &mut ActivityContext<'_>) {
        self.host.lifecycle(self.handle, LifecycleTransition::Start);
        self.drain_post(ctx);
    }
    fn on_resume(&mut self, ctx: &mut ActivityContext<'_>) {
        self.host.lifecycle(self.handle, LifecycleTransition::Resume);
        self.drain_post(ctx);
    }
    fn on_pause(&mut self, ctx: &mut ActivityContext<'_>) {
        self.host.lifecycle(self.handle, LifecycleTransition::Pause);
        self.drain_post(ctx);
    }
    fn on_stop(&mut self, ctx: &mut ActivityContext<'_>) {
        self.host.lifecycle(self.handle, LifecycleTransition::Stop);
        self.drain_post(ctx);
    }
    fn on_destroy(&mut self, _ctx: &mut ActivityContext<'_>) {
        // No drain_post — destroy is terminal, a finish flag here would
        // be nonsense anyway (can't finish a destroying activity).
        self.host.lifecycle(self.handle, LifecycleTransition::Destroy);
        self.host.unregister(self.handle);
    }

    fn on_pointer(&mut self, ctx: &mut ActivityContext<'_>, ev: PointerEvent) {
        self.host.pointer(self.handle, ev, ctx.timestamp_ms);
        self.drain_post(ctx);
    }
    fn on_key(&mut self, ctx: &mut ActivityContext<'_>, ev: KeyEvent) {
        self.host.key(self.handle, ev, ctx.timestamp_ms);
        self.drain_post(ctx);
    }
    fn on_resize(&mut self, ctx: &mut ActivityContext<'_>, m: SurfaceMetrics) {
        self.host.resize(self.handle, m);
        self.drain_post(ctx);
    }

    fn render(&mut self, ctx: &mut ActivityContext<'_>, out: &mut FrameCommands) {
        self.host.render(self.handle, ctx.timestamp_ms, out);
    }

    fn save_state_bytes(&self) -> Vec<u8> {
        self.host.save_state(self.handle)
    }
    fn restore_state_bytes(&mut self, bytes: &[u8]) {
        self.host.restore_state(self.handle, bytes);
    }

    fn on_new_intent(&mut self, ctx: &mut ActivityContext<'_>, intent: &Intent) {
        // Re-encode rather than passing the original bytes through —
        // `Framework` doesn't retain the wire buffer past `apply_post`,
        // and the cost is trivial compared to the wasm call that
        // follows. Keeps the boundary symmetric: framework side owns a
        // decoded `Intent`, wasm side receives an encoded blob.
        let bytes = intent.encode();
        self.host.new_intent(self.handle, &bytes);
        self.drain_post(ctx);
    }

    fn on_activity_result(
        &mut self,
        ctx: &mut ActivityContext<'_>,
        request_code: u32,
        result: &ActivityResult,
    ) {
        // Same symmetry point as `on_new_intent`: framework decodes,
        // re-encodes for the wasm boundary. The app's on_activity_result
        // could itself ask to launch/finish further — drain after the
        // call so those post-callback requests reach apply_post.
        let bytes = result.encode();
        self.host.activity_result(self.handle, request_code, &bytes);
        self.drain_post(ctx);
    }
}

// ── Paint-op wire codec ──────────────────────────────────────────────────
//
// Opcodes are stable: additions are additive-only. Decoder is tolerant of
// unknown opcodes at the end of the stream (future paint extensions) but
// treats them as a decode error mid-stream, because the length of their
// payload is unknown and skipping would desync the reader.

pub mod opcode {
    pub const CLEAR:           u8 = 0;
    pub const FILL_RECT:       u8 = 1;
    pub const DRAW_TEXT:       u8 = 2;
    pub const PUSH_CLIP:       u8 = 3;
    pub const POP_CLIP:        u8 = 4;
    pub const PUSH_TRANSFORM:  u8 = 5;
    pub const POP_TRANSFORM:   u8 = 6;
}

#[derive(Debug, PartialEq, Eq)]
pub enum DecodeError {
    /// Stream ended mid-op.
    Truncated,
    /// Opcode we don't recognise. Payload size unknown, can't resync.
    UnknownOp(u8),
    /// `DrawText` carried a length that doesn't fit the remaining bytes.
    BadLength,
    /// UTF-8 validation failed on a text payload.
    BadUtf8,
}

/// Serialise a single `PaintOp`. Exposed so the SDK (and tests) can
/// produce matching bytes. The framework itself never calls this — it
/// only decodes.
pub fn encode_op(out: &mut Vec<u8>, op: &PaintOp) {
    match op {
        PaintOp::Clear { rgba } => {
            out.push(opcode::CLEAR);
            out.extend_from_slice(&rgba.to_le_bytes());
        }
        PaintOp::FillRect { x, y, w, h, rgba } => {
            out.push(opcode::FILL_RECT);
            out.extend_from_slice(&x.to_le_bytes());
            out.extend_from_slice(&y.to_le_bytes());
            out.extend_from_slice(&w.to_le_bytes());
            out.extend_from_slice(&h.to_le_bytes());
            out.extend_from_slice(&rgba.to_le_bytes());
        }
        PaintOp::DrawText { x, y, size, rgba, text } => {
            out.push(opcode::DRAW_TEXT);
            out.extend_from_slice(&x.to_le_bytes());
            out.extend_from_slice(&y.to_le_bytes());
            out.extend_from_slice(&size.to_le_bytes());
            out.extend_from_slice(&rgba.to_le_bytes());
            let bytes = text.as_bytes();
            out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
            out.extend_from_slice(bytes);
        }
        PaintOp::PushClip { x, y, w, h } => {
            out.push(opcode::PUSH_CLIP);
            out.extend_from_slice(&x.to_le_bytes());
            out.extend_from_slice(&y.to_le_bytes());
            out.extend_from_slice(&w.to_le_bytes());
            out.extend_from_slice(&h.to_le_bytes());
        }
        PaintOp::PopClip => out.push(opcode::POP_CLIP),
        PaintOp::PushTransform { dx, dy, scale } => {
            out.push(opcode::PUSH_TRANSFORM);
            out.extend_from_slice(&dx.to_le_bytes());
            out.extend_from_slice(&dy.to_le_bytes());
            out.extend_from_slice(&scale.to_le_bytes());
        }
        PaintOp::PopTransform => out.push(opcode::POP_TRANSFORM),
    }
}

/// Decode a flat paint stream into `out`. Appends — does not clear — so
/// callers can splice app frames into a larger composed frame.
pub fn decode_frame(mut bytes: &[u8], out: &mut FrameCommands) -> Result<(), DecodeError> {
    while !bytes.is_empty() {
        let op = bytes[0];
        bytes = &bytes[1..];
        match op {
            opcode::CLEAR => {
                let rgba = read_u32(&mut bytes)?;
                out.push(PaintOp::Clear { rgba });
            }
            opcode::FILL_RECT => {
                let x = read_f32(&mut bytes)?;
                let y = read_f32(&mut bytes)?;
                let w = read_f32(&mut bytes)?;
                let h = read_f32(&mut bytes)?;
                let rgba = read_u32(&mut bytes)?;
                out.push(PaintOp::FillRect { x, y, w, h, rgba });
            }
            opcode::DRAW_TEXT => {
                let x = read_f32(&mut bytes)?;
                let y = read_f32(&mut bytes)?;
                let size = read_f32(&mut bytes)?;
                let rgba = read_u32(&mut bytes)?;
                let len = read_u32(&mut bytes)? as usize;
                if len > bytes.len() { return Err(DecodeError::BadLength); }
                let (text_bytes, rest) = bytes.split_at(len);
                bytes = rest;
                let text = core::str::from_utf8(text_bytes)
                    .map_err(|_| DecodeError::BadUtf8)?
                    .to_string();
                out.push(PaintOp::DrawText { x, y, size, rgba, text });
            }
            opcode::PUSH_CLIP => {
                let x = read_f32(&mut bytes)?;
                let y = read_f32(&mut bytes)?;
                let w = read_f32(&mut bytes)?;
                let h = read_f32(&mut bytes)?;
                out.push(PaintOp::PushClip { x, y, w, h });
            }
            opcode::POP_CLIP => out.push(PaintOp::PopClip),
            opcode::PUSH_TRANSFORM => {
                let dx = read_f32(&mut bytes)?;
                let dy = read_f32(&mut bytes)?;
                let scale = read_f32(&mut bytes)?;
                out.push(PaintOp::PushTransform { dx, dy, scale });
            }
            opcode::POP_TRANSFORM => out.push(PaintOp::PopTransform),
            other => return Err(DecodeError::UnknownOp(other)),
        }
    }
    Ok(())
}

fn read_u32(bytes: &mut &[u8]) -> Result<u32, DecodeError> {
    if bytes.len() < 4 { return Err(DecodeError::Truncated); }
    let (head, tail) = bytes.split_at(4);
    *bytes = tail;
    Ok(u32::from_le_bytes([head[0], head[1], head[2], head[3]]))
}

fn read_f32(bytes: &mut &[u8]) -> Result<f32, DecodeError> {
    Ok(f32::from_bits(read_u32(bytes)?))
}

// ── Phase/transition helpers for the real kernel adapter ─────────────────
//
// Kept here (rather than in kernel/) so both sides of the ABI are in the
// same place. The kernel's extern signatures take primitive ints so these
// free functions turn the framework-side enums into wire values.

pub fn pointer_phase_wire(p: PointerPhase) -> u32 {
    match p {
        PointerPhase::Down   => 0,
        PointerPhase::Move   => 1,
        PointerPhase::Up     => 2,
        PointerPhase::Cancel => 3,
    }
}

pub fn key_phase_wire(p: KeyPhase) -> u32 {
    match p {
        KeyPhase::Down   => 0,
        KeyPhase::Repeat => 1,
        KeyPhase::Up     => 2,
    }
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use core::cell::RefCell;

    #[derive(Debug, Clone, PartialEq)]
    enum Call {
        Register(String, String),
        Unregister(u32),
        Lifecycle(u32, LifecycleTransition),
        Pointer(u32, f32, f32, f64),
        Key(u32, u32, f64),
        Resize(u32, u32, u32),
        Render(u32, f64),
        SaveState(u32),
        RestoreState(u32, Vec<u8>),
        NewIntent(u32, Vec<u8>),
        ActivityResult(u32, u32, Vec<u8>),
        SetGrants(String, Vec<String>),
    }

    /// Mock that records every call and lets tests pre-seed encoded paint
    /// bytes that the next `render()` call will decode into `out`.
    struct MockHost {
        calls: RefCell<Vec<Call>>,
        next_handle: RefCell<u32>,
        paint_bytes: RefCell<Vec<u8>>,
        save_bytes: RefCell<Vec<u8>>,
        /// When `true`, the next call to `drain_finish` returns `true`
        /// once then resets to `false` — mirrors the real host's
        /// single-shot flag semantic.
        pending_finish: RefCell<bool>,
        /// Stone 9.4 — pending intent blob. Read-and-clear, matching
        /// `AppRegistry.drainIntent` semantics.
        pending_intent: RefCell<Option<Vec<u8>>>,
        /// Stone 9.5 — pending `(intent_bytes, request_code)` for
        /// `start_activity_for_result`. Read-and-clear.
        pending_start_for_result: RefCell<Option<(Vec<u8>, u32)>>,
        /// Stone 9.5 — pending set_result blob. Read-and-clear.
        pending_set_result: RefCell<Option<Vec<u8>>>,
    }

    impl MockHost {
        fn new() -> Self {
            Self {
                calls: RefCell::new(Vec::new()),
                next_handle: RefCell::new(1),
                paint_bytes: RefCell::new(Vec::new()),
                save_bytes: RefCell::new(Vec::new()),
                pending_finish: RefCell::new(false),
                pending_intent: RefCell::new(None),
                pending_start_for_result: RefCell::new(None),
                pending_set_result: RefCell::new(None),
            }
        }
        fn take_calls(&self) -> Vec<Call> { core::mem::take(&mut *self.calls.borrow_mut()) }
        fn set_paint_bytes(&self, bytes: Vec<u8>) { *self.paint_bytes.borrow_mut() = bytes; }
        fn set_save_bytes(&self, bytes: Vec<u8>) { *self.save_bytes.borrow_mut() = bytes; }
        fn arm_finish(&self) { *self.pending_finish.borrow_mut() = true; }
        fn arm_intent(&self, bytes: Vec<u8>) { *self.pending_intent.borrow_mut() = Some(bytes); }
        fn arm_start_for_result(&self, bytes: Vec<u8>, request_code: u32) {
            *self.pending_start_for_result.borrow_mut() = Some((bytes, request_code));
        }
        fn arm_set_result(&self, bytes: Vec<u8>) {
            *self.pending_set_result.borrow_mut() = Some(bytes);
        }
    }

    impl WasmHost for MockHost {
        fn register(&self, package_id: &str, class: &str) -> Option<AppHandle> {
            self.calls.borrow_mut().push(Call::Register(package_id.into(), class.into()));
            let mut n = self.next_handle.borrow_mut();
            let h = *n; *n += 1;
            AppHandle::new(h)
        }
        fn unregister(&self, handle: AppHandle) {
            self.calls.borrow_mut().push(Call::Unregister(handle.get()));
        }
        fn lifecycle(&self, handle: AppHandle, t: LifecycleTransition) {
            self.calls.borrow_mut().push(Call::Lifecycle(handle.get(), t));
        }
        fn pointer(&self, handle: AppHandle, ev: PointerEvent, ts: f64) {
            self.calls.borrow_mut().push(Call::Pointer(handle.get(), ev.x, ev.y, ts));
        }
        fn key(&self, handle: AppHandle, ev: KeyEvent, ts: f64) {
            self.calls.borrow_mut().push(Call::Key(handle.get(), ev.code, ts));
        }
        fn resize(&self, handle: AppHandle, m: SurfaceMetrics) {
            self.calls.borrow_mut().push(Call::Resize(handle.get(), m.width_px, m.height_px));
        }
        fn render(&self, handle: AppHandle, ts: f64, out: &mut FrameCommands) {
            self.calls.borrow_mut().push(Call::Render(handle.get(), ts));
            let bytes = self.paint_bytes.borrow();
            decode_frame(&bytes, out).expect("mock paint bytes decoded");
        }
        fn save_state(&self, handle: AppHandle) -> Vec<u8> {
            self.calls.borrow_mut().push(Call::SaveState(handle.get()));
            self.save_bytes.borrow().clone()
        }
        fn restore_state(&self, handle: AppHandle, bytes: &[u8]) {
            self.calls.borrow_mut().push(Call::RestoreState(handle.get(), bytes.to_vec()));
        }
        fn drain_finish(&self, _handle: AppHandle) -> bool {
            // Read-and-clear, matching AppRegistry.drainFinish in the JS.
            let mut flag = self.pending_finish.borrow_mut();
            let was = *flag;
            *flag = false;
            was
        }
        fn drain_intent(&self, _handle: AppHandle) -> Option<Vec<u8>> {
            self.pending_intent.borrow_mut().take()
        }
        fn new_intent(&self, handle: AppHandle, bytes: &[u8]) {
            self.calls.borrow_mut().push(Call::NewIntent(handle.get(), bytes.to_vec()));
        }
        fn drain_start_for_result(&self, _handle: AppHandle) -> Option<(Vec<u8>, u32)> {
            self.pending_start_for_result.borrow_mut().take()
        }
        fn drain_set_result(&self, _handle: AppHandle) -> Option<Vec<u8>> {
            self.pending_set_result.borrow_mut().take()
        }
        fn activity_result(&self, handle: AppHandle, request_code: u32, bytes: &[u8]) {
            self.calls.borrow_mut().push(
                Call::ActivityResult(handle.get(), request_code, bytes.to_vec()),
            );
        }
        fn set_grants(&self, package_id: &str, grants: &[&str]) {
            self.calls.borrow_mut().push(Call::SetGrants(
                package_id.into(),
                grants.iter().map(|s| (*s).to_string()).collect(),
            ));
        }
    }

    /// Owning wrapper for the four ctx out-params so each test only needs
    /// one local, not four. The old `make_ctx(&mut finish, &mut launch)`
    /// shape worked with two borrows but wouldn't extend cleanly to the
    /// 9.5 additions (the `&mut None` temporaries die at statement end).
    #[derive(Default)]
    struct CtxVars {
        finish: bool,
        launch: Option<crate::intent::Intent>,
        launch_for_result: Option<(crate::intent::Intent, u32)>,
        set_result: Option<ActivityResult>,
    }
    impl CtxVars {
        fn ctx(&mut self) -> ActivityContext<'_> {
            ActivityContext {
                metrics: SurfaceMetrics { width_px: 800, height_px: 600, density: 1.0 },
                timestamp_ms: 1234.0,
                pointer: None,
                finish_requested: &mut self.finish,
                launch_requested: &mut self.launch,
                start_for_result_requested: &mut self.launch_for_result,
                set_result_requested: &mut self.set_result,
            }
        }
    }

    #[test]
    fn lifecycle_transitions_forwarded_in_order() {
        let host = Rc::new(MockHost::new());
        let handle = host.register("com.test", "Main").unwrap();
        host.take_calls(); // drop register
        let mut act = WasmActivity::new(handle, host.clone());
        let mut vars = CtxVars::default();
        let mut ctx = vars.ctx();
        act.on_create(&mut ctx);
        act.on_start(&mut ctx);
        act.on_resume(&mut ctx);
        act.on_pause(&mut ctx);
        act.on_stop(&mut ctx);
        act.on_destroy(&mut ctx);
        let calls = host.take_calls();
        assert_eq!(calls, vec![
            Call::Lifecycle(handle.get(), LifecycleTransition::Create),
            // WasmActivity::on_create fires a resize immediately after
            // lifecycle(Create) so the SDK's slot picks up real surface
            // metrics before the first on_render runs — ctx above is
            // built with (800 × 600 @ 1.0×).
            Call::Resize(handle.get(), 800, 600),
            Call::Lifecycle(handle.get(), LifecycleTransition::Start),
            Call::Lifecycle(handle.get(), LifecycleTransition::Resume),
            Call::Lifecycle(handle.get(), LifecycleTransition::Pause),
            Call::Lifecycle(handle.get(), LifecycleTransition::Stop),
            Call::Lifecycle(handle.get(), LifecycleTransition::Destroy),
            Call::Unregister(handle.get()),
        ]);
    }

    #[test]
    fn pointer_event_carries_timestamp() {
        let host = Rc::new(MockHost::new());
        let handle = host.register("p", "c").unwrap();
        host.take_calls();
        let mut act = WasmActivity::new(handle, host.clone());
        let mut vars = CtxVars::default();
        let mut ctx = vars.ctx();
        ctx.timestamp_ms = 42.5;
        act.on_pointer(&mut ctx, PointerEvent { x: 10.0, y: 20.0, button: 0, phase: PointerPhase::Down });
        assert_eq!(host.take_calls(), vec![Call::Pointer(handle.get(), 10.0, 20.0, 42.5)]);
    }

    #[test]
    fn render_decodes_host_bytes_into_frame() {
        let host = Rc::new(MockHost::new());
        let handle = host.register("p", "c").unwrap();
        host.take_calls();

        // Build a tiny paint stream: Clear black, FillRect, DrawText.
        let mut wire = Vec::new();
        encode_op(&mut wire, &PaintOp::Clear { rgba: 0x000000ff });
        encode_op(&mut wire, &PaintOp::FillRect { x: 1.0, y: 2.0, w: 3.0, h: 4.0, rgba: 0xff0000ff });
        encode_op(&mut wire, &PaintOp::DrawText { x: 5.0, y: 6.0, size: 16.0, rgba: 0xffffffff, text: "hi".into() });
        host.set_paint_bytes(wire);

        let mut act = WasmActivity::new(handle, host.clone());
        let mut vars = CtxVars::default();
        let mut ctx = vars.ctx();
        let mut frame = FrameCommands::new();
        act.render(&mut ctx, &mut frame);

        assert_eq!(host.take_calls(), vec![Call::Render(handle.get(), ctx.timestamp_ms)]);
        assert_eq!(frame.ops().len(), 3);
        assert!(matches!(frame.ops()[0], PaintOp::Clear { rgba: 0x000000ff }));
        assert!(matches!(frame.ops()[1], PaintOp::FillRect { x: 1.0, y: 2.0, .. }));
        match &frame.ops()[2] {
            PaintOp::DrawText { text, .. } => assert_eq!(text, "hi"),
            other => panic!("expected DrawText, got {:?}", other),
        }
    }

    #[test]
    fn decode_roundtrips_every_op_variant() {
        let inputs = vec![
            PaintOp::Clear { rgba: 0x11223344 },
            PaintOp::FillRect { x: 1.5, y: 2.5, w: 3.5, h: 4.5, rgba: 0xdeadbeef },
            PaintOp::DrawText { x: 0.0, y: 1.0, size: 14.0, rgba: 0xabcdef01, text: "Hello, שלום".into() },
            PaintOp::PushClip { x: 0.0, y: 0.0, w: 100.0, h: 100.0 },
            PaintOp::PopClip,
            PaintOp::PushTransform { dx: 10.0, dy: 20.0, scale: 1.25 },
            PaintOp::PopTransform,
        ];
        let mut wire = Vec::new();
        for op in &inputs { encode_op(&mut wire, op); }

        let mut frame = FrameCommands::new();
        decode_frame(&wire, &mut frame).unwrap();
        assert_eq!(frame.ops().len(), inputs.len());
    }

    #[test]
    fn decode_rejects_truncated_stream() {
        // FillRect opcode + only 4 bytes of payload (need 20).
        let wire = vec![opcode::FILL_RECT, 0, 0, 0, 0];
        let mut frame = FrameCommands::new();
        assert_eq!(decode_frame(&wire, &mut frame), Err(DecodeError::Truncated));
    }

    #[test]
    fn decode_rejects_unknown_opcode() {
        let wire = vec![0xff, 0, 0, 0];
        let mut frame = FrameCommands::new();
        assert_eq!(decode_frame(&wire, &mut frame), Err(DecodeError::UnknownOp(0xff)));
    }

    #[test]
    fn decode_rejects_bad_utf8_in_text() {
        // DrawText header (16 bytes of params) + len=2 + two invalid bytes.
        let mut wire = vec![opcode::DRAW_TEXT];
        wire.extend_from_slice(&0f32.to_le_bytes());
        wire.extend_from_slice(&0f32.to_le_bytes());
        wire.extend_from_slice(&10f32.to_le_bytes());
        wire.extend_from_slice(&0u32.to_le_bytes());
        wire.extend_from_slice(&2u32.to_le_bytes());
        wire.push(0xff); wire.push(0xfe);
        let mut frame = FrameCommands::new();
        assert_eq!(decode_frame(&wire, &mut frame), Err(DecodeError::BadUtf8));
    }

    #[test]
    fn app_handle_is_niche_packed() {
        assert_eq!(core::mem::size_of::<Option<AppHandle>>(), core::mem::size_of::<u32>());
    }

    #[test]
    fn set_grants_delegates_to_host_with_owned_strings() {
        // Stone 10.3b — the framework boot loop calls
        // `host.set_grants(pkg, &[grant1, grant2, ...])` once per
        // installed package after rehydration. MockHost captures the
        // payload so a regression in the boot wiring (wrong package
        // id, missing iteration, lost grants) shows up here rather
        // than at first-app-launch.
        let host = MockHost::new();
        host.take_calls();
        host.set_grants("com.akiva.calculator", &[
            "akiva.permission.STORAGE_SANDBOX",
        ]);
        host.set_grants("com.akiva.shell", &[
            "akiva.permission.QUERY_PACKAGES",
            "akiva.permission.LAUNCH_ANY_APP",
        ]);
        assert_eq!(host.take_calls(), vec![
            Call::SetGrants(
                "com.akiva.calculator".into(),
                vec!["akiva.permission.STORAGE_SANDBOX".into()],
            ),
            Call::SetGrants(
                "com.akiva.shell".into(),
                vec![
                    "akiva.permission.QUERY_PACKAGES".into(),
                    "akiva.permission.LAUNCH_ANY_APP".into(),
                ],
            ),
        ]);
    }

    #[test]
    fn save_state_bytes_delegates_to_host() {
        let host = Rc::new(MockHost::new());
        let handle = host.register("p", "c").unwrap();
        host.take_calls();
        host.set_save_bytes(vec![1, 2, 3, 4]);
        let act = WasmActivity::new(handle, host.clone());
        let bytes = act.save_state_bytes();
        assert_eq!(bytes, vec![1, 2, 3, 4]);
        assert_eq!(host.take_calls(), vec![Call::SaveState(handle.get())]);
    }

    #[test]
    fn restore_state_bytes_forwards_slice_to_host() {
        let host = Rc::new(MockHost::new());
        let handle = host.register("p", "c").unwrap();
        host.take_calls();
        let mut act = WasmActivity::new(handle, host.clone());
        act.restore_state_bytes(&[9, 8, 7]);
        assert_eq!(
            host.take_calls(),
            vec![Call::RestoreState(handle.get(), vec![9, 8, 7])],
        );
    }

    // ── Stone 9.3: drain_finish propagation ─────────────────────────────

    #[test]
    fn drain_finish_flips_finish_requested_on_next_ctx_callback() {
        // The wasm side "asked to finish" (MockHost.arm_finish). The next
        // ctx-bearing callback (on_resume here) should see
        // *ctx.finish_requested flip to true. Subsequent callbacks
        // without a fresh arm must NOT see it re-fire — matches the
        // single-shot semantic of AppRegistry.drainFinish in runtime.
        let host = Rc::new(MockHost::new());
        let handle = host.register("p", "c").unwrap();
        let mut act = WasmActivity::new(handle, host.clone());
        host.arm_finish();

        let mut vars = CtxVars::default();
        let mut ctx = vars.ctx();
        act.on_resume(&mut ctx);
        assert!(vars.finish, "armed finish must surface to ctx");

        // Second callback with no re-arm: flag stays false.
        let mut vars2 = CtxVars::default();
        let mut ctx2 = vars2.ctx();
        act.on_resume(&mut ctx2);
        assert!(!vars2.finish, "finish must not sticky");
    }

    // ── Stone 9.4: drain_intent + on_new_intent ─────────────────────────

    #[test]
    fn drain_intent_sets_launch_requested_on_next_ctx_callback() {
        // App called akiva_host_start_activity; kernel must see the
        // request surface as ctx.launch_requested on the next
        // ctx-bearing callback, single-shot (mirrors drain_finish).
        let host = Rc::new(MockHost::new());
        let handle = host.register("p", "c").unwrap();
        let mut act = WasmActivity::new(handle, host.clone());
        let intent = Intent::explicit("com.example.target", "Main");
        host.arm_intent(intent.encode());

        let mut vars = CtxVars::default();
        let mut ctx = vars.ctx();
        act.on_resume(&mut ctx);
        let got = vars.launch.expect("armed intent must surface to ctx");
        assert_eq!(got.target_package.as_deref(), Some("com.example.target"));
        assert_eq!(got.target_class.as_deref(), Some("Main"));

        // Second callback without re-arm: no launch resurfaces.
        let mut vars2 = CtxVars::default();
        let mut ctx2 = vars2.ctx();
        act.on_resume(&mut ctx2);
        assert!(vars2.launch.is_none(), "intent must not sticky");
    }

    #[test]
    fn malformed_intent_blob_is_silently_dropped() {
        // A buggy app that writes junk into the host's intent slot
        // must not crash the whole dispatch. The decode error is
        // swallowed and ctx.launch_requested stays None.
        let host = Rc::new(MockHost::new());
        let handle = host.register("p", "c").unwrap();
        let mut act = WasmActivity::new(handle, host.clone());
        host.arm_intent(vec![0xff; 3]);  // too short for even the action_len header

        let mut vars = CtxVars::default();
        let mut ctx = vars.ctx();
        act.on_resume(&mut ctx);
        assert!(vars.launch.is_none());
        assert!(!vars.finish);
    }

    #[test]
    fn on_new_intent_forwards_encoded_blob_to_host() {
        let host = Rc::new(MockHost::new());
        let handle = host.register("p", "c").unwrap();
        host.take_calls();
        let mut act = WasmActivity::new(handle, host.clone());

        let intent = Intent::new(crate::intent::action::VIEW)
            .with_category(crate::intent::category::DEFAULT);
        let mut vars = CtxVars::default();
        let mut ctx = vars.ctx();
        act.on_new_intent(&mut ctx, &intent);

        let calls = host.take_calls();
        assert_eq!(calls.len(), 1, "exactly one host call per on_new_intent");
        match &calls[0] {
            Call::NewIntent(h, bytes) => {
                assert_eq!(*h, handle.get());
                // Round-trip the bytes the host received to prove the
                // payload carries the original fields — the encoding is
                // opaque from the perspective of this test.
                let round = Intent::decode(bytes).expect("host got decodable bytes");
                assert_eq!(round.action, intent.action);
                assert_eq!(round.categories, intent.categories);
            }
            other => panic!("expected NewIntent, got {other:?}"),
        }
    }

    #[test]
    fn drain_finish_not_consulted_on_destroy() {
        // destroy is terminal — even if the app somehow raised finish
        // during the destroy callback, there's nothing to do with it.
        // Stone 9.3 deliberately skips drain_post in on_destroy; this
        // test locks that in so a future refactor doesn't change it.
        let host = Rc::new(MockHost::new());
        let handle = host.register("p", "c").unwrap();
        let mut act = WasmActivity::new(handle, host.clone());
        host.arm_finish();

        let mut vars = CtxVars::default();
        let mut ctx = vars.ctx();
        act.on_destroy(&mut ctx);
        // The armed flag stays armed (never drained) and the context's
        // flag was never touched.
        assert!(!vars.finish);
        assert!(*host.pending_finish.borrow(),
            "destroy must not drain the host flag");
    }

    // ── Stone 9.5: start_activity_for_result / set_result plumbing ───────

    #[test]
    fn drain_start_for_result_sets_launch_for_result_on_next_callback() {
        let host = Rc::new(MockHost::new());
        let handle = host.register("p", "c").unwrap();
        let mut act = WasmActivity::new(handle, host.clone());
        let intent = Intent::explicit("com.example.picker", "Main");
        host.arm_start_for_result(intent.encode(), 42);

        let mut vars = CtxVars::default();
        let mut ctx = vars.ctx();
        act.on_resume(&mut ctx);
        let (got, rc) = vars.launch_for_result.expect("armed start_for_result must surface");
        assert_eq!(rc, 42);
        assert_eq!(got.target_package.as_deref(), Some("com.example.picker"));
        assert_eq!(got.target_class.as_deref(), Some("Main"));

        // Second callback without re-arm: no request resurfaces.
        let mut vars2 = CtxVars::default();
        let mut ctx2 = vars2.ctx();
        act.on_resume(&mut ctx2);
        assert!(vars2.launch_for_result.is_none(),
            "start_for_result must be single-shot like drain_intent");
    }

    #[test]
    fn drain_set_result_surfaces_encoded_activity_result() {
        let host = Rc::new(MockHost::new());
        let handle = host.register("p", "c").unwrap();
        let mut act = WasmActivity::new(handle, host.clone());
        let result = ActivityResult { code: 0, data: b"picked=7".to_vec() };
        host.arm_set_result(result.encode());

        let mut vars = CtxVars::default();
        let mut ctx = vars.ctx();
        act.on_pause(&mut ctx);  // drain runs on any ctx-bearing callback
        let got = vars.set_result.expect("armed set_result must surface");
        assert_eq!(got, result);
    }

    #[test]
    fn malformed_start_for_result_blob_is_silently_dropped() {
        let host = Rc::new(MockHost::new());
        let handle = host.register("p", "c").unwrap();
        let mut act = WasmActivity::new(handle, host.clone());
        // Too short to decode as an Intent.
        host.arm_start_for_result(vec![0xff; 3], 1);

        let mut vars = CtxVars::default();
        let mut ctx = vars.ctx();
        act.on_resume(&mut ctx);
        assert!(vars.launch_for_result.is_none(),
            "malformed intent blob must not surface a garbage launch_for_result");
    }

    #[test]
    fn on_activity_result_forwards_encoded_bytes_to_host() {
        let host = Rc::new(MockHost::new());
        let handle = host.register("p", "c").unwrap();
        host.take_calls();
        let mut act = WasmActivity::new(handle, host.clone());
        let result = ActivityResult { code: 0, data: vec![1, 2, 3] };

        let mut vars = CtxVars::default();
        let mut ctx = vars.ctx();
        act.on_activity_result(&mut ctx, 99, &result);

        let calls = host.take_calls();
        assert!(matches!(calls.first(), Some(Call::ActivityResult(_, 99, _))),
            "first call must be ActivityResult with the provided request_code");
        match &calls[0] {
            Call::ActivityResult(h, rc, bytes) => {
                assert_eq!(*h, handle.get());
                assert_eq!(*rc, 99);
                let round = ActivityResult::decode(bytes).expect("host got decodable bytes");
                assert_eq!(round, result);
            }
            _ => unreachable!(),
        }
    }
}
