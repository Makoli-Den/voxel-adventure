//! Who gets the player's input: the game or the pause menu.
//!
//! The mouse is captured while the player plays, and released while the menu is open. This
//! module is the state machine for that, kept apart from every winit call so each rule is a
//! unit test: the window code only feeds it events (Esc, focus gained or lost, a menu button)
//! and reads back what to do with the cursor and the input.
//!
//! The rules:
//! - Esc opens the menu while playing and closes it again.
//! - Losing the window focus opens the menu. Gaining it again does not close it: the player
//!   resumes on purpose, so the click that focused the window never breaks a block.
//! - A window opened with `--background` starts [`Mode::AwaitingFocus`]: no menu and no
//!   capture until the window is focused for the first time, so scripted checks that never
//!   focus the window behave exactly as before.

use crate::cli::Options;

/// What the input currently does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Started in the background and never focused yet: the game runs, the cursor is left
    /// alone and no menu is shown.
    AwaitingFocus,
    /// The game has the input: the cursor is hidden and captured and the mouse turns the camera.
    Playing,
    /// The menu is open: the cursor is free and the game ignores its input.
    Paused,
}

/// A button of the pause menu, in the order it is drawn from top to bottom.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuButton {
    /// Closes the menu and goes back to the game.
    Resume,
    /// Ends the game.
    Quit,
}

impl MenuButton {
    /// Every button, top to bottom.
    pub const ALL: [MenuButton; 2] = [MenuButton::Resume, MenuButton::Quit];

    /// The position of the button in [`MenuButton::ALL`].
    fn index(self) -> usize {
        match self {
            MenuButton::Resume => 0,
            MenuButton::Quit => 1,
        }
    }
}

/// What the window code has to do after a button was used.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Nothing more than what the mode change implies.
    Continue,
    /// End the game.
    Quit,
}

/// Whether a run starts in [`Mode::AwaitingFocus`]: a `--background` window may never be
/// focused, and a `--fly` run is a measurement nobody sits at, so neither captures the cursor,
/// shows a menu or pauses its autopilot.
pub fn starts_awaiting(options: &Options) -> bool {
    options.background || options.fly_seconds.is_some()
}

/// Whether a click on the menu may activate a button. The click that gave the window its focus
/// (`just_focused`) must not: it was meant to focus, not to choose.
pub fn click_activates(just_focused: bool) -> bool {
    !just_focused
}

/// What the window code has to do when the mode changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Effects {
    /// Forget held keys and pending requests: their releases now go to the menu.
    pub release_controls: bool,
    /// Capture or free the cursor to match the new mode.
    pub apply_cursor: bool,
}

/// The side effects of going from mode `before` to mode `after`.
pub fn effects(before: Mode, after: Mode) -> Effects {
    Effects {
        release_controls: before != Mode::Paused && after == Mode::Paused,
        apply_cursor: before != after,
    }
}

/// The input mode and the menu's selected button.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InputMode {
    mode: Mode,
    /// The button Enter would activate; only meaningful while paused.
    selected: MenuButton,
}

impl InputMode {
    /// The mode at startup: playing, or waiting for the first focus when the window was
    /// opened in the background.
    pub fn new(background: bool) -> Self {
        Self {
            mode: if background {
                Mode::AwaitingFocus
            } else {
                Mode::Playing
            },
            selected: MenuButton::Resume,
        }
    }

    /// The current mode.
    pub fn mode(&self) -> Mode {
        self.mode
    }

    /// Whether the cursor must be hidden and captured, and the mouse turns the camera.
    pub fn is_playing(&self) -> bool {
        self.mode == Mode::Playing
    }

    /// Whether the menu is open: it is drawn and gets the input, and the game stands still.
    pub fn is_paused(&self) -> bool {
        self.mode == Mode::Paused
    }

    /// The button Enter would activate.
    pub fn selected(&self) -> MenuButton {
        self.selected
    }

    /// Opens the menu with the first button selected.
    fn open_menu(&mut self) {
        self.mode = Mode::Paused;
        self.selected = MenuButton::Resume;
    }

