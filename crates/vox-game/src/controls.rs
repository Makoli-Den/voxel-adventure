//! Keyboard and mouse state, turned into a [`Movement`] for the camera.
//!
//! W, A, S, D move, Space goes up, Shift goes down. Holding the right mouse button
//! turns mouse motion into looking around, so the cursor never has to be captured.

use winit::keyboard::KeyCode;

use crate::camera::Movement;

/// Which movement keys are held, and whether the player is looking around.
#[derive(Debug, Default)]
pub struct Controls {
    forward: bool,
    backward: bool,
    left: bool,
    right: bool,
    up: bool,
    down: bool,
    looking: bool,
}

impl Controls {
    /// Records that `key` was pressed or released. Keys that do nothing are ignored.
    pub fn set_key(&mut self, key: KeyCode, pressed: bool) {
        match key {
            KeyCode::KeyW => self.forward = pressed,
            KeyCode::KeyS => self.backward = pressed,
            KeyCode::KeyA => self.left = pressed,
            KeyCode::KeyD => self.right = pressed,
            KeyCode::Space => self.up = pressed,
            KeyCode::ShiftLeft | KeyCode::ShiftRight => self.down = pressed,
            _ => {}
        }
    }

    /// Records that the look button (the right mouse button) was pressed or released.
    pub fn set_looking(&mut self, looking: bool) {
        self.looking = looking;
    }

    /// Whether mouse motion should turn the camera right now.
    pub fn is_looking(&self) -> bool {
        self.looking
    }

    /// Forgets everything that is held, for example when the window loses focus and the
    /// release events would never arrive.
    pub fn release_all(&mut self) {
        *self = Self::default();
    }

    /// The movement the held keys ask for. Opposite keys cancel each other.
    pub fn movement(&self) -> Movement {
        Movement {
            forward: axis(self.forward, self.backward),
            right: axis(self.right, self.left),
            up: axis(self.up, self.down),
        }
    }
}

/// `1.0` for the positive key alone, `-1.0` for the negative key alone, otherwise `0.0`.
fn axis(positive: bool, negative: bool) -> f32 {
    f32::from(positive) - f32::from(negative)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn held(keys: &[KeyCode]) -> Movement {
        let mut controls = Controls::default();
        for &key in keys {
            controls.set_key(key, true);
        }
        controls.movement()
    }

    #[test]
    fn nothing_held_means_no_movement() {
        assert_eq!(held(&[]), Movement::default());
    }

    #[test]
    fn each_key_moves_along_its_axis() {
        assert_eq!(held(&[KeyCode::KeyW]).forward, 1.0);
        assert_eq!(held(&[KeyCode::KeyS]).forward, -1.0);
        assert_eq!(held(&[KeyCode::KeyD]).right, 1.0);
        assert_eq!(held(&[KeyCode::KeyA]).right, -1.0);
        assert_eq!(held(&[KeyCode::Space]).up, 1.0);
        assert_eq!(held(&[KeyCode::ShiftLeft]).up, -1.0);
        assert_eq!(held(&[KeyCode::ShiftRight]).up, -1.0);
    }

    #[test]
    fn opposite_keys_cancel() {
        assert_eq!(held(&[KeyCode::KeyW, KeyCode::KeyS]).forward, 0.0);
        assert_eq!(held(&[KeyCode::KeyA, KeyCode::KeyD]).right, 0.0);
        assert_eq!(held(&[KeyCode::Space, KeyCode::ShiftLeft]).up, 0.0);
    }

    #[test]
    fn releasing_a_key_stops_the_movement() {
        let mut controls = Controls::default();
        controls.set_key(KeyCode::KeyW, true);
        controls.set_key(KeyCode::KeyW, false);
        assert_eq!(controls.movement(), Movement::default());
    }

    #[test]
    fn unrelated_keys_are_ignored() {
        assert_eq!(held(&[KeyCode::KeyQ, KeyCode::Enter]), Movement::default());
    }

    #[test]
    fn release_all_clears_keys_and_looking() {
        let mut controls = Controls::default();
        controls.set_key(KeyCode::KeyW, true);
        controls.set_looking(true);
        controls.release_all();
        assert_eq!(controls.movement(), Movement::default());
        assert!(!controls.is_looking());
    }
}
