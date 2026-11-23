//! Block editing decisions: what to do when the player breaks or places a block.
//!
//! Pure code with no window, renderer or threads: given what the ray hit and what the player
//! asked for, [`plan`] says which block to write where, and [`apply_streamed`] keeps the
//! game's world in step with the chunk streaming. The game (`main.rs`) casts the ray, writes
//! the block with `World::set_block` and rebuilds the meshes (decision 0011).

use vox_world::{BlockId, Chunk, ChunkPos, RayHit, World};

use crate::physics::Body;

/// How far the player reaches, in blocks, measured from the eye along the look direction.
/// About the reach of Minecraft's creative mode plus a margin: far enough to build without
/// walking up to every block, short enough that the block under the crosshair is clearly the
/// one meant.
pub const REACH: f32 = 6.0;

/// What the player asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditAction {
    /// Remove the block under the crosshair.
    Break,
    /// Put the selected block on the face of the block under the crosshair.
    Place,
}

/// One block to write.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Edit {
    /// World position of the block to write.
    pub position: [i32; 3],
    /// What to write there.
    pub block: BlockId,
}

/// Decides what `action` does when the player `body` looks at `hit`, or `None` if nothing may
/// happen.
///
/// - [`EditAction::Break`] writes air into the block that was hit.
/// - [`EditAction::Place`] writes `block` into the cell in front of the hit face, unless that
///   cell overlaps the player's box (the player would be stuck inside the block; this
///   includes the cell of the eye), or the ray started inside a solid block (`distance` is
///   zero: there is no face in front of which to place anything).
pub fn plan(action: EditAction, hit: &RayHit, body: &Body, block: BlockId) -> Option<Edit> {
    match action {
        EditAction::Break => Some(Edit {
            position: hit.block,
            block: BlockId::AIR,
        }),
        EditAction::Place => {
            let position = hit.adjacent();
            let inside_a_block = hit.distance <= 0.0;
            (!inside_a_block && !body.overlaps_cell(position)).then_some(Edit { position, block })
        }
    }
}

/// Brings `world` in step with one frame of streaming: drops the unloaded chunks and adds the
/// loaded ones.
///
/// The streaming tracker hands out a position once between two unloads, so a loaded chunk is
/// never already in the world; a debug build asserts it, because inserting over a chunk would
/// silently undo the edits of the player. Edits are not saved: a chunk that is unloaded and
/// loaded again is generated anew (decision 0011).
pub fn apply_streamed(world: &mut World, removed: &[ChunkPos], loaded: Vec<(ChunkPos, Chunk)>) {
    for pos in removed {
        world.remove(*pos);
    }
    for (pos, chunk) in loaded {
        debug_assert!(!world.contains(pos), "{pos:?} streamed in twice");
        world.insert(pos, chunk);
    }
}

#[cfg(test)]
mod tests {
    use vox_world::{CHUNK_SIZE, Face, LocalPos};

    use super::*;

    /// The block the tests place and hit; its value is arbitrary, the code only moves it around.
    const PLACE_BLOCK: BlockId = BlockId(3);

    /// The eye of the tests: in the middle of block (0, 1, 0).
    const EYE: [f32; 3] = [0.5, 1.5, 0.5];

    fn body(eye: [f32; 3]) -> Body {
        Body::from_eye(eye)
    }

    fn hit(block: [i32; 3], face: Face, distance: f32) -> RayHit {
        RayHit {
            block,
            id: PLACE_BLOCK,
            face,
            distance,
        }
    }

    #[test]
    fn breaking_writes_air_into_the_hit_block() {
        let edit = plan(
            EditAction::Break,
            &hit([3, 4, 5], Face::PosY, 2.0),
            &body(EYE),
            PLACE_BLOCK,
        );
        assert_eq!(
            edit,
            Some(Edit {
                position: [3, 4, 5],
                block: BlockId::AIR
            })
        );
    }

    #[test]
    fn placing_writes_the_block_into_the_cell_in_front_of_the_face() {
        let edit = plan(
            EditAction::Place,
            &hit([3, 4, 5], Face::PosY, 2.0),
            &body(EYE),
            PLACE_BLOCK,
        );
        assert_eq!(
            edit,
            Some(Edit {
                position: [3, 5, 5],
                block: PLACE_BLOCK
            })
        );
    }

