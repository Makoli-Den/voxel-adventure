//! View-frustum culling: deciding which meshes the camera cannot see.
//!
//! Pure math with no graphics API, so it is unit tested without a GPU. The planes are
//! extracted from the combined view-projection matrix (the Gribb-Hartmann method), so
//! the test follows whatever projection the game sets.
//!
//! Both depth mappings ([`crate::DepthMode`]) work unchanged. Vulkan clips to `0 <= z <= w`
//! either way; a reversed projection only maps the near plane to `z = w` and the far plane to
//! `z = 0` instead of the other way round. The two depth planes extracted below are therefore
//! the same two planes of the scene with their roles swapped (with reversed depth `z >= 0` is
//! the far plane and `z <= w` the near plane). The test needs no mode (pinned by a test with both matrices).

/// Planes of a view frustum: left, right, bottom, top, near, far.
const PLANE_COUNT: usize = 6;

/// A plane `a*x + b*y + c*z + d = 0`; points with a positive value lie on the inside.
type Plane = [f32; 4];

/// The six planes of the volume a view-projection matrix maps to Vulkan clip space
/// (`-w <= x <= w`, `-w <= y <= w`, `0 <= z <= w`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Frustum {
    planes: [Plane; PLANE_COUNT],
}

impl Frustum {
    /// Extracts the planes from a column-major matrix, exactly the layout of
    /// `glam::Mat4::to_cols_array_2d()`. The planes are not normalized: only the sign
    /// of the distance matters for the box test, so no square roots are taken.
    pub(crate) fn from_view_projection(m: &[[f32; 4]; 4]) -> Self {
        // Row `i` of the matrix, since `m[column][row]`.
        let row = |i: usize| [m[0][i], m[1][i], m[2][i], m[3][i]];
        let (x, y, z, w) = (row(0), row(1), row(2), row(3));
        let add = |a: Plane, b: Plane| [a[0] + b[0], a[1] + b[1], a[2] + b[2], a[3] + b[3]];
        let sub = |a: Plane, b: Plane| [a[0] - b[0], a[1] - b[1], a[2] - b[2], a[3] - b[3]];
        Self {
            planes: [
                add(w, x), // left:   x >= -w
                sub(w, x), // right:  x <=  w
                add(w, y), // bottom: y >= -w
                sub(w, y), // top:    y <=  w
                // The depth range 0..1: near and far with standard depth, far and near with
                // reversed depth (see the module docs).
                z,         // z >= 0
                sub(w, z), // z <= w
            ],
        }
    }

