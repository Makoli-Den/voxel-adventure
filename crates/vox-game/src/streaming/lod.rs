//! Which level-of-detail tiles are wanted around the camera, as pure functions
//! (decision 0015, step S6).
//!
//! The selection is a quadtree over the horizon disc. Root tiles of level [`MAX_LOD_LEVEL`]
//! cover the disc; a tile of level `k >= 1` is split into its four children while it is close
//! to the camera, and the leaves are the tiles to draw. Level-0 leaves are the full-detail
//! chunk columns of [`super::area`]; coarser leaves are the far rings.
//!
//! Tiles are horizontal columns: this module works on `(level, x, z)` tiles in the terrain
//! layer ([`TERRAIN_LAYER`], `y = 0`). Vertical layers are handled by the column span
//! (decision 0015 (a)), not by the selection.
//!
//! Two rules keep the selection calm while the camera moves:
//! - **The split uses the camera snapped to the grid of the tile's level.** The distance of a
//!   level-`k` tile is measured from the centre of the level-`k` tile the camera stands in, so
//!   a split changes only when the camera crosses a border of a tile of that level.
//! - **The horizon cut uses the real camera column.** It only adds or removes tiles at the far
//!   edge; it never changes the level of a tile that stays.
//!
//! **The horizon disc is centred on the centre of the camera's column**, which is up to
//! `CHUNK_BLOCKS * sqrt(2) / 2` (about 22.6 blocks) from the real camera position. A fog that
//! must hide the edge of the selection therefore ends at most
//! `horizon - CHUNK_BLOCKS * sqrt(2) / 2` blocks from the camera (a note for S7).
//!
//! **The number of leaves is a budget** ([`MAX_LEAVES`]): [`max_horizon_for`] gives the
//! largest horizon that keeps a view radius inside it, so the game cannot ask for more meshes
//! than the budgets of decision 0015 allow.
//!
//! Not wired into the streamer yet (step S7), hence the `dead_code` expectation (it does not
//! apply to the tests, which use everything).
#![cfg_attr(not(test), expect(dead_code, reason = "wired in S7"))]

use std::collections::HashSet;

use vox_world::{CHUNK_SIZE, ChunkPos, MAX_LOD_LEVEL, TilePos};

use super::area::TERRAIN_LAYER;

/// The horizon the game starts with: 2048 blocks (decision 0015, "Answers to the open
/// questions"). Tiles of level 4 are used only beyond `(view_radius / 2 + 0.375) * 512`
/// blocks (2240 at radius 8), so at the default radius the outermost tiles are level 3.
pub const DEFAULT_HORIZON_BLOCKS: i32 = 2048;

/// The largest horizon in blocks; a larger request is cut to it. It is eight root tiles
/// (`2^MAX_LOD_LEVEL` chunks, 512 blocks) from the camera: the far end the decision's
/// budgets (meshes, GPU memory, depth precision with reversed Z) were worked out for.
pub const MAX_HORIZON_BLOCKS: i32 = 4096;

/// The largest view radius the selection accepts, in chunks; a larger one counts as this. It
/// only keeps the arithmetic far from overflow; the game's own maximum is lower (16, S7) and
/// the mesh budget ([`max_horizon_for`]) is what limits a big radius in practice.
pub const MAX_VIEW_RADIUS_CHUNKS: i32 = 30;

/// The smallest view radius that is used, in chunks; a smaller one counts as this. Below it
/// the split threshold (`radius / 2 + slack` tiles) is under one tile and a ring can be
/// thinner than the snap error of the next one, so neighbouring leaves could differ by two
/// levels (measured with radius 0, see the test). An unbalanced selection is worse than a
/// column more of full detail.
pub const MIN_BALANCED_RADIUS: i32 = 1;

/// The most leaves (meshes) one selection may have: decision 0015 gives the mesh budget of
/// the far horizon as 3000 (about 90 MB of GPU memory at 30 KB a tile).
pub const MAX_LEAVES: usize = 3000;

/// The steps [`max_horizon_for`] moves the horizon in: half a root tile, so a horizon of a
/// whole number of steps ends on a tile border of level 3.
pub const HORIZON_STEP_BLOCKS: i32 = 256;

/// Safety margin in percent that [`max_horizon_for`] keeps below [`MAX_LEAVES`], because it
/// counts the leaves for one camera column and the count varies with the position inside
/// the root tile grid (measured: under 1 percent; the test checks every position).
const LEAF_COUNT_SAFETY_PERCENT: usize = 5;

/// How many tiles of its own size a tile must be away from the camera before it is split, per
/// chunk of view radius: `SPLIT_RATIO = view_radius / 2` (decision 0015). A level-1 tile is two
/// chunks wide, so it splits within `view_radius` chunks and the level-0 leaves form a disc of
/// about `view_radius` chunks. Each coarser ring is twice as wide as the one inside it, which
/// keeps the size of a cell on the screen about constant (the "screen error" of the decision).
const SPLIT_RATIO_PER_RADIUS_CHUNK: f64 = 0.5;

/// Extra distance, in tiles of the level being split, added to the split threshold.
///
/// The camera is snapped to the centre of its level-1 tile, which is at most half a chunk away
/// on each axis from the centre of the real camera column: `0.5 * sqrt 2 = 0.71` chunks, that
/// is `0.354` of a level-1 tile (two chunks). Without this slack a column at exactly the view
/// radius could fall in a level-1 tile that does not split, and [`select_tiles`] would miss a
/// column that [`super::area::wanted`] loads. `0.375` is the next eighth above `0.354` (a test
/// checks the relation). The same slack at coarser levels is harmless: their rings only get a
/// little wider.
const SNAP_SLACK_TILES: f64 = 0.375;

