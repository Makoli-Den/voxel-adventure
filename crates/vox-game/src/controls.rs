//! Keyboard and mouse state, turned into a [`Movement`] for the camera.
//!
//! W, A, S, D move, Space goes up, Shift goes down. Holding the right mouse button
//! turns mouse motion into looking around, so the cursor never has to be captured.
//! Clicking the left mouse button and pressing F ask for an edit (break or place a block);
//! each press is remembered until the game takes it, so a quick click is never lost between
//! two frames. The number keys 1 to 9 (the row above the letters or the numpad) and the mouse wheel choose the block to place, and are
//! remembered the same way.

use winit::keyboard::KeyCode;

use crate::camera::Movement;
use crate::editing::EditAction;
use crate::selection::SLOT_KEYS;

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
    /// The number key pressed since the last [`Controls::take_slot`], counted from 0; only the
    /// latest press counts.
    slot_requested: Option<usize>,
    /// Which of [`MODIFIER_CODES`] are held, in the same order. Shift is tracked above, because
    /// it also moves the player.
    modifiers: [bool; MODIFIER_CODES.len()],
    /// Wheel movement since the last [`Controls::take_wheel_steps`], in lines, wheel up
    /// positive. The part of a line that is not yet a step is kept for the next movement.
    wheel_lines: f32,
}

/// The number row keys in order: the n-th entry selects slot n (key 1 is slot 0).
const SLOT_CODES: [KeyCode; SLOT_KEYS] = [
    KeyCode::Digit1,
    KeyCode::Digit2,
    KeyCode::Digit3,
    KeyCode::Digit4,
    KeyCode::Digit5,
    KeyCode::Digit6,
    KeyCode::Digit7,
    KeyCode::Digit8,
    KeyCode::Digit9,
];

/// The numpad digits in the same order as [`SLOT_CODES`], which select the same slots.
const NUMPAD_SLOT_CODES: [KeyCode; SLOT_KEYS] = [
    KeyCode::Numpad1,
    KeyCode::Numpad2,
    KeyCode::Numpad3,
    KeyCode::Numpad4,
    KeyCode::Numpad5,
    KeyCode::Numpad6,
    KeyCode::Numpad7,
    KeyCode::Numpad8,
    KeyCode::Numpad9,
];

/// Modifier keys (besides Shift) that turn a number key into a shortcut of the system or the
/// window, so it must not change the block.
const MODIFIER_CODES: [KeyCode; 6] = [
    KeyCode::ControlLeft,
    KeyCode::ControlRight,
    KeyCode::AltLeft,
    KeyCode::AltRight,
    KeyCode::SuperLeft,
    KeyCode::SuperRight,
];

/// Pixels of smooth scrolling (a touchpad) that count as one wheel line, so a touchpad swipe
/// does not skip through the blocks. About the height of a text line.
const PIXELS_PER_WHEEL_LINE: f64 = 40.0;

/// The slot a number key selects, counted from 0, or `None` for any other key.
fn slot_of(key: KeyCode) -> Option<usize> {
    SLOT_CODES
        .iter()
        .chain(&NUMPAD_SLOT_CODES)
        .position(|&code| code == key)
        .map(|position| position % SLOT_KEYS)
}

/// Whether `key` is a number key that selects a block.
pub fn is_slot_key(key: KeyCode) -> bool {
    slot_of(key).is_some()
}

