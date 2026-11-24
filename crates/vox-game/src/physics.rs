//! The walking player's body: an axis-aligned box that falls, jumps and walks through the
//! blocks of a [`World`] without entering solid ones (decision 0012).
//!
//! Pure code with no window, renderer or threads. The box is moved one axis at a time with an
//! exact sweep: the cells the face of the box passes through are tested in order, so a body
//! never tunnels through a block however far it moves in one call. Axes are resolved Y first,
//! then X, then Z, and an axis that is blocked only loses its own velocity, so the body slides
//! along walls.
//!
//! What counts as solid (decision 0015): everything below the world's floor; every cell of a
//! column that is not loaded, at any height, so the player never falls into the void while
//! chunks stream in and cannot leave the loaded area; in a loaded column nothing at or above
//! the ceiling, any block that is not air, and in a chunk that is not in the world solid below
//! the column's span and air above it, which is what the generator has there.

use vox_world::{BlockId, ChunkPos, WORLD_CEILING_Y, WORLD_FLOOR_Y, World};

use crate::pacing::MAX_FRAME_TIME;
use crate::streaming::{Column, ColumnSpans};

/// Width of the box along X and Z, in blocks. Under one block so the player fits through
/// one-block gaps, wide enough to stand on a ledge edge: the proportions of a person.
pub const PLAYER_WIDTH: f32 = 0.6;
/// Height of the box, in blocks. Under two, so a two-block-high passage can be walked.
pub const PLAYER_HEIGHT: f32 = 1.8;
/// How far the eye is below the top of the box. A real head is a little taller than the eyes.
pub const EYE_BELOW_TOP: f32 = 0.2;
/// Height of the eye above the feet, derived from the two constants above.
pub const EYE_HEIGHT: f32 = PLAYER_HEIGHT - EYE_BELOW_TOP;
/// Half the width: the distance from the box's centre line to its side faces.
const HALF_WIDTH: f32 = PLAYER_WIDTH / 2.0;
/// The size of the box along X, Y and Z.
const SIZE: [f32; 3] = [PLAYER_WIDTH, PLAYER_HEIGHT, PLAYER_WIDTH];

/// Gap kept between the box and a block it stopped at, in blocks. Rounding errors of `f32`
/// positions must never leave the box a hair inside the block it touches, because the next
/// sweep would count that block as overlapped. The gap is far above the rounding error for
/// coordinates up to about 4000 blocks from the origin (the spacing of `f32` is 0.0005
/// there) and far too small to see. Farther out the spacing approaches the gap (0.002 beyond
/// 16000) and the body may stick to walls or float a hair above the ground; there is no
/// guard for it yet, because the loaded world is far smaller (decision 0012).
const SKIN: f32 = 1e-3;
/// How far below the box the ground probe looks, in blocks: twice [`SKIN`], so a body
/// resting at the gap (or slightly above it, after a move shorter than the gap) still counts
/// as standing.
const GROUND_PROBE: f32 = 2.0 * SKIN;

/// Downward acceleration, in blocks per second squared. About what Minecraft uses: a jump
/// feels snappy and a fall of a few blocks does not float.
pub const GRAVITY: f32 = 32.0;
/// Upward speed given by a jump, in blocks per second. With [`GRAVITY`] the peak is about
/// 1.2 blocks: enough to climb onto a one-block step, not onto a two-block wall.
pub const JUMP_SPEED: f32 = 9.0;
/// Walking speed, in blocks per second. Roughly a brisk walk in Minecraft; the speed is
/// reached at once, there is no acceleration to keep the controls simple.
pub const WALK_SPEED: f32 = 5.0;
/// Fastest fall, in blocks per second: a long fall stops getting quicker, and the fall per
/// sub-step stays below one block.
pub const TERMINAL_VELOCITY: f32 = 50.0;

/// Longest time of one integration sub-step, in seconds. Splitting a frame in equal parts
/// no longer than this makes the height of a jump nearly the same at every frame rate.
const MAX_SUBSTEP_TIME: f32 = 1.0 / 60.0;

/// What the body moves through: the chunks of the game's world and the spans of the loaded
/// columns, which tell what the layers missing from the world hold.
#[derive(Clone, Copy, Debug)]
pub struct Space<'a> {
    /// The chunks the game holds.
    pub world: &'a World,
    /// The span of every loaded column.
    pub spans: &'a ColumnSpans,
}

/// What the player asks of the body in one step.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct WalkInput {
    /// The wanted horizontal direction in world X and Z, of length 0 to 1.
    pub direction: [f32; 2],
    /// Whether the jump key is held; a jump happens whenever the body is on the ground.
    pub jump: bool,
}

