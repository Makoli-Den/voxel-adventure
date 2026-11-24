# 0016. Input capture and the pause menu

Status: accepted. Supersedes in part decision 0011 (the right mouse button as "look").

## Context
The camera turned only while the right mouse button was held, and the cursor was always visible (decision 0011). That makes looking and clicking awkward at the same time, lets the cursor leave the window mid-turn, and offers no way to stop the game or quit except closing the window. A first-person game captures the mouse while playing and frees it in a menu. Scripted runs (`--background`, the `--fly` autopilot) must keep working without anyone touching the window.

## Decision
- **Three input modes, a pure state machine** (`vox-game/src/input_mode.rs`, no winit calls): `AwaitingFocus`, `Playing`, `Paused`. The window code feeds it events and asks it what to do; every rule is a unit test.
  - Esc opens the menu while playing and closes it while paused.
  - Losing the window focus opens the menu and forgets held keys and pending requests.
  - Gaining focus does not close the menu: the player resumes on purpose.
  - A run started with `--background` or `--fly` begins in `AwaitingFocus` (`starts_awaiting(options)`): no capture, no menu, and the autopilot is never paused, so measurement runs behave as before.
- **What a mode change does** is one pure function, `effects(before, after) -> Effects { release_controls, apply_cursor }`, used by the window code, so the side effects are tested with the transitions.
- **Capture:** while playing the cursor is hidden and grabbed: `CursorGrabMode::Locked` first; `Confined` is the fallback for platforms that refuse `Locked`. Both failures are logged as warnings; a failed capture costs only the capture. Otherwise the grab is released and the cursor shown.
- **Focus gained always re-applies the cursor state** (it is idempotent): winit does not release the grab on focus loss on Windows, and the per-frame refresh runs only while focused, so a paused window releases the cursor the moment it becomes active again. The release on focus loss stays too.
- **The click that focuses the window does nothing:** it neither edits a block (as before) nor activates a menu button. Clicks in the same event batch as the focus gain are skipped; the decision is a pure, tested function.
- **The menu:** a dimmed screen, the title "Paused" and two buttons, Resume and Quit, drawn with the existing overlay rectangles (decision 0009). Up/W and Down/S move the selection (wrapping), Enter activates it, the mouse selects by hovering and activates by clicking. The button rectangles come from one cheap layout function (`menu_buttons(width, height)`) that both drawing and hit testing use, so they cannot disagree. The button width is derived from the longest label plus a named padding.

## Reasons
- Captured mouse look is what players expect from a first-person game and frees both buttons for actions.
- A pure state machine and a pure effects function keep the winit code thin and make focus edge cases testable without opening a window (the project's rule on window tests).
- Starting scripted runs in `AwaitingFocus` keeps benchmarks free of a menu and of a grabbed cursor that would interrupt the person at the machine.

## Rejected
- **Keeping right-button look:** both hands busy for one action, and the cursor can still leave the window while turning.
- **An always-visible cursor while playing:** it drifts over the view and out of the window; the crosshair already marks the target.
- **Closing the menu on focus gain:** the focusing click would land in the game.

## Known limitations
- **Keys held across the pause** are forgotten when the menu opens; a key still held on Resume must be pressed again (documented in the README).
- **`Locked` on Windows is clip-based:** winit clips the cursor to a one-pixel rectangle rather than truly locking it, and does not release the clip on its own when focus is lost; the focus handling above covers that.
- **The menu is not scaled by DPI**, consistent with the HUD panel: on a high-DPI screen it is small.
