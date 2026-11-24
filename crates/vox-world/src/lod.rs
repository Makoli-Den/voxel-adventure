//! Level-of-detail tiles (decision 0015, step S4): coarse copies of the terrain for the far
//! horizon, made by the same generator and meshed by the same mesher.
//!
//! A tile of level `k` is an ordinary [`Chunk`] of 32 x 32 x 32 **cells**, each cell
//! `2^k` blocks on a side, so it covers `32 * 2^k` blocks per axis. Level 0 is a chunk.
//! Tiles are fixed to the world grid (tile `(x, y, z)` of level `k` starts at block
//! `32 * 2^k * (x, y, z)`), never to the camera, so point sampling does not shimmer.
//!
//! The module holds the tile position ([`TilePos`]), the generator
//! ([`generate_lod_chunk`]) and the two small helpers that mesh a tile alone with a solid
//! stand-in below ([`solid_chunk`], [`build_tile_mesh_into`]), plus [`skirt_quads`] to count
//! the walls that close the cracks between levels.

use crate::block::BlockId;
use crate::bounds::{CHUNK_SIZE_I32, WORLD_FLOOR_Y};
use crate::chunk::{CHUNK_SIZE, Chunk, LocalPos};
use crate::chunk_pos::ChunkPos;
use crate::mesh::{ChunkMesh, Face, VERTICES_PER_QUAD, build_mesh_with_neighbours_into};
use crate::registry::FaceTextures;
use crate::terrain::{TerrainBlocks, block_at_depth, surface_height};
use crate::world::Neighbours;

/// The coarsest level. Its cells are `2^4 = 16` blocks and its tile covers
/// `32 * 16 = 512` blocks per axis, so a quadtree of such tiles reaches the 2048 to 4096
/// block horizon of decision 0015 with a handful of root tiles. Levels above it would make
/// cells larger than the terrain's smallest hills.
pub const MAX_LOD_LEVEL: u8 = 4;

/// `CHUNK_SIZE` in `i64`, the type cell indices are computed in: the cell index of a tile
/// at the end of the `i32` range does not fit `i32`.
const CHUNK_SIZE_I64: i64 = CHUNK_SIZE as i64;

/// Edge length in blocks of one cell of a tile of `level`: `2^level`.
///
/// A level above [`MAX_LOD_LEVEL`] counts as [`MAX_LOD_LEVEL`], so the function is total
/// (a shift by 32 or more would overflow).
///
/// # Example
///
/// ```
/// use vox_world::cell_size;
///
/// assert_eq!(cell_size(0), 1);
/// assert_eq!(cell_size(2), 4);
/// ```
pub const fn cell_size(level: u8) -> i32 {
    let level = if level > MAX_LOD_LEVEL {
        MAX_LOD_LEVEL
    } else {
        level
    };
    1 << level
}

/// Where a tile sits in the world: its level and its position in that level's own grid.
///
/// Level 0 is a chunk: `TilePos::chunk(pos)`. At level `k` the position counts tiles of
/// `32 * 2^k` blocks, so tile `(1, 0, 0)` of level 2 starts at block `x = 128`. A level
/// above [`MAX_LOD_LEVEL`] is clamped to [`MAX_LOD_LEVEL`] by [`TilePos::new`], so two equal
/// tiles are always equal as map keys too (the fields are private for that reason).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TilePos {
    /// Level of detail: cells are `2^level` blocks. At most [`MAX_LOD_LEVEL`].
    level: u8,
    /// Position in the grid of this level.
    pos: ChunkPos,
}

impl TilePos {
    /// The tile of `level` at `pos`. A level above [`MAX_LOD_LEVEL`] becomes
    /// [`MAX_LOD_LEVEL`].
    pub const fn new(level: u8, pos: ChunkPos) -> Self {
        let level = if level > MAX_LOD_LEVEL {
            MAX_LOD_LEVEL
        } else {
            level
        };
        Self { level, pos }
    }

    /// Level of detail: cells are [`cell_size`]`(level)` blocks.
    pub const fn level(self) -> u8 {
        self.level
    }

    /// Position in the grid of this tile's level.
    pub const fn pos(self) -> ChunkPos {
        self.pos
    }

    /// The full-detail tile (level 0) that is the chunk at `pos`.
    pub const fn chunk(pos: ChunkPos) -> Self {
        Self::new(0, pos)
    }

    /// Edge length of the tile in blocks: 32 cells of [`cell_size`] blocks each.
    pub const fn extent(self) -> i32 {
        CHUNK_SIZE_I32 * cell_size(self.level)
    }

