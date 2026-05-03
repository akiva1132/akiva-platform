//! Compositor.
//!
//! Collects paint output from each visible window, stacks them in z-order,
//! and produces the `FrameCommands` buffer the renderer backend consumes.
//! See ARCHITECTURE.md §10-11 for the renderer/surface model.
//!
//! Stone 13.1 — internal storage moved from a single global
//! `FrameCommands` to a per-`SurfaceId` `HashMap`. The legacy
//! `paint_target` / `finish_frame` callers keep working through a
//! reserved default surface id (`Compositor::DEFAULT_SURFACE`); new
//! code uses `begin_surface` / `surface_target` / `finish_frame_in_order`
//! to drive per-surface composition.

/// Abstract target for paint output. `FrameCommands` is the in-framework
/// implementation; activities that want to paint by method rather than by
/// pushing `PaintOp`s directly take `&mut dyn PaintTarget`.
pub trait PaintTarget {
    fn fill_rect(&mut self, x: f32, y: f32, w: f32, h: f32, rgba: u32);
    fn fill_round_rect(&mut self, x: f32, y: f32, w: f32, h: f32, radius: f32, rgba: u32);
    fn draw_text(&mut self, x: f32, y: f32, size: f32, rgba: u32, text: &str);
    fn push_clip(&mut self, x: f32, y: f32, w: f32, h: f32);
    fn pop_clip(&mut self);
}

#[derive(Debug, Default, Clone)]
pub struct FrameCommands {
    pub ops: Vec<PaintOp>,
}

#[derive(Debug, Clone)]
pub enum PaintOp {
    Clear { rgba: u32 },
    FillRect { x: f32, y: f32, w: f32, h: f32, rgba: u32 },
    FillRoundRect { x: f32, y: f32, w: f32, h: f32, radius: f32, rgba: u32 },
    DrawText { x: f32, y: f32, size: f32, rgba: u32, text: String },
    PushClip { x: f32, y: f32, w: f32, h: f32 },
    PopClip,
    PushTransform { dx: f32, dy: f32, scale: f32 },
    PopTransform,
    // Stone 13.0prep — outlined ops. `width` is the stroke in dp.
    StrokeRect { x: f32, y: f32, w: f32, h: f32, width: f32, rgba: u32 },
    StrokeRoundRect { x: f32, y: f32, w: f32, h: f32, radius: f32, width: f32, rgba: u32 },
    DrawLine { x0: f32, y0: f32, x1: f32, y1: f32, width: f32, rgba: u32 },
    // Stone 13.0prep C — bitmap blit by drawable_id (resolved via
    // `IO::DRAWABLE_RESOLVE`). `id == 0` is the missing-drawable
    // sentinel; renderer paints a magenta placeholder.
    DrawImage { drawable_id: u32, x: f32, y: f32, w: f32, h: f32 },
}

impl FrameCommands {
    pub fn new() -> Self { Self::default() }
    pub fn clear(&mut self) { self.ops.clear(); }
    pub fn push(&mut self, op: PaintOp) { self.ops.push(op); }
    pub fn ops(&self) -> &[PaintOp] { &self.ops }
}

// Activities paint through the `PaintTarget` trait; FrameCommands is the
// in-kernel target that ends up getting flushed to the HAL each frame.
impl PaintTarget for FrameCommands {
    fn fill_rect(&mut self, x: f32, y: f32, w: f32, h: f32, rgba: u32) {
        self.ops.push(PaintOp::FillRect { x, y, w, h, rgba });
    }
    fn fill_round_rect(&mut self, x: f32, y: f32, w: f32, h: f32, radius: f32, rgba: u32) {
        self.ops.push(PaintOp::FillRoundRect { x, y, w, h, radius, rgba });
    }
    fn draw_text(&mut self, x: f32, y: f32, size: f32, rgba: u32, text: &str) {
        self.ops.push(PaintOp::DrawText { x, y, size, rgba, text: text.to_string() });
    }
    fn push_clip(&mut self, x: f32, y: f32, w: f32, h: f32) {
        self.ops.push(PaintOp::PushClip { x, y, w, h });
    }
    fn pop_clip(&mut self) {
        self.ops.push(PaintOp::PopClip);
    }
}

use std::collections::HashMap;

use crate::window_manager::SurfaceId;