/// The player's box, its velocity and whether it stands on something.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Body {
    /// The corner of the box with the smallest coordinates. Kept instead of the centre so a
    /// stop at a block face needs no addition that could round it into the block.
    min: [f32; 3],
    /// Blocks per second.
    velocity: [f32; 3],
    /// Set when the last step ended with the body blocked from below.
    on_ground: bool,
}

impl Body {
    /// A resting body whose eye is at `eye`.
    pub fn from_eye(eye: [f32; 3]) -> Self {
        Self {
            min: [
                eye[0] - HALF_WIDTH,
                eye[1] - EYE_HEIGHT,
                eye[2] - HALF_WIDTH,
            ],
            velocity: [0.0; 3],
            on_ground: false,
        }
    }

    /// Where the eye is: the camera sits here.
    pub fn eye(&self) -> [f32; 3] {
        [
            self.min[0] + HALF_WIDTH,
            self.min[1] + EYE_HEIGHT,
            self.min[2] + HALF_WIDTH,
        ]
    }

    /// Whether no solid cell overlaps the box. Walking from inside solid blocks would leave
    /// the player stuck, so the game only starts walking when this holds.
    pub fn is_free(&self, space: Space<'_>) -> bool {
        let [x, y, z] = [0, 1, 2].map(|axis| covered_cells(self.min[axis], SIZE[axis]));
        (x.0..=x.1)
            .all(|cx| (y.0..=y.1).all(|cy| (z.0..=z.1).all(|cz| !is_solid(space, [cx, cy, cz]))))
    }

    /// Whether the body stands on a block. Only the tests read it for now; the jump uses the
    /// field directly.
    #[cfg(test)]
    pub fn on_ground(&self) -> bool {
        self.on_ground
    }

    /// Whether the box overlaps the block cell `cell`. Touching a face is not overlapping.
    pub fn overlaps_cell(&self, cell: [i32; 3]) -> bool {
        (0..3).all(|axis| {
            let low = cell[axis] as f32;
            self.min[axis] < low + 1.0 && self.min[axis] + SIZE[axis] > low
        })
    }

    /// Advances the body by `dt` seconds under gravity and `input`.
    ///
    /// A `dt` that is not a positive finite number does nothing; a long one is cut to
    /// [`MAX_FRAME_TIME`]. The time is split into equal sub-steps of at most
    /// [`MAX_SUBSTEP_TIME`].
    pub fn step(&mut self, space: Space<'_>, input: WalkInput, dt: f32) {
        if !(dt.is_finite() && dt > 0.0) {
            return;
        }
        let dt = dt.min(MAX_FRAME_TIME);
        let substeps = (dt / MAX_SUBSTEP_TIME).ceil().max(1.0);
        let sub_dt = dt / substeps;
        for _ in 0..substeps as u32 {
            self.substep(space, input, sub_dt);
        }
    }

    /// One integration step: velocities first, then the three swept moves.
    fn substep(&mut self, space: Space<'_>, input: WalkInput, dt: f32) {
        if self.on_ground && input.jump {
            self.velocity[1] = JUMP_SPEED;
        }
        self.velocity[1] = (self.velocity[1] - GRAVITY * dt).max(-TERMINAL_VELOCITY);
        self.velocity[0] = input.direction[0] * WALK_SPEED;
        self.velocity[2] = input.direction[1] * WALK_SPEED;

        // Y first, so `on_ground` describes this step before the sideways moves.
        if self.move_axis(space, 1, self.velocity[1] * dt) {
            self.velocity[1] = 0.0;
        }
        // Standing is a probe below the box, not "the last move was blocked": at a high frame
        // rate a fall is shorter than the gap to the ground and is never blocked. A body
        // that is still rising is not standing, even right after it left the ground.
        self.on_ground = self.velocity[1] <= 0.0 && self.ground_below(space);
        for axis in [0, 2] {
            if self.move_axis(space, axis, self.velocity[axis] * dt) {
                self.velocity[axis] = 0.0;
            }
        }
    }

    /// Moves the box by `delta` along `axis` (0 = X, 1 = Y, 2 = Z) and stops it [`SKIN`]
    /// short of the first solid block in the way. Returns whether it was stopped.
    ///
    /// The cells tested are those the leading face crosses, beyond the cells the box already
    /// occupies, so a box that starts inside solid blocks (after a block was placed on it, or
    /// above the loaded area) can move out and is never pushed. A box touching a face
    /// exactly does not overlap the block behind that face.
    fn move_axis(&mut self, space: Space<'_>, axis: usize, delta: f32) -> bool {
        if delta == 0.0 {
            return false;
        }
        // The cells are tested for the very position that is stored afterwards, computed the
        // way `covered_cells` and `overlaps_cell` read it (`min + size`), so rounding cannot
        // make the box end in a cell that was not tested.
        let target = self.min[axis] + delta;
        if delta > 0.0 {
            let front = self.min[axis] + SIZE[axis];
            let first = front.ceil() as i32;
            let last = (target + SIZE[axis]).ceil() as i32 - 1;
            for cell in first..=last {
                if self.slab_is_solid(space, axis, cell) {
                    let stop = cell as f32 - SKIN - SIZE[axis];
                    // Never move against the requested direction to reach the gap.
                    self.min[axis] = stop.max(self.min[axis]);
                    return true;
                }
            }
        } else {
            let front = self.min[axis];
            let first = front.floor() as i32 - 1;
            let last = target.floor() as i32;
            for cell in (last..=first).rev() {
                if self.slab_is_solid(space, axis, cell) {
                    let stop = (cell + 1) as f32 + SKIN;
                    self.min[axis] = stop.min(self.min[axis]);
                    return true;
                }
            }
        }
        self.min[axis] = target;
        false
    }