/// A chunk's edge in blocks, in `i64` (all the geometry is done in `i64`, which cannot
/// overflow for any `i32` camera and horizon).
const CHUNK_BLOCKS: i64 = CHUNK_SIZE as i64;

/// Half-tiles in a tile. The gap from the centre of the camera's tile to the nearest edge of
/// another tile is an odd number of half-tiles.
const HALF_TILES_PER_TILE: i64 = 2;

/// Edge length in chunks of a tile of `level`: `2^level`.
fn tile_chunks(level: u8) -> i64 {
    1 << level
}

/// Edge length in blocks of a tile of `level`.
fn extent_blocks(level: u8) -> i64 {
    CHUNK_BLOCKS * tile_chunks(level)
}

/// The camera as the selection sees it: its column in chunks and the centre of that column in
/// blocks.
struct CameraColumn {
    /// Chunk column (x, z).
    chunk: [i64; 2],
    /// Centre of the column in blocks (x, z); the point the horizon disc is centred on.
    centre: [i64; 2],
}

impl CameraColumn {
    fn new(camera: ChunkPos) -> Self {
        let chunk = [i64::from(camera.x), i64::from(camera.z)];
        let centre = chunk.map(|c| c * CHUNK_BLOCKS + CHUNK_BLOCKS / 2);
        Self { chunk, centre }
    }
}

/// The whole state of one selection run.
struct Selector {
    camera: CameraColumn,
    /// Squared horizon in blocks.
    horizon_squared: i64,
    /// Squared split threshold in tiles of the tile's own level.
    split_squared: f64,
    /// Leaves with their squared distance (nearest point, blocks) for the final sort.
    leaves: Vec<(i64, TilePos)>,
}

/// Squared distance from the point `(px, pz)` to the nearest point of the square that starts
/// at `(x0, z0)` and is `extent` wide. Zero inside.
fn nearest_distance_squared(point: [i64; 2], origin: [i64; 2], extent: i64) -> i64 {
    let gap = |p: i64, o: i64| (o - p).max(p - (o + extent)).max(0);
    let dx = gap(point[0], origin[0]);
    let dz = gap(point[1], origin[1]);
    dx * dx + dz * dz
}

/// Distance between tile `t` and the tile `c` the camera stands in, along one axis, in
/// half-tiles: the gap between the camera tile's centre and the nearest edge of `t`. Zero for
/// the camera's own tile, otherwise odd (`2 * |t - c| - 1`).
fn half_tile_gap(t: i64, c: i64) -> i64 {
    if t == c {
        0
    } else {
        HALF_TILES_PER_TILE * (t - c).abs() - 1
    }
}

impl Selector {
    fn should_split(&self, level: u8, x: i64, z: i64) -> bool {
        let camera_x = self.camera.chunk[0].div_euclid(tile_chunks(level));
        let camera_z = self.camera.chunk[1].div_euclid(tile_chunks(level));
        let gap_x = half_tile_gap(x, camera_x);
        let gap_z = half_tile_gap(z, camera_z);
        let half_tiles_squared = (gap_x * gap_x + gap_z * gap_z) as f64;
        let distance_squared =
            half_tiles_squared / (HALF_TILES_PER_TILE * HALF_TILES_PER_TILE) as f64;
        distance_squared < self.split_squared
    }

    fn visit(&mut self, level: u8, x: i64, z: i64) {
        let extent = extent_blocks(level);
        let distance =
            nearest_distance_squared(self.camera.centre, [x * extent, z * extent], extent);
        if distance > self.horizon_squared {
            return;
        }
        if level > 0 && self.should_split(level, x, z) {
            for dz in 0..2 {
                for dx in 0..2 {
                    self.visit(level - 1, 2 * x + dx, 2 * z + dz);
                }
            }
        } else if let (Ok(tile_x), Ok(tile_z)) = (i32::try_from(x), i32::try_from(z)) {
            // A tile whose index does not fit `i32` cannot exist (about 7e10 blocks out).
            let pos = ChunkPos::new(tile_x, TERRAIN_LAYER, tile_z);
            self.leaves.push((distance, TilePos::new(level, pos)));
        }
    }
}

/// The tiles to show around `camera` (only its column counts, the `y` is ignored), nearest
/// first.
///
/// `view_radius_chunks` is the full-detail radius in chunks, clamped to
/// [`MIN_BALANCED_RADIUS`]`..=`[`MAX_VIEW_RADIUS_CHUNKS`], and `horizon_blocks` the distance
/// to which the world is drawn, clamped to `0..=`[`MAX_HORIZON_BLOCKS`]. The count of leaves
/// is not limited here: keep the horizon at or below [`max_horizon_for`] the radius.
///
/// The leaves have no overlap, cover the disc of the horizon around the centre of the camera's
/// column, and neighbouring leaves differ by at most one level (tested over every position in
/// the root tile grid for small radii, and over many cameras on both sides of zero for the
/// rest). Every column that [`super::area::wanted`] loads for the same radius is a level-0
/// leaf; the level-0 leaves reach a few chunks beyond it (3.6 at most, tested), more than
/// [`super::area::UNLOAD_MARGIN`], so the keep range of level-0 leaves cannot be
/// [`super::area::is_kept`].
///
/// The order is by distance of the tile's nearest point, then level, then `(z, x)`, so it is the
/// same on every call. All tiles are in layer [`TERRAIN_LAYER`].
///
/// # Example
///
/// ```ignore
/// let tiles = select_tiles(ChunkPos::ORIGIN, 8, DEFAULT_HORIZON_BLOCKS);
/// assert_eq!(tiles[0].level(), 0);
/// ```
pub fn select_tiles(
    camera: ChunkPos,
    view_radius_chunks: i32,
    horizon_blocks: i32,
) -> Vec<TilePos> {
    let radius = view_radius_chunks.clamp(MIN_BALANCED_RADIUS, MAX_VIEW_RADIUS_CHUNKS);
    select_with_radius(camera, radius, horizon_blocks)
}

