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

/// Phase 15.3 — per-composite effects. Applied by the renderer
/// during `compositeSurface` (drawImage of the surface's offscreen
/// onto the screen), wrapped in save/translate/scale/globalAlpha/
/// restore.
///
/// `IDENTITY` is the no-op default — `(opacity 1.0, translate 0/0,
/// scale 1.0)` — so every surface that isn't animating emits opts
/// that the renderer can fast-path through (skip the save/restore).
///
/// 15.3 ships with the four basic effects shared by every desktop /
/// mobile compositor; backdrop_blur, drop_shadow, and tint land with
/// the WebGPU compositor in 15.4 (Canvas2D's `filter: blur()` is too
/// slow for live blur, so 15.3 has no Canvas2D fallback for those —
/// they just don't render until 15.4).
/// Phase 15.4.4 — soft drop shadow under a surface. Encoded in
/// `SurfaceDrawOpts.drop_shadow`. `rgba` is the shadow colour packed
/// `0xAARRGGBB`; an alpha byte of `0` means "no shadow" (fast path).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DropShadow {
    pub offset_x:    f32,
    pub offset_y:    f32,
    pub blur_radius: f32,
    pub rgba:        u32,
}

impl DropShadow {
    pub const NONE: DropShadow = DropShadow {
        offset_x: 0.0, offset_y: 0.0, blur_radius: 0.0, rgba: 0,
    };

    /// `true` when the alpha byte is zero — the renderer skips the
    /// 4-pass shadow pipeline entirely.
    pub fn is_none(self) -> bool {
        ((self.rgba >> 24) & 0xff) == 0
    }
}

impl Default for DropShadow {
    fn default() -> Self { Self::NONE }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SurfaceDrawOpts {
    pub opacity: f32,
    pub translate_x: f32,
    pub translate_y: f32,
    pub scale: f32,
    /// Phase 15.4.3 — backdrop blur radius in screen pixels. `0.0` =
    /// no blur (fast path). The WebGPU compositor downsamples the
    /// pixels behind the surface, runs a two-pass Gaussian, and
    /// composites the result under the surface — frosted-glass for
    /// sheets / dialogs / nav bars. Canvas2D ignores (no live blur).
    pub backdrop_blur_radius: f32,
    /// Phase 15.4.4 — soft drop shadow under the surface. The
    /// WebGPU compositor renders the surface's alpha as a tinted
    /// silhouette, blurs it, and composites it offset under the
    /// surface — modal / popover shadows. `DropShadow::NONE` (alpha
    /// byte zero) is the fast-path default; Canvas2D ignores.
    pub drop_shadow: DropShadow,
    /// Phase 15.4.5 — colour tint blended over the surface's RGB.
    /// `0xAARRGGBB`; alpha byte `0` = no tint (fast path). The
    /// fragment shader does `mix(surf.rgb, tint.rgb, tint.a)` so a
    /// half-alpha black tint dims the surface 50%, matching macOS-
    /// style "background app fade" during multitasking transitions.
    /// Doesn't change surface alpha — the tint colours the visible
    /// pixels; it doesn't add transparency.
    pub tint: u32,
}

impl SurfaceDrawOpts {
    pub const IDENTITY: SurfaceDrawOpts = SurfaceDrawOpts {
        opacity: 1.0, translate_x: 0.0, translate_y: 0.0, scale: 1.0,
        backdrop_blur_radius: 0.0,
        drop_shadow: DropShadow::NONE,
        tint: 0,
    };

