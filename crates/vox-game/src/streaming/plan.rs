//! What streaming wants around one camera column: the full-detail columns and the
//! level-of-detail tiles, as a pure value (decision 0015, step S7).
//!
//! The tile selection ([`super::lod::select_tiles`]) splits the horizon disc into leaves. The
//! leaves of level 0 are full-detail columns with blocks in the game's world; the leaves of level
//! 1 and up are tiles that keep only a mesh. The view radius still decides the columns the
//! player can always reach ([`area::wanted`]); the selection guarantees that each of them is a
//! level-0 leaf, and its level-0 leaves reach a little farther (up to 3.6 chunks), so those are
//! loaded as columns too. That makes the drawn ground a partition:
//!
//! **Coverage rule.** Every place of the ground inside the horizon is drawn by exactly one of a
//! loaded column or a loaded tile of level 1 or more, once nothing is queued. A column is kept
//! while it is a level-0 leaf, or while it is in the keep range of the view radius
//! ([`area::is_kept`]) **and** no selected tile of level 1 or more covers it. The second half
//! keeps the old hysteresis where no tile competes (beyond the horizon, or with no tiles at
//! all) and never lets a column stay under a tile.
//!
//! **Known gaps until step S8.** Inside the horizon columns have no hysteresis where tiles exist:
//! stepping back and forth across a level-1 border swaps up to 48 leaves each time, and the
//! ground they cover (about 260 to 370 blocks away at radius 8) shows holes until the new
//! columns or tiles arrive. A column one chunk beyond the radius that a tile now covers is
//! dropped at once, edits in it included. S8 keeps a column until its covering tile is uploaded
//! and a tile until its leaves are.
//!
//! With a horizon of 0 the plan has no tiles and the columns are exactly the old area.

use std::collections::HashSet;

use vox_world::{ChunkPos, MAX_LOD_LEVEL, TilePos};

use super::area;
use super::column::Column;
use super::job::TILE_LAYER;
use super::lod;

/// A key of streaming with its squared distance in blocks from the camera column's centre to
/// its nearest point ([`lod::distance_squared`]): the nearest-first order of all work.
pub type Near<K> = (i64, K);

/// The columns and tiles wanted around one camera column. Built when the camera enters another
/// column (one selection, about 26 us at horizon 2048), not every frame.
#[derive(Debug)]
pub struct Plan {
    /// The camera's column.
    centre: Column,
    /// The full-detail radius in chunks.
    radius: i32,
    /// Columns to load, nearest first: the view radius's area and the level-0 leaves.
    columns: Vec<Near<Column>>,
    /// The level-0 leaves, which are kept whatever the radius says.
    leaf_columns: HashSet<Column>,
    /// Tiles of level 1 or more to load, nearest first.
    tiles: Vec<Near<TilePos>>,
    /// The same tiles, to look up.
    tile_set: HashSet<TilePos>,
}

impl Plan {
    /// The plan around `centre` for a full-detail radius of `radius` chunks and a horizon of
    /// `horizon` blocks. A horizon of 0 or less selects no tiles: only the radius's area.
    pub fn new(centre: Column, radius: i32, horizon: i32) -> Self {
        let camera = centre.chunk(TILE_LAYER);
        let leaves = if horizon > 0 {
            lod::select_tiles(camera, radius, horizon)
        } else {
            Vec::new()
        };
        let (level0, coarse): (Vec<TilePos>, Vec<TilePos>) =
            leaves.into_iter().partition(|tile| tile.level() == 0);
        let tiles: Vec<Near<TilePos>> = coarse
            .into_iter()
            .map(|tile| (lod::distance_squared(camera, tile), tile))
            .collect();
        let mut plan = Self {
            centre,
            radius,
            columns: Vec::new(),
            leaf_columns: level0.iter().map(|tile| Column::of(tile.pos())).collect(),
            tile_set: tiles.iter().map(|(_, tile)| *tile).collect(),
            tiles,
        };
        plan.columns = plan.wanted_columns(camera);
        plan
    }

    /// The radius's area and the level-0 leaves, without repeats and without any column the
    /// plan would not keep, nearest first.
    fn wanted_columns(&self, camera: ChunkPos) -> Vec<Near<Column>> {
        let area = area::wanted(self.centre, self.radius);
        let mut seen: HashSet<Column> = HashSet::with_capacity(area.len());
        let mut columns: Vec<Near<Column>> = area
            .into_iter()
            .chain(self.leaf_columns.iter().copied())
            .filter(|column| seen.insert(*column) && self.keeps_column(*column))
            .map(|column| {
                let tile = TilePos::chunk(column.chunk(TILE_LAYER));
                (lod::distance_squared(camera, tile), column)
            })
            .collect();
        // Ties by (z, x), so the order is the same on every call.
        columns.sort_unstable_by_key(|(distance, column)| (*distance, column.z, column.x));
        columns
    }

    /// Columns to load, nearest first.
    pub fn columns(&self) -> &[Near<Column>] {
        &self.columns
    }