    /// The Escape key was pressed: opens the menu, or closes it again.
    pub fn escape(&mut self) {
        match self.mode {
            Mode::Playing => self.open_menu(),
            Mode::Paused => self.mode = Mode::Playing,
            // No focus yet, so no key can have come from the player.
            Mode::AwaitingFocus => {}
        }
    }

    /// The window gained focus. Ends the wait of a background window; an open menu stays open.
    pub fn focus_gained(&mut self) {
        if self.mode == Mode::AwaitingFocus {
            self.mode = Mode::Playing;
        }
    }

    /// The window lost focus: opens the menu, so the cursor is released and nothing happens
    /// behind the player's back. A background window that was never focused stays as it is.
    pub fn focus_lost(&mut self) {
        if self.mode == Mode::Playing {
            self.open_menu();
        }
    }

    /// Selects the button above the selected one, wrapping to the last. Only while paused.
    pub fn select_previous(&mut self) {
        self.step_selection(MenuButton::ALL.len() - 1);
    }

    /// Selects the button below the selected one, wrapping to the first. Only while paused.
    pub fn select_next(&mut self) {
        self.step_selection(1);
    }

    /// Moves the selection by `step` buttons, wrapping around.
    fn step_selection(&mut self, step: usize) {
        if self.is_paused() {
            let next = (self.selected.index() + step) % MenuButton::ALL.len();
            self.selected = MenuButton::ALL[next];
        }
    }

    /// Selects `button`, for example because the mouse is over it. Only while paused.
    pub fn select(&mut self, button: MenuButton) {
        if self.is_paused() {
            self.selected = button;
        }
    }

    /// Uses `button`. Only while paused; at any other time nothing happens.
    pub fn activate(&mut self, button: MenuButton) -> Outcome {
        if !self.is_paused() {
            return Outcome::Continue;
        }
        match button {
            MenuButton::Resume => {
                self.mode = Mode::Playing;
                Outcome::Continue
            }
            MenuButton::Quit => Outcome::Quit,
        }
    }

