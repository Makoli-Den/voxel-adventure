//! View-frustum culling: deciding which chunks the camera cannot see.
//!
//! Pure math with no graphics API, so it is unit tested without a GPU. The planes are
//! extracted from the combined view-projection matrix (the Gribb-Hartmann method), so
//! the test follows whatever projection the game sets.

use vox_world::CHUNK_SIZE;

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
                z,         // near:   z >=  0 (Vulkan depth range 0..1)
                sub(w, z), // far:    z <=  w
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

/// The far corner of a chunk's bounding box (its cube of `CHUNK_SIZE` blocks) whose near
/// corner, the chunk's origin, is `min`, in the same space: relative to the camera in the
/// renderer, where the box goes to both the frustum test and the per-direction culling.
// HOT PATH: runs for every chunk every frame; three additions.
pub(crate) fn chunk_box_max(min: [f32; 3]) -> [f32; 3] {
    let size = CHUNK_SIZE as f32;
    min.map(|value| value + size)
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

    #[test]
    fn a_broken_matrix_culls_nothing() {
        let frustum = Frustum::from_view_projection(&[[f32::NAN; 4]; 4]);
        assert!(frustum.may_see_box([0.0; 3], [1.0; 3]));
    }
}
