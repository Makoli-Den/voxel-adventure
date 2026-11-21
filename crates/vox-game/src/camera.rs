//! A free-flying camera: a position and a look direction, and the matrix it produces.
//!
//! The world is right-handed with Y up. At yaw 0 and pitch 0 the camera looks along
//! -Z; a positive yaw turns it towards +X (a right turn seen from above).

use std::f32::consts::{FRAC_PI_2, PI, TAU};

use glam::Vec3;
use glam::camera::rh::{proj::vulkan, view};

/// Vertical field of view of the projection, in degrees.
const FIELD_OF_VIEW_DEGREES: f32 = 70.0;
/// Distance of the near clipping plane, in blocks.
const NEAR: f32 = 0.1;
/// Distance of the far clipping plane, in blocks.
const FAR: f32 = 500.0;
/// Largest pitch, one degree short of straight up or down, where "up" would become ambiguous.
const MAX_PITCH: f32 = FRAC_PI_2 - PI / 180.0;

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
    /// Rotation around the Y axis in radians, kept within `0..TAU`.
    yaw: f32,
    /// Look up (positive) or down (negative) in radians, within `-MAX_PITCH..=MAX_PITCH`.
    pitch: f32,
}

impl Camera {
    /// Creates a camera. The pitch is limited to [`MAX_PITCH`].
    pub fn new(position: Vec3, yaw: f32, pitch: f32) -> Self {
        Self {
            position,
            yaw: yaw.rem_euclid(TAU),
            pitch: pitch.clamp(-MAX_PITCH, MAX_PITCH),
        }
    }

    /// Unit vector the camera looks along.
    fn forward(&self) -> Vec3 {
        let (sin_yaw, cos_yaw) = self.yaw.sin_cos();
        let (sin_pitch, cos_pitch) = self.pitch.sin_cos();
        Vec3::new(sin_yaw * cos_pitch, sin_pitch, -cos_yaw * cos_pitch)
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
    pub fn look(&mut self, yaw_delta: f32, pitch_delta: f32) {
        self.yaw = (self.yaw + yaw_delta).rem_euclid(TAU);
        self.pitch = (self.pitch - pitch_delta).clamp(-MAX_PITCH, MAX_PITCH);
    }

    /// Moves the eye by `distance` blocks in the direction the player asked for.
    ///
    /// Diagonal movement is normalized, so it is not faster than a straight line.
    pub fn translate(&mut self, movement: Movement, distance: f32) {
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

#[cfg(test)]
mod tests {
    use glam::Mat4;

    use super::*;

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
        assert!((0.0..TAU).contains(&camera.yaw));
        camera.look(100.0, 0.0);
        assert!((0.0..TAU).contains(&camera.yaw));
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
