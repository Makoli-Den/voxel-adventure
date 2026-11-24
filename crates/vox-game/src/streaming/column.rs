//! The unit of streaming: a column of chunks, and the chunk layers of each loaded column that
//! can show a face (decision 0015, "Deep world").
//!
//! Streaming loads and unloads whole columns. Only the layers of a column's span are generated
//! and meshed; below the span everything is solid and above it everything is air, which is what
//! physics and editing need to know about the layers that are not in the `World`.

use std::collections::HashMap;
use std::ops::RangeInclusive;

use vox_world::{ChunkPos, HIGHEST_CHUNK_LAYER, LOWEST_CHUNK_LAYER};

/// A column of chunks: every chunk with this `x` and `z`, from the floor layer to the ceiling.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Column {
    /// Chunk coordinate along X.
    pub x: i32,
    /// Chunk coordinate along Z.
    pub z: i32,
}

impl Column {
    /// The column at chunk coordinates `x` and `z`.
    pub const fn new(x: i32, z: i32) -> Self {
        Self { x, z }
    }

    /// The column that holds the chunk at `pos`.
    pub const fn of(pos: ChunkPos) -> Self {
        Self::new(pos.x, pos.z)
    }

    /// The chunk of this column in chunk layer `layer`.
    pub const fn chunk(self, layer: i32) -> ChunkPos {
        ChunkPos::new(self.x, layer, self.z)
    }

    /// Every chunk of the column inside the world, from the floor layer up: what unloading the
    /// column removes. 32 positions, cheap enough to look each up instead of keeping an index.
    pub fn chunks(self) -> impl Iterator<Item = ChunkPos> {
        (LOWEST_CHUNK_LAYER..=HIGHEST_CHUNK_LAYER).map(move |layer| self.chunk(layer))
    }
}

/// The chunk layers of a column that can show a face (`vox_world::column_span`).
pub type Span = RangeInclusive<i32>;

/// The span of every loaded column, kept by the game next to its `World` and updated from the
/// same streaming changes, so the two always agree.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ColumnSpans {
    spans: HashMap<Column, Span>,
}

impl ColumnSpans {
    /// The span of `column`, or `None` if the column is not loaded.
    pub fn get(&self, column: Column) -> Option<&Span> {
        self.spans.get(&column)
    }

    /// Whether `column` is loaded.
    pub fn contains(&self, column: Column) -> bool {
        self.spans.contains_key(&column)
    }

    /// Marks `column` loaded with `span`.
    pub fn insert(&mut self, column: Column, span: Span) {
        self.spans.insert(column, span);
    }

    /// Number of loaded columns.
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.spans.len()
    }

    /// Marks `column` not loaded.
    pub fn remove(&mut self, column: Column) {
        self.spans.remove(&column);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_column_names_its_chunks() {
        let column = Column::new(3, -4);
        assert_eq!(column.chunk(-2), ChunkPos::new(3, -2, -4));
        assert_eq!(Column::of(ChunkPos::new(3, 9, -4)), column);
    }

    #[test]
    fn a_column_has_one_chunk_per_world_layer() {
        let chunks: Vec<ChunkPos> = Column::new(1, 2).chunks().collect();
        // Literal: 16 layers below 0 and 16 from 0 up (decision 0015).
        assert_eq!(chunks.len(), 32);
        assert_eq!(chunks[0], ChunkPos::new(1, -16, 2));
        assert_eq!(chunks[31], ChunkPos::new(1, 15, 2));
    }

    #[test]
    fn spans_are_kept_per_column_until_removed() {
        let mut spans = ColumnSpans::default();
        spans.insert(Column::new(0, 0), 0..=0);
        spans.insert(Column::new(5, 0), -1..=2);
        assert_eq!(spans.get(Column::new(5, 0)), Some(&(-1..=2)));
        assert!(spans.get(Column::new(1, 0)).is_none());
        spans.remove(Column::new(5, 0));
        assert!(spans.contains(Column::new(0, 0)));
        assert!(!spans.contains(Column::new(5, 0)));
        assert_eq!(spans.len(), 1);
    }
}
