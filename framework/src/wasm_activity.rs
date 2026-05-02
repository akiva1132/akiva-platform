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

    /// Fire a lifecycle transition on the app. Returns a `future_id` the
    /// framework can pass to [`Self::poll_future`] to await completion.
    ///
    /// Stone 11.4a contract:
    /// - **`0`** — resolved synchronously inside this call. The framework
    ///   does not record a `PendingTransition` and advances state right
    ///   away. Every existing impl (KernelWasmHost, MockHost,
    ///   BootMockHost) returns `0` because the wasm app runs in the same
    ///   worker as the kernel today.
    /// - **non-zero** — the host accepted the dispatch but the app
    ///   hasn't acked yet. The framework records `PendingTransition`,
    ///   skips further dispatches at this activity, and polls each
    ///   frame via `poll_future`. Stone 11.4c WorkerHost returns
    ///   non-zero when the app lives in a separate Worker and the
    ///   ack travels back via `postMessage`.
    fn lifecycle(&self, handle: AppHandle, transition: LifecycleTransition) -> u64;
    /// Same `0` = sync, non-zero = pending contract as
    /// [`Self::lifecycle`]. Pointer/key/resize each map to their own
    /// extern (`hal_app_pointer` etc.) today; in 11.4c they collapse
    /// into the unified `dispatch` Worker channel alongside lifecycle,
    /// but the trait shape stays the same.
    fn pointer(&self, handle: AppHandle, ev: PointerEvent, ts_ms: f64) -> u64;
    fn key(&self, handle: AppHandle, ev: KeyEvent, ts_ms: f64) -> u64;
    fn resize(&self, handle: AppHandle, metrics: SurfaceMetrics) -> u64;

    /// Ask the app to paint one frame. Returns a `future_id` that, once
    /// polled via [`Self::poll_future`], yields the encoded paint-op
    /// stream the framework decodes via [`decode_frame`].
    ///
    /// Stone 11.4a contract:
    /// - **`0`** — empty frame (idle app, or no display) — the framework
    ///   skips the poll and leaves the [`FrameCommands`] untouched.
    /// - **non-zero** — paint bytes available; framework polls and
    ///   decodes. In 11.4c with worker-per-app, the `0` short-circuit
    ///   stays valid (idle apps still skip the round-trip), and
    ///   non-zero futures may genuinely defer until the app-worker
    ///   posts back.
    fn render(&self, handle: AppHandle, ts_ms: f64) -> u64;

    /// Ask the app to serialise its per-activity UI state for a possible
    /// process kill. Returns a `future_id`; `poll_future` yields the
    /// encoded `SavedState` blob (the framework treats them as opaque).
    ///
    /// Same `0` = empty, non-zero = poll-for-bytes contract as
    /// [`Self::render`]. Default returns `0` so in-process activities
    /// (Shell pre-11.0c, mocks) can keep ignoring the hook with no
    /// allocation.
    fn save_state(&self, handle: AppHandle) -> u64 {
        let _ = handle;
        0
    }

    /// Hand previously-saved bytes back to the app. Called after
    /// `lifecycle(Create)` and before `lifecycle(Start)` so the app sees
    /// its restored state in `on_start`/`on_resume` just like a fresh
    /// launch. Empty bytes = "no saved state", skipped by the framework.
    ///
    /// Same `0` = sync, non-zero = pending contract as
    /// [`Self::lifecycle`]. Default returns `0` for the same reason as
    /// `save_state` — in-process activities (Shell pre-11.0c, mocks)
    /// have nothing to restore and resolve immediately.
    fn restore_state(&self, handle: AppHandle, bytes: &[u8]) -> u64 {
        let _ = (handle, bytes);
        0
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
    /// dispatch exactly as if an in-kernel activity had written
    /// `*ctx.finish_requested = true` directly. (Pre-Stone-11.0c the
    /// Shell was such an in-kernel activity; today the Shell is itself
    /// a wasm app and routes through this path.)
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
    /// of stamping out a fresh instance.
    ///
    /// Same `0` = sync, non-zero = pending contract as
    /// [`Self::lifecycle`]. Default returns `0` so in-kernel hosts that
    /// don't expose a new-intent path stay quiet.
    fn new_intent(&self, handle: AppHandle, bytes: &[u8]) -> u64 {
        let _ = (handle, bytes);
        0
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
    /// on the real host.
    ///
    /// Same `0` = sync, non-zero = pending contract as
    /// [`Self::lifecycle`]. Default returns `0` for activities that
    /// don't launch for-result.
    fn activity_result(
        &self,
        handle: AppHandle,
        request_code: u32,
        bytes: &[u8],
    ) -> u64 {
        let _ = (handle, request_code, bytes);
        0
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
    /// Default is a no-op so test harnesses and MockHost can ignore
    /// the push without breaking.
    fn set_grants(&self, package_id: &str, grants: &[&str]) {
        let _ = (package_id, grants);
    }

    /// Stone 11.0a — push the encoded snapshot of installed packages to
    /// the host so apps holding `akiva.permission.QUERY_PACKAGES` can
    /// enumerate them via `akiva_host_query_packages`. Bytes are the
    /// output of [`crate::pkgs_wire::encode`]; opaque to the host
    /// implementation (it just stores the buffer and serves it back).
    ///
    /// Called once from `framework::boot` after rehydration, before any
    /// activity launches. Future install/uninstall flows will re-push
    /// the full set; the host applies last-write-wins.
    ///
    /// Default no-op so MockHost / in-kernel hosts can ignore the push.
    fn set_packages(&self, bytes: &[u8]) {
        let _ = bytes;
    }

    /// Stone 11.4a — poll a `future_id` previously returned from a host
    /// dispatch. Returns:
    ///
    /// - `None` — future is still in flight; the framework should leave
    ///   the activity in its [`crate::activity_manager::PendingTransition`]
    ///   slot and try again next frame.
    /// - `Some(Ok(bytes))` — resolved successfully. `bytes` is empty for
    ///   ops that don't carry a payload (lifecycle, pointer, key,
    ///   resize, restore_state, new_intent, activity_result), or the
    ///   produced blob for ops that do (render, save_state).
    /// - `Some(Err(reason))` — the dispatch trapped or the host
    ///   detected a protocol violation. The framework treats this as a
    ///   trap and (Stone 11.5) marks the activity `Trapped`.
    ///
    /// The default impl is a "resolves immediately to empty bytes"
    /// shim — every existing `WasmHost` (KernelWasmHost, MockHost,
    /// BootMockHost) returns `0` from its dispatch methods today, so
    /// the framework never actually polls. The default exists so test
    /// mocks that *want* to exercise the deferred path can opt in by
    /// returning a non-zero `future_id` and overriding `poll_future`,
    /// without forcing every existing mock to add a method.
    ///
    /// `future_id == 0` is reserved as the "resolved synchronously"
    /// sentinel and must never be passed in here; the framework calls
    /// `poll_future` only when a recorded `PendingTransition` carries a
    /// non-zero id.
    fn poll_future(&self, future_id: u64) -> Option<Result<Vec<u8>, String>> {
        let _ = future_id;
        Some(Ok(Vec::new()))
    }
}

// ── Proxy activity ───────────────────────────────────────────────────────

/// Framework-side [`Activity`] that forwards every callback to a hosted
/// wasm app via a [`WasmHost`]. Owns the `AppHandle` for its lifetime and
/// releases it in `on_destroy`. Carries an `Rc<dyn WasmHost>` so two
/// activities of the same package share one host instance.
pub struct WasmActivity {
    handle: AppHandle,
    host: Rc<dyn WasmHost>,
    /// Stone 11.4c — render is pipelined across frames. After we
    /// dispatch [`WasmHost::render`] we stash the returned future_id
    /// here; the next frame polls it, decodes the bytes, and queues
    /// the next render. Steady state is one-frame paint latency —
    /// the worker can't synchronously hand bytes back to the kernel
    /// thread, so the alternative would be a blank screen forever
    /// (the prior 11.4a path's `assert_sync` becomes a no-op in
    /// release and silently drops every paint).
    pending_render: Option<u64>,
}

impl WasmActivity {
    pub fn new(handle: AppHandle, host: Rc<dyn WasmHost>) -> Self {
        Self { handle, host, pending_render: None }
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

impl WasmActivity {
    /// Stone 11.4a — assert the host resolved a dispatch synchronously
    /// (returned the `0` sentinel). Today every `WasmHost` impl
    /// (KernelWasmHost, MockHost, BootMockHost) returns `0` from every
    /// dispatch method because the wasm app runs in the same worker as
    /// the kernel; the `WasmActivity` shim still drives transitions
    /// inline through the `Activity` trait.
    ///
    /// 11.4c moves the deferred path out of this helper: the framework
    /// state machine will dispatch directly to `WasmHost` (skipping the
    /// `Activity` trait) and own the `PendingTransition` slot. Until
    /// then a non-zero `future_id` here is a wiring bug — the host
    /// returned a future_id but no caller is set up to poll it. We
    /// `debug_assert!` loudly rather than silently dropping the
    /// dispatch.
    fn assert_sync(&self, op: &'static str, future_id: u64) {
        debug_assert_eq!(
            future_id, 0,
            "Stone 11.4a: {op} on the Activity-trait path expects sync resolution. \
             Host returned future_id={future_id:#x} on handle {handle:?}; the deferred \
             path lands when 11.4c routes WorkerHost through framework state machine \
             instead of WasmActivity.",
            handle = self.handle,
        );
    }

    fn fire_lifecycle_sync(&self, transition: LifecycleTransition) {
        let future_id = self.host.lifecycle(self.handle, transition);
        self.assert_sync("lifecycle", future_id);
    }
}

impl Activity for WasmActivity {
    fn on_create(&mut self, ctx: &mut ActivityContext<'_>) {
        self.fire_lifecycle_sync(LifecycleTransition::Create);
        // Prime the app's slot with the live surface metrics right after
        // construction. The SDK's `on_create` builds the slot with
        // `Metrics::default()` (zeros) because the `lifecycle(Create)`
        // wire doesn't carry a surface size; without this follow-up
        // resize, the first `on_render` would run with zero metrics and
        // lay out the whole tree at the origin — manifesting as a
        // "tiny calculator in the top-left until the user resizes the
        // window". One extra round-trip here is worth a much saner
        // first frame.
        self.assert_sync("resize", self.host.resize(self.handle, ctx.metrics));
        self.drain_post(ctx);
    }
    fn on_start(&mut self, ctx: &mut ActivityContext<'_>) {
        self.fire_lifecycle_sync(LifecycleTransition::Start);
        self.drain_post(ctx);
    }
    fn on_resume(&mut self, ctx: &mut ActivityContext<'_>) {
        self.fire_lifecycle_sync(LifecycleTransition::Resume);
        self.drain_post(ctx);
    }
    fn on_pause(&mut self, ctx: &mut ActivityContext<'_>) {
        self.fire_lifecycle_sync(LifecycleTransition::Pause);
        self.drain_post(ctx);
    }
    fn on_stop(&mut self, ctx: &mut ActivityContext<'_>) {
        self.fire_lifecycle_sync(LifecycleTransition::Stop);
        self.drain_post(ctx);
    }
    fn on_destroy(&mut self, _ctx: &mut ActivityContext<'_>) {
        // No drain_post — destroy is terminal, a finish flag here would
        // be nonsense anyway (can't finish a destroying activity).
        self.fire_lifecycle_sync(LifecycleTransition::Destroy);
        self.host.unregister(self.handle);
    }

    fn on_pointer(&mut self, ctx: &mut ActivityContext<'_>, ev: PointerEvent) {
        self.assert_sync("pointer", self.host.pointer(self.handle, ev, ctx.timestamp_ms));
        self.drain_post(ctx);
    }
    fn on_key(&mut self, ctx: &mut ActivityContext<'_>, ev: KeyEvent) {
        self.assert_sync("key", self.host.key(self.handle, ev, ctx.timestamp_ms));
        self.drain_post(ctx);
    }
    fn on_resize(&mut self, ctx: &mut ActivityContext<'_>, m: SurfaceMetrics) {
        self.assert_sync("resize", self.host.resize(self.handle, m));
        self.drain_post(ctx);
    }

    fn render(&mut self, ctx: &mut ActivityContext<'_>, out: &mut FrameCommands) {
        // Stone 11.4c — drain any host-side signals (`finish`,
        // `start_activity`, `set_result`, …) that the worker emitted
        // since the last input/lifecycle callback. With cross-worker
        // dispatch the side-effect for a click arrives one or more
        // frames AFTER the pointer dispatch returned, and there's
        // nothing else that drains them before the next user input —
        // without this hook, "first click does nothing, second click
        // launches the app from the FIRST click's queued intent" is
        // the user-visible symptom.
        self.drain_post(ctx);

        // Stone 11.4c — render pipeline. Each frame:
        //   1. If a prior render is still in-flight, poll it. READY
        //      bytes get decoded into `out` and the slot clears.
        //      PENDING leaves the slot armed and skips the new
        //      dispatch — the worker is still chewing on the prior
        //      one, queueing a second would waste cycles.
        //   2. Otherwise (or after a successful drain) kick off a
        //      fresh render so the next frame has bytes to paint.
        //
        // First frame paints nothing because there's no prior to
        // drain; from then on the screen lags one frame behind the
        // app's logical state. Acceptable — the worker boundary is
        // genuinely async and there's no way to synchronously wait
        // on a peer worker without SharedArrayBuffer.
        let mut may_dispatch = true;
        if let Some(fid) = self.pending_render {
            match self.host.poll_future(fid) {
                Some(Ok(bytes)) => {
                    self.pending_render = None;
                    if !bytes.is_empty() {
                        // Drop on decode error: same policy as the
                        // pre-11.4c inline path. The framework has no
                        // log binding here; the kernel will notice an
                        // empty FrameCommands if it cared.
                        let _ = decode_frame(&bytes, out);
                    }
                }
                None => {
                    // Worker still busy. Don't queue another dispatch —
                    // we'd just pile up futures in the registry. Try
                    // again next frame.
                    may_dispatch = false;
                }
                Some(Err(_)) => {
                    // Stone 11.5 will fold this into a `Trapped` mark.
                    // For now: clear the slot, leave `out` empty, and
                    // queue a fresh dispatch below.
                    self.pending_render = None;
                }
            }
        }
        if may_dispatch {
            let fid = self.host.render(self.handle, ctx.timestamp_ms);
            if fid != 0 {
                self.pending_render = Some(fid);
            }
        }
    }

    fn save_state_bytes(&self) -> Vec<u8> {
        // P0 #1 — cross-worker save can't return bytes synchronously
        // (the worker hasn't even processed the dispatch by the time
        // this returns). The framework calls
        // [`Activity::dispatch_save_state_async`] instead and tracks
        // the pending save through [`Framework::drive_pending_saves`].
        // Returning empty here is the contract for "use the async path"
        // — the framework only consults this when async returned 0.
        Vec::new()
    }

    fn dispatch_save_state_async(&self) -> u64 {
        // Fire-and-record. The framework records the future_id in its
        // `pending_saves` list and polls once per frame; bytes are
        // persisted whenever the worker replies. Empty save (host
        // returns `0`) means the activity has nothing to save, and the
        // framework treats the absent entry as a no-op.
        self.host.save_state(self.handle)
    }
    fn restore_state_bytes(&mut self, bytes: &[u8]) {
        self.assert_sync("restore_state", self.host.restore_state(self.handle, bytes));
    }

    fn on_new_intent(&mut self, ctx: &mut ActivityContext<'_>, intent: &Intent) {
        // Re-encode rather than passing the original bytes through —
        // `Framework` doesn't retain the wire buffer past `apply_post`,
        // and the cost is trivial compared to the wasm call that
        // follows. Keeps the boundary symmetric: framework side owns a
        // decoded `Intent`, wasm side receives an encoded blob.
        let bytes = intent.encode();
        self.assert_sync("new_intent", self.host.new_intent(self.handle, &bytes));
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
        self.assert_sync(
            "activity_result",
            self.host.activity_result(self.handle, request_code, &bytes),
        );
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
        SetPackages(Vec<u8>),
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
        /// Stone 11.4a-4 — future_id → result-bytes cache for the two
        /// data-returning dispatch methods (`render`, `save_state`).
        /// Each call allocates a fresh id, stores its bytes here, and
        /// returns the id; `poll_future` looks them up and removes the
        /// entry. Empty paint/save bytes shortcut to the `0` sentinel
        /// without ever touching this map.
        future_results: RefCell<std::collections::HashMap<u64, Vec<u8>>>,
        next_future_id: RefCell<u64>,
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
                future_results: RefCell::new(std::collections::HashMap::new()),
                next_future_id: RefCell::new(1),
            }
        }
        fn alloc_future(&self, bytes: Vec<u8>) -> u64 {
            let mut next = self.next_future_id.borrow_mut();
            let id = *next;
            *next = next.wrapping_add(1).max(1);  // skip the 0 sentinel on wrap
            self.future_results.borrow_mut().insert(id, bytes);
            id
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
        fn lifecycle(&self, handle: AppHandle, t: LifecycleTransition) -> u64 {
            self.calls.borrow_mut().push(Call::Lifecycle(handle.get(), t));
            // Sync-resolve sentinel — see WasmHost::lifecycle docs.
            0
        }
        fn pointer(&self, handle: AppHandle, ev: PointerEvent, ts: f64) -> u64 {
            self.calls.borrow_mut().push(Call::Pointer(handle.get(), ev.x, ev.y, ts));
            0
        }
        fn key(&self, handle: AppHandle, ev: KeyEvent, ts: f64) -> u64 {
            self.calls.borrow_mut().push(Call::Key(handle.get(), ev.code, ts));
            0
        }
        fn resize(&self, handle: AppHandle, m: SurfaceMetrics) -> u64 {
            self.calls.borrow_mut().push(Call::Resize(handle.get(), m.width_px, m.height_px));
            0
        }
        fn render(&self, handle: AppHandle, ts: f64) -> u64 {
            self.calls.borrow_mut().push(Call::Render(handle.get(), ts));
            let bytes = self.paint_bytes.borrow().clone();
            // Empty paint stream → 0 sentinel. Same shortcut KernelWasmHost
            // takes when `hal_app_render` reports zero bytes — saves the
            // framework a poll round-trip for an idle frame.
            if bytes.is_empty() { return 0; }
            self.alloc_future(bytes)
        }
        fn save_state(&self, handle: AppHandle) -> u64 {
            self.calls.borrow_mut().push(Call::SaveState(handle.get()));
            let bytes = self.save_bytes.borrow().clone();
            if bytes.is_empty() { return 0; }
            self.alloc_future(bytes)
        }
        fn restore_state(&self, handle: AppHandle, bytes: &[u8]) -> u64 {
            self.calls.borrow_mut().push(Call::RestoreState(handle.get(), bytes.to_vec()));
            0
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
        fn new_intent(&self, handle: AppHandle, bytes: &[u8]) -> u64 {
            self.calls.borrow_mut().push(Call::NewIntent(handle.get(), bytes.to_vec()));
            0
        }
        fn drain_start_for_result(&self, _handle: AppHandle) -> Option<(Vec<u8>, u32)> {
            self.pending_start_for_result.borrow_mut().take()
        }
        fn drain_set_result(&self, _handle: AppHandle) -> Option<Vec<u8>> {
            self.pending_set_result.borrow_mut().take()
        }
        fn activity_result(&self, handle: AppHandle, request_code: u32, bytes: &[u8]) -> u64 {
            self.calls.borrow_mut().push(
                Call::ActivityResult(handle.get(), request_code, bytes.to_vec()),
            );
            0
        }
        fn set_grants(&self, package_id: &str, grants: &[&str]) {
            self.calls.borrow_mut().push(Call::SetGrants(
                package_id.into(),
                grants.iter().map(|s| (*s).to_string()).collect(),
            ));
        }
        fn set_packages(&self, bytes: &[u8]) {
            self.calls.borrow_mut().push(Call::SetPackages(bytes.to_vec()));
        }
        fn poll_future(&self, future_id: u64) -> Option<Result<Vec<u8>, String>> {
            // Stone 11.4a-4: render and save_state route their bytes
            // through this map. Other dispatches return the `0` sentinel
            // and the framework never polls them — a stale future_id
            // here would mean a bug in the caller, so we report the
            // miss as an error rather than silently resolving empty
            // (the trait-default behaviour). Tests exercising the deferred
            // path opt in by using `alloc_future` directly.
            match self.future_results.borrow_mut().remove(&future_id) {
                Some(bytes) => Some(Ok(bytes)),
                None => Some(Err(format!("MockHost: unknown future_id {future_id:#x}"))),
            }
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
    fn render_decodes_host_bytes_into_frame_pipelined() {
        // Stone 11.4c — render is one-frame-pipelined: first call
        // queues a dispatch (no paint), second call drains its bytes
        // and queues the next dispatch. Tests the full two-frame
        // round-trip.
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

        // Frame 1 — dispatches render, no paint yet.
        let mut frame = FrameCommands::new();
        act.render(&mut ctx, &mut frame);
        assert_eq!(host.take_calls(), vec![Call::Render(handle.get(), ctx.timestamp_ms)]);
        assert_eq!(frame.ops().len(), 0,
            "first frame in the pipeline only dispatches; bytes drain on the next call");

        // Frame 2 — drains the prior render's bytes, queues the next
        // dispatch. `MockHost` resolves futures synchronously so the
        // first poll sees READY immediately.
        act.render(&mut ctx, &mut frame);
        // The MockHost's render call landed once again (the new dispatch).
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
    fn set_packages_delegates_to_host_with_owned_bytes() {
        // Stone 11.0a — the framework boot loop pushes the encoded
        // package-list snapshot once after rehydration. MockHost
        // captures the bytes so a regression in the encode call site
        // (wrong slice, missed call, double-call) shows up here rather
        // than as a silently-empty Shell launcher.
        let host = MockHost::new();
        host.take_calls();
        host.set_packages(&[1, 2, 3, 4]);
        host.set_packages(&[]);
        assert_eq!(host.take_calls(), vec![
            Call::SetPackages(vec![1, 2, 3, 4]),
            Call::SetPackages(vec![]),
        ]);
    }

    #[test]
    fn dispatch_save_state_async_returns_future_id_and_polls_to_bytes() {
        // P0 #1 — wasm activities serve save_state via the async
        // pipeline: dispatch returns a future_id (non-zero when there
        // are bytes), the framework polls and persists. The legacy sync
        // `save_state_bytes` path now returns empty for wasm activities
        // and the framework's destroy_with_persist routes through the
        // async path instead.
        let host = Rc::new(MockHost::new());
        let handle = host.register("p", "c").unwrap();
        host.take_calls();
        host.set_save_bytes(vec![1, 2, 3, 4]);
        let act = WasmActivity::new(handle, host.clone());

        let future_id = act.dispatch_save_state_async();
        assert_ne!(future_id, 0, "non-empty save bytes ⇒ non-zero future_id");
        assert_eq!(host.take_calls(), vec![Call::SaveState(handle.get())]);

        let resolved = host.poll_future(future_id);
        match resolved {
            Some(Ok(bytes)) => assert_eq!(bytes, vec![1, 2, 3, 4]),
            other => panic!("expected Some(Ok(bytes)), got {other:?}"),
        }

        // Sync save_state_bytes no longer reaches the host — it short-
        // circuits to empty.
        assert!(act.save_state_bytes().is_empty());
        assert!(host.take_calls().is_empty(),
            "save_state_bytes must not re-dispatch when async path is used");
    }

    #[test]
    fn dispatch_save_state_async_returns_zero_when_no_bytes() {
        // The MockHost shortcuts to the `0` sentinel when the seeded
        // save bytes are empty. The framework reads `0` as "nothing to
        // save" and skips the pending_saves entry entirely.
        let host = Rc::new(MockHost::new());
        let handle = host.register("p", "c").unwrap();
        host.take_calls();
        let act = WasmActivity::new(handle, host.clone());

        let future_id = act.dispatch_save_state_async();
        assert_eq!(future_id, 0);
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

    /// Stone 11.4a scaffolding — every existing `WasmHost` impl
    /// (production `KernelWasmHost`, in-process tests' `MockHost`,
    /// `BootMockHost` in `framework::lib`) currently resolves dispatches
    /// synchronously and the framework never opens a future. The default
    /// `poll_future` shim is the safety net for that — if a polluted
    /// pending slot ever pointed at it, returning `Some(Ok(empty))` makes
    /// the caller unblock cleanly rather than spin forever.
    ///
    /// 11.4c flips the default: the WorkerHost overrides `poll_future`
    /// to track per-package message responses, and dispatch methods
    /// start returning real future_ids. Until then this shim is the
    /// only impl, so it deserves a pin so a future refactor doesn't
    /// turn it into a real `unimplemented!` and silently break tests
    /// that opt into the deferred path with a custom mock.
    #[test]
    fn default_poll_future_resolves_immediately_to_empty_payload() {
        // Stub that takes the trait-default `poll_future`. `MockHost`
        // overrides it (it routes render/save_state bytes through a
        // future-id map), so this test would no longer exercise the
        // default if it used `MockHost`. The minimal stub here only
        // implements the required methods; everything else (lifecycle,
        // pointer, key, resize, render, poll_future, …) takes the trait
        // default we're pinning.
        struct DefaultHost;
        impl WasmHost for DefaultHost {
            fn register(&self, _: &str, _: &str) -> Option<AppHandle> {
                AppHandle::new(1)
            }
            fn unregister(&self, _: AppHandle) {}
            fn lifecycle(&self, _: AppHandle, _: LifecycleTransition) -> u64 { 0 }
            fn pointer(&self, _: AppHandle, _: PointerEvent, _: f64) -> u64 { 0 }
            fn key(&self, _: AppHandle, _: KeyEvent, _: f64) -> u64 { 0 }
            fn resize(&self, _: AppHandle, _: SurfaceMetrics) -> u64 { 0 }
            fn render(&self, _: AppHandle, _: f64) -> u64 { 0 }
        }
        let host = DefaultHost;
        let resolved = WasmHost::poll_future(&host, 1234);
        assert!(matches!(resolved, Some(Ok(ref bytes)) if bytes.is_empty()),
            "default poll_future is the immediate-resolve shim until 11.4c WorkerHost overrides it");
    }
}
