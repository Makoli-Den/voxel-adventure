//! World: the loaded chunks, and the view of the six chunks around one of them.
//!
//! The world is only storage. Generating, loading and meshing chunks are done by the
//! caller, so this module has one job: find a chunk by its position.

use std::collections::HashMap;

use crate::block::BlockId;
use crate::chunk::{AXIS_COUNT, CHUNK_SIZE, Chunk, LocalPos};
use crate::chunk_pos::ChunkPos;
use crate::mesh::Face;

/// The most chunks one block edit can change: the chunk of the block plus one neighbour
/// per axis, which a block in a corner of its chunk touches (one face per axis).
pub const MAX_CHANGED_CHUNKS: usize = 1 + AXIS_COUNT;

/// The loaded chunks whose mesh must be rebuilt after one block edit.
///
/// A fixed array with a length instead of a `Vec`: an edit happens in the game loop and
/// the answer never exceeds [`MAX_CHANGED_CHUNKS`], so it needs no allocation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChangedChunks {
    chunks: [ChunkPos; MAX_CHANGED_CHUNKS],
    len: usize,
}

impl ChangedChunks {
    const fn empty() -> Self {
        Self {
            chunks: [ChunkPos::ORIGIN; MAX_CHANGED_CHUNKS],
            len: 0,
        }
    }

    /// Adds a chunk. The edit code adds at most [`MAX_CHANGED_CHUNKS`]; extra ones are
    /// dropped instead of panicking.
    fn push(&mut self, pos: ChunkPos) {
        debug_assert!(self.len < MAX_CHANGED_CHUNKS, "too many changed chunks");
        if let Some(slot) = self.chunks.get_mut(self.len) {
            *slot = pos;
            self.len += 1;
        }
    }

    /// The changed chunks. The chunk that holds the block comes first, then the
    /// neighbours in [`Face::ALL`] order. Empty if the block already had that value.
    pub fn as_slice(&self) -> &[ChunkPos] {
        self.chunks.get(..self.len).unwrap_or_default()
    }

    /// Whether `pos` is among the changed chunks.
    pub fn contains(&self, pos: ChunkPos) -> bool {
        self.as_slice().contains(&pos)
    }

    /// Whether nothing changed.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

/// The loaded chunks of the world, keyed by their [`ChunkPos`].
///
/// A hash map instead of a dense array: the world has no fixed size or shape, and chunks
/// come and go as the player moves.
#[derive(Debug, Default)]
pub struct World {
    chunks: HashMap<ChunkPos, Chunk>,
}

impl World {
    /// Creates a world without chunks.
    pub fn new() -> Self {
        Self::default()
    }

    /// Stores a chunk at `pos` and returns the chunk that was there before, if any.
    pub fn insert(&mut self, pos: ChunkPos, chunk: Chunk) -> Option<Chunk> {
        self.chunks.insert(pos, chunk)
    }

    /// Removes the chunk at `pos` and returns it, or `None` if it was not loaded. This is
    /// how unloaded chunks are dropped, so memory stays flat while the player moves.
    pub fn remove(&mut self, pos: ChunkPos) -> Option<Chunk> {
        self.chunks.remove(&pos)
    }

    /// The chunk at `pos`, or `None` if it is not loaded.
    pub fn get(&self, pos: ChunkPos) -> Option<&Chunk> {
        self.chunks.get(&pos)
    }

    /// Whether a chunk is loaded at `pos`.
    pub fn contains(&self, pos: ChunkPos) -> bool {
        self.chunks.contains_key(&pos)
    }

    /// Number of loaded chunks.
    pub fn len(&self) -> usize {
        self.chunks.len()
    }

    /// Whether no chunk is loaded.
    pub fn is_empty(&self) -> bool {
        self.chunks.is_empty()
    }

    /// Iterates over all loaded chunks in no particular order.
    pub fn iter(&self) -> impl Iterator<Item = (ChunkPos, &Chunk)> {
        self.chunks.iter().map(|(&pos, chunk)| (pos, chunk))
    }

