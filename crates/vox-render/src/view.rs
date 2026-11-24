//! What the renderer is told about the viewer: the camera ([`CameraView`]) and the fog
//! ([`Fog`]), and the camera-relative math built on them.
//!
//! Everything is drawn relative to the camera (decision 0015): the renderer subtracts the
//! camera position from each chunk origin on the CPU, and the camera's matrix has no
//! translation. The GPU then only sees small numbers, so `f32` keeps sub-block precision
//! however far the camera travels from the world origin, and vertices never wobble against
//! each other. Pure math with no graphics API, so it is unit tested.
//!
//! The matrix comes without translation from the caller instead of being derived here as
//! `view_projection * translation(position)` from a full matrix: in a full `f32` matrix the
//! translation column holds products of the position, rounded to its precision, and the
//! projection's small depth term (about `-near`) drowns in that rounding, so the far plane
//! would move by a quarter at a million blocks (625 instead of 500 in a test below). A matrix
//! built around the origin has nothing to cancel.

/// The camera a frame is drawn from, as [`crate::Renderer::set_camera`] takes it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CameraView {
    /// The combined view-projection matrix **of the camera placed at the world origin**
    /// (rotation and projection only, no translation), column-major, exactly
    /// `glam::Mat4::to_cols_array_2d()`. It maps an offset from [`Self::position`] in
    /// blocks to Vulkan clip space (Y down, depth 0..1), for example
    /// `projection * look_at(Vec3::ZERO, forward, Vec3::Y)`.
    pub view_projection: [[f32; 4]; 4],
    /// Where the eye is, in blocks: the point offsets for [`Self::view_projection`] are
    /// taken from, and the point fog distances are measured from.
    ///
    /// `f32`, the type the game keeps its camera in: a wider type here would promise a
    /// precision the caller does not have. The renderer computes every difference to a
    /// chunk origin (whole blocks) in `f64`, so the only rounding is that of this position
    /// itself (1/128 block at 65,536 blocks from the origin). That rounding moves the whole
    /// picture by the same tiny amount, it does not move vertices against each other.
    pub position: [f32; 3],
}

impl CameraView {
    /// The view before the first [`crate::Renderer::set_camera`]: the identity matrix at the
    /// world origin.
    pub(crate) const IDENTITY: Self = Self {
        view_projection: [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ],
        position: [0.0; 3],
    };

    /// Where the world point `point` (whole blocks, such as a chunk origin) lies relative
    /// to the camera, in blocks. The difference is exact in `f64` and rounded once to `f32`.
    // HOT PATH: runs for every chunk every frame; three subtractions.
    pub(crate) fn relative(&self, point: [i32; 3]) -> [f32; 3] {
        std::array::from_fn(|axis| (f64::from(point[axis]) - f64::from(self.position[axis])) as f32)
    }
}

/// Distance fog, as [`crate::Renderer::set_fog`] takes it: faces fade into the sky colour
/// between [`Self::start`] and [`Self::end`], measured in blocks as the straight-line
/// distance from the camera.
///
/// The fraction of the colour replaced by the sky is `smoothstep(start, end, distance)`:
/// none before `start`, all of it from `end` on, with a soft beginning and end.
///
/// ```
/// use vox_render::Fog;
///
/// let fog = Fog { start: 100.0, end: 200.0 };
/// assert!(fog.is_enabled());
/// assert!(!Fog::OFF.is_enabled());
/// assert!(!Fog { start: 200.0, end: 100.0 }.is_enabled());
/// ```
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Fog {
    /// Where the fog begins, in blocks from the camera.
    pub start: f32,
    /// Where the fog is complete, in blocks from the camera. Fog is off unless this is
    /// greater than [`Self::start`].
    pub end: f32,
}

impl Fog {
    /// No fog: the renderer's state until [`crate::Renderer::set_fog`] is called.
    pub const OFF: Self = Self {
        start: 0.0,
        end: 0.0,
    };

    /// Whether this fog does anything: both distances are finite numbers and `end` lies
    /// beyond `start` by a span whose inverse is finite too (the shader multiplies by it).
    pub fn is_enabled(&self) -> bool {
        let span = self.end - self.start;
        span > 0.0 && span.is_finite() && span.recip().is_finite() && self.start.is_finite()
    }

    /// The fog `vec4` of the chunk push constants (`crate::interface::chunk`): start, end,
    /// `1 / (end - start)` and 0, or all zeros when the fog is off.
    pub(crate) fn push_values(&self) -> [f32; 4] {
        if !self.is_enabled() {
            return [0.0; 4];
        }
        [self.start, self.end, (self.end - self.start).recip(), 0.0]
    }
}

#[cfg(test)]
mod tests {
    use glam::{Mat4, Vec3, Vec4};

    use super::*;
    use crate::frustum::Frustum;

    /// The fraction of a colour the built-in `chunk.frag` replaces by the fog colour, written
    /// the same way from the pushed values, to pin down what the values mean. The shader's
    /// expression is pinned to this copy by a test in `shader/mod.rs`.
    fn fog_amount(values: [f32; 4], distance: f32) -> f32 {
        let t = ((distance - values[0]) * values[2]).clamp(0.0, 1.0);
        t * t * (3.0 - 2.0 * t)
    }

    /// Distance of the far plane of the test cameras, in blocks.
    const FAR: f32 = 500.0;