    /// Whether a solid block lies within [`GROUND_PROBE`] below the box.
    fn ground_below(&self, space: Space<'_>) -> bool {
        self.slab_is_solid(space, 1, (self.min[1] - GROUND_PROBE).floor() as i32)
    }

    /// Whether any solid cell of the layer `cell` along `axis` lies under the box, that is
    /// among the cells the box covers on the two other axes.
    fn slab_is_solid(&self, space: Space<'_>, axis: usize, cell: i32) -> bool {
        let (u, v) = other_axes(axis);
        let (u_first, u_last) = covered_cells(self.min[u], SIZE[u]);
        let (v_first, v_last) = covered_cells(self.min[v], SIZE[v]);
        for cu in u_first..=u_last {
            for cv in v_first..=v_last {
                let mut position = [0; 3];
                position[axis] = cell;
                position[u] = cu;
                position[v] = cv;
                if is_solid(space, position) {
                    return true;
                }
            }
        }
        false
    }
}

/// The two axes other than `axis`, in increasing order.
fn other_axes(axis: usize) -> (usize, usize) {
    match axis {
        0 => (1, 2),
        1 => (0, 2),
        _ => (0, 1),
    }
}

/// The first and last cell that a span starting at `start` with length `length` overlaps. A
/// span that ends exactly on a cell boundary does not overlap the next cell.
fn covered_cells(start: f32, length: f32) -> (i32, i32) {
    (start.floor() as i32, (start + length).ceil() as i32 - 1)
}

/// Whether the body cannot enter the cell at `position`: see the module documentation.
pub fn is_solid(space: Space<'_>, position: [i32; 3]) -> bool {
    let y = position[1];
    if y < WORLD_FLOOR_Y {
        return true;
    }
    let chunk = ChunkPos::containing(position).0;
    // Checked before the ceiling, so the edge of the loaded area is a wall at every height and
    // a player above the build limit cannot leave the area either.
    let Some(span) = space.spans.get(Column::of(chunk)) else {
        return true;
    };
    if y >= WORLD_CEILING_Y {
        return false;
    }
    if let Some(block) = space.world.block_at(position) {
        return block != BlockId::AIR;
    }
    // A layer inside the span is always in the world once the column is loaded; were one
    // missing, solid keeps the player from falling through it.
    chunk.y <= *span.end()
}

#[cfg(test)]
mod tests {
    use vox_world::{BlockId, CHUNK_SIZE, Chunk, LocalPos};

    /// Any solid block: physics only asks whether a cell is air.
    const SOLID: BlockId = BlockId(3);

    use super::*;

    const SIDE: i32 = CHUNK_SIZE as i32;
    /// A frame at 60 frames per second.
    const FRAME: f32 = 1.0 / 60.0;
    /// Tolerance for positions that went through float arithmetic.
    const EPS: f32 = 1e-3;

    /// A world and the spans of its loaded columns, as the game keeps them.
    struct Scene {
        world: World,
        spans: ColumnSpans,
    }

    impl Scene {
        /// No chunk and no loaded column.
        fn empty() -> Self {
            Self {
                world: World::new(),
                spans: ColumnSpans::default(),
            }
        }