    /// Uses the selected button (the Enter key).
    pub fn activate_selected(&mut self) -> Outcome {
        self.activate(self.selected)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paused() -> InputMode {
        let mut input = InputMode::new(false);
        input.escape();
        input
    }

    #[test]
    fn a_normal_start_plays() {
        let input = InputMode::new(false);
        assert_eq!(input.mode(), Mode::Playing);
        assert!(input.is_playing() && !input.is_paused());
    }

    #[test]
    fn a_background_start_waits_without_menu_or_capture() {
        let input = InputMode::new(true);
        assert_eq!(input.mode(), Mode::AwaitingFocus);
        assert!(!input.is_playing() && !input.is_paused());
    }

    #[test]
    fn escape_opens_and_closes_the_menu() {
        let mut input = InputMode::new(false);
        input.escape();
        assert_eq!(input.mode(), Mode::Paused);
        input.escape();
        assert_eq!(input.mode(), Mode::Playing);
    }

    #[test]
    fn escape_does_nothing_before_the_first_focus() {
        let mut input = InputMode::new(true);
        input.escape();
        assert_eq!(input.mode(), Mode::AwaitingFocus);
    }

    #[test]
    fn the_first_focus_starts_the_game_of_a_background_window() {
        let mut input = InputMode::new(true);
        input.focus_gained();
        assert_eq!(input.mode(), Mode::Playing);
    }

    #[test]
    fn losing_focus_before_the_first_focus_changes_nothing() {
        let mut input = InputMode::new(true);
        input.focus_lost();
        assert_eq!(input.mode(), Mode::AwaitingFocus);
    }

    #[test]
    fn losing_focus_opens_the_menu() {
        let mut input = InputMode::new(false);
        input.focus_lost();
        assert!(input.is_paused());
    }

    #[test]
    fn gaining_focus_does_not_close_the_menu() {
        let mut input = InputMode::new(false);
        input.focus_lost();
        input.focus_gained();
        assert!(input.is_paused());
    }

    #[test]
    fn gaining_focus_while_playing_changes_nothing() {
        let mut input = InputMode::new(false);
        input.focus_gained();
        assert_eq!(input.mode(), Mode::Playing);
    }

    #[test]
    fn losing_focus_while_paused_keeps_the_selection() {
        let mut input = paused();
        input.select_next();
        input.focus_lost();
        assert_eq!(input.selected(), MenuButton::Quit);
    }

    #[test]
    fn the_menu_opens_with_resume_selected() {
        let mut input = paused();
        input.select_next();
        input.escape();
        input.escape();
        assert_eq!(input.selected(), MenuButton::Resume);
    }

    #[test]
    fn selection_wraps_in_both_directions() {
        let mut input = paused();
        input.select_next();
        assert_eq!(input.selected(), MenuButton::Quit);
        input.select_next();
        assert_eq!(input.selected(), MenuButton::Resume);
        input.select_previous();
        assert_eq!(input.selected(), MenuButton::Quit);
        input.select_previous();
        assert_eq!(input.selected(), MenuButton::Resume);
    }

    #[test]
    fn selection_changes_only_while_paused() {
        let mut input = InputMode::new(false);
        input.select_next();
        input.select(MenuButton::Quit);
        assert_eq!(input.selected(), MenuButton::Resume);
    }

    #[test]
    fn the_mouse_can_select_a_button() {
        let mut input = paused();
        input.select(MenuButton::Quit);
        assert_eq!(input.selected(), MenuButton::Quit);
    }

    #[test]
    fn resume_closes_the_menu() {
        let mut input = paused();
        assert_eq!(input.activate_selected(), Outcome::Continue);
        assert_eq!(input.mode(), Mode::Playing);
    }

    #[test]
    fn quit_asks_to_end_the_game() {
        let mut input = paused();
        input.select_next();
        assert_eq!(input.activate_selected(), Outcome::Quit);
        assert_eq!(input.activate(MenuButton::Quit), Outcome::Quit);
    }

    #[test]
    fn buttons_do_nothing_while_the_menu_is_closed() {
        for background in [false, true] {
            let mut input = InputMode::new(background);
            let before = input.mode();
            assert_eq!(input.activate(MenuButton::Quit), Outcome::Continue);
            assert_eq!(input.activate_selected(), Outcome::Continue);
            assert_eq!(input.mode(), before);
        }
    }

    fn options(background: bool, fly: Option<f32>) -> Options {
        Options {
            background,
            fly_seconds: fly,
            ..Options::default()
        }
    }

    #[test]
    fn scripted_runs_start_awaiting_focus() {
        assert!(!starts_awaiting(&options(false, None)));
        assert!(starts_awaiting(&options(true, None)));
        assert!(starts_awaiting(&options(false, Some(5.0))));
        assert!(starts_awaiting(&options(true, Some(5.0))));
    }

    #[test]
    fn the_focusing_click_does_not_activate() {
        assert!(!click_activates(true));
        assert!(click_activates(false));
    }

    #[test]
    fn opening_the_menu_releases_the_controls_and_the_cursor() {
        let e = effects(Mode::Playing, Mode::Paused);
        assert!(e.release_controls && e.apply_cursor);
    }

    #[test]
    fn resuming_captures_without_releasing_the_controls() {
        let e = effects(Mode::Paused, Mode::Playing);
        assert!(!e.release_controls && e.apply_cursor);
    }

    #[test]
    fn the_first_focus_captures() {
        let e = effects(Mode::AwaitingFocus, Mode::Playing);
        assert!(!e.release_controls && e.apply_cursor);
    }

    #[test]
    fn no_change_has_no_effects() {
        for mode in [Mode::AwaitingFocus, Mode::Playing, Mode::Paused] {
            let e = effects(mode, mode);
            assert!(!e.release_controls && !e.apply_cursor, "{mode:?}");
        }
    }

    #[test]
    fn every_button_has_its_own_index() {
        for (position, button) in MenuButton::ALL.into_iter().enumerate() {
            assert_eq!(button.index(), position);
        }
    }
}
