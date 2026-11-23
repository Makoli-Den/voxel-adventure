//! A free-flying camera: a position and a look direction, and the matrix it produces.
//!
//! The world is right-handed with Y up. At yaw 0 and pitch 0 the camera looks along
//! -Z; a positive yaw turns it towards +X (a right turn seen from above).

use std::f32::consts::{FRAC_PI_2, TAU};

use glam::Vec3;
use glam::camera::rh::{proj::vulkan, view};

/// Vertical field of view of the projection, in degrees.
const FIELD_OF_VIEW_DEGREES: f32 = 70.0;
/// Distance of the near clipping plane, in blocks.
const NEAR: f32 = 0.1;
/// Distance of the far clipping plane, in blocks. It must cover the loaded area from any point
/// inside it (the largest `--view-radius`; a test checks it), otherwise the far edge of the world is cut off when the camera flies.
const FAR: f32 = 3000.0;
/// How far short of straight up or down the pitch may go, in degrees. At exactly 90 degrees
/// the look direction is parallel to the world's up axis and the view matrix degenerates.
const PITCH_MARGIN_DEGREES: f32 = 1.0;
/// Largest pitch, one degree short of straight up or down, where "up" would become ambiguous.
const MAX_PITCH: f32 = FRAC_PI_2 - PITCH_MARGIN_DEGREES.to_radians();

/// Which way the player wants to move, each axis from -1 to 1, relative to the camera.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Movement {
    /// Towards the look direction (positive) or away from it (negative).
    pub forward: f32,
    /// To the right of the look direction (positive) or to the left (negative).
    pub right: f32,
    /// Straight up along the world's Y axis (positive) or down (negative).
    pub up: f32,
}

/// Position and look direction of the player's eye.
#[derive(Clone, Copy, Debug)]
pub struct Camera {
    position: Vec3,
    /// Rotation around the Y axis in radians, kept within `0..=TAU` (a tiny negative angle
    /// can round up to exactly `TAU`, which points the same way as 0).
    yaw: f32,
    /// Look up (positive) or down (negative) in radians, within `-MAX_PITCH..=MAX_PITCH`.
    pitch: f32,
}

impl Camera {
    /// Creates a camera. The pitch is limited to [`MAX_PITCH`]; a angle that is not a
    /// finite number counts as 0.
    pub fn new(position: Vec3, yaw: f32, pitch: f32) -> Self {
        Self {
            position,
            yaw: finite_or_zero(yaw).rem_euclid(TAU),
            pitch: finite_or_zero(pitch).clamp(-MAX_PITCH, MAX_PITCH),
        }
    }

    /// Where the eye is, in blocks.
    pub fn position(&self) -> Vec3 {
        self.position
    }

    /// Unit vector the camera looks along.
    pub fn forward(&self) -> Vec3 {
        let (sin_yaw, cos_yaw) = self.yaw.sin_cos();
        let (sin_pitch, cos_pitch) = self.pitch.sin_cos();
        Vec3::new(sin_yaw * cos_pitch, sin_pitch, -cos_yaw * cos_pitch)
    }

    /// Puts the eye at `position`. A position that is not finite is ignored, for the reason
    /// given at [`Self::look`].
    pub fn set_position(&mut self, position: Vec3) {
        if position.is_finite() {
            self.position = position;
        }
    }

    /// The level direction (world X and Z) the player walks in for `movement`: forward and
    /// right follow the yaw but not the pitch, so looking down does not slow the walk.
    /// Its length is at most 1 (diagonals are normalized); `up` is ignored.
    pub fn walk_direction(&self, movement: Movement) -> [f32; 2] {
        let forward = self.forward();
        let level_forward = Vec3::new(forward.x, 0.0, forward.z).normalize_or_zero();
        let direction = level_forward * movement.forward + self.right() * movement.right;
        let clamped = direction.clamp_length_max(1.0);
        [clamped.x, clamped.z]
    }

    /// Unit vector to the camera's right, level with the ground.
    fn right(&self) -> Vec3 {
        let (sin_yaw, cos_yaw) = self.yaw.sin_cos();
        Vec3::new(cos_yaw, 0.0, sin_yaw)
    }

    /// Turns the camera by mouse motion converted to radians.
    ///
    /// `yaw_delta` turns right when positive; `pitch_delta` looks down when positive,
    /// the way a mouse moved towards you does.
    ///
    /// A delta that is not a finite number is ignored: it would turn the camera into NaN
    /// for good, and nothing would bring it back.
    pub fn look(&mut self, yaw_delta: f32, pitch_delta: f32) {
        if !(yaw_delta.is_finite() && pitch_delta.is_finite()) {
            return;
        }
        self.yaw = (self.yaw + yaw_delta).rem_euclid(TAU);
        self.pitch = (self.pitch - pitch_delta).clamp(-MAX_PITCH, MAX_PITCH);
    }