    /// Whether the axis-aligned box from `min` to `max` may be visible. `false` means
    /// the box is entirely outside one plane and can be skipped; `true` can include
    /// boxes near a corner that are in fact outside (a conservative answer). A plane that
    /// is not a finite number (a broken matrix) never culls: when in doubt, draw.
    // HOT PATH: runs for every chunk every frame; six planes, one comparison each, no
    // allocation. It tests only the box corner that lies farthest along the plane normal.
    pub(crate) fn may_see_box(&self, min: [f32; 3], max: [f32; 3]) -> bool {
        self.planes.iter().all(|plane| {
            let corner = |axis: usize| {
                if plane[axis] >= 0.0 {
                    max[axis]
                } else {
                    min[axis]
                }
            };
            let farthest = plane[0] * corner(0) + plane[1] * corner(1) + plane[2] * corner(2);
            let distance = farthest + plane[3];
            distance >= 0.0 || !distance.is_finite()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const IDENTITY: [[f32; 4]; 4] = [
        [1.0, 0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ];

    fn unit_box_at(x: f32, y: f32, z: f32) -> bool {
        Frustum::from_view_projection(&IDENTITY).may_see_box([x, y, z], [x + 0.5, y + 0.5, z + 0.5])
    }

    #[test]
    fn a_box_inside_the_clip_volume_is_visible() {
        assert!(unit_box_at(0.0, 0.0, 0.25));
    }

    #[test]
    fn a_box_outside_each_plane_is_culled() {
        assert!(!unit_box_at(-2.0, 0.0, 0.25), "left");
        assert!(!unit_box_at(1.5, 0.0, 0.25), "right");
        assert!(!unit_box_at(0.0, -2.0, 0.25), "bottom");
        assert!(!unit_box_at(0.0, 1.5, 0.25), "top");
        assert!(!unit_box_at(0.0, 0.0, -2.0), "near");
        assert!(!unit_box_at(0.0, 0.0, 1.5), "far");
    }

    #[test]
    fn a_box_that_straddles_a_plane_is_visible() {
        let frustum = Frustum::from_view_projection(&IDENTITY);
        assert!(frustum.may_see_box([-3.0, -3.0, -3.0], [3.0, 3.0, 3.0]));
        assert!(frustum.may_see_box([0.5, 0.0, 0.5], [2.0, 0.5, 0.75]));
    }

    #[test]
    fn a_perspective_view_culls_what_is_behind_the_camera() {
        // Camera at the origin looking along -Z, 90 degrees field of view, square window,
        // near 1, far 100, Vulkan clip space (Y down, depth 0..1), column-major.
        let (near, far) = (1.0_f32, 100.0_f32);
        let depth_scale = far / (near - far);
        let matrix = [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, -1.0, 0.0, 0.0],
            [0.0, 0.0, depth_scale, -1.0],
            [0.0, 0.0, near * depth_scale, 0.0],
        ];
        let frustum = Frustum::from_view_projection(&matrix);
        assert!(
            frustum.may_see_box([-1.0, -1.0, -11.0], [1.0, 1.0, -10.0]),
            "ahead"
        );
        assert!(
            !frustum.may_see_box([-1.0, -1.0, 10.0], [1.0, 1.0, 11.0]),
            "behind"
        );
        assert!(
            !frustum.may_see_box([50.0, -1.0, -11.0], [51.0, 1.0, -10.0]),
            "far to the side"
        );
        assert!(
            !frustum.may_see_box([-1.0, -1.0, -201.0], [1.0, 1.0, -200.0]),
            "beyond far"
        );
        assert!(
            !frustum.may_see_box([-1.0, -1.0, -0.5], [1.0, 1.0, -0.1]),
            "before near"
        );
        assert!(
            !frustum.may_see_box([-60.0, -1.0, -20.0], [-30.0, 1.0, -10.0]),
            "left"
        );
        assert!(
            !frustum.may_see_box([-1.0, 30.0, -20.0], [1.0, 40.0, -10.0]),
            "above"
        );
        assert!(
            !frustum.may_see_box([-1.0, -40.0, -20.0], [1.0, -30.0, -10.0]),
            "below"
        );
    }

    /// The far plane of [`game_like`], in blocks: a horizon of decision 0015.
    const GAME_FAR: f32 = 2000.0;

    /// A camera looking along `forward` with the game's field of view, a 16:9 window, near 0.1
    /// and far [`GAME_FAR`], standard or reversed depth (near and far swapped), built as the
    /// game builds it.
    fn game_like(forward: glam::Vec3, reversed: bool) -> [[f32; 4]; 4] {
        use glam::camera::rh::{proj::vulkan, view};
        let (fov, aspect, near, far) = (70.0_f32.to_radians(), 16.0 / 9.0, 0.1, GAME_FAR);
        let projection = if reversed {
            vulkan::perspective(fov, aspect, far, near)
        } else {
            vulkan::perspective(fov, aspect, near, far)
        };
        (projection * view::look_at_mat4(glam::Vec3::ZERO, forward, glam::Vec3::Y))
            .to_cols_array_2d()
    }

    #[test]
    fn a_reversed_matrix_maps_near_to_one_and_far_to_zero() {
        let matrix = glam::Mat4::from_cols_array_2d(&game_like(glam::Vec3::NEG_Z, true));
        let depth = |distance: f32| {
            let clip = matrix * glam::Vec4::new(0.0, 0.0, -distance, 1.0);
            clip.z / clip.w
        };
        assert!((depth(0.1) - 1.0).abs() < 1e-6);
        assert!(depth(GAME_FAR).abs() < 1e-6);
        assert!(depth(10.0) > depth(11.0), "nearer is greater");
    }

    /// The nearest distance along `forward` (a unit vector) of the box from `min` to `max`.
    fn nearest_depth(forward: glam::Vec3, min: [f32; 3], max: [f32; 3]) -> f32 {
        let corner = |axis: usize| {
            if forward[axis] >= 0.0 {
                min[axis]
            } else {
                max[axis]
            }
        };
        forward.dot(glam::Vec3::new(corner(0), corner(1), corner(2)))
    }

    #[test]
    fn reversed_and_standard_matrices_cull_the_same_boxes() {
        // The side planes come from the same rows in both matrices. The far plane does not:
        // with standard depth it is the difference of two nearly equal rows, which f32 puts
        // about two blocks off at 2000 (0.1 percent), while the reversed one lands on it. So
        // boxes whose nearest point lies within this band of the far plane are left out.
        let far_band = GAME_FAR / 100.0;
        let directions = [
            glam::Vec3::NEG_Z,
            glam::Vec3::new(0.3, -0.4, -0.8).normalize(),
            glam::Vec3::new(-0.9, 0.1, 0.2).normalize(),
        ];
        // Box sizes of a chunk, a level-2 tile and a level-4 tile, spread on an uneven grid
        // from behind the camera to beyond the far plane, near ones included.
        let sizes = [32.0, 128.0, 512.0];
        let steps = [
            -2500.0, -700.0, -97.0, -13.0, -0.31, 0.27, 11.0, 150.0, 1900.0,
        ];
        let (mut seen, mut culled, mut skipped) = (0, 0, 0);
        for forward in directions {
            let standard = Frustum::from_view_projection(&game_like(forward, false));
            let reversed = Frustum::from_view_projection(&game_like(forward, true));
            for size in sizes {
                for x in steps {
                    for y in steps {
                        for z in steps {
                            let min = [x, y * 0.25, z];
                            let max = min.map(|v| v + size);
                            let depth = nearest_depth(forward, min, max);
                            if (depth - GAME_FAR).abs() < far_band {
                                skipped += 1;
                                continue;
                            }
                            let expected = standard.may_see_box(min, max);
                            assert_eq!(reversed.may_see_box(min, max), expected, "{min:?}");
                            if expected { seen += 1 } else { culled += 1 }
                        }
                    }
                }
            }
        }
        // Both answers occur, so the comparison means something, and the band is narrow.
        assert!(seen > 100 && culled > 100, "{seen} seen, {culled} culled");
        assert!(skipped < (seen + culled) / 20, "{skipped} skipped");
    }

    #[test]
    fn a_reversed_matrix_culls_before_near_and_beyond_far() {
        let frustum = Frustum::from_view_projection(&game_like(glam::Vec3::NEG_Z, true));
        let ahead =
            |near: f32, far: f32| frustum.may_see_box([-1.0, -1.0, -far], [1.0, 1.0, -near]);
        assert!(ahead(10.0, 11.0), "ahead");
        assert!(ahead(1990.0, 2010.0), "across the far plane");
        assert!(!ahead(2001.0, 2100.0), "beyond the far plane");
        assert!(!ahead(0.01, 0.09), "before the near plane");
        assert!(
            !frustum.may_see_box([-1.0, -1.0, 10.0], [1.0, 1.0, 11.0]),
            "behind"
        );
    }

    #[test]
    fn a_broken_matrix_culls_nothing() {
        let frustum = Frustum::from_view_projection(&[[f32::NAN; 4]; 4]);
        assert!(frustum.may_see_box([0.0; 3], [1.0; 3]));
    }
}
