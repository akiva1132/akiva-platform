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
    }
    fn on_start(&mut self, _ctx: &mut ActivityContext<'_>) {
        self.host.lifecycle(self.handle, LifecycleTransition::Start);
    }
    fn on_resume(&mut self, _ctx: &mut ActivityContext<'_>) {
        self.host.lifecycle(self.handle, LifecycleTransition::Resume);
    }
    fn on_pause(&mut self, ctx: &mut ActivityContext<'_>) {
        let _ = ctx;
        self.host.lifecycle(self.handle, LifecycleTransition::Pause);
    }
    fn on_stop(&mut self, _ctx: &mut ActivityContext<'_>) {
        self.host.lifecycle(self.handle, LifecycleTransition::Stop);
    }
    fn on_destroy(&mut self, _ctx: &mut ActivityContext<'_>) {
        self.host.lifecycle(self.handle, LifecycleTransition::Destroy);
        self.host.unregister(self.handle);
    }

    fn on_pointer(&mut self, ctx: &mut ActivityContext<'_>, ev: PointerEvent) {
        self.host.pointer(self.handle, ev, ctx.timestamp_ms);
    }
    fn on_key(&mut self, ctx: &mut ActivityContext<'_>, ev: KeyEvent) {
        self.host.key(self.handle, ev, ctx.timestamp_ms);
    }
    fn on_resize(&mut self, _ctx: &mut ActivityContext<'_>, m: SurfaceMetrics) {
        self.host.resize(self.handle, m);
    }

    fn render(&mut self, ctx: &mut ActivityContext<'_>, out: &mut FrameCommands) {
        self.host.render(self.handle, ctx.timestamp_ms, out);
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
    }

    /// Mock that records every call and lets tests pre-seed encoded paint
    /// bytes that the next `render()` call will decode into `out`.
    struct MockHost {
        calls: RefCell<Vec<Call>>,
        next_handle: RefCell<u32>,
        paint_bytes: RefCell<Vec<u8>>,
    }

    impl MockHost {
        fn new() -> Self {
            Self {
                calls: RefCell::new(Vec::new()),
                next_handle: RefCell::new(1),
                paint_bytes: RefCell::new(Vec::new()),
            }
        }
        fn take_calls(&self) -> Vec<Call> { core::mem::take(&mut *self.calls.borrow_mut()) }
        fn set_paint_bytes(&self, bytes: Vec<u8>) { *self.paint_bytes.borrow_mut() = bytes; }
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
    }

    fn make_ctx<'a>(
        finish: &'a mut bool,
        launch: &'a mut Option<crate::activity::LaunchRequest>,
    ) -> ActivityContext<'a> {
        ActivityContext {
            metrics: SurfaceMetrics { width_px: 800, height_px: 600, density: 1.0 },
            timestamp_ms: 1234.0,
            pointer: None,
            finish_requested: finish,
            launch_requested: launch,
        }
    }

    #[test]
    fn lifecycle_transitions_forwarded_in_order() {
        let host = Rc::new(MockHost::new());
        let handle = host.register("com.test", "Main").unwrap();
        host.take_calls(); // drop register
        let mut act = WasmActivity::new(handle, host.clone());
        let mut finish = false;
        let mut launch = None;
        let mut ctx = make_ctx(&mut finish, &mut launch);
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
        let mut finish = false;
        let mut launch = None;
        let mut ctx = make_ctx(&mut finish, &mut launch);
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
        let mut finish = false;
        let mut launch = None;
        let mut ctx = make_ctx(&mut finish, &mut launch);
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
}