    /// True when the opts are exactly identity. Renderers can
    /// fast-path this to a single `drawImage` with no save/restore.
    /// Floating-point compare is exact: opts come from animation
    /// computations that emit literal `1.0` / `0.0` for rest, and
    /// `props_for` returns REST as a literal too — no rounding error
    /// path produces a near-identity-but-not.
    pub fn is_identity(self) -> bool {
        self.opacity == 1.0
            && self.translate_x == 0.0
            && self.translate_y == 0.0
            && self.scale == 1.0
            && self.backdrop_blur_radius == 0.0
            && self.drop_shadow.is_none()
            && (self.tint >> 24) & 0xff == 0
    }
}

impl Default for SurfaceDrawOpts {
    fn default() -> Self { Self::IDENTITY }
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
    /// Foundation gap — blit a drawable owned by **another** package.
    /// Looked up against the renderer's foreign-drawable cache by
    /// `(package_id, drawable_name)`. Used by the Shell to paint
    /// launcher icons; the package_id comes from
    /// `pkgs::PackageEntry.id` and the name from `entry.icon`. Empty
    /// either field paints the magenta missing-drawable placeholder
    /// (consistent fallback shape).
    DrawForeignImage { package_id: String, name: String, x: f32, y: f32, w: f32, h: f32 },
    /// Set the global alpha multiplier on the renderer. Persists until
    /// the next `SetGlobalAlpha` or until a `PopTransform` / `PopClip`
    /// restores the canvas state stack. Used by the animation pipeline
    /// to fade surfaces without rewriting every paint op's rgba —
    /// Canvas2D's `globalAlpha` is a free GPU-level multiplier, so a
    /// transition costs O(1) per frame instead of O(ops) per frame.
    SetGlobalAlpha { alpha: f32 },
    // Phase 15.1 — per-Activity surface compositor ops. The pre-15.1
    // model flattened every surface's ops into one stream and let the
    // single screen backend paint them in order. The new model gives
    // each surface its own backing OffscreenCanvas; these four ops are
    // the wire-level vocabulary that drives that.
    //
    // Frame shape produced by `finish_frame_in_order`:
    //   for each surface S in z-order:
    //       BeginSurface(S, bounds)
    //         ...S's paint ops...
    //       EndSurface
    //   for each surface S in z-order:
    //       CompositeSurface(S, bounds)
    //   [optional trailing DestroySurface(S) for any surface released
    //    since the previous frame — emitted exactly once]
    //
    // The renderer backend holds a Map<SurfaceId, surface canvas>;
    // BeginSurface routes subsequent paint ops to that surface's ctx,
    // CompositeSurface drawImages it into the screen at `bounds`.
    /// Switch the renderer's current paint target to surface `id`,
    /// allocating a backing OffscreenCanvas of size `(w, h)` if the
    /// renderer hasn't seen this id before. Subsequent paint ops paint
    /// into that surface's canvas, not the screen. `(x, y)` are the
    /// surface's bounds in screen-space — the renderer doesn't need
    /// them for paint (paint happens in surface-local coords) but does
    /// need them for `CompositeSurface`, so we carry them on every
    /// Begin to keep the compositor stateless.
    BeginSurface { id: u32, x: f32, y: f32, w: f32, h: f32 },
    /// Restore the renderer's paint target to the screen. Pairs with
    /// the most recent unmatched `BeginSurface`.
    EndSurface,
    /// Sample surface `id`'s backing canvas onto the current paint
    /// target (always the screen in v1) at `(x, y, w, h)`, with
    /// per-composite effects (`opts`). Implemented as
    /// `ctx.drawImage(surface.canvas, ...)` wrapped in
    /// translate/scale/globalAlpha — Canvas2D's drawImage with an
    /// OffscreenCanvas source is GPU-accelerated and effectively free
    /// vs. the texture-sample on a real GPU API.
    ///
    /// Phase 15.3 — `opts` carries the slide / fade animation state,
    /// applied during composite rather than by rewriting the
    /// activity's ops. Pre-15.3 `apply_props` wrapped the activity's
    /// own paint with `PushTransform`/`SetGlobalAlpha`/`PopTransform`,
    /// which translated content INSIDE the offscreen canvas — the
    /// edges past the surface bounds got clipped, breaking slide
    /// animations the moment per-Activity surfaces landed in 15.1.
    /// Compositor-level transform translates the entire offscreen
    /// during the screen composite, so slides preserve the activity's
    /// full content.
    CompositeSurface {
        id: u32, x: f32, y: f32, w: f32, h: f32,
        opts: SurfaceDrawOpts,
    },
    /// Free the renderer's backing canvas for surface `id`. Emitted
    /// once, at the next `finish_frame_in_order` after the framework
    /// calls `Compositor::destroy_surface`. Idempotent on the renderer
    /// side — duplicate destroys are a no-op rather than an error,
    /// because some test fixtures destroy a surface that never
    /// rendered.
    DestroySurface { id: u32 },
}

impl FrameCommands {
    pub fn new() -> Self { Self::default() }
    pub fn clear(&mut self) { self.ops.clear(); }
    pub fn push(&mut self, op: PaintOp) { self.ops.push(op); }
    pub fn ops(&self) -> &[PaintOp] { &self.ops }
}

/// Renderer-facing draw API. Kernel's `flush_primary` calls these in
/// the same order the SDK emitted them. The `extern "C"` HAL bindings
/// in the kernel implement this against the JS host; tests implement
/// it against a recording mock.
///
/// Why a trait, not direct extern calls: pre-Stone-13.3 the kernel's
/// dispatcher had `match op { PaintOp::PushTransform { .. } => {} }`
/// — silent no-ops with a comment claiming "Phase 2b honours these".
/// The compiler couldn't catch it (every variant was matched, nothing
/// was unused) and the JS side couldn't catch it (no HAL call ever
/// arrived). With `dispatch_paint_op` driven by this trait, a
/// `RecordingHal` test plus exhaustive matching ensures every
/// variant produces an observable call — adding a new variant
/// without an arm is a compile error, dropping a method into a no-op
/// flips the test red.
pub trait DrawHal {
    fn clear(&mut self, rgba: u32);
    fn fill_rect(&mut self, x: f32, y: f32, w: f32, h: f32, rgba: u32);
    fn fill_round_rect(&mut self, x: f32, y: f32, w: f32, h: f32, radius: f32, rgba: u32);
    fn draw_text(&mut self, x: f32, y: f32, size: f32, rgba: u32, text: &str);
    fn stroke_rect(&mut self, x: f32, y: f32, w: f32, h: f32, width: f32, rgba: u32);
    fn stroke_round_rect(
        &mut self, x: f32, y: f32, w: f32, h: f32, radius: f32, width: f32, rgba: u32,
    );
    fn draw_line(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, width: f32, rgba: u32);
    fn draw_image(&mut self, drawable_id: u32, x: f32, y: f32, w: f32, h: f32);
    fn draw_foreign_image(
        &mut self, package_id: &str, name: &str, x: f32, y: f32, w: f32, h: f32,
    );
    fn push_transform(&mut self, dx: f32, dy: f32, scale: f32);
    fn pop_transform(&mut self);
    fn push_clip(&mut self, x: f32, y: f32, w: f32, h: f32);
    fn pop_clip(&mut self);
    fn set_global_alpha(&mut self, alpha: f32);
    // Phase 15.1 — per-Activity surface ops. See PaintOp variants for
    // the frame shape and semantics.
    fn begin_surface(&mut self, id: u32, x: f32, y: f32, w: f32, h: f32);
    fn end_surface(&mut self);
    fn composite_surface(
        &mut self, id: u32, x: f32, y: f32, w: f32, h: f32,
        opts: SurfaceDrawOpts,
    );
    fn destroy_surface(&mut self, id: u32);
}

/// Dispatch one paint op to a `DrawHal`. Exhaustive `match` so a new
/// `PaintOp` variant without a matching arm is a compile error rather
/// than a silent runtime no-op.
pub fn dispatch_paint_op<H: DrawHal + ?Sized>(hal: &mut H, op: &PaintOp) {
    match op {
        PaintOp::Clear { rgba } => hal.clear(*rgba),
        PaintOp::FillRect { x, y, w, h, rgba } => hal.fill_rect(*x, *y, *w, *h, *rgba),
        PaintOp::FillRoundRect { x, y, w, h, radius, rgba } => {
            hal.fill_round_rect(*x, *y, *w, *h, *radius, *rgba);
        }
        PaintOp::DrawText { x, y, size, rgba, text } => {
            hal.draw_text(*x, *y, *size, *rgba, text);
        }
        PaintOp::StrokeRect { x, y, w, h, width, rgba } => {
            hal.stroke_rect(*x, *y, *w, *h, *width, *rgba);
        }
        PaintOp::StrokeRoundRect { x, y, w, h, radius, width, rgba } => {
            hal.stroke_round_rect(*x, *y, *w, *h, *radius, *width, *rgba);
        }
        PaintOp::DrawLine { x0, y0, x1, y1, width, rgba } => {
            hal.draw_line(*x0, *y0, *x1, *y1, *width, *rgba);
        }
        PaintOp::DrawImage { drawable_id, x, y, w, h } => {
            hal.draw_image(*drawable_id, *x, *y, *w, *h);
        }
        PaintOp::DrawForeignImage { package_id, name, x, y, w, h } => {
            hal.draw_foreign_image(package_id, name, *x, *y, *w, *h);
        }
        PaintOp::PushTransform { dx, dy, scale } => hal.push_transform(*dx, *dy, *scale),
        PaintOp::PopTransform => hal.pop_transform(),
        PaintOp::PushClip { x, y, w, h } => hal.push_clip(*x, *y, *w, *h),
        PaintOp::PopClip => hal.pop_clip(),
        PaintOp::SetGlobalAlpha { alpha } => hal.set_global_alpha(*alpha),
        PaintOp::BeginSurface { id, x, y, w, h } => hal.begin_surface(*id, *x, *y, *w, *h),
        PaintOp::EndSurface => hal.end_surface(),
        PaintOp::CompositeSurface { id, x, y, w, h, opts } => {
            hal.composite_surface(*id, *x, *y, *w, *h, *opts);
        }
        PaintOp::DestroySurface { id } => hal.destroy_surface(*id),
    }
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

use crate::window_manager::{Rect, SurfaceId};

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
    /// Phase 15.1 — surfaces released by `destroy_surface` since the
    /// last `finish_frame*` call. The renderer holds backing canvases
    /// for each live surface; we emit `PaintOp::DestroySurface(id)`
    /// once at the next finish so the renderer can free the
    /// OffscreenCanvas. Drained on each finish; never grows
    /// unbounded between frames in normal operation.
    pending_destroys: Vec<SurfaceId>,
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
        Self {
            surfaces: HashMap::new(),
            flat: FrameCommands::new(),
            pending_destroys: Vec::new(),
        }
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
    ///
    /// Phase 15.1 — also queues a wire-level `DestroySurface(id)` op
    /// to be emitted on the next `finish_frame_in_order`, so the
    /// renderer can free its backing OffscreenCanvas. Idempotent calls
    /// queue the destroy once per id (the renderer's own
    /// `destroy_surface` is itself idempotent, so emitting twice is
    /// harmless, but we deduplicate to keep the wire stream tight).
    pub fn destroy_surface(&mut self, id: SurfaceId) {
        let had_buffer = self.surfaces.remove(&id).is_some();
        if had_buffer && !self.pending_destroys.contains(&id) {
            self.pending_destroys.push(id);
        }
    }

    /// Phase 15.1 — flatten every listed surface's commands into one
    /// `FrameCommands` wrapped in per-surface BeginSurface/EndSurface
    /// markers, followed by CompositeSurface ops in z-order, followed
    /// by any pending DestroySurface ops accumulated since the last
    /// finish. Frame shape:
    ///
    /// ```text
    /// for each (id, bounds) in order:
    ///     BeginSurface(id, bounds)
    ///       ...id's paint ops...
    ///     EndSurface
    /// for each (id, bounds) in order:
    ///     CompositeSurface(id, bounds)
    /// for each id in pending_destroys:
    ///     DestroySurface(id)
    /// ```
    ///
    /// `order` is typically built from
    /// `WindowManager::surfaces_in_z_order()` zipped with each
    /// surface's `bounds`; surfaces in `order` that the compositor
    /// doesn't know about (allocated this frame, hadn't drawn) are
    /// silently skipped — both their Begin/End block and their
    /// Composite. This matches the pre-15.1 contract.
    ///
    /// The returned reference is to a buffer the compositor reuses
    /// across calls — it stays valid until the next `begin_frame` /
    /// `finish_frame_in_order` / `finish_frame` call invalidates it.
    pub fn finish_frame_in_order(
        &mut self, order: &[(SurfaceId, Rect, SurfaceDrawOpts)],
    ) -> &FrameCommands {
        self.flat.clear();
        // Per-surface paint passes. Phase 15.2 — emit Begin/End only
        // when the surface has fresh ops this frame. An empty buffer
        // means "the activity didn't paint this frame" (it's slow,
        // trapped, paused, etc.); we must NOT emit Begin/End in that
        // case because the renderer's `beginSurface` clears the
        // backing canvas, which would wipe the activity's last-known
        // pixels. With Begin/End suppressed, only the trailing
        // CompositeSurface fires for that surface and the renderer
        // drawImages the prior frame's pixels — the load-bearing
        // "frozen pane" property of the per-Activity surface model.
        //
        // `begin_frame` clears every surface's ops at the start of the
        // frame, so `fc.ops.is_empty()` reliably distinguishes "didn't
        // paint" from "painted, even if just a clear" — a paint always
        // produces at least one op.
        for (id, bounds, _opts) in order {
            if let Some(fc) = self.surfaces.get(id) {
                if fc.ops.is_empty() { continue; }
                self.flat.ops.push(PaintOp::BeginSurface {
                    id: id.0, x: bounds.x, y: bounds.y,
                    w: bounds.w, h: bounds.h,
                });
                // Clone — `PaintOp::DrawText` carries a `String` so
                // we can't just move ops without losing the per-surface
                // buffer for next frame. Cost is bounded by total ops
                // count, typically ≤ 200 even with 4 active surfaces.
                self.flat.ops.extend(fc.ops.iter().cloned());
                self.flat.ops.push(PaintOp::EndSurface);
            }
        }
        // Compositor pass — drawImage every surface in z-order onto
        // the screen, applying per-composite effects via `opts`. We
        // emit Composite even for surfaces that didn't paint this
        // frame, so a Paused activity's last-known pixels stay on
        // screen (the renderer's backing canvas retains the previous
        // frame's content). That's the load-bearing robustness
        // property of 15.1.
        for (id, bounds, opts) in order {
            self.flat.ops.push(PaintOp::CompositeSurface {
                id: id.0, x: bounds.x, y: bounds.y,
                w: bounds.w, h: bounds.h,
                opts: *opts,
            });
        }
        // Trailing destroys — the renderer can free its backing
        // canvas only after all composites complete (a destroyed
        // surface won't appear in `order`, but that's fine — the
        // destroy is the canonical "drop the texture" signal).
        for id in self.pending_destroys.drain(..) {
            self.flat.ops.push(PaintOp::DestroySurface { id: id.0 });
        }
        &self.flat
    }

    /// Backward-compat: flatten just the default surface, with zero
    /// bounds. Kept so pre-Phase-15 callers (`Framework::render_frame`
    /// fallback paths and tests) keep working without knowing about
    /// per-surface bounds. Bounds of zero are a sentinel the renderer
    /// interprets as "this is the screen-bound default surface, paint
    /// straight to the screen with no offscreen allocation"; matched
    /// by an equally zero-bounds CompositeSurface that's a no-op.
    pub fn finish_frame(&mut self) -> &FrameCommands {
        self.finish_frame_in_order(&[(
            Self::DEFAULT_SURFACE, Rect::ZERO, SurfaceDrawOpts::IDENTITY,
        )])
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

    fn r(x: f32, y: f32, w: f32, h: f32) -> Rect { Rect { x, y, w, h } }

    /// Helper for tests — pair a surface id with bounds and identity
    /// opts. Most tests don't exercise opts; this keeps the call
    /// sites readable. Phase 15.3 added the third tuple element.
    fn triple(id: SurfaceId, rect: Rect) -> (SurfaceId, Rect, SurfaceDrawOpts) {
        (id, rect, SurfaceDrawOpts::IDENTITY)
    }

    /// Phase 15.1 — two surfaces emit Begin/ops/End blocks in caller
    /// order, then Composite ops in caller order. App below, overlay
    /// above → overlay's content composites last, painted on top of
    /// the app at flush time.
    #[test]
    fn finish_frame_in_order_emits_per_surface_blocks_then_composites() {
        let mut c = Compositor::new();
        let app = SurfaceId(10);
        let overlay = SurfaceId(20);
        c.begin_frame();
        c.surface_target(app).push(one_op());
        c.surface_target(overlay).push(other_op());

        let flat = c.finish_frame_in_order(&[
            triple(app, r(0.0, 0.0, 100.0, 100.0)),
            triple(overlay, r(10.0, 10.0, 50.0, 50.0)),
        ]);
        assert!(matches!(flat.ops[0], PaintOp::BeginSurface { id: 10, .. }));
        assert!(matches!(flat.ops[1], PaintOp::FillRect { .. }));
        assert!(matches!(flat.ops[2], PaintOp::EndSurface));
        assert!(matches!(flat.ops[3], PaintOp::BeginSurface { id: 20, .. }));
        assert!(matches!(flat.ops[4], PaintOp::DrawText { .. }));
        assert!(matches!(flat.ops[5], PaintOp::EndSurface));
        assert!(matches!(flat.ops[6], PaintOp::CompositeSurface { id: 10, .. }));
        assert!(matches!(flat.ops[7], PaintOp::CompositeSurface { id: 20, .. }));
        assert_eq!(flat.ops.len(), 8);
    }

    #[test]
    fn finish_frame_in_order_reverse_z_swaps_composite_order() {
        let mut c = Compositor::new();
        let app = SurfaceId(10);
        let overlay = SurfaceId(20);
        c.begin_frame();
        c.surface_target(app).push(one_op());
        c.surface_target(overlay).push(other_op());

        // Reverse order → overlay composites first, app last.
        let flat = c.finish_frame_in_order(&[
            triple(overlay, r(0.0, 0.0, 50.0, 50.0)),
            triple(app, r(0.0, 0.0, 100.0, 100.0)),
        ]);
        // Last two ops are the Composites in caller order.
        let n = flat.ops.len();
        assert!(matches!(flat.ops[n - 2], PaintOp::CompositeSurface { id: 20, .. }));
        assert!(matches!(flat.ops[n - 1], PaintOp::CompositeSurface { id: 10, .. }));
    }

    #[test]
    fn destroy_surface_emits_destroy_op_on_next_finish() {
        let mut c = Compositor::new();
        let s = SurfaceId(7);
        c.surface_target(s).push(one_op());
        c.destroy_surface(s);
        let flat = c.finish_frame_in_order(&[triple(s, r(0.0, 0.0, 1.0, 1.0))]);
        // Surface gone → no Begin/End block, no Composite-with-content.
        // We do still emit the trailing Composite for `s` (the renderer
        // is told "the texture should land at these bounds") but the
        // surface is unknown to the renderer at composite time so the
        // op is a no-op there. The DestroySurface op trails the
        // composite list, freeing the renderer's canvas.
        assert!(!flat.ops.iter().any(|op| matches!(op, PaintOp::BeginSurface { .. })));
        assert!(flat.ops.iter().any(|op| matches!(op, PaintOp::DestroySurface { id: 7 })));
        // A second finish must not re-emit DestroySurface(7).
        let flat = c.finish_frame_in_order(&[]);
        assert!(!flat.ops.iter().any(|op| matches!(op, PaintOp::DestroySurface { id: 7 })));
    }

    #[test]
    fn unknown_surface_in_order_is_silently_skipped_for_paint() {
        // The WindowManager can list a surface the Compositor hasn't
        // seen yet (allocated this frame, hadn't drawn). Skip the
        // Begin/End block; Composite still emits so the renderer can
        // composite if it has its own backing texture from a prior
        // frame (Phase 15.2 robustness — last-known pixels stay
        // visible).
        let mut c = Compositor::new();
        let s = SurfaceId(7);
        c.surface_target(s).push(one_op());
        let flat = c.finish_frame_in_order(&[
            triple(s, r(0.0, 0.0, 100.0, 100.0)),
            triple(SurfaceId(999), r(0.0, 0.0, 100.0, 100.0)),
        ]);
        let begin_count = flat.ops.iter()
            .filter(|op| matches!(op, PaintOp::BeginSurface { .. })).count();
        let composite_count = flat.ops.iter()
            .filter(|op| matches!(op, PaintOp::CompositeSurface { .. })).count();
        assert_eq!(begin_count, 1, "only the surface with ops emits a Begin");
        assert_eq!(composite_count, 2, "every listed surface composites");
    }

    /// Phase 15.2 — load-bearing "frozen pane" property. A surface
    /// whose Activity didn't paint this frame must NOT emit Begin/End
    /// (which would clear the renderer's backing canvas), but must
    /// STILL emit CompositeSurface (so the renderer drawImages the
    /// last-known pixels). Without this guard, a slow / trapped /
    /// paused Activity would paint a black region every frame.
    #[test]
    fn empty_surface_skips_begin_end_but_still_composites() {
        let mut c = Compositor::new();
        let s = SurfaceId(7);
        // Frame 1: Activity painted normally.
        c.begin_frame();
        c.surface_target(s).push(one_op());
        let _ = c.finish_frame_in_order(&[triple(s, r(0.0, 0.0, 100.0, 100.0))]);

        // Frame 2: Activity didn't paint. begin_frame clears the
        // surface's ops vec; the buffer entry stays in `surfaces`.
        c.begin_frame();
        let flat = c.finish_frame_in_order(&[triple(s, r(0.0, 0.0, 100.0, 100.0))]);
        let begin_count = flat.ops.iter()
            .filter(|op| matches!(op, PaintOp::BeginSurface { .. })).count();
        let end_count = flat.ops.iter()
            .filter(|op| matches!(op, PaintOp::EndSurface)).count();
        let composite_count = flat.ops.iter()
            .filter(|op| matches!(op, PaintOp::CompositeSurface { .. })).count();
        assert_eq!(begin_count, 0,
            "empty-ops surface must not emit BeginSurface (would clear canvas)");
        assert_eq!(end_count, 0, "no Begin → no End");
        assert_eq!(composite_count, 1,
            "Composite still fires so the renderer drawImages last pixels");
    }

    /// Phase 15.2 — Begin/End suppression triggers per-surface, not
    /// per-frame. In a frame where surface A painted and surface B
    /// didn't, A must emit a Begin/End block and B must not — both
    /// composite at the end.
    #[test]
    fn finish_frame_emits_begin_end_only_for_surfaces_with_fresh_ops() {
        let mut c = Compositor::new();
        let a = SurfaceId(10);
        let b = SurfaceId(20);
        // Pre-allocate B's buffer (e.g. from a prior frame's paint)
        // so it sits in `self.surfaces` with empty ops in this frame.
        c.surface_target(b).push(one_op());
        let _ = c.finish_frame_in_order(&[triple(b, r(0.0, 0.0, 100.0, 100.0))]);

        c.begin_frame();
        c.surface_target(a).push(one_op());
        // Don't push to B — it stays empty this frame.
        let flat = c.finish_frame_in_order(&[
            triple(a, r(0.0, 0.0, 100.0, 100.0)),
            triple(b, r(0.0, 0.0, 50.0, 50.0)),
        ]);
        // Begin/End block only for A.
        let begin_ids: Vec<u32> = flat.ops.iter().filter_map(|op| match op {
            PaintOp::BeginSurface { id, .. } => Some(*id),
            _ => None,
        }).collect();
        assert_eq!(begin_ids, vec![10], "only the painting surface emits Begin");
        // Composite for both, in caller order.
        let composite_ids: Vec<u32> = flat.ops.iter().filter_map(|op| match op {
            PaintOp::CompositeSurface { id, .. } => Some(*id),
            _ => None,
        }).collect();
        assert_eq!(composite_ids, vec![10, 20]);
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
        let flat = c.finish_frame_in_order(&[triple(s, r(0.0, 0.0, 1.0, 1.0))]);
        // Begin, the op, End, Composite → 4 ops total.
        assert_eq!(flat.ops.len(), 4);
    }

    /// Recording `DrawHal` for the dispatch test. Every method appends
    /// a stable string tag so the test reads as a flat assertion list.
    #[derive(Default)]
    struct RecordingHal { trace: Vec<String> }

    impl DrawHal for RecordingHal {
        fn clear(&mut self, rgba: u32) {
            self.trace.push(format!("clear({rgba:#x})"));
        }
        fn fill_rect(&mut self, x: f32, y: f32, w: f32, h: f32, rgba: u32) {
            self.trace.push(format!("fill_rect({x},{y},{w},{h},{rgba:#x})"));
        }
        fn fill_round_rect(&mut self, x: f32, y: f32, w: f32, h: f32, radius: f32, rgba: u32) {
            self.trace.push(format!("fill_round_rect({x},{y},{w},{h},{radius},{rgba:#x})"));
        }
        fn draw_text(&mut self, x: f32, y: f32, size: f32, rgba: u32, text: &str) {
            self.trace.push(format!("draw_text({x},{y},{size},{rgba:#x},{text:?})"));
        }
        fn stroke_rect(&mut self, x: f32, y: f32, w: f32, h: f32, width: f32, rgba: u32) {
            self.trace.push(format!("stroke_rect({x},{y},{w},{h},{width},{rgba:#x})"));
        }
        fn stroke_round_rect(
            &mut self, x: f32, y: f32, w: f32, h: f32, radius: f32, width: f32, rgba: u32,
        ) {
            self.trace.push(format!(
                "stroke_round_rect({x},{y},{w},{h},{radius},{width},{rgba:#x})"
            ));
        }
        fn draw_line(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, width: f32, rgba: u32) {
            self.trace.push(format!("draw_line({x0},{y0},{x1},{y1},{width},{rgba:#x})"));
        }
        fn draw_image(&mut self, drawable_id: u32, x: f32, y: f32, w: f32, h: f32) {
            self.trace.push(format!("draw_image({drawable_id},{x},{y},{w},{h})"));
        }
        fn draw_foreign_image(
            &mut self, package_id: &str, name: &str, x: f32, y: f32, w: f32, h: f32,
        ) {
            self.trace.push(format!("draw_foreign_image({package_id:?},{name:?},{x},{y},{w},{h})"));
        }
        fn push_transform(&mut self, dx: f32, dy: f32, scale: f32) {
            self.trace.push(format!("push_transform({dx},{dy},{scale})"));
        }
        fn pop_transform(&mut self) { self.trace.push("pop_transform".into()); }
        fn push_clip(&mut self, x: f32, y: f32, w: f32, h: f32) {
            self.trace.push(format!("push_clip({x},{y},{w},{h})"));
        }
        fn pop_clip(&mut self) { self.trace.push("pop_clip".into()); }
        fn set_global_alpha(&mut self, alpha: f32) {
            self.trace.push(format!("set_global_alpha({alpha})"));
        }
        fn begin_surface(&mut self, id: u32, x: f32, y: f32, w: f32, h: f32) {
            self.trace.push(format!("begin_surface({id},{x},{y},{w},{h})"));
        }
        fn end_surface(&mut self) { self.trace.push("end_surface".into()); }
        fn composite_surface(
            &mut self, id: u32, x: f32, y: f32, w: f32, h: f32, opts: SurfaceDrawOpts,
        ) {
            self.trace.push(format!(
                "composite_surface({id},{x},{y},{w},{h},opacity={},tx={},ty={},scale={},blur={},shadow={:#010x},tint={:#010x})",
                opts.opacity, opts.translate_x, opts.translate_y, opts.scale,
                opts.backdrop_blur_radius,
                opts.drop_shadow.rgba,
                opts.tint,
            ));
        }
        fn destroy_surface(&mut self, id: u32) {
            self.trace.push(format!("destroy_surface({id})"));
        }
    }

    /// Catches the Stone 13.3 regression by construction: every PaintOp
    /// variant must produce a non-empty, distinct trace through
    /// `dispatch_paint_op`. A future "silent no-op" arm (the bug fixed
    /// in db77f30) would either drop an entry or duplicate one — both
    /// fail the assertion.
    #[test]
    fn dispatch_paint_op_routes_every_variant_to_a_unique_hal_method() {
        let ops = [
            PaintOp::Clear { rgba: 0xff112233 },
            PaintOp::FillRect { x: 1.0, y: 2.0, w: 3.0, h: 4.0, rgba: 0xff112233 },
            PaintOp::FillRoundRect {
                x: 1.0, y: 2.0, w: 3.0, h: 4.0, radius: 5.0, rgba: 0xff112233,
            },
            PaintOp::DrawText {
                x: 1.0, y: 2.0, size: 12.0, rgba: 0xff112233, text: "hi".into(),
            },
            PaintOp::StrokeRect {
                x: 1.0, y: 2.0, w: 3.0, h: 4.0, width: 1.0, rgba: 0xff112233,
            },
            PaintOp::StrokeRoundRect {
                x: 1.0, y: 2.0, w: 3.0, h: 4.0, radius: 5.0, width: 1.0, rgba: 0xff112233,
            },
            PaintOp::DrawLine {
                x0: 1.0, y0: 2.0, x1: 3.0, y1: 4.0, width: 1.0, rgba: 0xff112233,
            },
            PaintOp::DrawImage { drawable_id: 7, x: 1.0, y: 2.0, w: 3.0, h: 4.0 },
            PaintOp::DrawForeignImage {
                package_id: "com.akiva.calc".into(),
                name: "launcher".into(),
                x: 5.0, y: 6.0, w: 7.0, h: 8.0,
            },
            PaintOp::PushTransform { dx: 10.0, dy: 0.0, scale: 1.0 },
            PaintOp::PopTransform,
            PaintOp::PushClip { x: 1.0, y: 2.0, w: 3.0, h: 4.0 },
            PaintOp::PopClip,
            PaintOp::SetGlobalAlpha { alpha: 0.5 },
            PaintOp::BeginSurface { id: 42, x: 1.0, y: 2.0, w: 100.0, h: 200.0 },
            PaintOp::EndSurface,
            PaintOp::CompositeSurface {
                id: 42, x: 1.0, y: 2.0, w: 100.0, h: 200.0,
                opts: SurfaceDrawOpts::IDENTITY,
            },
            PaintOp::DestroySurface { id: 42 },
        ];
        let mut hal = RecordingHal::default();
        for op in &ops {
            dispatch_paint_op(&mut hal, op);
        }
        assert_eq!(hal.trace, vec![
            "clear(0xff112233)",
            "fill_rect(1,2,3,4,0xff112233)",
            "fill_round_rect(1,2,3,4,5,0xff112233)",
            "draw_text(1,2,12,0xff112233,\"hi\")",
            "stroke_rect(1,2,3,4,1,0xff112233)",
            "stroke_round_rect(1,2,3,4,5,1,0xff112233)",
            "draw_line(1,2,3,4,1,0xff112233)",
            "draw_image(7,1,2,3,4)",
            "draw_foreign_image(\"com.akiva.calc\",\"launcher\",5,6,7,8)",
            "push_transform(10,0,1)",
            "pop_transform",
            "push_clip(1,2,3,4)",
            "pop_clip",
            "set_global_alpha(0.5)",
            "begin_surface(42,1,2,100,200)",
            "end_surface",
            "composite_surface(42,1,2,100,200,opacity=1,tx=0,ty=0,scale=1,blur=0,shadow=0x00000000,tint=0x00000000)",
            "destroy_surface(42)",
        ]);
    }

    #[test]
    fn legacy_paint_target_and_finish_frame_round_trip() {
        // Stone 13.1 backward-compat — `Framework::render_frame`
        // paints into the default surface and calls `finish_frame`
        // without specifying an order. Phase 15.1 wraps the same
        // ops in Begin/End/Composite for the default surface (with
        // zero-bounds, sentinel for "screen-bound"); the renderer
        // interprets a zero-bounds Begin as paint-to-screen.
        let mut c = Compositor::new();
        c.begin_frame();
        c.paint_target().push(one_op());
        c.emit(other_op());  // `emit` also routes to default surface
        let flat = c.finish_frame();
        // Begin, two ops, End, Composite → 5 ops.
        assert_eq!(flat.ops.len(), 5);
        assert!(matches!(flat.ops[0], PaintOp::BeginSurface { .. }));
        assert!(matches!(flat.ops[3], PaintOp::EndSurface));
        assert!(matches!(flat.ops[4], PaintOp::CompositeSurface { .. }));
    }
}