        fn space(&self) -> Space<'_> {
            Space {
                world: &self.world,
                spans: &self.spans,
            }
        }
    }

    /// The layer the test terrain lies in: the span of every loaded column of the tests.
    const TERRAIN_LAYER: i32 = 0;

    /// The layer-0 chunks of the columns around the origin (-1..=1 in X and Z), each filled by
    /// `fill(world_x, world_y, world_z)`, with every such column loaded with the span of layer 0.
    fn world_with(fill: impl Fn(i32, i32, i32) -> bool) -> Scene {
        let mut world = World::new();
        let mut spans = ColumnSpans::default();
        for cx in -1..=1 {
            for cz in -1..=1 {
                let mut chunk = Chunk::new();
                for x in 0..SIDE {
                    for y in 0..SIDE {
                        for z in 0..SIDE {
                            let (wx, wy, wz) = (cx * SIDE + x, y, cz * SIDE + z);
                            if fill(wx, wy, wz) {
                                if let Some(pos) = LocalPos::new(x as usize, y as usize, z as usize)
                                {
                                    chunk.set(pos, SOLID);
                                }
                            }
                        }
                    }
                }
                world.insert(ChunkPos::new(cx, TERRAIN_LAYER, cz), chunk);
                spans.insert(Column::new(cx, cz), TERRAIN_LAYER..=TERRAIN_LAYER);
            }
        }
        Scene { world, spans }
    }

    /// Stone for every block below height 10: the floor's top face is at y = 10.
    fn flat() -> Scene {
        world_with(|_, y, _| y < 10)
    }

    /// A body standing with its feet at `feet` (centre of the box's bottom face).
    fn body_at(feet: [f32; 3]) -> Body {
        Body::from_eye([feet[0], feet[1] + EYE_HEIGHT, feet[2]])
    }

    fn feet(body: &Body) -> [f32; 3] {
        [
            body.min[0] + PLAYER_WIDTH / 2.0,
            body.min[1],
            body.min[2] + PLAYER_WIDTH / 2.0,
        ]
    }

    fn run(body: &mut Body, world: &Scene, input: WalkInput, seconds: f32) {
        let frames = (seconds / FRAME).round() as u32;
        for _ in 0..frames {
            body.step(world.space(), input, FRAME);
        }
    }

    fn walk(x: f32, z: f32) -> WalkInput {
        WalkInput {
            direction: [x, z],
            jump: false,
        }
    }

    #[test]
    fn eye_round_trips() {
        let body = Body::from_eye([1.5, 12.0, -3.25]);
        let eye = body.eye();
        assert!((eye[0] - 1.5).abs() < 1e-6 && (eye[2] + 3.25).abs() < 1e-6);
        assert!((eye[1] - 12.0).abs() < 1e-6);
        assert!((feet(&body)[1] - (12.0 - EYE_HEIGHT)).abs() < 1e-6);
    }

    #[test]
    fn standing_on_a_flat_floor_stays_put() {
        let world = flat();
        let mut body = body_at([0.5, 10.0, 0.5]);
        run(&mut body, &world, WalkInput::default(), 2.0);
        let at = feet(&body);
        assert!((at[1] - 10.0).abs() < 2.0 * SKIN, "{at:?}");
        assert!((at[0] - 0.5).abs() < EPS && (at[2] - 0.5).abs() < EPS);
        assert!(body.on_ground());
    }

    #[test]
    fn falls_and_lands() {
        let world = flat();
        let mut body = body_at([0.5, 15.0, 0.5]);
        body.step(world.space(), WalkInput::default(), FRAME);
        assert!(!body.on_ground() && feet(&body)[1] < 15.0);
        run(&mut body, &world, WalkInput::default(), 2.0);
        assert!(body.on_ground());
        assert!((feet(&body)[1] - 10.0).abs() < 2.0 * SKIN);
    }

    #[test]
    fn a_long_fall_does_not_exceed_terminal_velocity() {
        let world = world_with(|_, _, _| false);
        let mut body = body_at([0.5, 25.0, 0.5]);
        run(&mut body, &world, WalkInput::default(), 1.0);
        assert!(body.velocity[1] >= -TERMINAL_VELOCITY);
        assert!(feet(&body)[1] < 25.0);
    }

    #[test]
    fn walking_moves_at_walk_speed() {
        let world = flat();
        let mut body = body_at([0.5, 10.0, 0.5]);
        run(&mut body, &world, walk(1.0, 0.0), 1.0);
        assert!((feet(&body)[0] - (0.5 + WALK_SPEED)).abs() < 0.05);
        assert!(body.on_ground());
    }

    #[test]
    fn cannot_walk_into_a_wall() {
        // A wall at x = 5 (the cell 5), two blocks high above the floor, wide in Z.
        let world = world_with(|x, y, _| y < 10 || (x == 5 && y < 12));
        let mut body = body_at([2.5, 10.0, 0.5]);
        run(&mut body, &world, walk(1.0, 0.0), 2.0);
        let max_x = feet(&body)[0] + PLAYER_WIDTH / 2.0;
        assert!(max_x <= 5.0 && max_x > 5.0 - 2.0 * SKIN, "{max_x}");
    }

    #[test]
    fn slides_along_a_wall() {
        let world = world_with(|x, y, _| y < 10 || (x == 5 && y < 12));
        let mut body = body_at([4.5, 10.0, 0.5]);
        // Pushing diagonally into the wall keeps the Z motion.
        let direction = [std::f32::consts::FRAC_1_SQRT_2; 2];
        run(&mut body, &world, walk(direction[0], direction[1]), 1.0);
        let at = feet(&body);
        assert!(at[0] + PLAYER_WIDTH / 2.0 <= 5.0);
        let expected = 0.5 + WALK_SPEED * direction[1];
        assert!((at[2] - expected).abs() < 0.05, "{at:?}");
    }

    #[test]
    fn jumps_to_the_expected_height() {
        let world = flat();
        let mut body = body_at([0.5, 10.0, 0.5]);
        let jump = WalkInput {
            direction: [0.0, 0.0],
            jump: true,
        };
        body.step(world.space(), WalkInput::default(), FRAME);
        assert!(body.on_ground());
        let mut peak = 0.0_f32;
        // Hold the key for one frame only, then watch the arc.
        body.step(world.space(), jump, FRAME);
        for _ in 0..120 {
            peak = peak.max(feet(&body)[1] - 10.0);
            body.step(world.space(), WalkInput::default(), FRAME);
        }
        // About 1.2 blocks, as the constants' documentation says: the continuous v^2 / 2g
        // is 1.27, and gravity already acts in the step of the jump (measured: 1.19).
        assert!(peak > 1.15 && peak < 1.25, "{peak}");
        assert!(body.on_ground(), "landed again");
    }

    #[test]
    fn cannot_jump_in_the_air() {
        let world = world_with(|_, _, _| false);
        let mut body = body_at([0.5, 20.0, 0.5]);
        let jump = WalkInput {
            direction: [0.0, 0.0],
            jump: true,
        };
        run(&mut body, &world, jump, 0.2);
        assert!(body.velocity[1] < 0.0);
    }

    #[test]
    fn the_jump_height_hardly_depends_on_the_frame_rate() {
        let world = flat();
        let peak_at = |dt: f32| {
            let mut body = body_at([0.5, 10.0, 0.5]);
            body.step(world.space(), WalkInput::default(), dt);
            let jump = WalkInput {
                direction: [0.0, 0.0],
                jump: true,
            };
            body.step(world.space(), jump, dt);
            let mut peak = 0.0_f32;
            for _ in 0..(2.0 / dt) as u32 {
                peak = peak.max(feet(&body)[1] - 10.0);
                body.step(world.space(), WalkInput::default(), dt);
            }
            peak
        };
        assert!((peak_at(1.0 / 144.0) - peak_at(1.0 / 20.0)).abs() < 0.08);
    }

    #[test]
    fn standing_stays_grounded_at_a_very_high_frame_rate() {
        let world = flat();
        let dt = 1.0 / 500.0;
        let mut body = body_at([0.5, 10.0, 0.5]);
        for frame in 0..1000 {
            body.step(world.space(), WalkInput::default(), dt);
            assert!(body.on_ground(), "frame {frame}");
        }
        assert!((feet(&body)[1] - 10.0).abs() < 2.0 * SKIN);
    }

    #[test]
    fn a_jump_tap_works_at_300_frames_per_second() {
        let world = flat();
        let dt = 1.0 / 300.0;
        let mut body = body_at([0.5, 10.0, 0.5]);
        // Settle, then tap for one frame.
        for _ in 0..300 {
            body.step(world.space(), WalkInput::default(), dt);
        }
        let tap = WalkInput {
            direction: [0.0, 0.0],
            jump: true,
        };
        body.step(world.space(), tap, dt);
        let mut peak = 0.0_f32;
        for _ in 0..300 {
            peak = peak.max(feet(&body)[1] - 10.0);
            body.step(world.space(), WalkInput::default(), dt);
        }
        assert!(peak > 1.15 && peak < 1.3, "{peak}");
    }

    #[test]
    fn a_jump_is_not_dropped_on_any_of_many_frame_rates() {
        let world = flat();
        for fps in [60.0, 144.0, 178.0, 240.0, 500.0, 1000.0, 2000.0] {
            let dt = 1.0 / fps;
            let mut body = body_at([0.5, 10.0, 0.5]);
            for _ in 0..(fps as u32) {
                body.step(world.space(), WalkInput::default(), dt);
            }
            let tap = WalkInput {
                direction: [0.0, 0.0],
                jump: true,
            };
            body.step(world.space(), tap, dt);
            body.step(world.space(), WalkInput::default(), dt);
            assert!(feet(&body)[1] > 10.0 + 0.5 * dt * JUMP_SPEED, "{fps} fps");
        }
    }

    #[test]
    fn rounding_never_leaves_the_box_inside_a_wall() {
        // A wall at x = 5; many starting positions and steps near it, with awkward floats.
        let world = world_with(|x, y, _| y < 10 || (x == 5 && y < 12));
        let mut seed = 12345_u32;
        let mut next = move || {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (seed >> 8) as f32 / (1u32 << 24) as f32
        };
        for _ in 0..20_000 {
            let mut body = Body {
                min: [3.6 + next() * 0.8, 10.0, 0.0],
                velocity: [0.0; 3],
                on_ground: true,
            };
            body.move_axis(world.space(), 0, next() * 0.3);
            body.move_axis(world.space(), 0, next() * 0.01);
            assert!(!body.overlaps_cell([5, 10, 0]), "min x = {}", body.min[0]);
            assert!(!body.overlaps_cell([5, 11, 0]), "min x = {}", body.min[0]);
        }
    }

    #[test]
    fn a_body_is_free_only_outside_solid_blocks() {
        let world = flat();
        assert!(body_at([0.5, 10.0, 0.5]).is_free(world.space()));
        // Feet one block into the floor.
        assert!(!body_at([0.5, 9.0, 0.5]).is_free(world.space()));
        // Head into a block placed beside the box's column, on a corner cell.
        let corner = world_with(|x, y, z| y < 10 || (x == 1 && z == 0 && y == 11));
        assert!(!body_at([0.9, 10.0, 0.5]).is_free(corner.space()));
        assert!(body_at([0.5, 10.0, 0.5]).is_free(corner.space()));
        // Negative coordinates, and a column that is not loaded (solid at any height).
        assert!(body_at([-0.5, 10.0, -0.5]).is_free(flat().space()));
        assert!(!body_at([0.5, 10.0, 0.5]).is_free(Scene::empty().space()));
        assert!(!body_at([0.5, 60.0, 0.5]).is_free(Scene::empty().space()));
        // Above the span of a loaded column is air, though that layer is not in the world.
        assert!(body_at([0.5, 60.0, 0.5]).is_free(flat().space()));
    }

    #[test]
    fn a_bumped_head_stops_the_rise_and_is_not_standing() {
        // A ceiling at y = 12 (cells 12 and up) above the floor.
        let world = world_with(|_, y, _| !(10..12).contains(&y));
        let mut body = body_at([0.5, 10.0, 0.5]);
        body.step(world.space(), WalkInput::default(), FRAME);
        body.step(
            world.space(),
            WalkInput {
                direction: [0.0, 0.0],
                jump: true,
            },
            FRAME,
        );
        let mut highest = 0.0_f32;
        for _ in 0..30 {
            highest = highest.max(feet(&body)[1] + PLAYER_HEIGHT);
            body.step(world.space(), WalkInput::default(), FRAME);
        }
        assert!(highest <= 12.0, "{highest}");
    }

    #[test]
    fn steps_off_a_ledge_and_falls() {
        // A platform of height 10 for x < 4, ground at height 5 beyond it.
        let world = world_with(|x, y, _| y < if x < 4 { 10 } else { 5 });
        let mut body = body_at([2.5, 10.0, 0.5]);
        run(&mut body, &world, walk(1.0, 0.0), 0.5);
        assert!(feet(&body)[0] > 4.0);
        run(&mut body, &world, walk(1.0, 0.0), 1.0);
        assert!(body.on_ground());
        assert!((feet(&body)[1] - 5.0).abs() < 2.0 * SKIN);
    }

    #[test]
    fn works_with_negative_coordinates_and_chunk_borders() {
        let world = flat();
        let mut body = body_at([-0.5, 10.0, -0.5]);
        // Cross the border at x = 0 and z = 0 diagonally, then go back across x = -SIDE.
        run(&mut body, &world, walk(1.0, 1.0), 0.5);
        assert!(feet(&body)[0] > 0.0 && feet(&body)[2] > 0.0 && body.on_ground());
        run(&mut body, &world, walk(-1.0, 0.0), 1.0);
        assert!(feet(&body)[0] < -0.5 && body.on_ground());
        assert!((feet(&body)[1] - 10.0).abs() < 2.0 * SKIN);
        // A wall on the border cell x = -1 stops a body walking from the positive side.
        let walled = world_with(|x, y, _| y < 10 || (x == -1 && y < 12));
        let mut body = body_at([1.5, 10.0, 0.5]);
        run(&mut body, &walled, walk(-1.0, 0.0), 1.0);
        assert!(
            body.min[0] >= 0.0 && body.min[0] < 2.0 * SKIN,
            "{}",
            body.min[0]
        );
    }

    #[test]
    fn a_wall_on_a_chunk_border_blocks() {
        // The cell x = SIDE - 1 is the last of chunk 0; the next chunk starts at SIDE.
        let world = world_with(|x, y, _| y < 10 || (x == SIDE && y < 12));
        let mut body = body_at([SIDE as f32 - 2.0, 10.0, 0.5]);
        run(&mut body, &world, walk(1.0, 0.0), 2.0);
        let max_x = body.min[0] + PLAYER_WIDTH;
        assert!(
            max_x <= SIDE as f32 && max_x > SIDE as f32 - 2.0 * SKIN,
            "{max_x}"
        );
    }

    #[test]
    fn chunks_missing_from_a_loaded_column_are_solid_below_its_span_and_air_above() {
        // Column (0, 0) is loaded with the span of layer 0 but holds no chunk at all.
        let mut world = Scene::empty();
        world
            .spans
            .insert(Column::new(0, 0), TERRAIN_LAYER..=TERRAIN_LAYER);
        let space = world.space();
        // Inside the span (missing only by mistake) and below it: solid.
        assert!(is_solid(space, [0, 5, 0]));
        assert!(is_solid(space, [0, -1, 0]));
        assert!(is_solid(space, [SIDE - 1, -5 * SIDE, SIDE - 1]));
        // Above the span: air.
        assert!(!is_solid(space, [0, SIDE, 0]));
        assert!(!is_solid(space, [0, 3 * SIDE, 0]));
        // A span lower down moves the border with it.
        world.spans.insert(Column::new(0, 0), -3..=-2);
        assert!(!is_solid(world.space(), [0, -SIDE, 0]));
        assert!(is_solid(world.space(), [0, -2 * SIDE, 0]));
    }

    #[test]
    fn every_cell_of_a_column_that_is_not_loaded_is_solid() {
        let world = flat();
        // Column (3, -4) is not among the loaded 3 x 3; above the ceiling too.
        for y in [
            -SIDE,
            0,
            5,
            SIDE,
            3 * SIDE,
            WORLD_CEILING_Y,
            WORLD_CEILING_Y + SIDE,
        ] {
            assert!(is_solid(world.space(), [100, y, -100]), "y {y}");
        }
    }

    #[test]
    fn below_the_floor_is_solid_and_from_the_ceiling_up_is_air() {
        // Literal bounds of decision 0015.
        let world = flat();
        for x in [0, 1000] {
            assert!(is_solid(world.space(), [x, -513, 0]));
            assert!(is_solid(world.space(), [x, i32::MIN, 0]));
        }
        // From the ceiling up a loaded column is air, a column that is not loaded stays solid.
        assert!(!is_solid(world.space(), [0, 512, 0]));
        assert!(!is_solid(world.space(), [0, i32::MAX, 0]));
        assert!(is_solid(world.space(), [1000, 512, 0]));
        assert!(is_solid(world.space(), [1000, i32::MAX, 0]));
        // The last row inside is decided by the span (a loaded column, above its span: air).
        assert!(!is_solid(world.space(), [0, 511, 0]));
        // The floor row is bedrock, below the span: solid.
        assert!(is_solid(world.space(), [0, -512, 0]));
    }

    #[test]
    fn a_body_cannot_fall_below_the_floor() {
        // The two lowest layers of a loaded column are all air, even the bedrock row (the game
        // never allows that, but the rule must not depend on it): the body stands on the floor.
        let mut world = Scene::empty();
        for layer in -16..=-15 {
            world.world.insert(ChunkPos::new(0, layer, 0), Chunk::new());
        }
        world.spans.insert(Column::new(0, 0), -16..=-15);
        let mut body = body_at([16.0, -500.0, 16.0]);
        run(&mut body, &world, WalkInput::default(), 2.0);
        assert!(body.on_ground());
        assert!((feet(&body)[1] - WORLD_FLOOR_Y as f32).abs() < 2.0 * SKIN);
    }

    #[test]
    fn a_body_does_not_fall_into_an_unloaded_chunk() {
        // Only chunk (0, 0, 0) is loaded, and it is empty; beyond it nothing is loaded.
        let mut world = Scene::empty();
        world
            .world
            .insert(ChunkPos::new(0, TERRAIN_LAYER, 0), Chunk::new());
        world
            .spans
            .insert(Column::new(0, 0), TERRAIN_LAYER..=TERRAIN_LAYER);
        let mut body = body_at([SIDE as f32 / 2.0, 20.0, SIDE as f32 / 2.0]);
        run(&mut body, &world, walk(1.0, 0.0), 3.0);
        // Falls to the bottom of the loaded chunk (the layer below is unloaded, so solid)...
        assert!(body.on_ground());
        assert!((feet(&body)[1] - 0.0).abs() < 2.0 * SKIN);
        // ... and the unloaded chunk to the east stops it at the edge.
        assert!(body.min[0] + PLAYER_WIDTH <= SIDE as f32);
    }

    #[test]
    fn a_body_above_the_terrain_layer_can_move_sideways_while_falling() {
        let world = flat();
        let mut body = body_at([0.5, 60.0, 0.5]);
        run(&mut body, &world, walk(1.0, 0.0), 0.5);
        assert!(feet(&body)[0] > 2.0);
        assert!(feet(&body)[1] < 60.0);
    }

    #[test]
    fn a_huge_step_does_not_tunnel() {
        let world = flat();
        // The frame is cut to MAX_FRAME_TIME, and a thin floor is still not crossed.
        let thin = world_with(|_, y, _| y == 10);
        let mut body = body_at([0.5, 12.0, 0.5]);
        body.step(thin.space(), WalkInput::default(), 1000.0);
        body.step(thin.space(), WalkInput::default(), 1000.0);
        assert!(feet(&body)[1] >= 11.0 - EPS);
        // The sweep itself is exact for any distance.
        let mut body = body_at([0.5, 20.0, 0.5]);
        assert!(body.move_axis(world.space(), 1, -1000.0));
        assert!((feet(&body)[1] - (10.0 + SKIN)).abs() < 1e-5);
        let wall = world_with(|x, y, _| y < 10 || (x == 7 && y < 12));
        let mut body = body_at([0.5, 10.0, 0.5]);
        assert!(body.move_axis(wall.space(), 0, 1000.0));
        assert!(body.min[0] + PLAYER_WIDTH <= 7.0);
    }

    #[test]
    fn a_step_that_is_not_a_positive_number_does_nothing() {
        let world = flat();
        let start = body_at([0.5, 12.0, 0.5]);
        for dt in [0.0, -1.0, f32::NAN, f32::INFINITY] {
            let mut body = start;
            body.step(world.space(), WalkInput::default(), dt);
            assert_eq!(body, start);
        }
    }

    #[test]
    fn touching_a_block_edge_exactly_neither_blocks_nor_overlaps() {
        // A solid cell at x = 5 (y 10..12); the box's max X is exactly 5.0, feet on the floor.
        let world = world_with(|x, y, z| y < 10 || (x == 5 && y < 12 && z == 0));
        let mut body = Body {
            min: [5.0 - PLAYER_WIDTH, 10.0, -0.3],
            velocity: [0.0; 3],
            on_ground: false,
        };
        assert_eq!(body.min[0] + SIZE[0], 5.0, "the test needs an exact edge");
        // The box ends exactly on the face, beside the cell, so it overlaps nothing.
        assert!(!body.overlaps_cell([5, 10, 0]));
        assert!(body.overlaps_cell([4, 10, 0]));
        // Moving on X into the touching block does not move the box at all.
        let before = body.min;
        assert!(body.move_axis(world.space(), 0, 1.0));
        assert_eq!(body.min, before);
        // Moving on Z, parallel to the face, is free.
        assert!(!body.move_axis(world.space(), 2, 1.0));
        // Moving down onto the floor with feet exactly at its top does not move it.
        let mut resting = Body {
            min: [0.2, 10.0, 0.2],
            velocity: [0.0; 3],
            on_ground: false,
        };
        assert!(resting.move_axis(world.space(), 1, -1.0));
        assert_eq!(resting.min[1], 10.0);
    }

    #[test]
    fn a_body_inside_solid_blocks_can_leave_and_is_not_pushed() {
        let world = world_with(|x, y, z| y < 10 || (x == 0 && z == 0 && y < 13));
        let mut body = body_at([0.5, 10.0, 0.5]);
        let before = body.min;
        // Standing in a block placed on it: no push on a zero move.
        assert!(!body.move_axis(world.space(), 0, 0.0));
        assert_eq!(body.min, before);
        // Moving sideways out of it is possible (the cells already covered are not tested).
        assert!(!body.move_axis(world.space(), 0, 1.0));
        assert!(body.min[0] > before[0]);
    }

    #[test]
    fn overlap_uses_the_box_not_the_eye() {
        let body = body_at([0.5, 10.0, 0.5]);
        // Feet cell, head cell and the cell of the eye overlap; the cell above the head and the
        // cells beside the box do not.
        assert!(body.overlaps_cell([0, 10, 0]));
        assert!(body.overlaps_cell([0, 11, 0]));
        assert!(!body.overlaps_cell([0, 12, 0]));
        assert!(!body.overlaps_cell([1, 10, 0]));
        assert!(!body.overlaps_cell([0, 9, 0]));
        // Negative coordinates: a box at x in -0.8..-0.2 is in cell -1 only.
        let negative = body_at([-0.5, 10.0, -0.5]);
        assert!(negative.overlaps_cell([-1, 10, -1]));
        assert!(!negative.overlaps_cell([0, 10, -1]));
        assert!(!negative.overlaps_cell([-2, 10, -1]));
    }

    /// The relations the module's documentation promises, checked when the crate compiles.
    const _: () = {
        assert!(EYE_HEIGHT < PLAYER_HEIGHT && EYE_HEIGHT > 0.0);
        assert!(PLAYER_WIDTH < 1.0 && PLAYER_HEIGHT < 2.0);
        assert!(TERMINAL_VELOCITY * MAX_SUBSTEP_TIME < 1.0);
    };
}
