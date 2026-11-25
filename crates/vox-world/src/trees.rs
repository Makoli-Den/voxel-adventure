//! Trees: deterministic and safe across chunk borders (decision 0017).
//!
//! A tree is not stored anywhere: it exists because a hash says so. The world is cut into small
//! tree cells; each cell may hold one tree whose trunk column the hash picks. A chunk asks for
//! every tree cell within the leaf radius around its columns and stamps the parts of those
//! trees that fall inside it, so two chunks next to each other both know a tree on their border
//! and draw their own halves of it without talking to each other.
//!
//! Logs are stamped first and leaves second, and leaves only fill air, so the result does not
//! depend on the order of the trees: leaves never replace logs, water or the ground.

use crate::block::BlockId;
use crate::bounds::CHUNK_SIZE_I32;
use crate::chunk::{CHUNK_SIZE, Chunk, LocalPos};
use crate::landscape::{Biome, ColumnInfo, Landscape, MOUNTAIN_LINE, SEA_LEVEL};
use crate::noise::cell_hash;

/// Edge of a tree cell in blocks, a power of two: at most one tree per cell, so trees never
/// stand closer than one block and a forest is as dense as the cell chance says.
const TREE_CELL: i32 = 4;
/// `log2(TREE_CELL)`: the shift that turns a block coordinate into a tree cell.
const TREE_CELL_SHIFT: u32 = TREE_CELL.trailing_zeros();
/// Stream number of the tree hash, so trees are unrelated to the noise fields of the seed.
const TREE_STREAM: u32 = 0x7EE5;

/// Radius of the leaf blob in blocks.
const LEAF_RADIUS: i32 = 2;
/// How many columns beyond a chunk's own columns can hold a tree that reaches into the chunk.
pub(crate) const TREE_MARGIN: i32 = LEAF_RADIUS;
/// A leaf block is part of the blob if its squared distance from the blob's centre is at most
/// this: the radius squared plus one, which rounds the blob's corners off.
const LEAF_REACH_SQUARED: i32 = LEAF_RADIUS * LEAF_RADIUS + 1;
/// Shortest and tallest trunk in logs.
const MIN_TRUNK: i32 = 4;
/// See [`MIN_TRUNK`].
const MAX_TRUNK: i32 = 7;

/// The most blocks a tree rises above the ground: the tallest trunk and the leaves above it.
/// The column span reserves this much over every column where a tree may stand.
pub(crate) const TREE_MAX_HEIGHT: i32 = MAX_TRUNK + LEAF_RADIUS;

/// Chance out of 256 that a tree cell of a forest holds a tree.
const FOREST_CHANCE_PER_256: u32 = 175;
/// Chance out of 256 that a tree cell of the plains holds a tree.
const PLAINS_CHANCE_PER_256: u32 = 15;

/// Layout of the tree hash of a cell: the trunk's offset inside the cell on x and on z (a cell
/// is a power of two, so each is a mask), then the chance, then the trunk height.
const OFFSET_X_SHIFT: u32 = 0;
/// See [`OFFSET_X_SHIFT`].
const OFFSET_Z_SHIFT: u32 = TREE_CELL_SHIFT;
/// See [`OFFSET_X_SHIFT`]: the chance is the 8 bits from here, out of 256.
const CHANCE_SHIFT: u32 = 8;
/// Width of the chance field; the chances above are out of `2^CHANCE_BITS`.
const CHANCE_BITS: u32 = 8;
/// See [`OFFSET_X_SHIFT`]: the trunk height field starts here.
const TRUNK_SHIFT: u32 = 16;
/// Number of different trunk heights.
const TRUNK_CHOICES: u32 = (MAX_TRUNK - MIN_TRUNK + 1) as u32;