    /// World position of the tile's lowest corner in blocks. Saturates like
    /// [`ChunkPos::origin`] far outside the usable world instead of wrapping.
    pub const fn origin(self) -> [i32; 3] {
        let extent = self.extent();
        [
            self.pos.x.saturating_mul(extent),
            self.pos.y.saturating_mul(extent),
            self.pos.z.saturating_mul(extent),
        ]
    }
}

/// Half a cell in blocks: the offset of the centre block from a cell's lowest corner, and the
/// amount added before the division that rounds a height to a cell row.
const fn half_cell(cell: i32) -> i32 {
    cell / 2
}

/// The cell row (cell index along Y, in the level's grid) that holds the surface block at
/// `height`: `height / 2^level` rounded to the nearest integer, a tie rounding up. As
/// integer arithmetic that is `floor((height + 2^level / 2) / 2^level)`, which is also
/// right for negative heights (`div_euclid` floors, so -3 at level 2 gives -1, not 0).
/// At level 0 it is `height` itself.
fn surface_cell(height: i32, level: u8) -> i32 {
    let cell = cell_size(level);
    height.saturating_add(half_cell(cell)).div_euclid(cell)
}

/// Generates the terrain of the tile `tile` as a chunk of cells. The same arguments always
/// give the same chunk, and level 0 equals [`generate_chunk`](crate::generate_chunk).
///
/// A cell column samples the surface height `h` **once**, at its centre block: the block
/// `2^level / 2` blocks in from the cell's lowest corner on X and Z (the block just above
/// the middle, since a cell has an even number of blocks; at level 0 that is the block
/// itself). The cells are solid up to and including row `surface_cell(h)` (see that
/// function: `h / 2^level` rounded to nearest, ties up, floor-based for negative heights).
/// Which block a solid cell holds follows the full-detail column:
///
/// - the top solid cell is `blocks.grass`;
/// - a cell below it takes the block the full-detail column has at the cell's highest
///   block, but at most one below the surface (so a cell never shows grass under the top),
///   by the same layer rule as the generator (`blocks.dirt` within the dirt depth,
///   `blocks.stone` deeper). The dirt layer is 3 blocks, thinner than a cell at coarse
///   levels, so the cell under the grass is dirt in most columns at every level (stone in
///   the columns where it is more than 3 blocks below the surface) and deeper cells are stone;
/// - the cell row that holds [`WORLD_FLOOR_Y`] and every row below it is `blocks.bedrock`;
/// - everything above the top solid cell is air.
///
/// The result is correct for any tile layer, the way [`generate_chunk`](crate::generate_chunk)
/// is: a layer above every surface is all air, a layer below the floor all bedrock, so a
/// worker can ask for any layer without first checking the world bounds.
///
/// The cost is 1,024 noise samples and one fill, about that of one chunk.
///
/// # Example
///
/// ```
/// use vox_world::{generate_lod_chunk, BlockId, ChunkPos, LocalPos, TerrainBlocks, TilePos};
///
/// let blocks = TerrainBlocks {
///     grass: BlockId(1),
///     dirt: BlockId(2),
///     stone: BlockId(3),
///     bedrock: BlockId(4),
/// };
/// // Level 2: cells of 4 blocks, so this tile covers 128 x 128 x 128 blocks.
/// let tile = generate_lod_chunk(1, TilePos::new(2, ChunkPos::ORIGIN), &blocks);
/// assert!(LocalPos::all().any(|pos| tile.get(pos) == blocks.grass));
/// ```
pub fn generate_lod_chunk(seed: u32, tile: TilePos, blocks: &TerrainBlocks) -> Chunk {
    let mut chunk = Chunk::new();
    let level = tile.level();
    let cell = cell_size(level);
    let [origin_x, _, origin_z] = tile.origin();
    let base_row = i64::from(tile.pos().y) * CHUNK_SIZE_I64;
    let floor_row = i64::from(WORLD_FLOOR_Y).div_euclid(i64::from(cell));
    for z in 0..CHUNK_SIZE {
        for x in 0..CHUNK_SIZE {
            // Saturating like `generate_chunk`, so a far tile degrades without wrapping.
            let centre_x = origin_x.saturating_add(cell_offset(x, cell));
            let centre_z = origin_z.saturating_add(cell_offset(z, cell));
            let height = surface_height(seed, centre_x, centre_z);
            let column = CellColumn {
                base_row,
                top_row: i64::from(surface_cell(height, level)),
                floor_row,
                height,
                cell,
            };
            fill_cell_column(&mut chunk, x, z, &column, blocks);
        }
    }
    chunk
}

