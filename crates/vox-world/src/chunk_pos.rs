//! Position of a chunk in the world, counted in chunks.

use crate::chunk::{AXIS_X, AXIS_Y, AXIS_Z, CHUNK_SIZE, LocalPos};
use crate::mesh::Face;

/// `CHUNK_SIZE` as the integer type of world coordinates (the size is 32, so it always fits).
const CHUNK_SIZE_I32: i32 = CHUNK_SIZE as i32;

/// Where a chunk sits in the world, in chunk units: chunk `(1, 0, 0)` lies right next to
/// chunk `(0, 0, 0)` on the +X side. The world is cubic chunks in all three directions,
/// so there is no height limit.
///
/// The block at `LocalPos (x, y, z)` of the chunk `(cx, cy, cz)` has the world position
/// `(cx * 32 + x, cy * 32 + y, cz * 32 + z)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ChunkPos {
    /// Chunk coordinate along X.
    pub x: i32,
    /// Chunk coordinate along Y (up).
    pub y: i32,
    /// Chunk coordinate along Z.
    pub z: i32,
}

impl ChunkPos {
    /// The chunk at the world origin.
    pub const ORIGIN: Self = Self::new(0, 0, 0);

    /// Creates a chunk position.
    pub const fn new(x: i32, y: i32, z: i32) -> Self {
        Self { x, y, z }
    }

    /// Splits the world position of a block into the chunk that holds it and the block's
    /// position inside that chunk.
    ///
    /// Rounds towards negative infinity, so block `-1` belongs to chunk `-1` (local 31),
    /// not to chunk 0.
    pub const fn containing(block: [i32; 3]) -> (Self, LocalPos) {
        let chunk = Self::new(
            block[AXIS_X].div_euclid(CHUNK_SIZE_I32),
            block[AXIS_Y].div_euclid(CHUNK_SIZE_I32),
            block[AXIS_Z].div_euclid(CHUNK_SIZE_I32),
        );
        (chunk, LocalPos::from_block_coords(block))
    }

    /// World position of the chunk's lowest corner (its block `(0, 0, 0)`), in blocks.
    ///
    /// Positions so far out that the result would not fit an `i32` (more than 67 million
    /// chunks from the origin) saturate instead of wrapping.
    pub const fn origin(self) -> [i32; 3] {
        [
            self.x.saturating_mul(CHUNK_SIZE_I32),
            self.y.saturating_mul(CHUNK_SIZE_I32),
            self.z.saturating_mul(CHUNK_SIZE_I32),
        ]
    }

    /// The chunk next to this one in the direction `face` looks at.
    ///
    /// Saturates at the ends of the `i32` range instead of wrapping.
    pub fn neighbour(self, face: Face) -> Self {
        let [dx, dy, dz] = face.offset();
        Self {
            x: self.x.saturating_add(i32::from(dx)),
            y: self.y.saturating_add(i32::from(dy)),
            z: self.z.saturating_add(i32::from(dz)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn containing_splits_positive_positions() {
        let (chunk, local) = ChunkPos::containing([33, 0, 95]);
        assert_eq!(chunk, ChunkPos::new(1, 0, 2));
        assert_eq!(local, LocalPos::new(1, 0, 31).unwrap());
    }

    #[test]
    fn containing_floors_negative_positions() {
        let (chunk, local) = ChunkPos::containing([-1, -32, -33]);
        assert_eq!(chunk, ChunkPos::new(-1, -1, -2));
        assert_eq!(local, LocalPos::new(31, 0, 31).unwrap());
    }

    #[test]
    fn containing_agrees_with_origin_at_every_chunk_border() {
        for edge in [-65, -64, -33, -32, -1, 0, 31, 32, 63, 64] {
            let (chunk, local) = ChunkPos::containing([edge, edge, edge]);
            let origin = chunk.origin();
            assert_eq!(origin[0] + i32::from(local.x()), edge);
            assert_eq!(origin[1] + i32::from(local.y()), edge);
            assert_eq!(origin[2] + i32::from(local.z()), edge);
        }
    }

    #[test]
    fn containing_handles_the_ends_of_the_range() {
        let (chunk, local) = ChunkPos::containing([i32::MIN, i32::MAX, 0]);
        assert_eq!(chunk.x, i32::MIN / 32);
        assert_eq!(chunk.y, i32::MAX / 32);
        assert_eq!((local.x(), local.y(), local.z()), (0, 31, 0));
    }

    #[test]
    fn origin_of_the_origin_chunk_is_zero() {
        assert_eq!(ChunkPos::ORIGIN.origin(), [0, 0, 0]);
    }

    #[test]
    fn origin_scales_by_the_chunk_size_and_keeps_the_sign() {
        assert_eq!(ChunkPos::new(1, 2, 3).origin(), [32, 64, 96]);
        assert_eq!(ChunkPos::new(-1, 0, -2).origin(), [-32, 0, -64]);
    }

    #[test]
    fn origin_saturates_instead_of_wrapping() {
        let far = ChunkPos::new(i32::MAX, i32::MIN, 0).origin();
        assert_eq!(far, [i32::MAX, i32::MIN, 0]);
    }

    #[test]
    fn neighbours_step_one_chunk_in_the_direction_of_the_face() {
        let pos = ChunkPos::new(5, -3, 7);
        for face in Face::ALL {
            let [dx, dy, dz] = face.offset();
            let next = pos.neighbour(face);
            assert_eq!(
                (next.x - pos.x, next.y - pos.y, next.z - pos.z),
                (i32::from(dx), i32::from(dy), i32::from(dz))
            );
        }
    }

    #[test]
    fn the_neighbour_of_a_neighbour_in_the_opposite_direction_is_the_start() {
        let pos = ChunkPos::new(-4, 9, 2);
        let there = pos.neighbour(Face::PosX);
        assert_eq!(there.neighbour(Face::NegX), pos);
    }

    #[test]
    fn neighbour_saturates_at_the_end_of_the_range() {
        let edge = ChunkPos::new(i32::MAX, 0, 0);
        assert_eq!(edge.neighbour(Face::PosX), edge);
    }
}
