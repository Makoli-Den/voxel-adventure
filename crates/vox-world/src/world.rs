//! World: the loaded chunks, and the view of the six chunks around one of them.
//!
//! The world is only storage. Generating, loading and meshing chunks are done by the
//! caller, so this module has one job: find a chunk by its position.

use std::collections::HashMap;

use crate::chunk::Chunk;
use crate::chunk_pos::ChunkPos;
use crate::mesh::Face;

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

    /// The six chunks around `pos`; the ones that are not loaded are missing.
    ///
    /// The result borrows the world, so the world cannot change while it is in use.
    pub fn neighbours(&self, pos: ChunkPos) -> Neighbours<'_> {
        Neighbours::new(Face::ALL.map(|face| self.get(pos.neighbour(face))))
    }
}

/// The six chunks next to one chunk, one per [`Face`], each possibly missing.
///
/// It is a small `Copy` value of six references, so the mesher can take it without
/// knowing about [`World`]. It can also be built by hand for chunks that live elsewhere.
#[derive(Clone, Copy, Debug)]
pub struct Neighbours<'a> {
    chunks: [Option<&'a Chunk>; 6],
}

impl<'a> Neighbours<'a> {
    /// Builds the view from six chunks ordered by [`Face::index`]: element 0 is the
    /// chunk on the +X side, element 1 the one on the -X side, and so on.
    pub const fn new(chunks: [Option<&'a Chunk>; 6]) -> Self {
        Self { chunks }
    }

    /// No neighbour at all: everything outside the chunk counts as air.
    pub const fn none() -> Self {
        Self::new([None; 6])
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