impl Controls {
    /// Records that `key` was pressed or released. Keys that do nothing are ignored.
    pub fn set_key(&mut self, key: KeyCode, pressed: bool) {
        if let Some(held) = MODIFIER_CODES.iter().position(|&code| code == key) {
            self.modifiers[held] = pressed;
        }
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

    /// Records a press (not a release, not a key repeat) of a number key. Ignored while
    /// Ctrl, Alt, Shift or Super is held, and for keys that are not number keys.
    pub fn request_slot(&mut self, key: KeyCode) {
        let modifier_held = self.shift_left || self.shift_right || self.modifiers.contains(&true);
        if let (Some(slot), false) = (slot_of(key), modifier_held) {
            self.slot_requested = Some(slot);
        }
    }

    /// Records mouse wheel movement of `lines` (wheel up is positive). Input that is not a
    /// number (NaN or infinite, which a broken driver could send), or would make the sum
    /// infinite, is ignored, so it cannot poison the sum.
    pub fn scroll(&mut self, lines: f32) {
        let sum = self.wheel_lines + lines;
        if sum.is_finite() {
            self.wheel_lines = sum;
        }
    }

    /// Records smooth scrolling (a touchpad) of `pixels`, wheel up positive.
    pub fn scroll_pixels(&mut self, pixels: f64) {
        // Narrowing is fine: a wheel line count never nears the limits of f32.
        self.scroll((pixels / PIXELS_PER_WHEEL_LINE) as f32);
    }

    /// The whole wheel lines moved since the last call as selection steps, which are then
    /// forgotten: wheel down is forward (positive), wheel up is backward, like the hotbar of
    /// Minecraft.
    pub fn take_wheel_steps(&mut self) -> i32 {
        let whole = self.wheel_lines.trunc();
        self.wheel_lines -= whole;
        // Negated before the cast: the saturating cast of a huge value gives i32::MIN, which
        // could not be negated afterwards.
        (-whole) as i32
    }

    /// The number key pressed since the last call, counted from 0, which is then forgotten.
    pub fn take_slot(&mut self) -> Option<usize> {
        self.slot_requested.take()
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

    fn requested(keys: &[KeyCode]) -> Option<usize> {
        let mut controls = Controls::default();
        for &key in keys {
            controls.set_key(key, true);
        }
        controls.request_slot(*keys.last().unwrap());
        controls.take_slot()
    }

    #[test]
    fn number_keys_request_their_slot_once() {
        let mut controls = Controls::default();
        assert_eq!(controls.take_slot(), None);
        controls.request_slot(KeyCode::Digit1);
        assert_eq!(controls.take_slot(), Some(0));
        assert_eq!(controls.take_slot(), None);
        controls.request_slot(KeyCode::Digit9);
        assert_eq!(controls.take_slot(), Some(SLOT_KEYS - 1));
    }

    #[test]
    fn every_number_key_and_numpad_key_has_its_slot() {
        for slot in 0..SLOT_KEYS {
            assert_eq!(requested(&[SLOT_CODES[slot]]), Some(slot));
            assert_eq!(requested(&[NUMPAD_SLOT_CODES[slot]]), Some(slot));
            assert!(is_slot_key(SLOT_CODES[slot]) && is_slot_key(NUMPAD_SLOT_CODES[slot]));
        }
    }

    #[test]
    fn zero_and_other_keys_are_not_slots() {
        assert_eq!(requested(&[KeyCode::Digit0]), None);
        assert_eq!(requested(&[KeyCode::Numpad0]), None);
        assert_eq!(requested(&[KeyCode::KeyQ]), None);
        assert!(!is_slot_key(KeyCode::Digit0));
    }

    #[test]
    fn the_latest_number_key_wins() {
        let mut controls = Controls::default();
        controls.request_slot(KeyCode::Digit2);
        controls.request_slot(KeyCode::Digit5);
        assert_eq!(controls.take_slot(), Some(4));
    }

    #[test]
    fn a_modifier_turns_a_number_key_into_a_shortcut() {
        for modifier in [
            KeyCode::ControlLeft,
            KeyCode::ControlRight,
            KeyCode::AltLeft,
            KeyCode::AltRight,
            KeyCode::SuperLeft,
            KeyCode::SuperRight,
            KeyCode::ShiftLeft,
            KeyCode::ShiftRight,
        ] {
            assert_eq!(
                requested(&[modifier, KeyCode::Digit3]),
                None,
                "{modifier:?}"
            );
        }
    }

    #[test]
    fn number_keys_work_again_once_the_modifier_is_released() {
        let mut controls = Controls::default();
        controls.set_key(KeyCode::ControlLeft, true);
        controls.set_key(KeyCode::ControlLeft, false);
        controls.request_slot(KeyCode::Digit3);
        assert_eq!(controls.take_slot(), Some(2));
    }

    #[test]
    fn number_keys_are_not_movement_keys() {
        assert_eq!(held(&[KeyCode::Digit1]), Movement::default());
    }

    #[test]
    fn wheel_down_steps_forward_and_up_steps_back() {
        let mut controls = Controls::default();
        controls.scroll(-1.0);
        assert_eq!(controls.take_wheel_steps(), 1);
        controls.scroll(1.0);
        assert_eq!(controls.take_wheel_steps(), -1);
        assert_eq!(controls.take_wheel_steps(), 0);
    }

    #[test]
    fn several_notches_in_one_frame_add_up() {
        let mut controls = Controls::default();
        controls.scroll(-1.0);
        controls.scroll(-1.0);
        controls.scroll(1.0);
        controls.scroll(-1.0);
        assert_eq!(controls.take_wheel_steps(), 2);
    }

    #[test]
    fn a_fraction_of_a_line_waits_for_the_rest() {
        let mut controls = Controls::default();
        controls.scroll(-0.5);
        assert_eq!(controls.take_wheel_steps(), 0);
        controls.scroll(-0.75);
        assert_eq!(controls.take_wheel_steps(), 1);
        // A quarter line is left over and no step is invented from it.
        assert_eq!(controls.take_wheel_steps(), 0);
        controls.scroll(-0.75);
        assert_eq!(controls.take_wheel_steps(), 1);
    }

    #[test]
    fn a_non_finite_scroll_is_ignored() {
        let mut controls = Controls::default();
        controls.scroll(-1.0);
        controls.scroll(f32::NAN);
        controls.scroll(f32::NEG_INFINITY);
        controls.scroll(f32::INFINITY);
        assert_eq!(controls.take_wheel_steps(), 1);
    }

    #[test]
    fn a_huge_scroll_saturates_without_overflow() {
        let mut controls = Controls::default();
        controls.scroll(f32::MAX);
        assert_eq!(controls.take_wheel_steps(), i32::MIN);
        // Two huge movements must not add up to infinity either.
        controls.scroll(f32::MAX);
        controls.scroll(f32::MAX);
        assert_eq!(controls.take_wheel_steps(), i32::MIN);
        controls.scroll(f32::MIN);
        assert_eq!(controls.take_wheel_steps(), i32::MAX);
    }

    #[test]
    fn smooth_scrolling_counts_pixels_per_line() {
        let mut controls = Controls::default();
        controls.scroll_pixels(-PIXELS_PER_WHEEL_LINE / 2.0);
        assert_eq!(controls.take_wheel_steps(), 0);
        controls.scroll_pixels(-PIXELS_PER_WHEEL_LINE / 2.0);
        assert_eq!(controls.take_wheel_steps(), 1);
        controls.scroll_pixels(PIXELS_PER_WHEEL_LINE * 3.0);
        assert_eq!(controls.take_wheel_steps(), -3);
        controls.scroll_pixels(f64::NAN);
        assert_eq!(controls.take_wheel_steps(), 0);
    }

    #[test]
    fn release_all_forgets_the_selection_requests() {
        let mut controls = Controls::default();
        controls.request_slot(KeyCode::Digit4);
        controls.scroll(-3.0);
        controls.release_all();
        assert_eq!(controls.take_slot(), None);
        assert_eq!(controls.take_wheel_steps(), 0);
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