    /// The block at world block position `pos`, or `None` if its chunk is not loaded.
    pub fn block_at(&self, pos: [i32; 3]) -> Option<BlockId> {
        let (chunk, local) = ChunkPos::containing(pos);
        self.get(chunk).map(|chunk| chunk.get(local))
    }

    /// Sets the block at world block position `pos`.
    ///
    /// Returns `None` if the chunk of `pos` is not loaded (nothing is written). Otherwise
    /// returns the loaded chunks whose mesh is now out of date: the chunk of the block,
    /// and, when the block lies on a chunk border, the loaded chunk behind that border,
    /// because its mesh hides or shows faces depending on this block. Neighbours that are
    /// not loaded have no mesh and are left out. Writing the value the block already has
    /// changes nothing and returns an empty set.
    pub fn set_block(&mut self, pos: [i32; 3], block: BlockId) -> Option<ChangedChunks> {
        let (chunk_pos, local) = ChunkPos::containing(pos);
        let chunk = self.chunks.get_mut(&chunk_pos)?;
        let mut changed = ChangedChunks::empty();
        if chunk.get(local) == block {
            return Some(changed);
        }
        chunk.set(local, block);
        changed.push(chunk_pos);
        for face in Face::ALL {
            if on_border(local, face) {
                let neighbour = chunk_pos.neighbour(face);
                if self.contains(neighbour) {
                    changed.push(neighbour);
                }
            }
        }
        Some(changed)
    }

    /// The six chunks around `pos`; the ones that are not loaded are missing.
    ///
    /// The result borrows the world, so the world cannot change while it is in use.
    pub fn neighbours(&self, pos: ChunkPos) -> Neighbours<'_> {
        Neighbours::new(Face::ALL.map(|face| self.get(pos.neighbour(face))))
    }
}

/// Whether the block at `local` touches the border of its chunk on the side of `face`.
fn on_border(local: LocalPos, face: Face) -> bool {
    let [dx, dy, dz] = face.offset();
    let edge = |coordinate: u8, step: i8| match step {
        1 => usize::from(coordinate) == CHUNK_SIZE - 1,
        -1 => coordinate == 0,
        _ => false,
    };
    edge(local.x(), dx) || edge(local.y(), dy) || edge(local.z(), dz)
}

/// The six chunks next to one chunk, one per [`Face`], each possibly missing.
///
/// It is a small `Copy` value of six references, so the mesher can take it without
/// knowing about [`World`]. It can also be built by hand for chunks that live elsewhere.
#[derive(Clone, Copy, Debug)]
pub struct Neighbours<'a> {
    chunks: [Option<&'a Chunk>; Face::COUNT],
}

