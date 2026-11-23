//! Chunk: a fixed-size cubic piece of the world.

use crate::block::BlockId;

/// Edge length of a chunk in blocks.
///
/// A power of two, so coordinates fit in 5 bits and the index can be computed
/// with shifts. 32 is a reasonable balance between the number of chunks and the size of one.
pub const CHUNK_SIZE: usize = 32;

/// Number of blocks in a chunk.
pub const CHUNK_VOLUME: usize = CHUNK_SIZE * CHUNK_SIZE * CHUNK_SIZE;

/// Distance in the flat block array between two blocks that differ by one along the
/// axis X, Y or Z. Layers go along `y`, then `z` within a layer, then `x`. The mesher
/// walks the array with these strides, so they and [`LocalPos`]'s index come from here.
pub(crate) const AXIS_STRIDES: [usize; 3] = {
    let mut strides = [0; 3];
    strides[AXIS_X] = 1;
    strides[AXIS_Y] = CHUNK_SIZE * CHUNK_SIZE;
    strides[AXIS_Z] = CHUNK_SIZE;
    strides
};

/// Index of the X axis in per-axis arrays such as [`AXIS_STRIDES`] or a coordinate triple.
pub(crate) const AXIS_X: usize = 0;

/// Index of the Y axis in per-axis arrays.
pub(crate) const AXIS_Y: usize = 1;

/// Index of the Z axis in per-axis arrays.
pub(crate) const AXIS_Z: usize = 2;

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

    /// Returns the position shifted by `offset` with wrap-around: leaving the chunk on
    /// one side comes back in on the opposite side.
    ///
    /// For a block on the border and an offset that points out of the chunk, this is the
    /// position of the touching block inside the neighbouring chunk. The mask keeps every
    /// coordinate below [`CHUNK_SIZE`], so the invariant of `LocalPos` holds.
    pub(crate) fn wrapped(self, offset: [i8; 3]) -> Self {
        const MASK: u8 = (CHUNK_SIZE - 1) as u8;
        Self {
            x: self.x.wrapping_add_signed(offset[0]) & MASK,
            y: self.y.wrapping_add_signed(offset[1]) & MASK,
            z: self.z.wrapping_add_signed(offset[2]) & MASK,
        }
    }

    /// Iterates over every position of the chunk in storage order.
    pub fn all() -> impl Iterator<Item = Self> {
        (0..CHUNK_VOLUME).map(Self::from_index)
    }

    /// Inverse of [`LocalPos::index`], derived from [`AXIS_STRIDES`] so the layout is
    /// written in one place. The remainder keeps every coordinate below `CHUNK_SIZE`, so
    /// the cast to `u8` cannot lose bits.
    const fn from_index(index: usize) -> Self {
        Self {
            x: coordinate_of(index, AXIS_X),
            y: coordinate_of(index, AXIS_Y),
            z: coordinate_of(index, AXIS_Z),
        }
    }

    /// Index of the block in the chunk's flat array. Layers go along `y`, then `z` within a layer, then `x`.
    const fn index(self) -> usize {
        self.x as usize * AXIS_STRIDES[AXIS_X]
            + self.y as usize * AXIS_STRIDES[AXIS_Y]
            + self.z as usize * AXIS_STRIDES[AXIS_Z]
    }
}

/// Coordinate along `axis` of the block at `index` in the flat array. The remainder keeps
/// it below `CHUNK_SIZE`, so the cast to `u8` cannot lose bits.
const fn coordinate_of(index: usize, axis: usize) -> u8 {
    (index / AXIS_STRIDES[axis] % CHUNK_SIZE) as u8
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

    /// All blocks in storage order, indexed by [`AXIS_STRIDES`]. For the mesher, which
    /// walks whole slices and cannot afford a position check per block.
    pub(crate) fn blocks(&self) -> &[BlockId] {
        &self.blocks
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
    fn blocks_are_laid_out_by_the_axis_strides() {
        let mut chunk = Chunk::new();
        chunk.set(pos(3, 4, 5), BlockId(7));
        let index = 3 * AXIS_STRIDES[AXIS_X] + 4 * AXIS_STRIDES[AXIS_Y] + 5 * AXIS_STRIDES[AXIS_Z];
        assert_eq!(chunk.blocks()[index], BlockId(7));
        assert_eq!(chunk.blocks().len(), CHUNK_VOLUME);
        assert_eq!(chunk.blocks().iter().filter(|b| !b.is_air()).count(), 1);
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
    fn wrapped_comes_back_in_on_the_opposite_side() {
        let p = pos(0, 31, 5);
        assert_eq!(p.wrapped([-1, 0, 0]), pos(31, 31, 5));
        assert_eq!(p.wrapped([0, 1, 0]), pos(0, 0, 5));
        assert_eq!(p.wrapped([1, 0, -1]), pos(1, 31, 4));
    }

    #[test]
    fn out_of_range_position_is_rejected() {
        assert!(LocalPos::new(CHUNK_SIZE, 0, 0).is_none());
        assert!(LocalPos::new(0, CHUNK_SIZE, 0).is_none());
        assert!(LocalPos::new(0, 0, CHUNK_SIZE).is_none());
    }
}