/// [`select_tiles`] without the radius clamp (the tests use it to show why the minimum exists).
fn select_with_radius(camera: ChunkPos, radius: i32, horizon_blocks: i32) -> Vec<TilePos> {
    let horizon = i64::from(horizon_blocks.clamp(0, MAX_HORIZON_BLOCKS));
    let split_tiles = f64::from(radius.max(0)) * SPLIT_RATIO_PER_RADIUS_CHUNK + SNAP_SLACK_TILES;
    let mut selector = Selector {
        camera: CameraColumn::new(camera),
        horizon_squared: horizon * horizon,
        split_squared: split_tiles * split_tiles,
        leaves: Vec::new(),
    };
    let root_extent = extent_blocks(MAX_LOD_LEVEL);
    let [centre_x, centre_z] = selector.camera.centre;
    let root_range = |centre: i64| {
        (centre - horizon).div_euclid(root_extent)..=(centre + horizon).div_euclid(root_extent)
    };
    for z in root_range(centre_z) {
        for x in root_range(centre_x) {
            selector.visit(MAX_LOD_LEVEL, x, z);
        }
    }
    let mut leaves = selector.leaves;
    leaves.sort_unstable_by_key(|(distance, tile)| {
        (*distance, tile.level(), tile.pos().z, tile.pos().x)
    });
    leaves.into_iter().map(|(_, tile)| tile).collect()
}

/// The largest horizon, in whole [`HORIZON_STEP_BLOCKS`], for which a selection at
/// `view_radius_chunks` has at most [`MAX_LEAVES`] leaves (with the
/// [`LEAF_COUNT_SAFETY_PERCENT`] margin). At most [`MAX_HORIZON_BLOCKS`].
///
/// The game clamps its horizon option to this, so the mesh budget is enforced by code. If even
/// one step does not fit, one step is returned (the horizon also cuts the full-detail disc, so
/// a very large radius still has few leaves; at the largest radius the answer is 768). More
/// horizon never has fewer
/// leaves, so the answer is found by bisection; the cost is a few selections (well under a
/// millisecond), meant to run when an option changes, not every frame.
pub fn max_horizon_for(view_radius_chunks: i32) -> i32 {
    let fits = |horizon: i32| {
        let leaves = select_tiles(ChunkPos::ORIGIN, view_radius_chunks, horizon).len();
        leaves * (100 + LEAF_COUNT_SAFETY_PERCENT) <= MAX_LEAVES * 100
    };
    let steps = MAX_HORIZON_BLOCKS / HORIZON_STEP_BLOCKS;
    // Invariant: `low` steps fit (or it is the minimum), `high + 1` steps do not.
    let (mut low, mut high) = (1, steps);
    while low < high {
        let middle = (low + high + 1) / 2;
        if fits(middle * HORIZON_STEP_BLOCKS) {
            low = middle;
        } else {
            high = middle - 1;
        }
    }
    low * HORIZON_STEP_BLOCKS
}

/// The tiles that appear and disappear between two selections: `(added, removed)`.
///
/// `added` keeps the order of `next` (nearest first for a nearest-first selection, the order
/// to load in). `removed` is the order of `previous` **reversed**, so the tile farthest from
/// the old camera comes first, the order to unload in.
///
/// The lists carry no relation between tiles: a level change shows as one removed tile and
/// four added (or the reverse) with no link. Step S8, which keeps the old tile until the new
/// ones are drawn, must derive the replacement from the tile rectangles
/// ([`TilePos::origin`] and [`TilePos::extent`]).
pub fn diff(previous: &[TilePos], next: &[TilePos]) -> (Vec<TilePos>, Vec<TilePos>) {
    let old: HashSet<TilePos> = previous.iter().copied().collect();
    let new: HashSet<TilePos> = next.iter().copied().collect();
    let added = next.iter().copied().filter(|t| !old.contains(t)).collect();
    let removed = previous
        .iter()
        .rev()
        .copied()
        .filter(|t| !new.contains(t))
        .collect();
    (added, removed)
}

#[cfg(test)]
mod tests {
    use std::f64::consts::SQRT_2;
    use std::time::Instant;

    use super::*;
    use crate::streaming::area::wanted;