impl<'a> Neighbours<'a> {
    /// Builds the view from six chunks ordered by [`Face::index`]: element 0 is the
    /// chunk on the +X side, element 1 the one on the -X side, and so on.
    pub const fn new(chunks: [Option<&'a Chunk>; Face::COUNT]) -> Self {
        Self { chunks }
    }

    /// No neighbour at all: everything outside the chunk counts as air.
    pub const fn none() -> Self {
        Self::new([None; Face::COUNT])
    }

    /// The chunk on the side `face` looks at, or `None` if it is missing.
    pub fn get(&self, face: Face) -> Option<&'a Chunk> {
        // `Face::index` is below 6 by construction, so `get` always finds the slot.
        self.chunks
            .get(usize::from(face.index()))
            .copied()
            .flatten()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block::BlockId;
    use crate::chunk::LocalPos;

    fn marked_chunk(id: u16) -> Chunk {
        let mut chunk = Chunk::new();
        if let Some(pos) = LocalPos::new(0, 0, 0) {
            chunk.set(pos, BlockId(id));
        }
        chunk
    }

    fn mark(chunk: &Chunk) -> BlockId {
        LocalPos::new(0, 0, 0).map_or(BlockId::AIR, |pos| chunk.get(pos))
    }

    #[test]
    fn new_world_is_empty() {
        let world = World::new();
        assert!(world.is_empty());
        assert_eq!(world.len(), 0);
        assert!(world.get(ChunkPos::ORIGIN).is_none());
        assert!(!world.contains(ChunkPos::ORIGIN));
    }

    #[test]
    fn insert_get_contains_and_len() {
        let mut world = World::new();
        let pos = ChunkPos::new(-2, 1, 5);
        assert!(world.insert(pos, marked_chunk(7)).is_none());
        assert!(world.contains(pos));
        assert_eq!(world.len(), 1);
        assert!(!world.is_empty());
        assert_eq!(world.get(pos).map(mark), Some(BlockId(7)));
        assert!(world.get(ChunkPos::ORIGIN).is_none());
    }

    #[test]
    fn insert_returns_the_replaced_chunk() {
        let mut world = World::new();
        world.insert(ChunkPos::ORIGIN, marked_chunk(1));
        let old = world.insert(ChunkPos::ORIGIN, marked_chunk(2));
        assert_eq!(old.as_ref().map(mark), Some(BlockId(1)));
        assert_eq!(world.len(), 1);
        assert_eq!(world.get(ChunkPos::ORIGIN).map(mark), Some(BlockId(2)));
    }

    #[test]
    fn iter_visits_every_chunk_once() {
        let mut world = World::new();
        let positions = [
            ChunkPos::ORIGIN,
            ChunkPos::new(1, 0, 0),
            ChunkPos::new(0, -1, 0),
        ];
        for (id, pos) in (1..).zip(positions) {
            world.insert(pos, marked_chunk(id));
        }
        let mut seen: Vec<(i32, i32, i32, u16)> = world
            .iter()
            .map(|(pos, chunk)| (pos.x, pos.y, pos.z, mark(chunk).0))
            .collect();
        seen.sort_unstable();
        assert_eq!(seen, [(0, -1, 0, 3), (0, 0, 0, 1), (1, 0, 0, 2)]);
    }

    #[test]
    fn neighbours_are_found_by_face() {
        let mut world = World::new();
        let centre = ChunkPos::new(4, 4, 4);
        world.insert(centre, marked_chunk(100));
        for (id, face) in (1..).zip(Face::ALL) {
            world.insert(centre.neighbour(face), marked_chunk(id));
        }
        let neighbours = world.neighbours(centre);
        for (id, face) in (1..).zip(Face::ALL) {
            assert_eq!(
                neighbours.get(face).map(mark),
                Some(BlockId(id)),
                "{face:?}"
            );
        }
    }

    #[test]
    fn missing_neighbours_are_none() {
        let mut world = World::new();
        world.insert(ChunkPos::ORIGIN, marked_chunk(1));
        world.insert(ChunkPos::new(0, 0, -1), marked_chunk(2));
        let neighbours = world.neighbours(ChunkPos::ORIGIN);
        assert!(neighbours.get(Face::NegZ).is_some());
        for face in [Face::PosX, Face::NegX, Face::PosY, Face::NegY, Face::PosZ] {
            assert!(neighbours.get(face).is_none(), "{face:?}");
        }
    }

    fn world_with_empty_chunks(positions: &[ChunkPos]) -> World {
        let mut world = World::new();
        for &pos in positions {
            world.insert(pos, Chunk::new());
        }
        world
    }

    #[test]
    fn block_at_reads_world_coordinates_including_negative() {
        let mut world = world_with_empty_chunks(&[ChunkPos::new(-1, 0, 0), ChunkPos::ORIGIN]);
        world.set_block([-1, 5, 0], BlockId(9));
        assert_eq!(world.block_at([-1, 5, 0]), Some(BlockId(9)));
        assert_eq!(world.block_at([0, 5, 0]), Some(BlockId::AIR));
        assert_eq!(world.block_at([-33, 5, 0]), None);
        let chunk = world.get(ChunkPos::new(-1, 0, 0));
        let local = LocalPos::new(31, 5, 0).unwrap();
        assert_eq!(chunk.map(|c| c.get(local)), Some(BlockId(9)));
    }

    #[test]
    fn set_block_in_an_unloaded_chunk_does_nothing() {
        let mut world = World::new();
        assert!(world.set_block([0, 0, 0], BlockId(1)).is_none());
        assert!(world.is_empty());
    }

    #[test]
    fn remove_returns_the_chunk_once_and_unloads_it() {
        let mut world = world_with_empty_chunks(&[ChunkPos::ORIGIN, ChunkPos::new(1, 0, 0)]);
        assert!(world.remove(ChunkPos::ORIGIN).is_some());
        assert!(world.remove(ChunkPos::ORIGIN).is_none());
        assert!(!world.contains(ChunkPos::ORIGIN));
        assert!(world.contains(ChunkPos::new(1, 0, 0)));
        assert_eq!(world.block_at([0, 0, 0]), None);
    }

    #[test]
    fn set_block_inside_a_chunk_changes_only_that_chunk() {
        let mut world = world_with_empty_chunks(&[ChunkPos::ORIGIN, ChunkPos::new(1, 0, 0)]);
        let changed = world.set_block([10, 10, 10], BlockId(1)).unwrap();
        assert_eq!(changed.as_slice(), [ChunkPos::ORIGIN]);
    }

    #[test]
    fn set_block_on_a_border_also_changes_the_loaded_neighbour() {
        let mut world = world_with_empty_chunks(&[
            ChunkPos::ORIGIN,
            ChunkPos::new(1, 0, 0),
            ChunkPos::new(0, -1, 0),
        ]);
        let changed = world.set_block([31, 0, 7], BlockId(1)).unwrap();
        assert_eq!(changed.as_slice().len(), 3);
        assert_eq!(changed.as_slice()[0], ChunkPos::ORIGIN);
        assert!(changed.contains(ChunkPos::new(1, 0, 0)));
        assert!(changed.contains(ChunkPos::new(0, -1, 0)));
    }

    #[test]
    fn set_block_skips_unloaded_neighbours() {
        let mut world = world_with_empty_chunks(&[ChunkPos::ORIGIN]);
        let changed = world.set_block([0, 0, 0], BlockId(1)).unwrap();
        assert_eq!(changed.as_slice(), [ChunkPos::ORIGIN]);
    }

    #[test]
    fn set_block_in_a_corner_reaches_the_maximum() {
        let mut positions = vec![ChunkPos::ORIGIN];
        positions.extend(
            [Face::NegX, Face::NegY, Face::NegZ].map(|face| ChunkPos::ORIGIN.neighbour(face)),
        );
        let mut world = world_with_empty_chunks(&positions);
        let changed = world.set_block([0, 0, 0], BlockId(1)).unwrap();
        assert_eq!(changed.as_slice().len(), MAX_CHANGED_CHUNKS);
    }

    #[test]
    fn border_neighbour_of_a_negative_chunk_is_found() {
        let a = ChunkPos::new(-1, -1, -1);
        let mut world = world_with_empty_chunks(&[a, ChunkPos::new(-2, -1, -1)]);
        // Block -32 is local 0 of chunk -1: the -X neighbour is chunk -2.
        let changed = world.set_block([-32, -1, -1], BlockId(1)).unwrap();
        assert!(changed.contains(ChunkPos::new(-2, -1, -1)));
        assert_eq!(changed.as_slice().len(), 2);
    }

    #[test]
    fn writing_the_same_block_changes_nothing() {
        let mut world = world_with_empty_chunks(&[ChunkPos::ORIGIN]);
        let changed = world.set_block([0, 0, 0], BlockId::AIR).unwrap();
        assert!(changed.is_empty());
    }

    #[test]
    fn none_has_no_chunk_and_new_keeps_the_face_order() {
        for face in Face::ALL {
            assert!(Neighbours::none().get(face).is_none());
        }
        let a = marked_chunk(1);
        let neighbours = Neighbours::new([None, Some(&a), None, None, None, None]);
        assert!(neighbours.get(Face::PosX).is_none());
        assert!(neighbours.get(Face::NegX).is_some());
    }
}