// The fields do not overlap, and the trunk choices divide the field above `TRUNK_SHIFT`
// (a power of two), so the reduction by `%` is exact.
const _: () = assert!(OFFSET_Z_SHIFT + TREE_CELL_SHIFT <= CHANCE_SHIFT);
const _: () = assert!(CHANCE_SHIFT + CHANCE_BITS <= TRUNK_SHIFT);
const _: () = assert!(TRUNK_CHOICES.is_power_of_two());

/// Tree cells along one axis that touch a chunk's columns widened by the leaf radius: the
/// 32 columns plus the radius on both sides, counted in cells, rounded up, plus one for the
/// start not being aligned.
const CELLS_PER_AXIS: usize =
    ((CHUNK_SIZE as i32 + 2 * LEAF_RADIUS + TREE_CELL - 1) / TREE_CELL + 1) as usize;
/// The most trees a chunk needs to know about.
const MAX_TREES: usize = CELLS_PER_AXIS * CELLS_PER_AXIS;

/// Lowest height a tree's block can have: nothing grows below the sea.
const LOWEST_TREE_Y: i32 = SEA_LEVEL;
/// Highest height a tree's block can have: the highest grassy ground, mountains start above it.
const HIGHEST_TREE_Y: i32 = MOUNTAIN_LINE - 1 + TREE_MAX_HEIGHT;

/// One tree: where it stands and how tall its trunk is.
#[derive(Clone, Copy, Default)]
pub(crate) struct Tree {
    x: i32,
    z: i32,
    /// Height of the ground block under the trunk.
    ground: i32,
    /// Number of logs.
    trunk: i32,
}

impl Tree {
    /// Height of the highest log: the centre of the leaf blob.
    const fn crown(&self) -> i32 {
        self.ground + self.trunk
    }
}

/// The height of the highest block of a column including its tree, if a tree may stand there.
/// A bound, not the tree itself: the span of a column must hold every tree that can be there.
pub(crate) const fn top_with_tree(info: &ColumnInfo) -> i32 {
    if info.biome.has_trees() {
        info.ground + TREE_MAX_HEIGHT
    } else {
        info.top()
    }
}

/// The tree of the tree cell `(cell_x, cell_z)`, if the hash puts one there and the biome at
/// its trunk allows it.
fn tree_in_cell(landscape: &Landscape, seed: u32, cell_x: i32, cell_z: i32) -> Option<Tree> {
    let hash = cell_hash(seed ^ TREE_STREAM, cell_x, cell_z);
    let chance = (hash >> CHANCE_SHIFT) & ((1 << CHANCE_BITS) - 1);
    if chance >= FOREST_CHANCE_PER_256 {
        return None;
    }
    let mask = (TREE_CELL - 1) as u32;
    // Saturating, like the chunk origin: far outside the usable world trees degrade but
    // nothing wraps or panics.
    let x = (cell_x << TREE_CELL_SHIFT).saturating_add(((hash >> OFFSET_X_SHIFT) & mask) as i32);
    let z = (cell_z << TREE_CELL_SHIFT).saturating_add(((hash >> OFFSET_Z_SHIFT) & mask) as i32);
    let info = landscape.column(x, z);
    let allowed = match info.biome {
        Biome::Forest => true,
        Biome::Plains => chance < PLAINS_CHANCE_PER_256,
        _ => false,
    };
    let trunk = MIN_TRUNK + ((hash >> TRUNK_SHIFT) % TRUNK_CHOICES) as i32;
    allowed.then_some(Tree {
        x,
        z,
        ground: info.ground,
        trunk,
    })
}

/// Whether a trunk stands in the world column `(x, z)`. `landscape` must cover the column.
pub(crate) fn tree_stands_at(landscape: &Landscape, seed: u32, x: i32, z: i32) -> bool {
    tree_in_cell(landscape, seed, x >> TREE_CELL_SHIFT, z >> TREE_CELL_SHIFT)
        .is_some_and(|tree| tree.x == x && tree.z == z)
}