    /// Tiles of level 1 or more to load, nearest first.
    pub fn tiles(&self) -> &[Near<TilePos>] {
        &self.tiles
    }

    /// Whether a column that is tracked stays: see the coverage rule in the module comment.
    pub fn keeps_column(&self, column: Column) -> bool {
        self.leaf_columns.contains(&column)
            || (area::is_kept(self.centre, column, self.radius) && !self.covers(column))
    }

    /// Whether a tile that is tracked stays: only while it is selected. A tile that leaves the
    /// selection is replaced by tiles of another level at once; the hole until they arrive is
    /// accepted until step S8.
    pub fn keeps_tile(&self, tile: TilePos) -> bool {
        self.tile_set.contains(&tile)
    }

    /// Whether a selected tile of level 1 or more contains `column`.
    pub fn covers(&self, column: Column) -> bool {
        (1..=MAX_LOD_LEVEL).any(|level| {
            let pos = ChunkPos::new(column.x >> level, TILE_LAYER, column.z >> level);
            self.tile_set.contains(&TilePos::new(level, pos))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::streaming::lod::DEFAULT_HORIZON_BLOCKS;

    /// The default radius of the game.
    const RADIUS: i32 = 8;

    #[test]
    fn without_a_horizon_the_plan_is_the_old_area() {
        let centre = Column::new(3, -5);
        let plan = Plan::new(centre, RADIUS, 0);
        assert!(plan.tiles().is_empty());
        let mut columns: Vec<Column> = plan.columns().iter().map(|(_, c)| *c).collect();
        let mut wanted = area::wanted(centre, RADIUS);
        columns.sort_by_key(|c| (c.z, c.x));
        wanted.sort_by_key(|c| (c.z, c.x));
        assert_eq!(columns, wanted);
        // The old hysteresis: one margin beyond the radius is kept, farther is not.
        let edge = |d| Column::new(centre.x + d, centre.z);
        assert!(plan.keeps_column(edge(RADIUS + area::UNLOAD_MARGIN)));
        assert!(!plan.keeps_column(edge(RADIUS + area::UNLOAD_MARGIN + 1)));
    }

    #[test]
    fn the_columns_are_the_area_and_the_level_0_leaves() {
        let centre = Column::new(-7, 12);
        let plan = Plan::new(centre, RADIUS, DEFAULT_HORIZON_BLOCKS);
        let columns: HashSet<Column> = plan.columns().iter().map(|(_, c)| *c).collect();
        assert_eq!(columns.len(), plan.columns().len(), "no repeats");
        for column in area::wanted(centre, RADIUS) {
            assert!(columns.contains(&column), "{column:?}");
        }
        let leaves: HashSet<Column> =
            lod::select_tiles(centre.chunk(0), RADIUS, DEFAULT_HORIZON_BLOCKS)
                .into_iter()
                .filter(|t| t.level() == 0)
                .map(|t| Column::of(t.pos()))
                .collect();
        assert!(leaves.is_subset(&columns));
        assert!(plan.tiles().iter().all(|(_, t)| t.level() >= 1));
        assert!(!plan.tiles().is_empty());
    }

    #[test]
    fn columns_and_tiles_are_nearest_first() {
        let plan = Plan::new(Column::new(1, 1), RADIUS, DEFAULT_HORIZON_BLOCKS);
        assert!(plan.columns().windows(2).all(|w| w[0].0 <= w[1].0));
        assert!(plan.tiles().windows(2).all(|w| w[0].0 <= w[1].0));
        assert_eq!(plan.columns()[0], (0, Column::new(1, 1)));
    }

    #[test]
    fn no_wanted_or_kept_column_lies_under_a_tile() {
        for centre in [Column::new(0, 0), Column::new(-1, -1), Column::new(37, -90)] {
            let plan = Plan::new(centre, RADIUS, DEFAULT_HORIZON_BLOCKS);
            for (_, column) in plan.columns() {
                assert!(!plan.covers(*column), "{column:?}");
            }
            // Every column a tile covers is not kept, even inside the radius's keep range.
            for (_, tile) in plan.tiles() {
                let size = 1 << tile.level();
                let column = Column::new(tile.pos().x * size, tile.pos().z * size);
                assert!(plan.covers(column));
                assert!(!plan.keeps_column(column));
            }
        }
    }

    #[test]
    fn covers_finds_the_tile_of_a_negative_column() {
        let plan = Plan::new(Column::new(0, 0), RADIUS, DEFAULT_HORIZON_BLOCKS);
        let (_, tile) = *plan
            .tiles()
            .iter()
            .find(|(_, t)| t.pos().x < 0 && t.pos().z < 0)
            .expect("a tile at negative coordinates");
        let size = 1 << tile.level();
        // The last column of the tile on both axes.
        let column = Column::new(
            tile.pos().x * size + size - 1,
            tile.pos().z * size + size - 1,
        );
        assert!(plan.covers(column));
        assert!(plan.keeps_tile(tile));
    }
}