    /// The projection of the test cameras: 90 degrees field of view, square window, near
    /// 0.1, far [`FAR`], as the game builds it (glam, Vulkan clip space).
    fn projection() -> Mat4 {
        glam::camera::rh::proj::vulkan::perspective(std::f32::consts::FRAC_PI_2, 1.0, 0.1, FAR)
    }

    /// A camera at `eye` looking along -Z, with its matrix built around the origin as
    /// [`CameraView::view_projection`] asks.
    fn camera_at(eye: Vec3) -> CameraView {
        let view = glam::camera::rh::view::look_at_mat4(Vec3::ZERO, Vec3::NEG_Z, Vec3::Y);
        CameraView {
            view_projection: (projection() * view).to_cols_array_2d(),
            position: eye.to_array(),
        }
    }

    /// The distance at which the far plane of a column-major Vulkan projection lies along
    /// the view axis: where clip `z` reaches clip `w`.
    fn far_plane_distance(matrix: Mat4) -> f32 {
        // Along the view axis (-Z), z_clip = a * d + b and w_clip = c * d + e at distance d.
        let row = |i: usize| matrix.row(i);
        let along = |r: Vec4| (-r.z, r.w);
        let (a, b) = along(row(2));
        let (c, e) = along(row(3));
        (e - b) / (a - c)
    }

    #[test]
    fn a_full_f32_matrix_loses_the_far_plane_a_million_blocks_out() {
        // Why the caller passes the matrix without translation: removing the translation
        // from a full matrix afterwards, even in f64, cannot bring back what f32 rounded.
        let eye = Vec3::new(1_048_576.0, 60.0, -1_048_576.0);
        let full =
            projection() * glam::camera::rh::view::look_at_mat4(eye, eye + Vec3::NEG_Z, Vec3::Y);
        let m = full.to_cols_array_2d();
        let mut removed = m;
        for (row, value) in removed[3].iter_mut().enumerate() {
            let sum: f64 = (0..3)
                .map(|column| f64::from(m[column][row]) * f64::from(eye[column]))
                .sum();
            *value = (sum + f64::from(m[3][row])) as f32;
        }
        let lost = far_plane_distance(Mat4::from_cols_array_2d(&removed));
        // 625 is what this glam version and this position give; the point is "far off".
        assert!((lost - 625.0).abs() < 1.0, "far plane at {lost}");
        let kept = far_plane_distance(Mat4::from_cols_array_2d(&camera_at(eye).view_projection));
        assert!((kept - FAR).abs() < 1.0, "far plane at {kept}");
    }

    #[test]
    fn relative_origins_are_exact_far_from_the_world_origin() {
        let camera = CameraView {
            position: [1_000_000.5, 60.25, -2_000_000.0],
            ..CameraView::IDENTITY
        };
        assert_eq!(
            camera.relative([1_000_032, 64, -2_000_032]),
            [31.5, 3.75, -32.0]
        );
    }

    #[test]
    fn the_relative_frustum_culls_relative_boxes_far_from_the_origin() {
        let eye = Vec3::new(1_048_576.0, 60.0, -1_048_576.0);
        let camera = camera_at(eye);
        let frustum = Frustum::from_view_projection(&camera.view_projection);
        let sees = |origin: [i32; 3]| frustum.may_see_chunk(camera.relative(origin));
        let [x, y, z] = eye.to_array().map(|value| value as i32);
        assert!(sees([x - 16, y - 16, z - 64]), "ahead");
        assert!(!sees([x - 16, y - 16, z + 64]), "behind");
        assert!(!sees([x - 16, y - 16, z - 600]), "beyond the far plane");
        assert!(!sees([x + 200, y - 16, z - 64]), "to the right");
    }

    #[test]
    fn fog_values_are_start_end_and_the_inverse_span() {
        let values = Fog {
            start: 100.0,
            end: 300.0,
        }
        .push_values();
        assert_eq!(values, [100.0, 300.0, 0.005, 0.0]);
    }

    #[test]
    fn fog_is_none_before_start_full_from_end_and_smooth_between() {
        let values = Fog {
            start: 100.0,
            end: 300.0,
        }
        .push_values();
        assert_eq!(fog_amount(values, 0.0), 0.0);
        assert_eq!(fog_amount(values, 100.0), 0.0);
        assert_eq!(fog_amount(values, 200.0), 0.5);
        assert_eq!(fog_amount(values, 300.0), 1.0);
        assert_eq!(fog_amount(values, 10_000.0), 1.0);
        // Smoothstep: flat at both ends, so the first tenth adds less than a tenth.
        assert!(fog_amount(values, 120.0) < 0.1);
    }

    #[test]
    fn fog_that_is_off_pushes_zeros_and_fogs_nothing() {
        let broken = [
            Fog::OFF,
            Fog {
                start: 300.0,
                end: 100.0,
            },
            Fog {
                start: f32::NAN,
                end: 100.0,
            },
            Fog {
                start: f32::NEG_INFINITY,
                end: 100.0,
            },
            Fog {
                start: 0.0,
                end: f32::INFINITY,
            },
            Fog {
                start: -f32::MAX,
                end: f32::MAX,
            },
            Fog {
                start: 0.0,
                end: f32::MIN_POSITIVE / 8.0,
            },
        ];
        for fog in broken {
            assert!(!fog.is_enabled(), "{fog:?}");
            assert_eq!(fog.push_values(), [0.0; 4], "{fog:?}");
            assert_eq!(fog_amount(fog.push_values(), 1.0e9), 0.0, "{fog:?}");
        }
    }
}