    /// Moves the eye by `distance` blocks in the direction the player asked for.
    ///
    /// Diagonal movement is normalized, so it is not faster than a straight line.
    /// A distance that is not a finite number is ignored, for the reason given at [`Self::look`].
    pub fn translate(&mut self, movement: Movement, distance: f32) {
        if !distance.is_finite() {
            return;
        }
        let direction = self.forward() * movement.forward
            + self.right() * movement.right
            + Vec3::Y * movement.up;
        self.position += direction.normalize_or_zero() * distance;
    }

    /// The view-projection matrix for a window of `width` by `height` pixels, column-major.
    ///
    /// Vulkan clip space: Y points down and depth runs from 0 to 1.
    pub fn view_projection(&self, width: u32, height: u32) -> [[f32; 4]; 4] {
        // A zero side only happens while minimized, when nothing is drawn anyway.
        let aspect = width.max(1) as f32 / height.max(1) as f32;
        // glam's Vulkan projection already flips Y and maps depth to 0..1.
        let projection = vulkan::perspective(FIELD_OF_VIEW_DEGREES.to_radians(), aspect, NEAR, FAR);
        let view = view::look_at_mat4(self.position, self.position + self.forward(), Vec3::Y);
        (projection * view).to_cols_array_2d()
    }
}

/// The value itself, or 0 if it is NaN or infinite.
fn finite_or_zero(value: f32) -> f32 {
    if value.is_finite() { value } else { 0.0 }
}

#[cfg(test)]
mod tests {
    use glam::Mat4;

    use super::*;

    #[test]
    fn the_far_plane_covers_the_largest_world_diagonal() {
        let side = (2 * crate::cli::MAX_VIEW_RADIUS + 1) as f32 * vox_world::CHUNK_SIZE as f32;
        assert!(
            FAR >= side * std::f32::consts::SQRT_2,
            "{FAR} < diagonal of {side}"
        );
    }

    fn close(a: Vec3, b: Vec3) -> bool {
        (a - b).length() < 1e-5
    }

    /// Normalized device coordinates of a world point seen through `camera`.
    fn ndc(camera: &Camera, point: Vec3) -> Vec3 {
        let clip = Mat4::from_cols_array_2d(&camera.view_projection(800, 600)) * point.extend(1.0);
        clip.truncate() / clip.w
    }

    #[test]
    fn default_direction_is_minus_z() {
        let camera = Camera::new(Vec3::ZERO, 0.0, 0.0);
        assert!(close(camera.forward(), Vec3::NEG_Z));
        assert!(close(camera.right(), Vec3::X));
    }

    #[test]
    fn positive_yaw_turns_towards_plus_x() {
        let camera = Camera::new(Vec3::ZERO, FRAC_PI_2, 0.0);
        assert!(close(camera.forward(), Vec3::X));
        assert!(close(camera.right(), Vec3::Z));
    }

    #[test]
    fn pitch_is_limited_short_of_straight_up_and_down() {
        let mut camera = Camera::new(Vec3::ZERO, 0.0, 0.0);
        camera.look(0.0, -100.0);
        assert!((camera.pitch - MAX_PITCH).abs() < 1e-6);
        camera.look(0.0, 200.0);
        assert!((camera.pitch + MAX_PITCH).abs() < 1e-6);
    }

    #[test]
    fn mouse_down_looks_down_and_mouse_right_turns_right() {
        let mut camera = Camera::new(Vec3::ZERO, 0.0, 0.0);
        camera.look(0.0, 0.2);
        assert!(camera.forward().y < 0.0);
        let mut camera = Camera::new(Vec3::ZERO, 0.0, 0.0);
        camera.look(0.2, 0.0);
        assert!(camera.forward().x > 0.0);
    }

    #[test]
    fn yaw_stays_within_one_turn() {
        let mut camera = Camera::new(Vec3::ZERO, 0.0, 0.0);
        camera.look(-0.1, 0.0);
        assert!((0.0..=TAU).contains(&camera.yaw));
        camera.look(100.0, 0.0);
        assert!((0.0..=TAU).contains(&camera.yaw));
    }

    #[test]
    fn values_that_are_not_finite_never_corrupt_the_camera() {
        let mut camera = Camera::new(Vec3::new(1.0, 2.0, 3.0), 0.5, 0.25);
        camera.look(f32::NAN, 0.1);
        camera.look(0.1, f32::INFINITY);
        camera.translate(
            Movement {
                forward: 1.0,
                ..Movement::default()
            },
            f32::NAN,
        );
        camera.translate(
            Movement {
                forward: f32::NAN,
                ..Movement::default()
            },
            1.0,
        );
        assert_eq!(camera.yaw, 0.5);
        assert_eq!(camera.pitch, 0.25);
        assert!(close(camera.position, Vec3::new(1.0, 2.0, 3.0)));
        assert!(
            camera
                .view_projection(800, 600)
                .iter()
                .flatten()
                .all(|v| v.is_finite())
        );
    }

