//! Chunk: a fixed-size cubic piece of the world.

use crate::block::BlockId;

/// Edge length of a chunk in blocks.
///
/// A power of two, so coordinates fit in 5 bits and the index can be computed
/// with shifts. 32 is a reasonable balance between the number of chunks and the size of one.
pub const CHUNK_SIZE: usize = 32;

/// Number of blocks in a chunk.
pub const CHUNK_VOLUME: usize = CHUNK_SIZE * CHUNK_SIZE * CHUNK_SIZE;

/// Position of a block inside a chunk.
///
/// Exists only with valid coordinates (each less than [`CHUNK_SIZE`]), so accessing
/// a chunk through such a position can never go out of bounds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LocalPos {
    x: u8,
    y: u8,
    z: u8,
}

impl LocalPos {
    /// Creates a position, or returns `None` if any coordinate is outside the chunk.
    pub const fn new(x: usize, y: usize, z: usize) -> Option<Self> {
        if x < CHUNK_SIZE && y < CHUNK_SIZE && z < CHUNK_SIZE {
            // The casts are safe: values below CHUNK_SIZE (32) fit in u8.
            Some(Self {
                x: x as u8,
                y: y as u8,
                z: z as u8,
            })
        } else {
            None
        }
    }

    /// X coordinate, always below [`CHUNK_SIZE`].
    pub const fn x(self) -> u8 {
        self.x
    }

    /// Y coordinate, always below [`CHUNK_SIZE`].
    pub const fn y(self) -> u8 {
        self.y
    }

    /// Z coordinate, always below [`CHUNK_SIZE`].
    pub const fn z(self) -> u8 {
        self.z
    }

    /// Returns the position shifted by `offset`, or `None` if it leaves the chunk.
    ///
    /// Checked arithmetic on `u8` makes underflow below zero and overflow past the
    /// far edge both come back as `None`, with no signed tricks at the call site.
    pub fn offset(self, offset: [i8; 3]) -> Option<Self> {
        let x = self.x.checked_add_signed(offset[0])?;
        let y = self.y.checked_add_signed(offset[1])?;
        let z = self.z.checked_add_signed(offset[2])?;
        Self::new(x as usize, y as usize, z as usize)
    }

    /// Iterates over every position of the chunk in storage order.
    pub fn all() -> impl Iterator<Item = Self> {
        (0..CHUNK_VOLUME).map(Self::from_index)
    }

    /// Inverse of [`LocalPos::index`]. The masks keep every coordinate below 32.
    const fn from_index(index: usize) -> Self {
        Self {
            x: (index & (CHUNK_SIZE - 1)) as u8,
            z: ((index / CHUNK_SIZE) & (CHUNK_SIZE - 1)) as u8,
            y: ((index / (CHUNK_SIZE * CHUNK_SIZE)) & (CHUNK_SIZE - 1)) as u8,
        }
    }

    /// Index of the block in the chunk's flat array. Layers go along `y`, then `z` within a layer, then `x`.
    const fn index(self) -> usize {
        (self.y as usize * CHUNK_SIZE + self.z as usize) * CHUNK_SIZE + self.x as usize
    }
}

/// A chunk stores its blocks in a single flat array.
///
/// A flat array instead of one object per block: compact, no per-block
/// allocations, friendly to the CPU cache.
#[derive(Clone, Debug)]
pub struct Chunk {
    blocks: Box<[BlockId]>,
}

impl Chunk {
    /// Creates an empty chunk made entirely of air.
    pub fn new() -> Self {
        Self {
            blocks: vec![BlockId::AIR; CHUNK_VOLUME].into_boxed_slice(),
        }
    }

    /// Returns the block at the given position.
    pub fn get(&self, pos: LocalPos) -> BlockId {
        // Indexing is safe: LocalPos guarantees coordinates inside the chunk.
        self.blocks[pos.index()]
    }

    /// Sets the block at the given position.
    pub fn set(&mut self, pos: LocalPos, block: BlockId) {
        self.blocks[pos.index()] = block;
    }
}

impl Default for Chunk {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pos(x: usize, y: usize, z: usize) -> LocalPos {
        LocalPos::new(x, y, z).expect("test coordinates are known to be inside the chunk")
    }

    #[test]
    fn new_chunk_is_all_air() {
        let chunk = Chunk::new();
        assert!(chunk.get(pos(0, 0, 0)).is_air());
        assert!(chunk.get(pos(31, 31, 31)).is_air());
    }

    #[test]
    fn set_then_get_returns_same_block() {
        let mut chunk = Chunk::new();
        chunk.set(pos(3, 4, 5), BlockId(7));
        assert_eq!(chunk.get(pos(3, 4, 5)), BlockId(7));
    }

    #[test]
    fn set_does_not_touch_other_blocks() {
        let mut chunk = Chunk::new();
        chunk.set(pos(3, 4, 5), BlockId(7));
        assert!(chunk.get(pos(5, 4, 3)).is_air());
    }

    #[test]
    fn every_position_has_unique_index() {
        let mut seen = vec![false; CHUNK_VOLUME];
        for y in 0..CHUNK_SIZE {
            for z in 0..CHUNK_SIZE {
                for x in 0..CHUNK_SIZE {
                    let i = pos(x, y, z).index();
                    assert!(!seen[i], "index {i} was produced twice");
                    seen[i] = true;
                }
            }
        }
        assert!(seen.iter().all(|&s| s));
    }

    #[test]
    fn all_visits_every_position_in_storage_order() {
        let mut count = 0;
        for (i, p) in LocalPos::all().enumerate() {
            assert_eq!(p.index(), i);
            count += 1;
        }
        assert_eq!(count, CHUNK_VOLUME);
    }

    #[test]
    fn offset_moves_inside_and_rejects_outside() {
        let p = pos(0, 31, 5);
        assert_eq!(p.offset([1, 0, -1]), Some(pos(1, 31, 4)));
        assert_eq!(p.offset([-1, 0, 0]), None);
        assert_eq!(p.offset([0, 1, 0]), None);
    }

    #[test]
    fn out_of_range_position_is_rejected() {
        assert!(LocalPos::new(CHUNK_SIZE, 0, 0).is_none());
        assert!(LocalPos::new(0, CHUNK_SIZE, 0).is_none());
        assert!(LocalPos::new(0, 0, CHUNK_SIZE).is_none());
    }
}