    /// The radius the decision's numbers were first given for.
    const RADIUS: i32 = 8;
    /// The largest radius the game will allow (decision 0015, S7).
    const MAX_RADIUS: i32 = 16;
    /// Spacing in blocks of the sample points that check the disc is covered.
    const SAMPLE_STEP_BLOCKS: i64 = 24;
    /// The cost the decision expects of one selection, in microseconds.
    const MAX_SELECTION_MICROS: u128 = 1000;
    /// Camera positions repeat with the root tile (the selection depends on the camera only
    /// modulo this many chunks): the number of distinct positions per axis.
    const RESIDUES: i32 = 1 << MAX_LOD_LEVEL;
    /// How far, in chunks, a level-0 leaf may lie beyond the view radius (centre of the
    /// column to centre of the camera column). Measured over every position in the root
    /// tile grid and radii 0 to 16: see the test.
    const LEVEL0_MARGIN_CHUNKS: f64 = 3.6;
    /// How far under the bound the measured margin must be, so the constant stays tight.
    const MARGIN_TOLERANCE_CHUNKS: f64 = 0.1;

    /// A tile as a block rectangle `(x0, z0, x1, z1)`, ends exclusive.
    fn rect(tile: TilePos) -> [i64; 4] {
        let extent = i64::from(tile.extent());
        let x0 = i64::from(tile.pos().x) * extent;
        let z0 = i64::from(tile.pos().z) * extent;
        [x0, z0, x0 + extent, z0 + extent]
    }

    fn overlaps(a: [i64; 4], b: [i64; 4]) -> bool {
        a[0] < b[2] && b[0] < a[2] && a[1] < b[3] && b[1] < a[3]
    }

    /// Whether two rectangles share a piece of border (positive length).
    fn touches(a: [i64; 4], b: [i64; 4]) -> bool {
        let overlap_x = a[0] < b[2] && b[0] < a[2];
        let overlap_z = a[1] < b[3] && b[1] < a[3];
        ((a[2] == b[0] || b[2] == a[0]) && overlap_z)
            || ((a[3] == b[1] || b[3] == a[1]) && overlap_x)
    }

    fn contains_point(r: [i64; 4], x: i64, z: i64) -> bool {
        r[0] <= x && x < r[2] && r[1] <= z && z < r[3]
    }

    /// Deterministic cameras: a linear congruential generator (Knuth's MMIX constants) over
    /// +-6000 chunks, so negative columns and tile borders of every level are met, plus the
    /// borders themselves.
    fn cameras() -> Vec<ChunkPos> {
        let mut state: u64 = 0x2545_F491_4F6C_DD1D;
        let mut next = || {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            ((state >> 33) % 12_001) as i32 - 6000
        };
        let mut cameras: Vec<ChunkPos> =
            (0..40).map(|_| ChunkPos::new(next(), 0, next())).collect();
        for border in [-17, -16, -15, -1, 0, 1, 15, 16, 17] {
            cameras.push(ChunkPos::new(border, 0, -border));
        }
        cameras
    }

    /// One camera for every position in the root tile grid, in negative columns far from
    /// the origin.
    fn all_residues() -> Vec<ChunkPos> {
        let base = -RESIDUES * 40;
        (0..RESIDUES)
            .flat_map(|dz| (0..RESIDUES).map(move |dx| ChunkPos::new(base + dx, 0, base + dz)))
            .collect()
    }

    /// Whether any two touching leaves differ by more than one level.
    fn has_two_level_step(tiles: &[TilePos]) -> bool {
        let rects: Vec<[i64; 4]> = tiles.iter().copied().map(rect).collect();
        tiles.iter().enumerate().any(|(i, a)| {
            tiles
                .iter()
                .enumerate()
                .skip(i + 1)
                .any(|(j, b)| touches(rects[i], rects[j]) && a.level().abs_diff(b.level()) > 1)
        })
    }

    #[test]
    fn the_snap_slack_covers_the_snap_error() {
        // Half a chunk on each axis is 0.5 * sqrt 2 chunks; a level-1 tile is two chunks.
        let snap_error_tiles = 0.5 * SQRT_2 / 2.0;
        assert!(SNAP_SLACK_TILES >= snap_error_tiles);
        assert!(
            SNAP_SLACK_TILES < snap_error_tiles + 0.05,
            "slack not tight"
        );
    }

    #[test]
    fn leaves_do_not_overlap_and_cover_the_horizon_disc() {
        for camera in cameras() {
            for radius in [MIN_BALANCED_RADIUS, 4, RADIUS, MAX_RADIUS] {
                for horizon in [300, DEFAULT_HORIZON_BLOCKS] {
                    let rects: Vec<[i64; 4]> = select_tiles(camera, radius, horizon)
                        .into_iter()
                        .map(rect)
                        .collect();
                    for (i, a) in rects.iter().enumerate() {
                        for b in &rects[i + 1..] {
                            assert!(!overlaps(*a, *b), "{camera:?} r{radius}: {a:?} {b:?}");
                        }
                    }
                    // Every sample point of a grid over the disc is in a leaf (no overlap is
                    // shown above, so "at least one" is "exactly one").
                    let centre = CameraColumn::new(camera).centre;
                    let reach = i64::from(horizon);
                    let mut z = -reach;
                    while z <= reach {
                        let mut x = -reach;
                        while x <= reach {
                            if x * x + z * z <= reach * reach {
                                let (px, pz) = (centre[0] + x, centre[1] + z);
                                assert!(
                                    rects.iter().any(|r| contains_point(*r, px, pz)),
                                    "{camera:?} r{radius} h{horizon}: ({px}, {pz}) uncovered"
                                );
                            }
                            x += SAMPLE_STEP_BLOCKS;
                        }
                        z += SAMPLE_STEP_BLOCKS;
                    }
                }
            }
        }
    }