/// Blocks from a tile's edge to the centre block of its cell number `index`: the cell's
/// start plus half a cell.
fn cell_offset(index: usize, cell: i32) -> i32 {
    // `index < 32` and `cell <= 16`, so the product is far inside `i32`.
    index as i32 * cell + half_cell(cell)
}

/// What [`fill_cell_column`] needs to know about one column of cells.
struct CellColumn {
    /// Cell row of the tile's lowest cell row.
    base_row: i64,
    /// Cell row of the surface cell (the grass).
    top_row: i64,
    /// Cell row that holds the world floor.
    floor_row: i64,
    /// Surface height in blocks, sampled at the column's centre.
    height: i32,
    /// Cell size in blocks.
    cell: i32,
}

/// Writes the solid cells of one column: rows from the tile's bottom up to the surface cell
/// or the tile's top, whichever is lower. Rows above are left as air.
fn fill_cell_column(
    chunk: &mut Chunk,
    x: usize,
    z: usize,
    column: &CellColumn,
    blocks: &TerrainBlocks,
) {
    let highest = (column.top_row - column.base_row).min(CHUNK_SIZE_I64 - 1);
    if highest < 0 {
        return;
    }
    for local in 0..=highest {
        let block = cell_block(column.base_row + local, column, blocks);
        // `local` is in `0..32` and `x`, `z` come from the tile loops, so `new` cannot fail;
        // the library never panics, so the impossible case is skipped.
        if let Some(pos) = LocalPos::new(x, local as usize, z) {
            chunk.set(pos, block);
        }
    }
}

/// The block of the solid cell at `row` (at or below the surface cell), see
/// [`generate_lod_chunk`] for the rules.
fn cell_block(row: i64, column: &CellColumn, blocks: &TerrainBlocks) -> BlockId {
    if row <= column.floor_row {
        return blocks.bedrock;
    }
    if row == column.top_row {
        return blocks.grass;
    }
    let cell = i64::from(column.cell);
    let highest_block = row * cell + (cell - 1);
    // At most one below the surface: a cell under the top is never grass.
    let sampled = highest_block.min(i64::from(column.height) - 1);
    // At least 1 because `sampled <= height - 1`; the conversion cannot fail for heights and
    // rows near the world, and the fallback (a very deep cell: stone) is only a guard.
    let depth = i32::try_from(i64::from(column.height) - sampled).unwrap_or(i32::MAX);
    block_at_depth(depth, blocks)
}

/// A chunk made of `block` only. The stand-in below a tile (use the bedrock block) so the
/// underside of the terrain, which nobody can see, produces no faces.
pub fn solid_chunk(block: BlockId) -> Chunk {
    let mut chunk = Chunk::new();
    for pos in LocalPos::all() {
        chunk.set(pos, block);
    }
    chunk
}

/// Meshes a tile **alone**: no horizontal neighbours, so every side of the tile that holds
/// solid cells becomes a wall down to the bottom (the skirts), and `below` stands in for
/// the layer under it (see [`solid_chunk`]). A tile mesh therefore depends only on the tile,
/// never on the tiles beside it or their levels. Replaces the contents of `mesh`.
pub fn build_tile_mesh_into(
    tile: &Chunk,
    below: &Chunk,
    textures: &FaceTextures,
    mesh: &mut ChunkMesh,
) {
    let mut around = [None; Face::COUNT];
    around[usize::from(Face::NegY.index())] = Some(below);
    build_mesh_with_neighbours_into(tile, &Neighbours::new(around), textures, mesh);
}

/// Vertex coordinate of the far border of a tile: `CHUNK_SIZE`, which fits the `u8` of
/// [`ChunkVertex`](crate::ChunkVertex) positions (checked below).
const BORDER: u8 = CHUNK_SIZE as u8;
const _: () = assert!(CHUNK_SIZE <= u8::MAX as usize);