    #[test]
    fn a_new_camera_with_non_finite_angles_looks_straight_ahead() {
        let camera = Camera::new(Vec3::ZERO, f32::NAN, f32::INFINITY);
        assert!(close(camera.forward(), Vec3::NEG_Z));
    }

    #[test]
    fn forward_moves_along_the_look_direction() {
        let mut camera = Camera::new(Vec3::new(1.0, 2.0, 3.0), 0.0, 0.0);
        camera.translate(
            Movement {
                forward: 1.0,
                ..Movement::default()
            },
            2.0,
        );
        assert!(close(camera.position, Vec3::new(1.0, 2.0, 1.0)));
    }

    #[test]
    fn walking_ignores_the_pitch_and_is_not_faster_on_diagonals() {
        let camera = Camera::new(Vec3::ZERO, FRAC_PI_2, -1.2);
        let ahead = camera.walk_direction(Movement {
            forward: 1.0,
            ..Movement::default()
        });
        assert!((ahead[0] - 1.0).abs() < 1e-5 && ahead[1].abs() < 1e-5);
        let diagonal = camera.walk_direction(Movement {
            forward: 1.0,
            right: 1.0,
            up: 1.0,
        });
        assert!((Vec3::new(diagonal[0], 0.0, diagonal[1]).length() - 1.0).abs() < 1e-5);
        assert_eq!(camera.walk_direction(Movement::default()), [0.0, 0.0]);
    }

    #[test]
    fn set_position_ignores_values_that_are_not_finite() {
        let mut camera = Camera::new(Vec3::ONE, 0.0, 0.0);
        camera.set_position(Vec3::new(f32::NAN, 0.0, 0.0));
        assert!(close(camera.position, Vec3::ONE));
        camera.set_position(Vec3::new(4.0, 5.0, 6.0));
        assert!(close(camera.position, Vec3::new(4.0, 5.0, 6.0)));
    }

    #[test]
    fn no_input_means_no_movement() {
        let mut camera = Camera::new(Vec3::new(1.0, 2.0, 3.0), 0.5, 0.3);
        camera.translate(Movement::default(), 5.0);
        assert!(close(camera.position, Vec3::new(1.0, 2.0, 3.0)));
    }

    #[test]
    fn diagonal_movement_is_not_faster() {
        let mut camera = Camera::new(Vec3::ZERO, 0.0, 0.0);
        camera.translate(
            Movement {
                forward: 1.0,
                right: 1.0,
                up: 1.0,
            },
            3.0,
        );
        assert!((camera.position.length() - 3.0).abs() < 1e-5);
    }

    #[test]
    fn up_moves_along_the_world_axis_even_when_looking_down() {
        let mut camera = Camera::new(Vec3::ZERO, 0.0, -1.0);
        camera.translate(
            Movement {
                up: 1.0,
                ..Movement::default()
            },
            2.0,
        );
        assert!(close(camera.position, Vec3::new(0.0, 2.0, 0.0)));
    }

    #[test]
    fn point_straight_ahead_is_centered_and_inside_the_depth_range() {
        let camera = Camera::new(Vec3::ZERO, 0.0, 0.0);
        let point = ndc(&camera, Vec3::new(0.0, 0.0, -10.0));
        assert!(point.x.abs() < 1e-4 && point.y.abs() < 1e-4);
        assert!((0.0..1.0).contains(&point.z));
    }

    #[test]
    fn world_up_points_up_on_screen() {
        // Vulkan's NDC Y points down, so "higher in the world" must be a smaller Y.
        let camera = Camera::new(Vec3::ZERO, 0.0, 0.0);
        assert!(ndc(&camera, Vec3::new(0.0, 1.0, -10.0)).y < 0.0);
    }

    #[test]
    fn world_right_points_right_on_screen() {
        let camera = Camera::new(Vec3::ZERO, 0.0, 0.0);
        assert!(ndc(&camera, Vec3::new(1.0, 0.0, -10.0)).x > 0.0);
    }

    #[test]
    fn zero_sized_window_still_gives_a_finite_matrix() {
        let camera = Camera::new(Vec3::ZERO, 0.0, 0.0);
        let matrix = camera.view_projection(0, 0);
        assert!(matrix.iter().flatten().all(|v| v.is_finite()));
    }
}
