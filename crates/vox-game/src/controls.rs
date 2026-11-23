//! Keyboard and mouse state, turned into a [`Movement`] for the camera.
//!
//! W, A, S, D move, Space goes up, Shift goes down. Holding the right mouse button
//! turns mouse motion into looking around, so the cursor never has to be captured.
//! Clicking the left mouse button and pressing F ask for an edit (break or place a block);
//! each press is remembered until the game takes it, so a quick click is never lost between
//! two frames.

use winit::keyboard::KeyCode;

use crate::camera::Movement;
use crate::editing::EditAction;

/// Which movement keys are held, and whether the player is looking around.
#[derive(Debug, Default)]
pub struct Controls {
    forward: bool,
    backward: bool,
    left: bool,
    right: bool,
    up: bool,
    /// Each Shift key is tracked on its own, so releasing one while the other is still
    /// held does not stop the descent.
    shift_left: bool,
    shift_right: bool,
    looking: bool,
    /// Edit requests since the last [`Controls::take_edit`]: break first, then place.
    break_requested: bool,
    place_requested: bool,
    /// Set when the window gained focus and cleared once the events of that moment are
    /// handled: a click in it is the one that focused the window, not an edit.
    just_focused: bool,
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
            KeyCode::ShiftLeft => self.shift_left = pressed,
            KeyCode::ShiftRight => self.shift_right = pressed,
            _ => {}
        }
    }

    /// Records that the look button (the right mouse button) was pressed or released.
    pub fn set_looking(&mut self, looking: bool) {
        self.looking = looking;
    }

    /// Records a press (not a release, not a key repeat) of an edit button.
    pub fn request_edit(&mut self, action: EditAction) {
        match action {
            EditAction::Break => self.break_requested = true,
            EditAction::Place => self.place_requested = true,
        }
    }

    /// The window gained focus.
    pub fn focus_gained(&mut self) {
        self.just_focused = true;
    }

    /// Called when the current batch of window events is handled, which ends the moment in
    /// which a click still counts as the one that focused the window.
    pub fn end_of_events(&mut self) {
        self.just_focused = false;
    }

    /// A press of the left mouse button: a request to break a block, unless the click is
    /// the one that focused the window.
    pub fn click(&mut self) {
        if !self.just_focused {
            self.request_edit(EditAction::Break);
        }
    }

    /// The next requested edit, which is then forgotten. One per call, so two presses within
    /// one frame are carried out in two frames, break before place.
    pub fn take_edit(&mut self) -> Option<EditAction> {
        if std::mem::take(&mut self.break_requested) {
            Some(EditAction::Break)
        } else if std::mem::take(&mut self.place_requested) {
            Some(EditAction::Place)
        } else {
            None
        }
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
            up: axis(self.up, self.shift_left || self.shift_right),
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
    fn releasing_one_shift_key_keeps_descending_while_the_other_is_held() {
        let mut controls = Controls::default();
        controls.set_key(KeyCode::ShiftLeft, true);
        controls.set_key(KeyCode::ShiftRight, true);
        controls.set_key(KeyCode::ShiftLeft, false);
        assert_eq!(controls.movement().up, -1.0);
        controls.set_key(KeyCode::ShiftRight, false);
        assert_eq!(controls.movement().up, 0.0);
    }

    #[test]
    fn unrelated_keys_are_ignored() {
        assert_eq!(held(&[KeyCode::KeyQ, KeyCode::Enter]), Movement::default());
    }

    #[test]
    fn an_edit_request_is_taken_once() {
        let mut controls = Controls::default();
        assert_eq!(controls.take_edit(), None);
        controls.request_edit(EditAction::Place);
        assert_eq!(controls.take_edit(), Some(EditAction::Place));
        assert_eq!(controls.take_edit(), None);
    }

    #[test]
    fn repeated_requests_before_a_frame_count_once() {
        let mut controls = Controls::default();
        controls.request_edit(EditAction::Break);
        controls.request_edit(EditAction::Break);
        assert_eq!(controls.take_edit(), Some(EditAction::Break));
        assert_eq!(controls.take_edit(), None);
    }

    #[test]
    fn break_is_taken_before_place_and_neither_is_lost() {
        let mut controls = Controls::default();
        controls.request_edit(EditAction::Place);
        controls.request_edit(EditAction::Break);
        assert_eq!(controls.take_edit(), Some(EditAction::Break));
        assert_eq!(controls.take_edit(), Some(EditAction::Place));
        assert_eq!(controls.take_edit(), None);
    }

    #[test]
    fn the_click_that_focuses_the_window_does_not_break() {
        let mut controls = Controls::default();
        controls.focus_gained();
        controls.click();
        assert_eq!(controls.take_edit(), None);
        // Once the events of that moment are handled, clicks count again.
        controls.end_of_events();
        controls.click();
        assert_eq!(controls.take_edit(), Some(EditAction::Break));
    }

    #[test]
    fn a_click_in_a_focused_window_breaks() {
        let mut controls = Controls::default();
        controls.click();
        assert_eq!(controls.take_edit(), Some(EditAction::Break));
    }

    #[test]
    fn the_edit_key_is_not_a_movement_key() {
        assert_eq!(held(&[KeyCode::KeyF]), Movement::default());
    }

    #[test]
    fn release_all_forgets_edit_requests() {
        let mut controls = Controls::default();
        controls.request_edit(EditAction::Break);
        controls.release_all();
        assert_eq!(controls.take_edit(), None);
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