pub struct Compositor {
    /// Per-surface paint buffers. Lazily allocated by `begin_surface`
    /// / `surface_target`; never auto-evicted — the WindowManager
    /// owns the surface lifecycle and calls `destroy_surface` here
    /// when its surface is dropped.
    surfaces: HashMap<SurfaceId, FrameCommands>,
    /// Flattened output of `finish_frame_in_order`. One buffer reused
    /// across frames so the HAL flush path never sees a temporary;
    /// cleared and refilled each `finish_frame*` call.
    flat: FrameCommands,
}

impl Compositor {
    /// Reserved surface id for the legacy single-surface code path
    /// (`Framework::render_frame`'s `paint_target` / `finish_frame`).
    ///
    /// Set to `u32::MAX` so it cannot collide with any
    /// `WindowManager::create_surface` allocation — the WindowManager
    /// allocates from `1` upward and would have to mint 4 billion
    /// surfaces in one process lifetime to reach this sentinel. The
    /// pre-Stone-13.2 value of `1` did collide: Stone 13.2's dialog
    /// path allocates an Overlay surface (the very first one would
    /// be `SurfaceId(1)`) and `begin_surface` would then clear the
    /// activity's paint buffer, wiping the calculator content
    /// behind the dialog. The 0.4-alpha dim was painted on whatever
    /// pixels survived from the previous frame, and over several
    /// frames the dim compounded onto itself → fully opaque
    /// background.
    ///
    /// `SurfaceId::NONE` (`0`) remains the never-allocated sentinel.
    pub const DEFAULT_SURFACE: SurfaceId = SurfaceId(u32::MAX);

    pub fn new() -> Self {
        Self { surfaces: HashMap::new(), flat: FrameCommands::new() }
    }

    /// Reset every per-surface buffer to empty. Surfaces stay
    /// allocated — `begin_frame` is the per-frame "clear paint",
    /// not "release surfaces". Memory used by ops Vecs gets reused
    /// (`Vec::clear` keeps the capacity), so steady-state allocation
    /// is zero past warm-up.
    pub fn begin_frame(&mut self) {
        for fc in self.surfaces.values_mut() { fc.clear(); }
        self.flat.clear();
    }

    /// Stone 13.1 — start populating commands for `id`. Auto-creates
    /// a `FrameCommands` if the surface hadn't been seen before;
    /// clears it if it had. Idempotent within a frame.
    pub fn begin_surface(&mut self, id: SurfaceId) {
        self.surfaces.entry(id).or_insert_with(FrameCommands::new).clear();
    }

    /// Stone 13.1 — mutable handle to a surface's command buffer.
    /// Lazy: creates the buffer on first call. Implements `PaintTarget`
    /// so activities can paint through the trait or push `PaintOp`s
    /// by hand. **No auto-clear** — call `begin_surface` / `begin_frame`
    /// first if you want a fresh buffer; otherwise ops accumulate.
    pub fn surface_target(&mut self, id: SurfaceId) -> &mut FrameCommands {
        self.surfaces.entry(id).or_insert_with(FrameCommands::new)
    }

    /// Stone 13.1 — drop a surface's buffer entirely. Called by the
    /// framework when the WindowManager `destroy_surface`s a surface,
    /// so the compositor doesn't leak buffers for surfaces that no
    /// longer exist. Idempotent on unknown ids.
    pub fn destroy_surface(&mut self, id: SurfaceId) {
        self.surfaces.remove(&id);
    }

    /// Stone 13.1 — flatten every listed surface's commands into one
    /// `FrameCommands`. `order` is typically
    /// `WindowManager::surfaces_in_z_order()` filtered by visibility;
    /// surfaces in `order` that the compositor doesn't know about are
    /// silently skipped (the WindowManager could have a surface that
    /// hasn't drawn yet this frame).
    ///
    /// The returned reference is to a buffer the compositor reuses
    /// across calls — it stays valid until the next `begin_frame` /
    /// `finish_frame_in_order` / `finish_frame` call invalidates it.
    pub fn finish_frame_in_order(&mut self, order: &[SurfaceId]) -> &FrameCommands {
        self.flat.clear();
        for id in order {
            if let Some(fc) = self.surfaces.get(id) {
                // Clone — `PaintOp::DrawText` carries a `String` so
                // we can't just move ops without losing the per-surface
                // buffer for next frame. Cost is bounded by total ops
                // count, typically ≤ 200 even with 4 active surfaces.
                self.flat.ops.extend(fc.ops.iter().cloned());
            }
        }
        &self.flat
    }

