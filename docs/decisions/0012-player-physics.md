# 0012. Player physics: a walking mode with collision and gravity

Status: accepted.

## Context
The camera only flew through the terrain. Walking needs a body that collides with blocks, falls and jumps. Collision is a rule of the game (how a player moves), not of the world store, so the narrow core (`vox-world`) is left alone: the body only reads `World::block_at`.

## Decision
- **`vox-game/src/physics.rs`**, pure code: `Body` (an axis-aligned box of 0.6 x 1.8 x 0.6 blocks, eye 0.2 below the top, stored as its minimum corner) and `Body::step(world, WalkInput, dt)`.
- **Exact sweep per axis, Y then X then Z.** The cells the leading face crosses are tested in order and the box stops `SKIN` (0.001) short of the first solid one. No distance can tunnel, whatever the speed or frame time. A blocked axis loses only its own velocity, so the body slides along walls. The `SKIN` gap keeps rounding errors from leaving the box inside the block it touches; a box touching a face exactly overlaps nothing.
- **Sub-steps for consistent feel:** a frame is cut to the shared maximum frame time (`pacing::MAX_FRAME_TIME`, 0.1 s, also used by the flying camera) and split into equal parts of at most 1/60 s, so the height of a jump hardly depends on the frame rate (a test compares 144 and 20 fps).
- **What is solid:** every block that is not air; a cell of an unloaded chunk in or below the terrain layer (the player never falls into the void while chunks stream in, and does not leave the loaded area); a cell above the terrain layer is air. The last rule exists because there is no vertical streaming: without it a player switching to walking at the default camera height (60) would be inside "solid" cells and could not move sideways while falling.
- **Walking feel:** instant horizontal velocity (5 blocks/s, no acceleration), gravity 32 blocks/s^2, jump speed 9 blocks/s (peak about 1.2 blocks: one-block steps yes, two no), terminal velocity 50 blocks/s. All are named constants with reasons in the module.
- **G toggles fly/walk;** flying stays the default, and the `--fly` autopilot always flies. While flying the body just follows the camera (at rest); when switching to walking it starts from the camera's position. While walking the camera is set to the body's eye. Space jumps (held: it repeats on landing), Shift does nothing. Switching to walking is refused while the box overlaps solid blocks (`Body::is_free`), so the player cannot get trapped; flying is always allowed.
- **Standing is a ground probe**, a slab test `2 * SKIN` below the box, together with "not rising". "The last move was blocked" fails at high frame rates, where a fall is shorter than the gap and is never blocked (jumps were dropped above about 178 fps).
- **The swept cells are tested for the position that is stored** (`min + delta`, read as `min + size`), so float rounding cannot put the box into a cell that was not tested (a randomized test checks it against a wall).
- **Placing refuses any cell that overlaps the player's box** (`Body::overlaps_cell`), replacing the old rule that only protected the eye's cell. It is applied in fly mode too.

## Reasons
- The swept test is exact, short and has no tuning against speed; sub-steps are only for gravity integration.
- Storing the minimum corner means a stop at a block face is `cell - SKIN - size` or `cell + 1 + SKIN`, with no add-back of half a width.

## Rejected
- **Collision in `vox-world`:** it is a gameplay rule; the world core would gain a notion of "player" and solidity before blocks have properties. When blocks get properties (water, ladders) the solid test becomes the registry's question and the function `is_solid` is the one place to change.
- **Fixed small steps instead of a sweep:** needs a speed limit to be safe and costs more per frame.
- **Acceleration and friction, step-up onto slabs, swimming, crouching:** not needed yet.

## Known limitations
- **Not tried in the game window.** Feel (speed, jump, gravity numbers) is untested by a human.
- **Precision:** `SKIN` is reliable up to about 4000 blocks from the origin (the spacing of `f32` is 0.0005 there). Beyond about 16000 blocks the spacing (0.002) exceeds it and the body may stick to walls or hover a hair above the ground. There is deliberately no distance guard: the loaded world is far smaller. The fix, when worlds grow, is world-relative (chunk-local) coordinates for the body, not a bigger gap.
- **A box that ends up inside a solid block** (a block placed from elsewhere, a chunk that streams in around the player) is not pushed out; it can move out in any direction, since cells it already covers are ignored.
- **No on-screen indication** of the mode (no HUD line).
