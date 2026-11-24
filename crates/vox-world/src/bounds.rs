//! The vertical limits of the world (decision 0015): a floor of bedrock and a build ceiling.
//!
//! The world is endless sideways but has a bottom and a top, so the generator, the editing
//! rules and streaming agree on one pair of named numbers instead of repeating them. The
//! chunk-layer helpers are derived from those numbers and [`CHUNK_SIZE`], never typed twice.

use crate::chunk::CHUNK_SIZE;

/// `CHUNK_SIZE` as the integer type of world coordinates (the size is 32, so it always fits).
/// The one definition the world modules share.
pub(crate) const CHUNK_SIZE_I32: i32 = CHUNK_SIZE as i32;

/// The chunk layer (chunk `y` coordinate) that holds the block row `y`. Rounds towards
/// negative infinity, so row -1 is in layer -1.
pub(crate) const fn chunk_layer_of(y: i32) -> i32 {
    y.div_euclid(CHUNK_SIZE_I32)
}

/// World height of the lowest block row, in blocks. The generator puts one row of bedrock
/// here and nothing exists below. -512 is 16 chunk layers under the surface layer: deep
/// enough for a long shaft, small enough that a column's layers stay cheap to enumerate.
/// It lies on a chunk border (checked below), so the floor is the bottom row of a layer.
pub const WORLD_FLOOR_Y: i32 = -512;

/// World height of the first block that can never be set (the build limit), in blocks.
/// The highest usable block is `WORLD_CEILING_Y - 1`. 512 is 16 chunk layers over the
/// surface layer, symmetric to the floor; it lies on a chunk border (checked below).
pub const WORLD_CEILING_Y: i32 = 512;

// The bounds are whole chunk layers, so no layer is cut in half by the floor or the ceiling.
const _: () = assert!(WORLD_FLOOR_Y % CHUNK_SIZE_I32 == 0);
const _: () = assert!(WORLD_CEILING_Y % CHUNK_SIZE_I32 == 0);
const _: () = assert!(WORLD_FLOOR_Y < WORLD_CEILING_Y);

/// The lowest chunk layer (chunk `y` coordinate) of the world: the one holding the floor row.
pub const LOWEST_CHUNK_LAYER: i32 = chunk_layer_of(WORLD_FLOOR_Y);

/// The highest chunk layer of the world: the one holding block `WORLD_CEILING_Y - 1`.
pub const HIGHEST_CHUNK_LAYER: i32 = chunk_layer_of(WORLD_CEILING_Y - 1);

/// Whether a block at world height `y` is inside the world: `WORLD_FLOOR_Y <= y < WORLD_CEILING_Y`.
///
/// # Example
///
/// ```
/// use vox_world::{contains_block_y, WORLD_CEILING_Y, WORLD_FLOOR_Y};
///
/// assert!(contains_block_y(WORLD_FLOOR_Y));
/// assert!(!contains_block_y(WORLD_FLOOR_Y - 1));
/// assert!(!contains_block_y(WORLD_CEILING_Y));
/// ```
pub const fn contains_block_y(y: i32) -> bool {
    WORLD_FLOOR_Y <= y && y < WORLD_CEILING_Y
}

/// Whether the chunk layer `layer` (a chunk `y` coordinate) is inside the world:
/// `LOWEST_CHUNK_LAYER <= layer <= HIGHEST_CHUNK_LAYER`.
///
/// # Example
///
/// ```
/// use vox_world::contains_chunk_layer;
///
/// assert!(contains_chunk_layer(0));
/// assert!(!contains_chunk_layer(i32::MAX));
/// ```
pub const fn contains_chunk_layer(layer: i32) -> bool {
    LOWEST_CHUNK_LAYER <= layer && layer <= HIGHEST_CHUNK_LAYER
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_layers_follow_from_the_bounds() {
        // Literal numbers: -512 / 32 and 511 / 32, the point of this test.
        assert_eq!(LOWEST_CHUNK_LAYER, -16);
        assert_eq!(HIGHEST_CHUNK_LAYER, 15);
    }

    #[test]
    fn chunk_layer_of_rounds_towards_negative_infinity() {
        for (y, layer) in [
            (0, 0),
            (31, 0),
            (32, 1),
            (-1, -1),
            (-32, -1),
            (-33, -2),
            (-512, -16),
            (-513, -17),
        ] {
            assert_eq!(chunk_layer_of(y), layer, "y {y}");
        }
    }

    #[test]
    fn the_first_and_last_block_rows_are_inside() {
        assert!(contains_block_y(WORLD_FLOOR_Y));
        assert!(contains_block_y(WORLD_CEILING_Y - 1));
        assert!(!contains_block_y(WORLD_FLOOR_Y - 1));
        assert!(!contains_block_y(WORLD_CEILING_Y));
        assert!(!contains_block_y(i32::MIN));
        assert!(!contains_block_y(i32::MAX));
    }

    #[test]
    fn the_layer_test_matches_the_block_test_at_every_layer_edge() {
        for layer in LOWEST_CHUNK_LAYER - 2..=HIGHEST_CHUNK_LAYER + 2 {
            let lowest = layer * CHUNK_SIZE_I32;
            let highest = lowest + CHUNK_SIZE_I32 - 1;
            // A layer is inside exactly when all its blocks are, and outside when none is.
            assert_eq!(contains_chunk_layer(layer), contains_block_y(lowest));
            assert_eq!(contains_chunk_layer(layer), contains_block_y(highest));
        }
    }

    #[test]
    fn layers_outside_are_rejected() {
        assert!(contains_chunk_layer(LOWEST_CHUNK_LAYER));
        assert!(contains_chunk_layer(HIGHEST_CHUNK_LAYER));
        assert!(!contains_chunk_layer(LOWEST_CHUNK_LAYER - 1));
        assert!(!contains_chunk_layer(HIGHEST_CHUNK_LAYER + 1));
    }
}