/// How many quads of a tile mesh are skirts: sideways quads (`+-X`, `+-Z`) that lie on the
/// tile's own border plane, the walls that close the crack to a neighbour of another level.
/// A tile meshed with [`build_tile_mesh_into`] has them on every side where it is solid.
pub fn skirt_quads(mesh: &ChunkMesh) -> usize {
    let border = BORDER;
    mesh.vertices
        .chunks_exact(VERTICES_PER_QUAD)
        .filter(|quad| {
            let [x, _, z] = quad[0].position;
            let face = quad[0].face;
            (face == Face::NegX.index() && x == 0)
                || (face == Face::PosX.index() && x == border)
                || (face == Face::NegZ.index() && z == 0)
                || (face == Face::PosZ.index() && z == border)
        })
        .count()
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;
    use crate::bounds::{HIGHEST_CHUNK_LAYER, LOWEST_CHUNK_LAYER};
    use crate::mesh::identity_textures;
    use crate::terrain::{TEST_BLOCKS, generate_chunk};

    const GRASS: BlockId = TEST_BLOCKS.grass;
    const DIRT: BlockId = TEST_BLOCKS.dirt;
    const STONE: BlockId = TEST_BLOCKS.stone;
    const BEDROCK: BlockId = TEST_BLOCKS.bedrock;

    fn lod(seed: u32, level: u8, x: i32, y: i32, z: i32) -> Chunk {
        generate_lod_chunk(
            seed,
            TilePos::new(level, ChunkPos::new(x, y, z)),
            &TEST_BLOCKS,
        )
    }

    fn at(chunk: &Chunk, x: usize, y: usize, z: usize) -> BlockId {
        LocalPos::new(x, y, z).map_or(BlockId::AIR, |pos| chunk.get(pos))
    }

    fn same(a: &Chunk, b: &Chunk) -> bool {
        LocalPos::all().all(|pos| a.get(pos) == b.get(pos))
    }

    /// Row of the topmost non-air cell of a column, or `None` for an empty column.
    fn top_row(chunk: &Chunk, x: usize, z: usize) -> Option<usize> {
        (0..CHUNK_SIZE)
            .rev()
            .find(|&y| !at(chunk, x, y, z).is_air())
    }

    #[test]
    fn level_zero_equals_generate_chunk() {
        for seed in [0, 1, 2, 99, 0xDEAD_BEEF] {
            for layer in [
                LOWEST_CHUNK_LAYER - 1,
                LOWEST_CHUNK_LAYER,
                LOWEST_CHUNK_LAYER + 1,
                -1,
                0,
                1,
                HIGHEST_CHUNK_LAYER,
                HIGHEST_CHUNK_LAYER + 1,
            ] {
                for (x, z) in [(0, 0), (-1, 0), (3, -2), (-17, 40), (1000, -1000)] {
                    let pos = ChunkPos::new(x, layer, z);
                    let expected = generate_chunk(seed, pos, &TEST_BLOCKS);
                    let tile = generate_lod_chunk(seed, TilePos::chunk(pos), &TEST_BLOCKS);
                    assert!(same(&expected, &tile), "seed {seed}, chunk {pos:?}");
                }
            }
        }
    }

    #[test]
    fn cell_size_is_a_power_of_two_and_clamped() {
        assert_eq!(cell_size(0), 1);
        assert_eq!(cell_size(1), 2);
        assert_eq!(cell_size(MAX_LOD_LEVEL), 16);
        assert_eq!(cell_size(MAX_LOD_LEVEL + 1), 16);
        assert_eq!(cell_size(u8::MAX), 16);
    }

    #[test]
    fn tile_origin_and_extent_follow_the_level() {
        let tile = TilePos::new(2, ChunkPos::new(1, -1, 3));
        assert_eq!(tile.extent(), 128);
        assert_eq!(tile.origin(), [128, -128, 384]);
        assert_eq!(
            TilePos::chunk(ChunkPos::new(1, 2, 3)).origin(),
            [32, 64, 96]
        );
        assert_eq!(TilePos::new(4, ChunkPos::ORIGIN).extent(), 512);
    }

    #[test]
    fn tile_origin_saturates() {
        let far = TilePos::new(4, ChunkPos::new(i32::MAX, i32::MIN, i32::MAX));
        assert_eq!(far.origin(), [i32::MAX, i32::MIN, i32::MAX]);
    }

    #[test]
    fn surface_cell_rounds_to_nearest_with_ties_up() {
        // Hand-computed: level 2 has cells of 4, so heights 8..=9 are row 2, 10..=13 row 3.
        for (height, row) in [(8, 2), (9, 2), (10, 3), (11, 3), (12, 3), (13, 3), (14, 4)] {
            assert_eq!(surface_cell(height, 2), row, "height {height}");
        }
        // Level 1: cells of 2, ties (odd heights) round up.
        for (height, row) in [(8, 4), (9, 5), (10, 5), (11, 6)] {
            assert_eq!(surface_cell(height, 1), row, "height {height}");
        }
        // Level 4: cells of 16; 8 is exactly the tie, 7 is below it.
        assert_eq!(surface_cell(7, 4), 0);
        assert_eq!(surface_cell(8, 4), 1);
        assert_eq!(surface_cell(23, 4), 1);
        assert_eq!(surface_cell(24, 4), 2);
        // Level 0 is the identity, negative heights included.
        for height in [-513, -1, 0, 24] {
            assert_eq!(surface_cell(height, 0), height);
        }
    }

    #[test]
    fn surface_cell_rounds_negative_heights_down_towards_the_nearer_row() {
        // Level 2: -2 is the tie between rows -1 (-4) and 0 (0), and ties go up.
        for (height, row) in [(-1, 0), (-2, 0), (-3, -1), (-5, -1), (-6, -1), (-7, -2)] {
            assert_eq!(surface_cell(height, 2), row, "height {height}");
        }
        for (height, row) in [(-1, 0), (-2, -1), (-3, -1), (-4, -2), (-5, -2)] {
            assert_eq!(surface_cell(height, 1), row, "height {height}");
        }
    }

    #[test]
    fn surface_cell_is_within_half_a_cell_of_the_height() {
        for level in 0..=MAX_LOD_LEVEL {
            let cell = cell_size(level);
            for height in -600..600 {
                let blocks_from_row = i64::from(surface_cell(height, level)) * i64::from(cell);
                let error = (blocks_from_row - i64::from(height)).abs();
                assert!(
                    error <= i64::from(cell) / 2,
                    "level {level}, height {height}"
                );
            }
        }
    }

    /// Height of the surface at world column `(x, z)` the way the generator samples it, but
    /// from a literal world coordinate (never from the tile code under test).
    fn height_at(seed: u32, x: i32, z: i32) -> i32 {
        surface_height(seed, x, z)
    }

    #[test]
    fn cell_columns_sample_the_centre_block_of_the_cell() {
        // Level 2, tile (1, 0, -1): its origin is block (128, 0, -128), a cell is 4 blocks,
        // so cell (5, ., 7) has its centre block at x = 128 + 5*4 + 2 = 150 and
        // z = -128 + 7*4 + 2 = -98.
        let seed = 7;
        let tile = lod(seed, 2, 1, 0, -1);
        let height = height_at(seed, 150, -98);
        let expected_top = usize::try_from(surface_cell(height, 2)).unwrap();
        assert_eq!(top_row(&tile, 5, 7), Some(expected_top));
        assert_eq!(at(&tile, 5, expected_top, 7), GRASS);
        // And the corner cells: (0, 0) centres at (130, -126), (31, 31) at (254, -2).
        for ((x, z), (wx, wz)) in [((0, 0), (130, -126)), ((31, 31), (254, -2))] {
            let row = surface_cell(height_at(seed, wx, wz), 2) as usize;
            assert_eq!(top_row(&tile, x, z), Some(row), "cell ({x}, {z})");
        }
    }

    #[test]
    fn surface_cell_is_grass_and_cells_below_are_dirt_or_stone() {
        for level in 1..=MAX_LOD_LEVEL {
            let tile = lod(3, level, -2, 0, 5);
            let mut dirt = 0;
            for z in 0..CHUNK_SIZE {
                for x in 0..CHUNK_SIZE {
                    let top = top_row(&tile, x, z).expect("every column has a surface");
                    assert_eq!(at(&tile, x, top, z), GRASS);
                    for y in 0..top {
                        let block = at(&tile, x, y, z);
                        assert!(
                            block == DIRT || block == STONE,
                            "level {level} ({x},{y},{z})"
                        );
                        dirt += usize::from(block == DIRT);
                    }
                }
            }
            // The dirt layer is 3 blocks: the cell under the top is dirt in most columns.
            assert!(dirt > 0, "level {level}");
        }
    }

    #[test]
    fn cells_below_the_surface_follow_the_block_rule_at_their_top_block() {
        // Independent of `block_at_depth`: dirt if the cell's highest block (capped one
        // below the surface) is within 3 blocks of the surface, else stone.
        let seed = 5;
        for level in 1..=MAX_LOD_LEVEL {
            let cell = cell_size(level);
            let tile = lod(seed, level, 0, 0, 0);
            for z in 0..CHUNK_SIZE {
                for x in 0..CHUNK_SIZE {
                    let wx = x as i32 * cell + cell / 2;
                    let wz = z as i32 * cell + cell / 2;
                    let height = height_at(seed, wx, wz);
                    let top = top_row(&tile, x, z).unwrap();
                    for y in 0..top {
                        let highest = (y as i32 * cell + cell - 1).min(height - 1);
                        let expected = if height - highest <= 3 { DIRT } else { STONE };
                        assert_eq!(at(&tile, x, y, z), expected, "level {level} ({x},{y},{z})");
                    }
                }
            }
        }
    }

    /// The block of cell row `row` of a column with surface height `height` at `level`.
    fn column_block(height: i32, level: u8, row: i64) -> BlockId {
        let column = CellColumn {
            base_row: 0,
            top_row: i64::from(surface_cell(height, level)),
            floor_row: WORLD_FLOOR_Y.div_euclid(cell_size(level)).into(),
            height,
            cell: cell_size(level),
        };
        cell_block(row, &column, &TEST_BLOCKS)
    }

    #[test]
    fn hand_computed_columns() {
        // Height 21, level 2 (cells of 4): the surface row is (21 + 2) / 4 = 5. Row 4 has its
        // highest block at 19, 2 below the surface: dirt. Row 3: block 15, 6 below: stone.
        assert_eq!(column_block(21, 2, 5), GRASS);
        assert_eq!(column_block(21, 2, 4), DIRT);
        assert_eq!(column_block(21, 2, 3), STONE);
        // Height 9, level 1 (cells of 2): surface row (9 + 1) / 2 = 5, whose cell reaches
        // block 11. Row 4 reaches block 9 = the surface, capped to 8 (never grass under the
        // top): dirt. Row 3: block 7, 2 below: dirt. Row 2: block 5, 4 below: stone.
        assert_eq!(column_block(9, 1, 5), GRASS);
        assert_eq!(column_block(9, 1, 4), DIRT);
        assert_eq!(column_block(9, 1, 3), DIRT);
        assert_eq!(column_block(9, 1, 2), STONE);
        // Level 0 is the plain layer rule: height 9, rows 8..=6 dirt, 5 stone.
        assert_eq!(column_block(9, 0, 9), GRASS);
        assert_eq!(column_block(9, 0, 6), DIRT);
        assert_eq!(column_block(9, 0, 5), STONE);
    }

    #[test]
    fn tiles_are_fixed_to_the_world_grid() {
        let seed = 11;
        // Level 1: tile (1, 0, 0) cell (0, 3) has its centre block at x = 64 + 1,
        // z = 3*2 + 1; tile (-1, 0, 0) cell (31, 0) at x = -64 + 62 + 1 = -1, z = 1.
        let east = lod(seed, 1, 1, 0, 0);
        let west = lod(seed, 1, -1, 0, 0);
        assert_eq!(
            top_row(&east, 0, 3),
            Some(surface_cell(height_at(seed, 65, 7), 1) as usize)
        );
        assert_eq!(
            top_row(&west, 31, 0),
            Some(surface_cell(height_at(seed, -1, 1), 1) as usize)
        );
    }

    #[test]
    fn border_cells_stay_close_to_the_full_detail_heights() {
        // A level-1 cell covers two block columns; its row times 2 is within one block of
        // the height at its centre (rounding), and adjacent block columns of the terrain
        // differ by at most 3 (seam bound in `terrain`), so it stays within 4 of both.
        const BOUND: i32 = 4;
        let seed = 2;
        let tile = lod(seed, 1, 0, 0, 0);
        for index in 0..CHUNK_SIZE {
            for (cx, cz) in [
                (index, 0),
                (index, CHUNK_SIZE - 1),
                (0, index),
                (CHUNK_SIZE - 1, index),
            ] {
                let row = top_row(&tile, cx, cz).unwrap() as i32;
                for dx in 0..2 {
                    for dz in 0..2 {
                        let full = height_at(seed, cx as i32 * 2 + dx, cz as i32 * 2 + dz);
                        assert!((row * 2 - full).abs() <= BOUND, "cell ({cx}, {cz})");
                    }
                }
            }
        }
    }

    #[test]
    fn the_floor_row_cell_is_bedrock_at_every_level() {
        for level in 0..=MAX_LOD_LEVEL {
            let tile_layer =
                WORLD_FLOOR_Y.div_euclid(TilePos::new(level, ChunkPos::ORIGIN).extent());
            let tile = lod(1, level, 4, tile_layer, -3);
            for z in 0..CHUNK_SIZE {
                for x in 0..CHUNK_SIZE {
                    assert_eq!(at(&tile, x, 0, z), BEDROCK, "level {level}");
                    assert_eq!(at(&tile, x, 1, z), STONE, "level {level}");
                    assert_eq!(at(&tile, x, CHUNK_SIZE - 1, z), STONE, "level {level}");
                }
            }
        }
    }

    #[test]
    fn layers_below_the_floor_are_bedrock_and_above_the_surface_are_air() {
        for level in 0..=MAX_LOD_LEVEL {
            let extent = TilePos::new(level, ChunkPos::ORIGIN).extent();
            let floor_layer = WORLD_FLOOR_Y.div_euclid(extent);
            let below = lod(1, level, 0, floor_layer - 1, 0);
            assert!(LocalPos::all().all(|pos| below.get(pos) == BEDROCK));
            // The first layer that starts above the highest possible surface.
            let ceiling_layer = (crate::terrain::MAX_SURFACE as i32).div_euclid(extent) + 1;
            for layer in [ceiling_layer, ceiling_layer + 7, i32::MAX] {
                let above = lod(1, level, 0, layer, 0);
                assert!(
                    LocalPos::all().all(|pos| above.get(pos).is_air()),
                    "level {level}"
                );
            }
        }
    }

    #[test]
    fn layers_between_the_floor_and_the_surface_are_solid() {
        // Level 1, tile layer -1 spans blocks -64..0: all stone (floor is at -512).
        let tile = lod(1, 1, 0, -1, 0);
        assert!(LocalPos::all().all(|pos| tile.get(pos) == STONE));
    }

    #[test]
    fn extreme_positions_do_not_panic() {
        let extremes = [i32::MIN, i32::MIN + 1, -1, 0, 1, i32::MAX - 1, i32::MAX];
        for level in [0, 1, 3, MAX_LOD_LEVEL, MAX_LOD_LEVEL + 1, u8::MAX] {
            for x in extremes {
                for y in extremes {
                    for z in [i32::MIN, 0, i32::MAX] {
                        lod(1, level, x, y, z);
                    }
                }
            }
        }
    }

    #[test]
    fn generation_is_deterministic_and_depends_on_the_seed() {
        assert!(same(&lod(7, 2, 3, 0, -4), &lod(7, 2, 3, 0, -4)));
        assert!(!same(&lod(1, 2, 3, 0, -4), &lod(2, 2, 3, 0, -4)));
        // Neighbouring tiles differ (the terrain is not flat across 128 blocks).
        assert!(!same(&lod(1, 2, 0, 0, 0), &lod(1, 2, 1, 0, 0)));
    }

    /// Meshes a tile alone with the solid stand-in below, as a worker would.
    fn mesh_chunk(tile: &Chunk) -> ChunkMesh {
        let below = solid_chunk(BEDROCK);
        let mut mesh = ChunkMesh::default();
        build_tile_mesh_into(tile, &below, &identity_textures(), &mut mesh);
        mesh
    }

    fn mesh_tile(level: u8) -> ChunkMesh {
        mesh_chunk(&lod(1, level, 0, 0, 0))
    }

    const SIDES: [Face; 4] = [Face::PosX, Face::NegX, Face::PosZ, Face::NegZ];

    /// The cell column `(x, z)` that lies along border `side` at position `index`.
    fn border_column(side: Face, index: usize) -> (usize, usize) {
        match side {
            Face::NegX => (0, index),
            Face::PosX => (CHUNK_SIZE - 1, index),
            Face::NegZ => (index, 0),
            _ => (index, CHUNK_SIZE - 1),
        }
    }

    #[test]
    fn border_plane_quads_cover_every_border_column_from_row_zero_to_its_top() {
        for level in 1..=MAX_LOD_LEVEL {
            let tile = lod(1, level, 0, 0, 0);
            let mesh = mesh_tile(level);
            for side in SIDES {
                let plane = if matches!(side, Face::PosX | Face::PosZ) {
                    BORDER
                } else {
                    0
                };
                // Which unit squares of the border plane (position along it, row) are covered.
                let mut covered = HashSet::new();
                for quad in mesh.vertices.chunks_exact(VERTICES_PER_QUAD) {
                    let along_x = matches!(side, Face::PosX | Face::NegX);
                    let normal_axis = if along_x { 0 } else { 2 };
                    let along_axis = 2 - normal_axis;
                    if quad[0].face != side.index() || quad[0].position[normal_axis] != plane {
                        continue;
                    }
                    let along = quad.iter().map(|v| usize::from(v.position[along_axis]));
                    let rows = quad.iter().map(|v| usize::from(v.position[1]));
                    for index in along.clone().min().unwrap()..along.max().unwrap() {
                        for row in rows.clone().min().unwrap()..rows.clone().max().unwrap() {
                            covered.insert((index, row));
                        }
                    }
                }
                for index in 0..CHUNK_SIZE {
                    let (x, z) = border_column(side, index);
                    let height = top_row(&tile, x, z).unwrap() + 1;
                    for row in 0..CHUNK_SIZE {
                        assert_eq!(
                            covered.contains(&(index, row)),
                            row < height,
                            "level {level}, {side:?}, column {index}, row {row}"
                        );
                    }
                }
            }
            // The solid stand-in below leaves no bottom faces.
            assert_eq!(mesh.face_quads[usize::from(Face::NegY.index())], 0);
        }
    }

    #[test]
    fn skirt_quads_counts_only_quads_on_the_border_planes() {
        let single = |x, z| {
            let mut chunk = Chunk::new();
            chunk.set(LocalPos::new(x, 0, z).unwrap(), STONE);
            skirt_quads(&crate::mesh::build_mesh(&chunk, &identity_textures()))
        };
        // A lone block in a corner touches two border planes, the middle none.
        assert_eq!(single(0, 0), 2);
        assert_eq!(single(CHUNK_SIZE - 1, CHUNK_SIZE - 1), 2);
        assert_eq!(single(5, 5), 0);
    }

    #[test]
    fn skirts_are_part_of_the_side_groups() {
        let mesh = mesh_tile(2);
        let skirts = skirt_quads(&mesh);
        let sideways: u32 = SIDES
            .iter()
            .map(|face| mesh.face_quads[usize::from(face.index())])
            .sum();
        assert!(skirts > 0);
        assert!(skirts <= sideways as usize);
        assert!(skirts < mesh.quad_count());
    }

    /// Decision 0015 keeps neighbour-blind skirts while they are under this share of a
    /// tile's quads (levels 1 to 3).
    const SKIRT_SHARE_LIMIT: f64 = 0.40;

    /// A level-4 tile is nearly flat (cells of 16 blocks against hills of 16 blocks), so its
    /// few quads are mostly walls and the share says nothing; it is judged by its absolute
    /// cost instead. The largest tile over the seeds and the 3 x 3 tiles tested has 45 quads,
    /// far below the 760 quads of a full-detail chunk; this bound is a round number above
    /// twice that.
    const LEVEL_4_MAX_QUADS: usize = 100;

    /// The seeds of the share test.
    const SHARE_SEEDS: [u32; 5] = [1, 2, 3, 17, 99];

    #[test]
    fn skirt_share_stays_under_the_limit_at_levels_1_to_3_and_level_4_is_cheap() {
        for seed in SHARE_SEEDS {
            for level in 1..=MAX_LOD_LEVEL {
                for x in -1..=1 {
                    for z in -1..=1 {
                        let mesh = mesh_chunk(&lod(seed, level, x, 0, z));
                        let quads = mesh.quad_count();
                        let share = skirt_quads(&mesh) as f64 / quads as f64;
                        println!("seed {seed} level {level} ({x},{z}): {quads} quads, {share:.3}");
                        if level < MAX_LOD_LEVEL {
                            assert!(share < SKIRT_SHARE_LIMIT, "seed {seed}, level {level}");
                        } else {
                            assert!(quads < LEVEL_4_MAX_QUADS, "seed {seed}");
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn solid_chunk_is_all_one_block() {
        let chunk = solid_chunk(BEDROCK);
        assert!(LocalPos::all().all(|pos| chunk.get(pos) == BEDROCK));
    }

    #[test]
    fn a_level_above_the_maximum_is_the_same_tile_as_the_maximum() {
        let pos = ChunkPos::new(1, 0, -2);
        assert_eq!(
            TilePos::new(MAX_LOD_LEVEL + 1, pos),
            TilePos::new(MAX_LOD_LEVEL, pos)
        );
        assert_eq!(TilePos::new(u8::MAX, pos).level(), MAX_LOD_LEVEL);
        assert_eq!(TilePos::chunk(pos).level(), 0);
        assert_eq!(TilePos::chunk(pos).pos(), pos);
    }
}