    /// Backward-compat: flatten just the default surface. Kept so
    /// pre-Stone-13.x callers (`Framework::render_frame` today) keep
    /// working without knowing about `SurfaceId`.
    pub fn finish_frame(&mut self) -> &FrameCommands {
        self.finish_frame_in_order(&[Self::DEFAULT_SURFACE])
    }

    /// Backward-compat: emit into the default surface.
    pub fn emit(&mut self, op: PaintOp) {
        self.surface_target(Self::DEFAULT_SURFACE).push(op);
    }

    /// Backward-compat: mutable handle to the default surface's
    /// buffer. Activities that paint through `PaintTarget` use this.
    pub fn paint_target(&mut self) -> &mut FrameCommands {
        self.surface_target(Self::DEFAULT_SURFACE)
    }
}

impl Default for Compositor {
    fn default() -> Self { Self::new() }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one_op() -> PaintOp {
        PaintOp::FillRect { x: 0.0, y: 0.0, w: 1.0, h: 1.0, rgba: 0xff112233 }
    }
    fn other_op() -> PaintOp {
        PaintOp::DrawText { x: 0.0, y: 0.0, size: 14.0, rgba: 0xffffffff, text: "hi".into() }
    }

    /// Stone 13.1 — two surfaces flatten in the order the caller
    /// passes. App below, overlay above → overlay's ops come last in
    /// the flattened stream, which the renderer paints on top.
    #[test]
    fn finish_frame_in_order_concatenates_in_caller_order() {
        let mut c = Compositor::new();
        let app = SurfaceId(10);
        let overlay = SurfaceId(20);
        c.begin_frame();
        c.surface_target(app).push(one_op());
        c.surface_target(overlay).push(other_op());

        let flat = c.finish_frame_in_order(&[app, overlay]);
        assert!(matches!(&flat.ops[0], PaintOp::FillRect { .. }));
        assert!(matches!(&flat.ops[1], PaintOp::DrawText { .. }));
        assert_eq!(flat.ops.len(), 2);
    }

    #[test]
    fn finish_frame_in_order_reverse_z_swaps_output() {
        let mut c = Compositor::new();
        let app = SurfaceId(10);
        let overlay = SurfaceId(20);
        c.begin_frame();
        c.surface_target(app).push(one_op());
        c.surface_target(overlay).push(other_op());

        // Reverse order → overlay's text appears first, app's rect last.
        let flat = c.finish_frame_in_order(&[overlay, app]);
        assert!(matches!(&flat.ops[0], PaintOp::DrawText { .. }));
        assert!(matches!(&flat.ops[1], PaintOp::FillRect { .. }));
    }

    #[test]
    fn destroy_surface_drops_buffer_and_subsequent_finish_skips_it() {
        let mut c = Compositor::new();
        let s = SurfaceId(7);
        c.surface_target(s).push(one_op());
        c.destroy_surface(s);
        let flat = c.finish_frame_in_order(&[s]);
        assert!(flat.ops.is_empty(), "destroyed surface must not contribute ops");
    }

    #[test]
    fn unknown_surface_in_order_is_silently_skipped() {
        // The WindowManager can list a surface the Compositor hasn't
        // seen yet (allocated this frame, hadn't drawn). Skip silently.
        let mut c = Compositor::new();
        let s = SurfaceId(7);
        c.surface_target(s).push(one_op());
        let flat = c.finish_frame_in_order(&[s, SurfaceId(999)]);
        assert_eq!(flat.ops.len(), 1);
    }

    #[test]
    fn begin_frame_clears_per_surface_buffers_keeps_them_allocated() {
        let mut c = Compositor::new();
        let s = SurfaceId(7);
        c.surface_target(s).push(one_op());
        c.begin_frame();
        assert!(c.surface_target(s).ops.is_empty(),
            "begin_frame should clear ops");
        // Surface is still tracked — it's just empty. Push again works
        // without a re-create cost (the inner Vec keeps its capacity).
        c.surface_target(s).push(other_op());
        let flat = c.finish_frame_in_order(&[s]);
        assert_eq!(flat.ops.len(), 1);
    }

    #[test]
    fn legacy_paint_target_and_finish_frame_round_trip() {
        // Stone 13.1 backward-compat — `Framework::render_frame`
        // paints into the default surface and calls `finish_frame`
        // without specifying an order. Exercise that path.
        let mut c = Compositor::new();
        c.begin_frame();
        c.paint_target().push(one_op());
        c.emit(other_op());  // `emit` also routes to default surface
        let flat = c.finish_frame();
        assert_eq!(flat.ops.len(), 2);
    }
}
