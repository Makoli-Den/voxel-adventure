//! Which way the depth buffer runs ([`DepthMode`]) and what that means for the game: the
//! projection it must build and how far it may draw.
//!
//! Decision 0015 draws terrain thousands of blocks away. With the standard mapping (near at
//! depth 0, far at 1) the depth step at distance `d` is about `d^2 * 2^-24 / near` blocks,
//! 2.5 blocks at 2048, so far hills and skirts would fight. **Reversed Z** on a 32-bit float
//! depth buffer (near at 1, far at 0, the buffer cleared to 0 and tested with "greater")
//! spends the float's fine steps near 0 on the far distance, which makes the step nearly
//! proportional to the distance. The renderer picks it whenever the device has a
//! `D32_SFLOAT` depth buffer and reports the choice, so the game builds the matching
//! projection. Pure data with no graphics API, so it is unit tested.

/// The farthest the terrain may reach, in blocks, on a device that keeps the standard depth
/// mapping (no 32-bit float depth buffer, only 24-bit fixed point, where reversed Z gains
/// nothing). The depth step there is `1024^2 * 2^-24 / 0.1`, about 0.63 blocks with the
/// game's near plane of 0.1: still under a block, so a hill one block behind another does
/// not fight with it, where 2048 blocks would give 2.5 (decision 0015, "Depth precision").
pub const D24_MAX_HORIZON: u32 = 1024;

/// How the renderer maps distance to depth, as [`crate::Renderer::depth_mode`] reports it.
///
/// The game must build its projection to match (see [`crate::CameraView::view_projection`]):
/// with [`DepthMode::Standard`] the near plane maps to depth 0 and the far plane to 1, with
/// [`DepthMode::Reversed`] the near plane maps to 1 and the far plane to 0 (the same
/// perspective with near and far swapped).
///
/// ```
/// use vox_render::{D24_MAX_HORIZON, DepthMode};
///
/// assert_eq!(DepthMode::Reversed.max_horizon(), None);
/// assert_eq!(DepthMode::Standard.max_horizon(), Some(D24_MAX_HORIZON));
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DepthMode {
    /// Near at depth 0, far at 1; nearer fragments have smaller depth. Used on devices without
    /// a 32-bit float depth buffer; the horizon is capped at [`D24_MAX_HORIZON`].
    Standard,
    /// Near at depth 1, far at 0; nearer fragments have greater depth. Used with a 32-bit
    /// float depth buffer; precise enough for any horizon of decision 0015.
    Reversed,
}

impl DepthMode {
    /// The depth the buffer is cleared to: that of the far plane, so anything drawn passes.
    pub const fn far_depth(self) -> f32 {
        match self {
            Self::Standard => 1.0,
            Self::Reversed => 0.0,
        }
    }

    /// The farthest distance in blocks the game may draw terrain at with this mode, or `None`
    /// when depth precision sets no limit below the horizons of decision 0015.
    pub const fn max_horizon(self) -> Option<u32> {
        match self {
            Self::Standard => Some(D24_MAX_HORIZON),
            Self::Reversed => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_buffer_is_cleared_to_the_far_plane() {
        assert_eq!(DepthMode::Standard.far_depth(), 1.0);
        assert_eq!(DepthMode::Reversed.far_depth(), 0.0);
    }

    #[test]
    fn only_the_standard_mode_caps_the_horizon() {
        assert_eq!(DepthMode::Standard.max_horizon(), Some(1024));
        assert_eq!(DepthMode::Reversed.max_horizon(), None);
    }

    #[test]
    fn the_d24_cap_keeps_the_depth_step_under_a_block() {
        // Standard depth with 24 bits and the game's near plane of 0.1: the step at the cap.
        let near = 0.1_f64;
        let distance = f64::from(D24_MAX_HORIZON);
        let step = distance * distance * 2.0_f64.powi(-24) / near;
        assert!(step < 1.0, "{step}");
    }
}