    #[test]
    fn placing_into_the_cell_of_the_eye_is_refused() {
        // The block at (0, 0, 0) is hit on its top face, and the eye is in the cell above it.
        let target = hit([0, 0, 0], Face::PosY, 0.5);
        assert_eq!(
            plan(EditAction::Place, &target, &body(EYE), PLACE_BLOCK),
            None
        );
        // Breaking it is fine.
        assert!(plan(EditAction::Break, &target, &body(EYE), PLACE_BLOCK).is_some());
    }

    #[test]
    fn placing_into_any_cell_of_the_box_is_refused() {
        // The box of the eye at EYE covers the cells y = -1 (feet), 0 and 1 above (0, _, 0).
        let player = body(EYE);
        for y in -1..=1 {
            let target = hit([0, y - 1, 0], Face::PosY, 1.0);
            assert_eq!(
                plan(EditAction::Place, &target, &player, PLACE_BLOCK),
                None,
                "cell y = {y}"
            );
        }
        // The cell above the head (the box ends at 1.7) and the cell beside the box are free.
        let above = hit([0, 1, 0], Face::PosY, 1.0);
        assert!(plan(EditAction::Place, &above, &player, PLACE_BLOCK).is_some());
        let beside = hit([0, 0, 0], Face::PosX, 1.0);
        assert!(plan(EditAction::Place, &beside, &player, PLACE_BLOCK).is_some());
    }

    #[test]
    fn the_box_is_found_with_floor_for_negative_coordinates() {
        // Eye at (-0.5, -0.5, -0.5) is in block (-1, -1, -1), not (0, 0, 0).
        let player = body([-0.5, -0.5, -0.5]);
        let target = hit([-1, -2, -1], Face::PosY, 0.5);
        assert_eq!(plan(EditAction::Place, &target, &player, PLACE_BLOCK), None);
        let beside = hit([0, -2, 0], Face::PosY, 0.5);
        assert!(plan(EditAction::Place, &beside, &player, PLACE_BLOCK).is_some());
    }

    #[test]
    fn placing_when_the_ray_starts_inside_a_block_is_refused() {
        let target = hit([9, 9, 9], Face::NegX, 0.0);
        assert_eq!(
            plan(EditAction::Place, &target, &body(EYE), PLACE_BLOCK),
            None
        );
        assert!(plan(EditAction::Break, &target, &body(EYE), PLACE_BLOCK).is_some());
    }

    #[test]
    fn the_placed_block_is_not_air() {
        assert!(!PLACE_BLOCK.is_air());
    }

    /// A chunk with a marker block at the origin, to tell chunks apart.
    fn marked(id: u16) -> Chunk {
        let mut chunk = Chunk::new();
        if let Some(origin) = LocalPos::new(0, 0, 0) {
            chunk.set(origin, BlockId(id));
        }
        chunk
    }

    fn marker(world: &World, pos: ChunkPos) -> Option<BlockId> {
        let size = CHUNK_SIZE as i32;
        world.block_at([pos.x * size, pos.y * size, pos.z * size])
    }

    #[test]
    fn streamed_chunks_are_added_and_unloaded_ones_dropped() {
        let first = ChunkPos::ORIGIN;
        let second = ChunkPos::new(1, 0, 0);
        let mut world = World::new();
        apply_streamed(
            &mut world,
            &[],
            vec![(first, marked(1)), (second, marked(2))],
        );
        assert_eq!(marker(&world, second), Some(BlockId(2)));

        apply_streamed(&mut world, &[first], Vec::new());
        assert!(!world.contains(first));
        assert!(world.contains(second));
        assert_eq!(world.len(), 1);
    }

    #[test]
    fn an_unloaded_and_reloaded_chunk_is_generated_anew() {
        let pos = ChunkPos::ORIGIN;
        let mut world = World::new();
        apply_streamed(&mut world, &[], vec![(pos, marked(1))]);
        world.set_block([0, 0, 0], BlockId(7));
        apply_streamed(&mut world, &[pos], vec![]);
        apply_streamed(&mut world, &[], vec![(pos, marked(1))]);
        assert_eq!(world.block_at([0, 0, 0]), Some(BlockId(1)));
    }
}