    #[test]
    fn the_disc_edge_is_covered_all_around() {
        // Points exactly on the horizon circle, every degree.
        let camera = ChunkPos::new(-3, 0, 5);
        let horizon = 700;
        let rects: Vec<[i64; 4]> = select_tiles(camera, RADIUS, horizon)
            .into_iter()
            .map(rect)
            .collect();
        let centre = CameraColumn::new(camera).centre;
        for degree in 0..360 {
            let angle = f64::from(degree).to_radians();
            let px = centre[0] + (f64::from(horizon) * angle.cos()).floor() as i64;
            let pz = centre[1] + (f64::from(horizon) * angle.sin()).floor() as i64;
            assert!(rects.iter().any(|r| contains_point(*r, px, pz)), "{degree}");
        }
    }

    #[test]
    fn neighbouring_leaves_differ_by_at_most_one_level() {
        // Small radii, where the property is least safe: every position in the root tile
        // grid. The horizon reaches level 4 leaves without making the pair check slow.
        for camera in all_residues() {
            for radius in MIN_BALANCED_RADIUS..=3 {
                let tiles = select_tiles(camera, radius, 1536);
                assert!(!has_two_level_step(&tiles), "{camera:?} r{radius}");
            }
        }
        // Larger radii, the full horizon, many cameras around zero and near tile borders.
        for camera in cameras() {
            for radius in [4, RADIUS, 12, MAX_RADIUS] {
                let tiles = select_tiles(camera, radius, MAX_HORIZON_BLOCKS);
                assert!(!has_two_level_step(&tiles), "{camera:?} r{radius}");
            }
        }
    }

    #[test]
    fn radius_zero_would_break_the_balance_which_is_why_it_is_clamped() {
        let broken = all_residues().into_iter().any(|camera| {
            has_two_level_step(&select_with_radius(camera, MIN_BALANCED_RADIUS - 1, 1536))
        });
        assert!(broken, "the minimum radius could be lowered");
    }

    #[test]
    fn every_wanted_column_is_a_level_0_leaf_and_the_margin_is_bounded() {
        let mut widest: f64 = 0.0;
        for camera in all_residues() {
            for radius in 0..=MAX_RADIUS {
                let tiles = select_tiles(camera, radius, MAX_HORIZON_BLOCKS);
                let level0: HashSet<ChunkPos> = tiles
                    .iter()
                    .filter(|t| t.level() == 0)
                    .map(|t| t.pos())
                    .collect();
                for column in wanted(camera, radius) {
                    assert!(level0.contains(&column), "{camera:?} r{radius}: {column:?}");
                }
                // The clamped radius is what the selection used.
                let used = radius.max(MIN_BALANCED_RADIUS);
                for pos in &level0 {
                    let (dx, dz) = (f64::from(pos.x - camera.x), f64::from(pos.z - camera.z));
                    widest = widest.max(dx.hypot(dz) - f64::from(used));
                }
            }
        }
        println!("widest level-0 margin beyond the radius: {widest:.3} chunks");
        assert!(widest <= LEVEL0_MARGIN_CHUNKS);
        assert!(
            widest >= LEVEL0_MARGIN_CHUNKS - MARGIN_TOLERANCE_CHUNKS,
            "the constant is not tight"
        );
        assert!(widest > f64::from(crate::streaming::area::UNLOAD_MARGIN));
    }

    #[test]
    fn level_0_leaves_stay_close_to_the_wanted_area_in_number() {
        let wanted_len = wanted(ChunkPos::ORIGIN, RADIUS).len();
        let mut most = 0;
        for camera in cameras() {
            let level0 = select_tiles(camera, RADIUS, DEFAULT_HORIZON_BLOCKS)
                .iter()
                .filter(|t| t.level() == 0)
                .count();
            assert!(level0 >= wanted_len);
            most = most.max(level0);
        }
        println!("level-0 leaves at radius 8: {wanted_len} wanted, at most {most}");
        // Whole 2 x 2 groups of a level-1 tile plus the snap slack: measured 308 (56 percent
        // more than the disc).
        assert!(most * 100 <= wanted_len * 160);
    }

    /// Leaves of level `>= level` that lie wholly within the horizon of both cameras (the
    /// horizon cut follows the real camera and is not part of the invariant).
    fn inner_leaves(
        camera: ChunkPos,
        other: ChunkPos,
        level: u8,
        radius: i32,
        horizon: i32,
    ) -> HashSet<TilePos> {
        let far = |tile: TilePos, camera: ChunkPos| {
            let [x0, z0, x1, z1] = rect(tile);
            let centre = CameraColumn::new(camera).centre;
            let dx = (x0 - centre[0]).abs().max((x1 - centre[0]).abs());
            let dz = (z0 - centre[1]).abs().max((z1 - centre[1]).abs());
            dx * dx + dz * dz
        };
        let limit = i64::from(horizon) * i64::from(horizon);
        select_tiles(camera, radius, horizon)
            .into_iter()
            .filter(|t| t.level() >= level)
            .filter(|t| far(*t, camera) <= limit && far(*t, other) <= limit)
            .collect()
    }

