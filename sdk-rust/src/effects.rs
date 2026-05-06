//! Surface compositor effects — the per-Activity equivalent of CSS
//! `backdrop-filter`, `box-shadow`, and tinting filters, exposed at
//! Phase 15.4.
//!
//! An Activity sets its surface effects by calling
//! [`crate::activity::Context::set_surface_effects`]. The values
//! persist on the framework side until the next call (or until the
//! activity is destroyed); apps don't need to re-set them every
//! frame. Mirror of the framework's
//! `akiva_framework::compositor::SurfaceDrawOpts` effect fields —
//! the SDK can't depend on the kernel-side crate, so the duplication
//! is the price of the layering. A workspace-level Rust test pins
//! the wire format size at 24 bytes so a drift here would be
//! caught immediately.
//!
//! Canvas2D backends silently ignore every effect (no live blur or
//! drop-shadow at 60Hz); the WebGPU compositor implements all three
//! via separable Gaussian blur + alpha-tint silhouettes (see Phase
//! 15.4.3 / 15.4.4 / 15.4.5).

/// Drop-shadow descriptor. `rgba` is `0xAARRGGBB`; alpha byte `0`
/// disables the shadow without other fields mattering.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct DropShadow {
    pub offset_x:    f32,
    pub offset_y:    f32,
    pub blur_radius: f32,
    pub rgba:        u32,
}

impl DropShadow {
    /// `true` when the alpha byte is zero — the renderer skips the
    /// shadow pipeline entirely (4-pass cost saved).
    pub fn is_none(self) -> bool {
        ((self.rgba >> 24) & 0xff) == 0
    }
}

/// All compositor effects for a single Activity surface, bundled so
/// callers set everything in one round-trip.
///
/// Builder-style helpers (`with_*`) let callers set just what they
/// care about:
/// ```ignore
/// ctx.set_surface_effects(
///     SurfaceEffects::default()
///         .with_backdrop_blur(20.0)
///         .with_drop_shadow(DropShadow {
///             offset_y: 4.0, blur_radius: 16.0, rgba: 0x40_00_00_00,
///             ..Default::default()
///         }),
/// );
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct SurfaceEffects {
    /// Frosted-glass blur radius in screen pixels. `0.0` = no blur
    /// (fast path). 15.4.3 implements via 1/4-resolution two-pass
    /// Gaussian on the WebGPU compositor.
    pub backdrop_blur_radius: f32,
    /// Soft drop shadow. [`DropShadow::default`] disables.
    pub drop_shadow: DropShadow,
    /// `0xAARRGGBB` colour blended over the surface RGB (alpha byte
    /// = mix factor). `0` = no tint. 15.4.5.
    pub tint: u32,
}

impl SurfaceEffects {
    /// Length of [`Self::encode`]'s output. Pinned at 24 bytes.
    /// The `set_surface_effects` host op prepends a 4-byte handle
    /// for a 28-byte total payload.
    pub const ENCODED_LEN: usize = 24;

    pub const NONE: Self = Self {
        backdrop_blur_radius: 0.0,
        drop_shadow: DropShadow {
            offset_x: 0.0, offset_y: 0.0, blur_radius: 0.0, rgba: 0,
        },
        tint: 0,
    };

    pub fn with_backdrop_blur(mut self, radius: f32) -> Self {
        self.backdrop_blur_radius = radius;
        self
    }

    pub fn with_drop_shadow(mut self, shadow: DropShadow) -> Self {
        self.drop_shadow = shadow;
        self
    }

    pub fn with_tint(mut self, rgba: u32) -> Self {
        self.tint = rgba;
        self
    }

    /// Wire format (24 bytes, all little-endian):
    ///   [backdrop_blur_radius: f32]      // 4
    ///   [shadow.offset_x: f32]           // 4
    ///   [shadow.offset_y: f32]           // 4
    ///   [shadow.blur_radius: f32]        // 4
    ///   [shadow.rgba: u32]               // 4
    ///   [tint: u32]                      // 4
    pub fn encode(&self) -> [u8; Self::ENCODED_LEN] {
        let mut out = [0u8; Self::ENCODED_LEN];
        out[0..4].copy_from_slice(&self.backdrop_blur_radius.to_le_bytes());
        out[4..8].copy_from_slice(&self.drop_shadow.offset_x.to_le_bytes());
        out[8..12].copy_from_slice(&self.drop_shadow.offset_y.to_le_bytes());
        out[12..16].copy_from_slice(&self.drop_shadow.blur_radius.to_le_bytes());
        out[16..20].copy_from_slice(&self.drop_shadow.rgba.to_le_bytes());
        out[20..24].copy_from_slice(&self.tint.to_le_bytes());
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encoded_len_matches_wire_size() {
        let bytes = SurfaceEffects::NONE.encode();
        assert_eq!(bytes.len(), 24);
    }

    #[test]
    fn none_encodes_to_all_zeros() {
        assert_eq!(SurfaceEffects::NONE.encode(), [0u8; 24]);
    }

    #[test]
    fn with_builders_compose() {
        let e = SurfaceEffects::default()
            .with_backdrop_blur(20.0)
            .with_tint(0x80_00_00_00);
        assert_eq!(e.backdrop_blur_radius, 20.0);
        assert_eq!(e.tint, 0x80_00_00_00);
        assert!(e.drop_shadow.is_none());
    }

    #[test]
    fn drop_shadow_is_none_when_alpha_byte_is_zero() {
        assert!(DropShadow::default().is_none());
        assert!(DropShadow { rgba: 0x00_ff_ff_ff, ..Default::default() }.is_none());
        assert!(!DropShadow { rgba: 0x01_00_00_00, ..Default::default() }.is_none());
    }
}