/// Every tree whose blocks can reach the 32 x 32 columns starting at `origin` (x, z), of any
/// height. `landscape` must cover the columns widened by [`TREE_MARGIN`].
pub(crate) fn trees_around(landscape: &Landscape, seed: u32, origin: [i32; 2]) -> Vec<Tree> {
    // Not a hot loop: one small allocation per column of chunks, against its 1,024 column
    // evaluations.
    let mut trees = Vec::with_capacity(MAX_TREES);
    let cells = |start: i32| {
        let first = start.saturating_sub(TREE_MARGIN) >> TREE_CELL_SHIFT;
        let last = start.saturating_add(CHUNK_SIZE_I32 - 1 + TREE_MARGIN) >> TREE_CELL_SHIFT;
        first..=last
    };
    for cell_z in cells(origin[1]) {
        for cell_x in cells(origin[0]) {
            trees.extend(tree_in_cell(landscape, seed, cell_x, cell_z));
        }
    }
    trees
}

/// Writes `block` at the world block `(x, y, z)` if it lies in the chunk whose lowest block is
/// `origin` and the block there is air.
fn place_in_air(chunk: &mut Chunk, origin: [i32; 3], at: [i32; 3], block: BlockId) {
    let local = [0, 1, 2].map(|axis| {
        let offset = at[axis].checked_sub(origin[axis])?;
        usize::try_from(offset).ok()
    });
    let [Some(x), Some(y), Some(z)] = local else {
        return;
    };
    match LocalPos::new(x, y, z) {
        Some(pos) if chunk.get(pos).is_air() => chunk.set(pos, block),
        _ => {}
    }
}

/// Stamps the `trees` (from [`trees_around`]) into the chunk whose lowest block is `origin`:
/// all logs, then all leaves. Trees that do not reach the chunk's rows are skipped.
pub(crate) fn stamp(
    chunk: &mut Chunk,
    trees: &[Tree],
    origin: [i32; 3],
    log: BlockId,
    leaves: BlockId,
) {
    let rows = origin[1]..origin[1] + CHUNK_SIZE_I32;
    let reaching: Vec<&Tree> = trees
        .iter()
        .filter(|tree| tree.ground < rows.end && tree.ground + TREE_MAX_HEIGHT >= rows.start)
        .collect();
    for tree in &reaching {
        for y in tree.ground + 1..=tree.crown() {
            place_in_air(chunk, origin, [tree.x, y, tree.z], log);
        }
    }
    for tree in &reaching {
        for dz in -LEAF_RADIUS..=LEAF_RADIUS {
            for dy in -LEAF_RADIUS..=LEAF_RADIUS {
                for dx in -LEAF_RADIUS..=LEAF_RADIUS {
                    if dx * dx + dy * dy + dz * dz <= LEAF_REACH_SQUARED {
                        let at = [
                            tree.x.saturating_add(dx),
                            tree.crown() + dy,
                            tree.z.saturating_add(dz),
                        ];
                        place_in_air(chunk, origin, at, leaves);
                    }
                }
            }
        }
    }
}

/// Whether the chunk row `origin_y..origin_y + 32` can hold any block of a tree. A cheap test
/// that spares the layers far under or above the land the tree search.
pub(crate) const fn may_hold_trees(origin_y: i32) -> bool {
    origin_y <= HIGHEST_TREE_Y && origin_y + CHUNK_SIZE_I32 > LOWEST_TREE_Y
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tree_bounds_match_the_tallest_tree() {
        assert_eq!(TREE_MAX_HEIGHT, 9);
        assert_eq!(MAX_TREES, 100);
        assert_eq!(LEAF_REACH_SQUARED, 5);
    }

    #[test]
    fn a_chunk_far_from_the_land_holds_no_trees() {
        assert!(!may_hold_trees(0));
        assert!(!may_hold_trees(HIGHEST_TREE_Y + 1));
        assert!(may_hold_trees(64));
        assert!(may_hold_trees(HIGHEST_TREE_Y));
    }
}