    #[test]
    fn moving_inside_a_level_k_tile_keeps_the_leaves_of_level_k_and_up() {
        // The largest horizon, so that level-4 leaves lie wholly inside it too.
        let horizon = MAX_HORIZON_BLOCKS;
        for level in 1..=MAX_LOD_LEVEL {
            let size = 1_i32 << level;
            for base in [
                ChunkPos::new(-3 * size, 0, 2 * size),
                ChunkPos::new(5 * size, 0, -7 * size),
            ] {
                // Every column of the level-k tile that starts at `base`.
                assert!(!inner_leaves(base, base, level, RADIUS, horizon).is_empty());
                for dz in 0..size {
                    for dx in 0..size {
                        let moved = ChunkPos::new(base.x + dx, 0, base.z + dz);
                        assert_eq!(
                            inner_leaves(base, moved, level, RADIUS, horizon),
                            inner_leaves(moved, base, level, RADIUS, horizon),
                            "level {level} moved ({dx}, {dz})"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn crossing_a_level_1_border_changes_the_level_0_leaves_and_not_before() {
        let at = |x: i32| select_tiles(ChunkPos::new(x, 0, 0), RADIUS, DEFAULT_HORIZON_BLOCKS);
        // Columns 0 and 1 share a level-1 tile; 1 and 2 do not.
        let level_below = |tiles: &[TilePos]| -> HashSet<TilePos> {
            tiles.iter().copied().filter(|t| t.level() <= 1).collect()
        };
        let (a, b, c) = (at(0), at(1), at(2));
        assert_ne!(level_below(&b), level_below(&c));
        // Inside one level-1 tile the leaves of level 0 and 1 near the camera are the same.
        let near = |tiles: &[TilePos]| -> HashSet<TilePos> {
            tiles
                .iter()
                .copied()
                .filter(|t| {
                    t.level() <= 1 && t.pos().x.abs() < 2 * RADIUS && t.pos().z.abs() < 2 * RADIUS
                })
                .collect()
        };
        assert_eq!(near(&a), near(&b));
    }

    /// Leaf counts of one selection: the total and the count per level.
    fn counts(camera: ChunkPos, radius: i32, horizon: i32) -> (usize, [usize; 5]) {
        let mut per_level = [0_usize; MAX_LOD_LEVEL as usize + 1];
        for tile in select_tiles(camera, radius, horizon) {
            per_level[usize::from(tile.level())] += 1;
        }
        (per_level.iter().sum(), per_level)
    }

    /// The worst total over `cameras`, with its per-level counts.
    fn worst_counts(cameras: &[ChunkPos], radius: i32, horizon: i32) -> (usize, [usize; 5]) {
        cameras
            .iter()
            .map(|camera| counts(*camera, radius, horizon))
            .max_by_key(|(total, _)| *total)
            .unwrap_or_default()
    }

    /// The worst leaf counts over every position in the root tile grid, radius by horizon
    /// (radius 8, 12, 16 by horizon 2048, 4096): the numbers of the table in decision 0015.
    const WORST_COUNTS: [(i32, i32, usize); 6] = [
        (8, 2048, 934),
        (8, 4096, 1165),
        (12, 2048, 1582),
        (12, 4096, 2029),
        (16, 2048, 2367),
        (16, 4096, 3116),
    ];

    #[test]
    fn counts_are_pinned_on_a_sample_of_positions() {
        // A quarter of the positions per axis (every 5th): cheap, within 2 percent below the
        // worst (the count varies by under 1 percent).
        let sample: Vec<ChunkPos> = all_residues()
            .into_iter()
            .enumerate()
            .filter(|(i, _)| i % 5 == 0)
            .map(|(_, camera)| camera)
            .collect();
        for (radius, horizon, worst) in WORST_COUNTS {
            let (total, per_level) = worst_counts(&sample, radius, horizon);
            println!("radius {radius} horizon {horizon}: {per_level:?} total {total}");
            assert!(total <= worst, "r{radius} h{horizon}: {total} > {worst}");
            assert!(
                total * 100 >= worst * 98,
                "r{radius} h{horizon}: {total} << {worst}"
            );
        }
    }

    #[test]
    #[ignore = "every position in the root tile grid, slow in debug"]
    fn counts_are_exact_over_every_position() {
        let cameras = all_residues();
        for (radius, horizon, worst) in WORST_COUNTS {
            let (total, per_level) = worst_counts(&cameras, radius, horizon);
            println!("radius {radius} horizon {horizon}: {per_level:?} total {total}");
            assert_eq!(total, worst, "r{radius} h{horizon}");
        }
    }

    #[test]
    fn the_counts_stay_near_the_decisions_estimates_at_radius_8() {
        // The estimates (650 at 2048, 800 at 4096) were too low by about 45 percent; the real
        // numbers are in WORST_COUNTS, the estimate remains a bound at 1.5 times.
        for (horizon, estimate) in [(2048, 650_usize), (4096, 800)] {
            let (total, _) = worst_counts(&cameras(), RADIUS, horizon);
            assert!(total * 2 <= estimate * 3, "{total} vs {estimate}");
        }
    }

    #[test]
    fn max_horizon_keeps_every_position_inside_the_mesh_budget() {
        let cameras = all_residues();
        for radius in [MIN_BALANCED_RADIUS, 4, 8, 12, 16] {
            let horizon = max_horizon_for(radius);
            assert_eq!(horizon % HORIZON_STEP_BLOCKS, 0);
            assert!((HORIZON_STEP_BLOCKS..=MAX_HORIZON_BLOCKS).contains(&horizon));
            let (total, _) = worst_counts(&cameras, radius, horizon);
            assert!(total <= MAX_LEAVES, "r{radius} h{horizon}: {total}");
            // Not needlessly strict: one step more is over the budget for the reference
            // camera (the search's own measure) or already at the maximum.
            if horizon < MAX_HORIZON_BLOCKS {
                let more = select_tiles(ChunkPos::ORIGIN, radius, horizon + HORIZON_STEP_BLOCKS);
                assert!(more.len() * (100 + LEAF_COUNT_SAFETY_PERCENT) > MAX_LEAVES * 100);
            }
        }
    }

    #[test]
    fn max_horizon_values_for_the_known_radii() {
        // Radius 8 (and below) keeps the full horizon; radius 16 is cut below 4096 by the
        // budget (3116 > 3000); the horizon never grows with the radius.
        assert_eq!(max_horizon_for(RADIUS), MAX_HORIZON_BLOCKS);
        assert!(max_horizon_for(MAX_RADIUS) < MAX_HORIZON_BLOCKS);
        let horizons: Vec<i32> = (MIN_BALANCED_RADIUS..=MAX_VIEW_RADIUS_CHUNKS)
            .map(max_horizon_for)
            .collect();
        println!("max horizon by radius: {horizons:?}");
        assert!(horizons.windows(2).all(|pair| pair[0] >= pair[1]));
        // The largest radius is cut to the same horizon as itself, however large the request.
        assert_eq!(
            max_horizon_for(i32::MAX),
            max_horizon_for(MAX_VIEW_RADIUS_CHUNKS)
        );
        assert!(max_horizon_for(MAX_VIEW_RADIUS_CHUNKS) >= HORIZON_STEP_BLOCKS);
        assert_eq!(max_horizon_for(-3), max_horizon_for(MIN_BALANCED_RADIUS));
    }

    #[test]
    fn selection_is_deterministic_and_nearest_first() {
        let camera = ChunkPos::new(-12, 9, 33);
        let tiles = select_tiles(camera, RADIUS, DEFAULT_HORIZON_BLOCKS);
        assert_eq!(tiles, select_tiles(camera, RADIUS, DEFAULT_HORIZON_BLOCKS));
        assert_eq!(
            tiles[0],
            TilePos::chunk(ChunkPos::new(-12, TERRAIN_LAYER, 33))
        );
        let centre = CameraColumn::new(camera).centre;
        let distances: Vec<i64> = tiles
            .iter()
            .map(|t| {
                let [x0, z0, ..] = rect(*t);
                nearest_distance_squared(centre, [x0, z0], i64::from(t.extent()))
            })
            .collect();
        assert!(distances.windows(2).all(|pair| pair[0] <= pair[1]));
        assert!(tiles.iter().all(|t| t.pos().y == TERRAIN_LAYER));
        let unique: HashSet<_> = tiles.iter().collect();
        assert_eq!(unique.len(), tiles.len());
    }

    #[test]
    fn a_small_horizon_gives_only_level_0() {
        let full_detail_blocks = RADIUS * CHUNK_SIZE as i32;
        for camera in cameras() {
            let tiles = select_tiles(camera, RADIUS, full_detail_blocks / 2);
            assert!(!tiles.is_empty());
            assert!(tiles.iter().all(|t| t.level() == 0), "{camera:?}");
        }
    }

    #[test]
    fn a_radius_below_the_minimum_counts_as_the_minimum() {
        let camera = ChunkPos::new(7, 0, -7);
        let minimum = select_tiles(camera, MIN_BALANCED_RADIUS, DEFAULT_HORIZON_BLOCKS);
        assert_eq!(minimum[0], TilePos::chunk(ChunkPos::new(7, 0, -7)));
        for radius in [0, -5, i32::MIN] {
            assert_eq!(
                select_tiles(camera, radius, DEFAULT_HORIZON_BLOCKS),
                minimum
            );
        }
    }

    #[test]
    fn a_radius_above_the_maximum_counts_as_the_maximum() {
        let camera = ChunkPos::new(-40, 0, 15);
        let maximum = select_tiles(camera, MAX_VIEW_RADIUS_CHUNKS, DEFAULT_HORIZON_BLOCKS);
        for radius in [MAX_VIEW_RADIUS_CHUNKS + 1, 1000, i32::MAX] {
            assert_eq!(
                select_tiles(camera, radius, DEFAULT_HORIZON_BLOCKS),
                maximum
            );
        }
    }

    #[test]
    fn horizon_zero_is_one_column_and_a_large_horizon_is_clamped() {
        let camera = ChunkPos::new(-1, 0, -1);
        assert_eq!(
            select_tiles(camera, RADIUS, 0),
            vec![TilePos::chunk(ChunkPos::new(-1, 0, -1))]
        );
        assert_eq!(
            select_tiles(camera, RADIUS, -50),
            select_tiles(camera, RADIUS, 0)
        );
        assert_eq!(
            select_tiles(camera, RADIUS, i32::MAX),
            select_tiles(camera, RADIUS, MAX_HORIZON_BLOCKS)
        );
    }

    #[test]
    fn extreme_cameras_do_not_panic_or_overflow() {
        for x in [i32::MIN, i32::MIN + 1, -1, 0, i32::MAX - 1, i32::MAX] {
            for z in [i32::MIN, 0, i32::MAX] {
                let camera = ChunkPos::new(x, i32::MAX, z);
                let tiles = select_tiles(camera, i32::MAX, MAX_HORIZON_BLOCKS);
                assert!(!tiles.is_empty());
                let _ = select_tiles(camera, i32::MIN, i32::MIN);
                let _ = select_tiles(camera, RADIUS, MAX_HORIZON_BLOCKS);
            }
        }
    }

    #[test]
    fn diff_lists_added_in_load_order_and_removed_farthest_first() {
        let tile = |level: u8, x: i32| TilePos::new(level, ChunkPos::new(x, 0, 0));
        let previous = [tile(0, 1), tile(0, 2), tile(1, 3), tile(1, 5)];
        let next = [tile(0, 2), tile(1, 9), tile(0, 7), tile(1, 3)];
        let (added, removed) = diff(&previous, &next);
        assert_eq!(added, vec![tile(1, 9), tile(0, 7)]);
        assert_eq!(removed, vec![tile(1, 5), tile(0, 1)]);
        // A tile at another level is another tile.
        assert_eq!(
            diff(&[tile(0, 4)], &[tile(1, 4)]),
            (vec![tile(1, 4)], vec![tile(0, 4)])
        );
        assert_eq!(diff(&next, &next), (vec![], vec![]));
        assert_eq!(diff(&[], &next).0, next.to_vec());
        let mut reversed = next.to_vec();
        reversed.reverse();
        assert_eq!(diff(&next, &[]).1, reversed);
    }

    #[test]
    fn a_level_change_is_one_tile_against_four_in_the_diff() {
        // Camera at the border of two level-1 tiles: moving across makes a level-1 tile split
        // or four level-0 leaves merge, with no link between the lists.
        let counts_by_level =
            |tiles: &[TilePos], level: u8| tiles.iter().filter(|t| t.level() == level).count();
        let from = select_tiles(ChunkPos::new(0, 0, 0), RADIUS, DEFAULT_HORIZON_BLOCKS);
        let to = select_tiles(ChunkPos::new(2, 0, 0), RADIUS, DEFAULT_HORIZON_BLOCKS);
        let (added, removed) = diff(&from, &to);
        assert!(counts_by_level(&added, 0) > 0 && counts_by_level(&removed, 1) > 0);
        assert_eq!(counts_by_level(&added, 0) % 4, 0);
    }

    /// The highest level whose tile border a step from column `x` to `x + 1` crosses: the
    /// camera's tile of level `k` changes when `x + 1` is a multiple of `2^k`.
    fn crossed_level(x: i32) -> u8 {
        (0..=MAX_LOD_LEVEL)
            .rev()
            .find(|level| i64::from(x + 1).rem_euclid(tile_chunks(*level)) == 0)
            .unwrap_or(0)
    }

    #[test]
    fn a_step_of_one_chunk_changes_only_leaves_up_to_the_crossed_level_or_at_the_edge() {
        // What S7 will load or drop per chunk crossed, over every position in the root tile
        // grid, one step along x at radius 8 and horizon 2048. A split of a level-k tile
        // changes with the camera tile of level k, so the changed leaves have a level up to
        // the crossed one; the horizon cut changes tiles within one column of the edge.
        let horizon = i64::from(DEFAULT_HORIZON_BLOCKS);
        let mut most = [0_usize; MAX_LOD_LEVEL as usize + 1];
        for camera in all_residues() {
            let next = ChunkPos::new(camera.x + 1, 0, camera.z);
            let from = select_tiles(camera, RADIUS, DEFAULT_HORIZON_BLOCKS);
            let to = select_tiles(next, RADIUS, DEFAULT_HORIZON_BLOCKS);
            let (added, removed) = diff(&from, &to);
            let crossed = crossed_level(camera.x);
            let centre = CameraColumn::new(camera).centre;
            for tile in added.iter().chain(&removed) {
                let [x0, z0, ..] = rect(*tile);
                let nearest = nearest_distance_squared(centre, [x0, z0], i64::from(tile.extent()));
                let edge = horizon - CHUNK_BLOCKS;
                assert!(
                    tile.level() <= crossed || nearest >= edge * edge,
                    "{camera:?}: {tile:?} changed, crossed level {crossed}"
                );
            }
            let slot = &mut most[usize::from(crossed)];
            *slot = (*slot).max(added.len()).max(removed.len());
        }
        println!("most leaves added or removed by a step, by crossed level: {most:?}");
        assert_eq!(most, STEP_CHANGES_BY_CROSSED_LEVEL);
    }

    /// The most leaves added or removed by a one-chunk step, by the highest level border
    /// crossed (measured, see the test). A step inside one level-1 tile moves only the
    /// horizon edge (at most 6 leaves); each level up adds the tiles that split or merge there.
    const STEP_CHANGES_BY_CROSSED_LEVEL: [usize; MAX_LOD_LEVEL as usize + 1] =
        [6, 48, 93, 141, 141];

    /// Cost of one selection. Release only: `cargo test -p vox-game --release lod -- --ignored
    /// --nocapture`.
    #[test]
    #[ignore = "timing: run in release"]
    fn selection_cost() {
        let cameras = cameras();
        for horizon in [DEFAULT_HORIZON_BLOCKS, MAX_HORIZON_BLOCKS] {
            let start = Instant::now();
            let mut leaves = 0;
            for camera in &cameras {
                leaves += select_tiles(*camera, RADIUS, horizon).len();
            }
            let per_call = start.elapsed() / cameras.len() as u32;
            println!(
                "horizon {horizon}: {per_call:?} per selection, {} leaves on average",
                leaves / cameras.len()
            );
            assert!(per_call.as_micros() < MAX_SELECTION_MICROS);
        }
    }
}
