//! Terrain: turns the landscape (heights, water, biomes) and the trees into chunks, and
//! tells streaming which layers of a column of chunks can hold anything to see.
//!
//! The shape itself lives in [`crate::landscape`] and the trees in [`crate::trees`]; this
//! module writes blocks. Everything is a pure function of the seed and the world position,
//! so a seed produces the same chunk on every platform and chunks next to each other agree.

use std::ops::RangeInclusive;

use crate::block::BlockId;
use crate::bounds::{
    CHUNK_SIZE_I32, HIGHEST_CHUNK_LAYER, LOWEST_CHUNK_LAYER, WORLD_CEILING_Y, WORLD_FLOOR_Y,
    chunk_layer_of,
};
use crate::chunk::{CHUNK_SIZE, Chunk, LocalPos};
use crate::chunk_pos::ChunkPos;
use crate::landscape::{ColumnInfo, FILLER_DEPTH, Landscape, SAND_DEPTH, SNOW_LINE, spawn_point};
pub use crate::landscape::{MAX_SURFACE, MIN_SURFACE, SEA_LEVEL};
use crate::lod::solid_chunk;
use crate::trees::{
    TREE_MARGIN, TREE_MAX_HEIGHT, Tree, may_hold_trees, stamp, top_with_tree, trees_around,
};

/// The blocks the terrain generator places, as ids.
///
/// The generator names no content: whoever owns the [`BlockRegistry`](crate::BlockRegistry)
/// looks the blocks up by name and passes their ids in (decision 0013).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TerrainBlocks {
    /// The top block of grassy ground (plains and forests).
    pub grass: BlockId,
    /// The layer right below the grass.
    pub dirt: BlockId,
    /// Everything below the filler layer, and the surface of mountains.
    pub stone: BlockId,
    /// The surface of beaches and deserts, and of shallow sea, river and lake floors.
    pub sand: BlockId,
    /// The floor of the deep sea.
    pub gravel: BlockId,
    /// The cover of the highest peaks.
    pub snow: BlockId,
    /// Fills the air under the sea level, rivers and lakes.
    pub water: BlockId,
    /// The trunks of trees.
    pub log: BlockId,
    /// The crowns of trees.
    pub leaves: BlockId,
    /// The row at [`WORLD_FLOOR_Y`] (and everything below it, which is never meshed).
    pub bedrock: BlockId,
}

/// Ids 1 to 9 in the order of the fields and 10 for bedrock (a test-only choice: the game's
/// ids come from its blocks file), for tests in this crate.
#[cfg(test)]
pub(crate) const TEST_BLOCKS: TerrainBlocks = TerrainBlocks {
    grass: BlockId(1),
    dirt: BlockId(2),
    stone: BlockId(3),
    sand: BlockId(4),
    gravel: BlockId(5),
    snow: BlockId(6),
    water: BlockId(7),
    log: BlockId(8),
    leaves: BlockId(9),
    bedrock: BlockId(10),
};

/// Highest block that is stone in every column: the lowest ground minus the filler layer
/// minus one. A chunk whose top is at or below it is all stone, without any noise.
const ALWAYS_STONE_Y: i32 = MIN_SURFACE - FILLER_DEPTH - 1;

/// Highest block that can be anything but air: the highest ground plus the tallest tree.
/// Water never rises that high. A chunk above it is all air, without any noise.
const HIGHEST_BLOCK_Y: i32 = MAX_SURFACE + TREE_MAX_HEIGHT;

const _: () = assert!(HIGHEST_BLOCK_Y < WORLD_CEILING_Y);

/// The blocks of the layers of one column: what is on top, what is under it, and the stone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Layers {
    /// The block at the ground height.
    pub surface: BlockId,
    /// The [`FILLER_DEPTH`] blocks below the surface.
    pub filler: BlockId,
    /// Everything deeper.
    pub stone: BlockId,
}

impl Layers {
    /// The layers of a column: grass over dirt on grassland, sand on beaches, deserts and
    /// shallow water, gravel on the deep sea floor, stone (snow-capped on the peaks) on
    /// mountains.
    pub(crate) fn of(info: &ColumnInfo, blocks: &TerrainBlocks) -> Self {
        use crate::landscape::Biome;
        let stone = blocks.stone;
        let (surface, filler) = match info.biome {
            Biome::Ocean | Biome::River | Biome::Lake => {
                let floor = if info.ground >= SEA_LEVEL - SAND_DEPTH {
                    blocks.sand
                } else {
                    blocks.gravel
                };
                (floor, floor)
            }
            Biome::Beach | Biome::Desert => (blocks.sand, blocks.sand),
            Biome::Plains | Biome::Forest => (blocks.grass, blocks.dirt),
            Biome::Mountains if info.ground >= SNOW_LINE => (blocks.snow, stone),
            Biome::Mountains => (stone, stone),
        };
        Self {
            surface,
            filler,
            stone,
        }
    }
}

/// The block `depth` blocks below the surface of a column (`depth == 0` is the surface block,
/// a negative depth is above it: air). This is the layer rule of [`fill_column`] for one
/// block, for callers that classify single blocks (level-of-detail cells); a test checks
/// that both agree.
pub(crate) const fn block_at_depth(depth: i32, layers: &Layers) -> BlockId {
    if depth < 0 {
        BlockId::AIR
    } else if depth == 0 {
        layers.surface
    } else if depth <= FILLER_DEPTH {
        layers.filler
    } else {
        layers.stone
    }
}

/// A chunk with a surface in it: the chunk that holds the [`spawn_point`] of `seed`. A
/// representative chunk for examples, benchmarks and tests; the same `seed` always gives the
/// same chunk. It is [`generate_chunk`] there.
///
/// # Example
///
/// ```
/// use vox_world::{generate_terrain, BlockId, LocalPos, TerrainBlocks};
///
/// let blocks = TerrainBlocks {
///     grass: BlockId(1),
///     dirt: BlockId(2),
///     stone: BlockId(3),
///     sand: BlockId(4),
///     gravel: BlockId(5),
///     snow: BlockId(6),
///     water: BlockId(7),
///     log: BlockId(8),
///     leaves: BlockId(9),
///     bedrock: BlockId(10),
/// };
/// let chunk = generate_terrain(1, &blocks);
/// let has_grass = LocalPos::all().any(|pos| chunk.get(pos) == blocks.grass);
/// assert!(has_grass);
/// ```
pub fn generate_terrain(seed: u32, blocks: &TerrainBlocks) -> Chunk {
    let (pos, _) = ChunkPos::containing(spawn_point(seed));
    generate_chunk(seed, pos, blocks)
}

/// Generates the terrain of the chunk at `pos`, trees included. The same `seed` and `pos`
/// always give the same chunk.
///
/// The ground height, the water and the biome of a column depend only on its world column
/// `(x, z)`, never on the chunk, and a tree is placed from hashes around the chunk, so
/// neighbouring chunks agree along their borders and everything continues across them. For a
/// block at world height `y` of a column with ground height `h` and water level `w`: above
/// `max(h, w)` is air (or a tree), above `h` up to `w` is `blocks.water`, `y == h` is the
/// biome's surface block, the three blocks below are its filler and everything deeper is
/// `blocks.stone`, with `blocks.bedrock` in the row at [`WORLD_FLOOR_Y`].
///
/// Consequently every chunk above the highest mountain and tree is all air and every chunk
/// below the lowest sea floor is all stone (bedrock in the floor row), and both are made
/// without sampling any noise.
///
/// Outside the world's layers ([`contains_chunk_layer`](crate::contains_chunk_layer)) the
/// answer is a stand-in, so the function stays total: above the highest layer the chunk is
/// all air, below the lowest layer it is all `blocks.bedrock`. A mesh of the lowest layer that
/// is given a chunk of this kind below it therefore has no bottom faces. Nothing is ever
/// stored there.
///
/// # Example
///
/// ```
/// use vox_world::{generate_chunk, BlockId, ChunkPos, LocalPos, TerrainBlocks};
///
/// let blocks = TerrainBlocks {
///     grass: BlockId(1),
///     dirt: BlockId(2),
///     stone: BlockId(3),
///     sand: BlockId(4),
///     gravel: BlockId(5),
///     snow: BlockId(6),
///     water: BlockId(7),
///     log: BlockId(8),
///     leaves: BlockId(9),
///     bedrock: BlockId(10),
/// };
/// // Layer -1 is all stone under the whole world.
/// let chunk = generate_chunk(1, ChunkPos::new(-1, -1, 2), &blocks);
/// assert!(LocalPos::all().all(|pos| chunk.get(pos) == blocks.stone));
/// ```
pub fn generate_chunk(seed: u32, pos: ChunkPos, blocks: &TerrainBlocks) -> Chunk {
    generate(seed, pos, blocks, true)
}

/// [`generate_chunk`] without the trees: the terrain a level-of-detail tile of level 0 shows.
#[cfg(test)]
pub(crate) fn generate_chunk_without_trees(
    seed: u32,
    pos: ChunkPos,
    blocks: &TerrainBlocks,
) -> Chunk {
    generate(seed, pos, blocks, false)
}

fn generate(seed: u32, pos: ChunkPos, blocks: &TerrainBlocks, with_trees: bool) -> Chunk {
    match shortcut(pos, blocks) {
        Some(chunk) => chunk,
        None => ColumnFill::new(seed, [pos.x, pos.z], blocks, with_trees).chunk(pos.y, blocks),
    }
}

/// Generates the chunks of the layers `layers` of the column of chunks `(cx, cz)`, trees
/// included, lowest layer first: the same chunks as [`generate_chunk`] one by one, but the
/// landscape of the column's 1,024 block columns and its trees are worked out once for all
/// the layers, not once per layer (the streaming of a column does this for every layer of its
/// span and its four neighbours).
///
/// # Example
///
/// ```
/// use vox_world::{generate_chunk, generate_column, BlockId, ChunkPos, LocalPos, TerrainBlocks};
///
/// let blocks = TerrainBlocks {
///     grass: BlockId(1),
///     dirt: BlockId(2),
///     stone: BlockId(3),
///     sand: BlockId(4),
///     gravel: BlockId(5),
///     snow: BlockId(6),
///     water: BlockId(7),
///     log: BlockId(8),
///     leaves: BlockId(9),
///     bedrock: BlockId(10),
/// };
/// let column = generate_column(1, 2, -1, 1..=3, &blocks);
/// let single = generate_chunk(1, ChunkPos::new(2, 2, -1), &blocks);
/// assert!(LocalPos::all().all(|pos| column[1].get(pos) == single.get(pos)));
/// ```
pub fn generate_column(
    seed: u32,
    cx: i32,
    cz: i32,
    layers: RangeInclusive<i32>,
    blocks: &TerrainBlocks,
) -> Vec<Chunk> {
    let mut fill: Option<ColumnFill> = None;
    layers
        .map(|layer| {
            let pos = ChunkPos::new(cx, layer, cz);
            shortcut(pos, blocks).unwrap_or_else(|| {
                fill.get_or_insert_with(|| ColumnFill::new(seed, [cx, cz], blocks, true))
                    .chunk(layer, blocks)
            })
        })
        .collect()
}

/// The chunk at `pos` if it needs no landscape: outside the world, or all air above every
/// mountain and tree, or all stone under every sea floor (the world floor row bedrock).
fn shortcut(pos: ChunkPos, blocks: &TerrainBlocks) -> Option<Chunk> {
    if pos.y > HIGHEST_CHUNK_LAYER {
        return Some(Chunk::new());
    }
    if pos.y < LOWEST_CHUNK_LAYER {
        return Some(solid_chunk(blocks.bedrock));
    }
    let origin_y = pos.origin()[1];
    if origin_y > HIGHEST_BLOCK_Y {
        return Some(Chunk::new());
    }
    if origin_y + CHUNK_SIZE_I32 - 1 <= ALWAYS_STONE_Y {
        let mut chunk = solid_chunk(blocks.stone);
        fill_floor_row(&mut chunk, floor_row_in_chunk(origin_y), blocks.bedrock);
        return Some(chunk);
    }
    None
}

/// The landscape of one column of chunks, evaluated once: what every layer of it is filled from.
struct ColumnFill {
    /// World position of the column's lowest x and z.
    origin: [i32; 2],
    /// The ground, water and layer blocks of the 32 x 32 block columns, `z` rows of `x`.
    columns: Vec<(ColumnInfo, Layers)>,
    /// The trees that reach the column, found when the first layer that can hold one is made.
    trees: Option<Vec<Tree>>,
    landscape: Landscape,
    seed: u32,
    with_trees: bool,
}

impl ColumnFill {
    fn new(seed: u32, column: [i32; 2], blocks: &TerrainBlocks, with_trees: bool) -> Self {
        let origin = column.map(|c| c.saturating_mul(CHUNK_SIZE_I32));
        // The trees of neighbouring columns reach into this one, so the lakes (which decide
        // where trees may stand) are needed a margin further out.
        let margin = if with_trees { TREE_MARGIN } else { 0 };
        // Saturating, like `ChunkPos::origin`: far outside the usable world the terrain
        // degrades but nothing wraps or panics.
        let last = CHUNK_SIZE_I32 - 1;
        let landscape = Landscape::for_area(
            seed,
            origin.map(|o| o.saturating_sub(margin)),
            origin.map(|o| o.saturating_add(last + margin)),
        );
        // HOT PATH: one column evaluation per block column, 1,024 per column of chunks, shared
        // by every layer of it.
        let mut columns = Vec::with_capacity(CHUNK_SIZE * CHUNK_SIZE);
        for z in 0..CHUNK_SIZE {
            for x in 0..CHUNK_SIZE {
                let info = landscape.column(
                    origin[0].saturating_add(x as i32),
                    origin[1].saturating_add(z as i32),
                );
                columns.push((info, Layers::of(&info, blocks)));
            }
        }
        Self {
            origin,
            columns,
            trees: None,
            landscape,
            seed,
            with_trees,
        }
    }

    /// The chunk of layer `layer`. Layers outside the world are never asked for (the shortcuts
    /// answer them).
    fn chunk(&mut self, layer: i32, blocks: &TerrainBlocks) -> Chunk {
        let mut chunk = Chunk::new();
        let origin_y = layer.saturating_mul(CHUNK_SIZE_I32);
        for z in 0..CHUNK_SIZE {
            for x in 0..CHUNK_SIZE {
                let (info, layers) = &self.columns[z * CHUNK_SIZE + x];
                fill_column(&mut chunk, x, z, origin_y, info, layers, blocks.water);
            }
        }
        fill_floor_row(&mut chunk, floor_row_in_chunk(origin_y), blocks.bedrock);
        if self.with_trees && may_hold_trees(origin_y) {
            let (landscape, seed, origin) = (&self.landscape, self.seed, self.origin);
            let trees = self
                .trees
                .get_or_insert_with(|| trees_around(landscape, seed, origin));
            stamp(
                &mut chunk,
                trees,
                [origin[0], origin_y, origin[1]],
                blocks.log,
                blocks.leaves,
            );
        }
        chunk
    }
}

/// The height inside a chunk whose lowest block is at `base_y` of the row at
/// [`WORLD_FLOOR_Y`], or `None` if the chunk does not hold that row.
fn floor_row_in_chunk(base_y: i32) -> Option<usize> {
    let row = WORLD_FLOOR_Y.checked_sub(base_y)?;
    (0..CHUNK_SIZE_I32)
        .contains(&row)
        .then(|| clamp_to_chunk(row))
}

/// Fills the row `row` of every column with `bedrock`, if the chunk holds the world floor.
fn fill_floor_row(chunk: &mut Chunk, row: Option<usize>, bedrock: BlockId) {
    if let Some(row) = row {
        for z in 0..CHUNK_SIZE {
            for x in 0..CHUNK_SIZE {
                fill_range(chunk, x, z, row..row + 1, bedrock);
            }
        }
    }
}

/// How far past the 32 x 32 columns of a chunk column [`column_span`] looks for the lowest
/// height: one block, because a side face is exposed by the neighbour next to it.
const SPAN_BORDER: i32 = 1;

/// The chunk layers (chunk `y` coordinates, inclusive) of the column of chunks `(cx, cz)` that
/// can hold a visible face. Every layer below the range is all non-air (stone, ground and
/// water) and every layer above it is all air, so they have no faces and need not be
/// generated, stored or meshed.
///
/// With `lowest` the lowest top (ground or water surface) among the column's 32 x 32 blocks and
/// the [`SPAN_BORDER`] ring around them, and `highest` the highest top of the 32 x 32 blocks, or
/// the top of a tree that can stand in the 32 x 32 columns widened by the tree margin: a top
/// face sits at a block column's own top, and a side face lies above the lower of two
/// neighbouring tops, so every face is in `lowest..=highest`. The range is the layers of those
/// two heights, clamped to the world ([`contains_chunk_layer`](crate::contains_chunk_layer)).
/// It is derived from the generator's own columns, so it stays right when the landscape
/// changes; it is only valid while terrain is a heightmap (no caves or overhangs). The tree
/// part is a bound, not the trees themselves: wherever a tree may stand (grass) it assumes
/// the tallest one, so a layer may be included although the hash placed no tree in it.
///
/// Costs one column evaluation per block column of the 36 x 36 area, about one chunk generation.
///
/// # Example
///
/// ```
/// use vox_world::column_span;
///
/// let span = column_span(1, 5, -3);
/// assert!(span.start() <= span.end());
/// ```
pub fn column_span(seed: u32, cx: i32, cz: i32) -> RangeInclusive<i32> {
    let origin_x = cx.saturating_mul(CHUNK_SIZE_I32);
    let origin_z = cz.saturating_mul(CHUNK_SIZE_I32);
    let last = CHUNK_SIZE_I32 - 1;
    let landscape = Landscape::for_area(
        seed,
        [
            origin_x.saturating_sub(TREE_MARGIN),
            origin_z.saturating_sub(TREE_MARGIN),
        ],
        [
            origin_x.saturating_add(last + TREE_MARGIN),
            origin_z.saturating_add(last + TREE_MARGIN),
        ],
    );
    column_span_with(|x, z| landscape.column(x, z), cx, cz)
}

/// [`column_span`] for any columns `column(x, z)` (world block columns), so the scan can be
/// tested with a surface that has a pit or a peak exactly where it matters.
fn column_span_with(
    column: impl Fn(i32, i32) -> ColumnInfo,
    cx: i32,
    cz: i32,
) -> RangeInclusive<i32> {
    let origin_x = cx.saturating_mul(CHUNK_SIZE_I32);
    let origin_z = cz.saturating_mul(CHUNK_SIZE_I32);
    let inner = 0..CHUNK_SIZE_I32;
    let ring = -SPAN_BORDER..CHUNK_SIZE_I32 + SPAN_BORDER;
    let mut lowest = i32::MAX;
    let mut highest = i32::MIN;
    for dz in -TREE_MARGIN..CHUNK_SIZE_I32 + TREE_MARGIN {
        for dx in -TREE_MARGIN..CHUNK_SIZE_I32 + TREE_MARGIN {
            let info = column(origin_x.saturating_add(dx), origin_z.saturating_add(dz));
            if ring.contains(&dx) && ring.contains(&dz) {
                lowest = lowest.min(info.top());
            }
            if inner.contains(&dx) && inner.contains(&dz) {
                highest = highest.max(top_with_tree(&info));
            } else if info.biome.has_trees() {
                // Outside the column only a tree's leaves can reach in.
                highest = highest.max(top_with_tree(&info));
            }
        }
    }
    span_of(lowest, highest)
}

/// The chunk layers of the heights `lowest..=highest`, clamped to the world.
fn span_of(lowest: i32, highest: i32) -> RangeInclusive<i32> {
    let layer = |height: i32| chunk_layer_of(height).clamp(LOWEST_CHUNK_LAYER, HIGHEST_CHUNK_LAYER);
    layer(lowest)..=layer(highest)
}

/// Writes one column of a chunk whose lowest block is at world height `base_y`.
///
/// The layer boundaries are worked out once, as heights inside the chunk, and each layer
/// is then filled by a plain loop: stone below `stone_end`, filler up to `filler_end`, one
/// surface block, water up to the water level, and air above, which is left as it is. The
/// boundaries are clamped to the chunk, so a chunk that holds only part of the column (or
/// none of it) gets an empty range.
fn fill_column(
    chunk: &mut Chunk,
    x: usize,
    z: usize,
    base_y: i32,
    info: &ColumnInfo,
    layers: &Layers,
    water: BlockId,
) {
    let surface = info.ground.saturating_sub(base_y);
    let stone_end = clamp_to_chunk(surface.saturating_sub(FILLER_DEPTH));
    let filler_end = clamp_to_chunk(surface);
    fill_range(chunk, x, z, 0..stone_end, layers.stone);
    fill_range(chunk, x, z, stone_end..filler_end, layers.filler);
    if (0..CHUNK_SIZE_I32).contains(&surface) {
        fill_range(chunk, x, z, filler_end..filler_end + 1, layers.surface);
    }
    if info.has_water() {
        let water_start = clamp_to_chunk(surface.saturating_add(1));
        let water_end = clamp_to_chunk(info.water_level.saturating_sub(base_y).saturating_add(1));
        fill_range(chunk, x, z, water_start..water_end, water);
    }
}

/// Limits a height inside a chunk to `0..=CHUNK_SIZE`, the valid range of a layer boundary.
fn clamp_to_chunk(y: i32) -> usize {
    y.clamp(0, CHUNK_SIZE_I32) as usize
}

/// Sets the blocks of the column `(x, z)` at the heights in `layer` to `block`.
fn fill_range(
    chunk: &mut Chunk,
    x: usize,
    z: usize,
    layer: std::ops::Range<usize>,
    block: BlockId,
) {
    for y in layer {
        // `LocalPos::new` cannot fail here: `x` and `z` come from the chunk loops and the
        // layers are clamped to the chunk. The library never panics, so the impossible
        // case is skipped instead of unwrapped.
        if let Some(pos) = LocalPos::new(x, y, z) {
            chunk.set(pos, block);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bounds::{HIGHEST_CHUNK_LAYER, LOWEST_CHUNK_LAYER};
    use crate::landscape::Biome;
    use crate::mesh::{Face, build_mesh};
    use crate::world::Neighbours;

    const BEDROCK: BlockId = TEST_BLOCKS.bedrock;
    const STONE: BlockId = TEST_BLOCKS.stone;
    const WATER: BlockId = TEST_BLOCKS.water;
    const LOG: BlockId = TEST_BLOCKS.log;
    const LEAVES: BlockId = TEST_BLOCKS.leaves;

    // The tests always use `TEST_BLOCKS` (see there), so these shadow the public functions
    // with the blocks filled in.
    fn generate_terrain(seed: u32) -> Chunk {
        super::generate_terrain(seed, &TEST_BLOCKS)
    }

    fn generate_chunk(seed: u32, pos: ChunkPos) -> Chunk {
        super::generate_chunk(seed, pos, &TEST_BLOCKS)
    }

    fn without_trees(seed: u32, pos: ChunkPos) -> Chunk {
        generate_chunk_without_trees(seed, pos, &TEST_BLOCKS)
    }

    fn same_blocks(a: &Chunk, b: &Chunk) -> bool {
        LocalPos::all().all(|pos| a.get(pos) == b.get(pos))
    }

    /// The chunk position of the spawn point of `seed`: a chunk with a surface, with grass.
    fn spawn_chunk(seed: u32) -> ChunkPos {
        ChunkPos::containing(spawn_point(seed)).0
    }

    /// The column info of a world column, computed on its own.
    fn info_at(seed: u32, x: i32, z: i32) -> ColumnInfo {
        Landscape::for_area(seed, [x, z], [x, z]).column(x, z)
    }

    /// The block the layer rule gives at world height `y` of a column, written out by hand
    /// from the module's documentation (no shared helper but `Layers::of`).
    fn expected_block(info: &ColumnInfo, y: i32) -> BlockId {
        let layers = Layers::of(info, &TEST_BLOCKS);
        if y == WORLD_FLOOR_Y {
            BEDROCK
        } else if y > info.top() {
            BlockId::AIR
        } else if y > info.ground {
            WATER
        } else if y == info.ground {
            layers.surface
        } else if y >= info.ground - FILLER_DEPTH {
            layers.filler
        } else {
            STONE
        }
    }

    /// Every block of the tree-less chunk `pos` against the rule, with literal world
    /// coordinates (32 blocks per chunk).
    fn assert_chunk_follows_the_rule(seed: u32, pos: ChunkPos) {
        let chunk = without_trees(seed, pos);
        for z in 0..CHUNK_SIZE {
            for x in 0..CHUNK_SIZE {
                let info = info_at(seed, pos.x * 32 + x as i32, pos.z * 32 + z as i32);
                for y in 0..CHUNK_SIZE {
                    let world_y = pos.y * 32 + y as i32;
                    let block = LocalPos::new(x, y, z).map(|p| chunk.get(p)).unwrap();
                    assert_eq!(
                        block,
                        expected_block(&info, world_y),
                        "seed {seed} chunk {pos:?} block ({x}, {y}, {z}) of {info:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn block_at_depth_agrees_with_fill_column() {
        let info = |ground: i32, water_level: i32| ColumnInfo {
            ground,
            water_level,
            biome: Biome::Plains,
        };
        for ground in [-70, -3, 0, 5, 9, 31, 32, 40] {
            for base_y in [-64, -32, 0, 32] {
                let mut chunk = Chunk::new();
                let info = info(ground, ground);
                let layers = Layers::of(&info, &TEST_BLOCKS);
                fill_column(&mut chunk, 0, 0, base_y, &info, &layers, WATER);
                for y in 0..CHUNK_SIZE {
                    let world_y = base_y + y as i32;
                    assert_eq!(
                        LocalPos::new(0, y, 0).map(|p| chunk.get(p)).unwrap(),
                        block_at_depth(ground - world_y, &layers),
                        "ground {ground}, y {world_y}"
                    );
                }
            }
        }
    }

    #[test]
    fn water_fills_from_the_ground_up_to_the_water_level_only() {
        let info = ColumnInfo {
            ground: 61,
            water_level: SEA_LEVEL,
            biome: Biome::River,
        };
        let layers = Layers::of(&info, &TEST_BLOCKS);
        let mut chunk = Chunk::new();
        fill_column(&mut chunk, 3, 4, 48, &info, &layers, WATER);
        let at = |y: i32| chunk.get(LocalPos::new(3, (y - 48) as usize, 4).unwrap());
        assert_eq!(at(61), TEST_BLOCKS.sand);
        assert_eq!(at(62), WATER);
        assert_eq!(at(64), WATER);
        assert_eq!(at(65), BlockId::AIR);
        assert_eq!(at(60), TEST_BLOCKS.sand);
        assert_eq!(at(57), STONE);
    }

    #[test]
    fn the_surface_blocks_follow_the_biome() {
        let layers = |biome, ground| {
            let info = ColumnInfo {
                ground,
                water_level: SEA_LEVEL.max(ground),
                biome,
            };
            Layers::of(&info, &TEST_BLOCKS)
        };
        assert_eq!(layers(Biome::Plains, 70).surface, TEST_BLOCKS.grass);
        assert_eq!(layers(Biome::Forest, 70).filler, TEST_BLOCKS.dirt);
        assert_eq!(layers(Biome::Desert, 70).surface, TEST_BLOCKS.sand);
        assert_eq!(layers(Biome::Beach, 65).filler, TEST_BLOCKS.sand);
        assert_eq!(layers(Biome::Mountains, 100).surface, STONE);
        assert_eq!(
            layers(Biome::Mountains, SNOW_LINE).surface,
            TEST_BLOCKS.snow
        );
        assert_eq!(layers(Biome::Mountains, SNOW_LINE).filler, STONE);
        assert_eq!(
            layers(Biome::Ocean, SEA_LEVEL - 5).surface,
            TEST_BLOCKS.sand
        );
        assert_eq!(
            layers(Biome::Ocean, SEA_LEVEL - SAND_DEPTH - 1).surface,
            TEST_BLOCKS.gravel
        );
    }

    #[test]
    fn same_seed_gives_identical_chunk() {
        assert!(same_blocks(&generate_terrain(7), &generate_terrain(7)));
    }

    #[test]
    fn different_seeds_give_different_chunks() {
        assert!(!same_blocks(&generate_terrain(1), &generate_terrain(2)));
    }

    #[test]
    fn the_terrain_chunk_follows_the_layer_rule_everywhere() {
        // Surface chunks around the spawn (grass, trees' ground) and a far one, in every
        // layer that holds land, for several seeds; negative coordinates included.
        for seed in [1, 2, 99] {
            let spawn = spawn_chunk(seed);
            for (dx, dz) in [(0, 0), (1, 0), (-1, 0), (0, 1), (0, -1)] {
                for layer in 0..=6 {
                    assert_chunk_follows_the_rule(
                        seed,
                        ChunkPos::new(spawn.x + dx, layer, spawn.z + dz),
                    );
                }
            }
        }
        for pos in [
            ChunkPos::new(-5, 2, 3),
            ChunkPos::new(2, 2, -7),
            ChunkPos::new(-1, 1, -1),
            ChunkPos::new(40, 3, -40),
        ] {
            assert_chunk_follows_the_rule(1, pos);
        }
    }

    #[test]
    fn a_wide_world_has_trees_and_water_in_its_chunks() {
        let mut counts = [0_usize; 2];
        for z in -12..12 {
            for x in -12..12 {
                for layer in 0..=3 {
                    let chunk = generate_chunk(1, ChunkPos::new(x, layer, z));
                    for pos in LocalPos::all() {
                        match chunk.get(pos) {
                            LOG => counts[0] += 1,
                            WATER => counts[1] += 1,
                            _ => {}
                        }
                    }
                }
            }
        }
        assert!(counts.iter().all(|&c| c > 0), "logs, water: {counts:?}");
    }

    #[test]
    fn mountain_peaks_are_capped_with_snow() {
        // Finds the highest ground of a wide scan and generates the chunks around it.
        let (mut best, mut at) = (0, (0, 0));
        for z in (-4000..4000).step_by(16) {
            for x in (-4000..4000).step_by(16) {
                let ground = info_at(1, x, z).ground;
                if ground > best {
                    (best, at) = (ground, (x, z));
                }
            }
        }
        assert!(best >= SNOW_LINE, "highest ground {best}");
        let (pos, _) = ChunkPos::containing([at.0, best, at.1]);
        let chunk = generate_chunk(1, pos);
        assert!(LocalPos::all().any(|p| chunk.get(p) == TEST_BLOCKS.snow));
    }

    #[test]
    fn trees_only_add_logs_and_leaves_in_air() {
        // The chunk with trees differs from the tree-less one only in cells that were air
        // and became log or leaves; some chunks have trees.
        let mut with_trees = 0;
        for seed in [1, 2, 3] {
            let spawn = spawn_chunk(seed);
            for (dx, dz) in [(0, 0), (1, 1), (-2, 0), (0, 3)] {
                let pos = ChunkPos::new(spawn.x + dx, spawn.y, spawn.z + dz);
                let (plain, full) = (without_trees(seed, pos), generate_chunk(seed, pos));
                for p in LocalPos::all() {
                    if plain.get(p) != full.get(p) {
                        assert!(plain.get(p).is_air(), "{pos:?} {p:?}");
                        assert!(matches!(full.get(p), LOG | LEAVES));
                        with_trees += 1;
                    }
                }
            }
        }
        assert!(with_trees > 0);
    }

    #[test]
    fn trees_straddle_chunk_borders_and_both_chunks_draw_them() {
        // Leaves on both sides of a border at the same height and row: a tree that
        // crosses a border is drawn by both chunks, each from the hash alone.
        let spawn = spawn_chunk(1);
        let mut pairs = 0;
        for dz in -2..2 {
            for dx in -10..10 {
                let a = generate_chunk(1, ChunkPos::new(spawn.x + dx, spawn.y, spawn.z + dz));
                let b = generate_chunk(1, ChunkPos::new(spawn.x + dx + 1, spawn.y, spawn.z + dz));
                for z in 0..CHUNK_SIZE {
                    for y in 0..CHUNK_SIZE {
                        let last = LocalPos::new(CHUNK_SIZE - 1, y, z).unwrap();
                        let first = LocalPos::new(0, y, z).unwrap();
                        if a.get(last) == LEAVES && b.get(first) == LEAVES {
                            pairs += 1;
                        }
                    }
                }
            }
        }
        assert!(pairs > 0, "no tree crosses a border near the spawn");
    }

    #[test]
    fn every_chunk_is_generated_independently_of_its_neighbours() {
        // The generator has no state: asking for a chunk twice, in any order, gives the same
        // blocks, trees included.
        let spawn = spawn_chunk(3);
        let first = generate_chunk(3, spawn);
        let _other = generate_chunk(3, ChunkPos::new(spawn.x + 1, spawn.y, spawn.z));
        assert!(same_blocks(&first, &generate_chunk(3, spawn)));
    }

    /// A tree's top in this chunk: the highest log and its leaves are in the chunk, and
    /// leaves surround the top log (the crown), for the trees that are whole inside it.
    #[test]
    fn a_tree_has_four_to_seven_logs_under_a_crown_of_leaves() {
        let pos = ChunkPos::new(spawn_chunk(1).x, spawn_chunk(1).y, spawn_chunk(1).z);
        let chunk = generate_chunk(1, pos);
        let mut found = false;
        for z in 3..CHUNK_SIZE - 3 {
            for x in 3..CHUNK_SIZE - 3 {
                // The lowest log of a column stands on the ground; count the logs above it.
                let column: Vec<BlockId> = (0..CHUNK_SIZE)
                    .map(|y| chunk.get(LocalPos::new(x, y, z).unwrap()))
                    .collect();
                let Some(first) = column.iter().position(|&b| b == LOG) else {
                    continue;
                };
                let logs = column[first..].iter().take_while(|&&b| b == LOG).count();
                let top = first + logs - 1;
                if top + 3 >= CHUNK_SIZE {
                    continue;
                }
                found = true;
                assert!((4..=7).contains(&logs), "{logs} logs at ({x}, {z})");
                assert_ne!(column[first - 1], BlockId::AIR);
                let crown = |dx: i32, dy: i32, dz: i32| {
                    chunk.get(
                        LocalPos::new(
                            (x as i32 + dx) as usize,
                            (top as i32 + dy) as usize,
                            (z as i32 + dz) as usize,
                        )
                        .unwrap(),
                    )
                };
                assert_eq!(crown(1, 0, 0), LEAVES);
                assert_eq!(crown(0, 1, 0), LEAVES);
                assert_eq!(crown(0, 2, 0), LEAVES);
                assert_eq!(crown(-2, 0, 0), LEAVES);
            }
        }
        assert!(found, "no whole tree in the spawn chunk");
    }

    /// FNV-1a over every block and its position in storage order. Unlike a plain sum
    /// of block ids, it changes when blocks move: swapped axes, mirrored noise or
    /// reordered octaves all give a different value.
    fn fingerprint(chunk: &Chunk) -> u64 {
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for (index, pos) in LocalPos::all().enumerate() {
            let index = u32::try_from(index).unwrap_or(u32::MAX);
            let bytes = index
                .to_le_bytes()
                .into_iter()
                .chain(chunk.get(pos).0.to_le_bytes());
            for byte in bytes {
                hash ^= u64::from(byte);
                hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
            }
        }
        hash
    }

    #[test]
    fn terrain_matches_pinned_fingerprints() {
        // Pins the exact output of the algorithm for two seeds. If you change the noise,
        // the hash, the layers or the trees on purpose, recompute these numbers (the failure
        // prints them).
        assert_eq!(fingerprint(&generate_terrain(1)), 7_196_292_810_354_471_196);
        assert_eq!(
            fingerprint(&generate_terrain(2)),
            11_889_291_948_083_898_712
        );
    }

    #[test]
    fn far_chunk_matches_pinned_fingerprint() {
        // A surface chunk with negative Z, so a change of the floor division or of the way
        // the lattice hash takes negative cells shows up. Recompute on purpose only.
        assert_eq!(
            fingerprint(&generate_chunk(1, ChunkPos::new(1, 2, -1))),
            10_230_870_403_014_947_642
        );
    }

    /// Quads of the greedy mesh of `generate_terrain(1)`.
    const GREEDY_QUADS: usize = 730;
    /// Quads of the naive mesh (one per visible face) of `generate_terrain(1)`.
    const NAIVE_QUADS: usize = 3280;

    #[test]
    fn terrain_mesh_has_the_pinned_number_of_quads() {
        // The exact counts for seed 1, so a change in the terrain or the mesher shows up.
        // Recompute them when either changes on purpose. The naive mesher gives one quad
        // per visible face; the greedy one merges them.
        let chunk = generate_terrain(1);
        let naive = crate::mesh::naive_mesh(
            &chunk,
            &crate::Neighbours::none(),
            &crate::mesh::identity_textures(),
        );
        assert_eq!(naive.quad_count(), NAIVE_QUADS);
        assert_eq!(
            build_mesh(&chunk, &crate::mesh::identity_textures()).quad_count(),
            GREEDY_QUADS
        );
    }

    #[test]
    fn origin_of_terrain_is_the_chunk_of_the_spawn_point() {
        for seed in [0, 1, 2, 99, u32::MAX] {
            assert!(same_blocks(
                &generate_terrain(seed),
                &generate_chunk(seed, spawn_chunk(seed))
            ));
        }
    }

    #[test]
    fn border_columns_use_the_world_columns_on_both_sides_of_a_border() {
        // Literal world columns: chunk (1, y, 0) starts at world x = 32, chunk (0, y, 0)
        // ends at 31 and chunk (-1, y, 0) ends at -1. A chunk origin that is off by one
        // column would put a different column in each of these places. The heights come
        // from the landscape, the chunk is read back for the highest ground block.
        let ground_in = |seed: u32, pos: ChunkPos, x: usize, z: usize| {
            let chunk = without_trees(seed, pos);
            (0..CHUNK_SIZE)
                .rev()
                .map(|y| (y, chunk.get(LocalPos::new(x, y, z).unwrap())))
                .find(|&(_, b)| b != BlockId::AIR && b != WATER)
                .map(|(y, _)| pos.y * 32 + y as i32)
        };
        for seed in [1, 2] {
            for i in 0..CHUNK_SIZE {
                let along = i as i32;
                let last = CHUNK_SIZE - 1;
                let check = |pos: ChunkPos, x: usize, z: usize, world_x: i32, world_z: i32| {
                    let ground = info_at(seed, world_x, world_z).ground;
                    // Only layers that hold the ground can show it.
                    if (pos.y * 32..pos.y * 32 + 32).contains(&ground) {
                        assert_eq!(ground_in(seed, pos, x, z), Some(ground));
                    }
                };
                for y in 0..=5 {
                    check(ChunkPos::new(0, y, 0), last, i, 31, along);
                    check(ChunkPos::new(1, y, 0), 0, i, 32, along);
                    check(ChunkPos::new(-1, y, 0), last, i, -1, along);
                    check(ChunkPos::new(0, y, 0), i, last, along, 31);
                    check(ChunkPos::new(0, y, 1), i, 0, along, 32);
                    check(ChunkPos::new(0, y, -1), i, last, along, -1);
                }
            }
        }
    }

    #[test]
    fn chunks_above_everything_are_air_and_below_everything_are_stone() {
        for seed in [1, 2] {
            let above = generate_chunk(seed, ChunkPos::new(4, HIGHEST_CHUNK_LAYER, -3));
            let layer_over_peaks = HIGHEST_BLOCK_Y.div_euclid(32) + 1;
            let over = generate_chunk(seed, ChunkPos::new(4, layer_over_peaks, -3));
            let below = generate_chunk(seed, ChunkPos::new(4, -1, -3));
            let deep = generate_chunk(seed, ChunkPos::new(0, LOWEST_CHUNK_LAYER + 1, 0));
            assert!(LocalPos::all().all(|pos| above.get(pos).is_air()));
            assert!(LocalPos::all().all(|pos| over.get(pos).is_air()));
            assert!(LocalPos::all().all(|pos| below.get(pos) == STONE));
            assert!(LocalPos::all().all(|pos| deep.get(pos) == STONE));
        }
    }

    #[test]
    fn the_fast_paths_equal_the_full_generator() {
        // The all-air and all-stone shortcuts skip the noise; the layer rule must give the
        // same blocks (checked on the layers next to the shortcuts' limits).
        for layer in [
            -1,
            0,
            HIGHEST_BLOCK_Y.div_euclid(32),
            HIGHEST_BLOCK_Y.div_euclid(32) + 1,
        ] {
            assert_chunk_follows_the_rule(1, ChunkPos::new(3, layer, 4));
        }
    }

    #[test]
    fn the_floor_row_is_bedrock_and_the_rest_of_the_floor_layer_is_stone() {
        for seed in [1, 2, 99] {
            let chunk = generate_chunk(seed, ChunkPos::new(-3, LOWEST_CHUNK_LAYER, 4));
            for pos in LocalPos::all() {
                let expected = if pos.y() == 0 { BEDROCK } else { STONE };
                assert_eq!(chunk.get(pos), expected, "seed {seed}, {pos:?}");
            }
        }
    }

    #[test]
    fn the_floor_row_is_at_the_literal_world_height_minus_512() {
        // The floor layer starts at block -512 = 16 * -32: row 0 of chunk layer -16.
        assert_eq!(WORLD_FLOOR_Y, -512);
        assert_eq!(floor_row_in_chunk(-512), Some(0));
        assert_eq!(floor_row_in_chunk(-480), None);
        assert_eq!(floor_row_in_chunk(-544), None);
        assert_eq!(floor_row_in_chunk(0), None);
    }

    #[test]
    fn outside_the_world_the_stand_ins_are_air_above_and_bedrock_below() {
        for pos in [
            ChunkPos::new(0, HIGHEST_CHUNK_LAYER + 1, 0),
            ChunkPos::new(2, i32::MAX, -2),
        ] {
            let chunk = generate_chunk(1, pos);
            assert!(LocalPos::all().all(|p| chunk.get(p).is_air()), "{pos:?}");
        }
        for pos in [
            ChunkPos::new(0, LOWEST_CHUNK_LAYER - 1, 0),
            ChunkPos::new(-2, i32::MIN, 2),
        ] {
            let chunk = generate_chunk(1, pos);
            assert!(LocalPos::all().all(|p| chunk.get(p) == BEDROCK), "{pos:?}");
        }
    }

    #[test]
    fn chunks_at_the_far_end_of_the_x_and_z_range_do_not_panic() {
        for (x, z) in [(i32::MAX, 0), (i32::MIN, i32::MIN), (0, i32::MAX)] {
            for layer in [0, 2, 3] {
                let _ = generate_chunk(1, ChunkPos::new(x, layer, z));
            }
            let _ = column_span(1, x, z);
        }
    }

    #[test]
    fn span_of_uses_floor_division_for_negative_heights() {
        // Literal layers: floor(-1/32) = -1, floor(-32/32) = -1, floor(-33/32) = -2.
        assert_eq!(span_of(-1, -1), -1..=-1);
        assert_eq!(span_of(-32, -1), -1..=-1);
        assert_eq!(span_of(-33, 0), -2..=0);
        assert_eq!(span_of(-512, -512), -16..=-16);
        assert_eq!(span_of(-513, -512), -16..=-16);
    }

    #[test]
    fn span_of_splits_at_the_layer_boundary() {
        assert_eq!(span_of(31, 31), 0..=0);
        assert_eq!(span_of(32, 32), 1..=1);
        assert_eq!(span_of(31, 32), 0..=1);
    }

    #[test]
    fn span_of_clamps_to_the_world() {
        assert_eq!(
            span_of(i32::MIN, i32::MAX),
            LOWEST_CHUNK_LAYER..=HIGHEST_CHUNK_LAYER
        );
        assert_eq!(
            span_of(-5000, -4000),
            LOWEST_CHUNK_LAYER..=LOWEST_CHUNK_LAYER
        );
        assert_eq!(
            span_of(4000, 5000),
            HIGHEST_CHUNK_LAYER..=HIGHEST_CHUNK_LAYER
        );
    }

    /// A column of mountain stone (no trees) with no water at `height`.
    fn rock(height: i32) -> ColumnInfo {
        ColumnInfo {
            ground: height,
            water_level: height,
            biome: Biome::Mountains,
        }
    }

    /// A flat surface at height 40 (layer 1) with one block column changed.
    fn flat_with(spot: (i32, i32), height: i32) -> impl Fn(i32, i32) -> ColumnInfo {
        move |x, z| rock(if (x, z) == spot { height } else { 40 })
    }

    #[test]
    fn a_pit_on_the_border_ring_lowers_the_span() {
        // Column (0, 0) covers x, z in 0..32; the ring is x or z = -1 or 32. The pit is at
        // height 10 (layer 0): a side face of the inner block next to it is in layer 0.
        for spot in [(-1, 5), (5, -1), (32, 5), (5, 32), (-1, -1), (32, 32)] {
            assert_eq!(
                column_span_with(flat_with(spot, 10), 0, 0),
                0..=1,
                "{spot:?}"
            );
        }
        // One block further out is not looked at.
        for spot in [(-2, 5), (5, -2), (33, 5), (5, 33)] {
            assert_eq!(
                column_span_with(flat_with(spot, 10), 0, 0),
                1..=1,
                "{spot:?}"
            );
        }
    }

    #[test]
    fn a_peak_counts_only_inside_the_column() {
        let peak = 100; // layer 3
        assert_eq!(column_span_with(flat_with((0, 0), peak), 0, 0), 1..=3);
        assert_eq!(column_span_with(flat_with((31, 31), peak), 0, 0), 1..=3);
        // On the ring the peak belongs to the next column; its faces are in that span.
        for spot in [(-1, 5), (32, 5), (5, -1), (5, 32), (-2, -2), (33, 33)] {
            assert_eq!(
                column_span_with(flat_with(spot, peak), 0, 0),
                1..=1,
                "{spot:?}"
            );
        }
    }

    #[test]
    fn water_counts_as_the_top_of_a_column() {
        // A lake bed at 20 under water up to 40: the visible top is 40 (layer 1), not 20.
        let sea = |_: i32, _: i32| ColumnInfo {
            ground: 20,
            water_level: 40,
            biome: Biome::Ocean,
        };
        assert_eq!(column_span_with(sea, 0, 0), 1..=1);
    }

    #[test]
    fn a_tree_may_reach_into_the_next_layer_and_over_the_margin() {
        // Grass at 60 (layer 1, top 60): a tree can reach 60 + TREE_MAX_HEIGHT = 69 (layer 2).
        let grass = |height: i32| ColumnInfo {
            ground: height,
            water_level: SEA_LEVEL.min(height),
            biome: Biome::Forest,
        };
        let flat = |_: i32, _: i32| grass(60);
        assert_eq!(column_span_with(flat, 0, 0), 1..=2);
        // Grass 2 columns outside (the margin): its tree's leaves reach in; 3 outside: not.
        let outside = |spot: (i32, i32)| {
            move |x: i32, z: i32| {
                if (x, z) == spot { grass(60) } else { rock(40) }
            }
        };
        assert_eq!(column_span_with(outside((-2, 5)), 0, 0), 1..=2);
        assert_eq!(column_span_with(outside((33, 5)), 0, 0), 1..=2);
        assert_eq!(column_span_with(outside((34, 5)), 0, 0), 1..=1);
        assert_eq!(column_span_with(outside((-3, 5)), 0, 0), 1..=1);
        // A tree-less column of the same height at the margin counts for nothing.
        let bare = |x: i32, z: i32| {
            if (x, z) == (-2, 5) {
                rock(100)
            } else {
                rock(40)
            }
        };
        assert_eq!(column_span_with(bare, 0, 0), 1..=1);
    }

    #[test]
    fn the_scan_follows_the_column_position_including_negative_columns() {
        // A pit at world (-33, 5): column -2 covers x in -64..=-33, so it is inside that
        // column; column -1 covers -32..=-1 and sees it on its ring (one left of -32).
        let height = flat_with((-33, 5), 10);
        assert_eq!(column_span_with(&height, -2, 0), 0..=1);
        assert_eq!(column_span_with(&height, -1, 0), 0..=1);
        assert_eq!(column_span_with(&height, 0, 0), 1..=1);
        assert_eq!(column_span_with(&height, -3, 0), 1..=1);
    }

    #[test]
    fn mountain_columns_have_spans_of_several_layers() {
        let mut tallest = 0;
        for cz in -40..40 {
            for cx in -40..40 {
                let span = column_span(1, cx, cz);
                tallest = tallest.max(span.end() - span.start() + 1);
            }
        }
        assert!(tallest >= 3, "tallest span {tallest} layers");
    }

    /// The layer of a height, written out by hand (no `div_euclid`).
    fn layer_of(height: i32) -> i32 {
        if height >= 0 {
            height / 32
        } else {
            -((-height + 31) / 32)
        }
    }

    #[test]
    fn the_span_covers_exactly_the_layers_of_the_lowest_and_highest_top() {
        // Brute force over the 36 x 36 area with literal world coordinates.
        for seed in [1, 2, 3, 99] {
            for (cx, cz) in [(0, 0), (1, 0), (-1, 0), (0, -1), (4, 9), (-6, -2), (30, 17)] {
                let (x0, z0) = (cx * 32, cz * 32);
                let mut lowest = i32::MAX;
                let mut highest = i32::MIN;
                for dz in -2..=33 {
                    for dx in -2..=33 {
                        let info = info_at(seed, x0 + dx, z0 + dz);
                        let ring = (-1..=32).contains(&dx) && (-1..=32).contains(&dz);
                        let inner = (0..=31).contains(&dx) && (0..=31).contains(&dz);
                        let tree = if info.biome.has_trees() {
                            info.ground + 9
                        } else {
                            info.top()
                        };
                        if ring {
                            lowest = lowest.min(info.top());
                        }
                        if inner || info.biome.has_trees() {
                            highest = highest.max(tree);
                        }
                    }
                }
                let span = column_span(seed, cx, cz);
                assert_eq!(*span.start(), layer_of(lowest), "seed {seed} ({cx}, {cz})");
                assert_eq!(*span.end(), layer_of(highest), "seed {seed} ({cx}, {cz})");
            }
        }
    }

    /// The layers to examine around the span of a column: three either side of it and the
    /// two ends of the world.
    fn layers_to_check(span: &RangeInclusive<i32>) -> Vec<i32> {
        let mut layers: Vec<i32> = (*span.start() - 3..=*span.end() + 3)
            .filter(|&l| (LOWEST_CHUNK_LAYER..=HIGHEST_CHUNK_LAYER).contains(&l))
            .collect();
        layers.extend([LOWEST_CHUNK_LAYER, HIGHEST_CHUNK_LAYER]);
        layers.sort_unstable();
        layers.dedup();
        layers
    }

    /// Columns of chunks with land, water, mountains and trees: the spawn and what surrounds
    /// it, plus the columns with the tallest spans found in a scan.
    fn interesting_columns(seed: u32) -> Vec<(i32, i32)> {
        let spawn = spawn_chunk(seed);
        let mut columns = vec![(spawn.x, spawn.z), (spawn.x + 1, spawn.z - 1)];
        let mut tallest = (0, (0, 0));
        for cz in -30..30 {
            for cx in -30..30 {
                let span = column_span(seed, cx, cz);
                let height = span.end() - span.start();
                if height > tallest.0 {
                    tallest = (height, (cx, cz));
                }
            }
        }
        columns.push(tallest.1);
        columns.push((-3, 4));
        columns
    }

    #[test]
    fn layers_outside_the_span_are_solid_below_and_air_above() {
        for seed in 0..4 {
            for (cx, cz) in interesting_columns(seed) {
                let span = column_span(seed, cx, cz);
                for layer in layers_to_check(&span) {
                    let chunk = generate_chunk(seed, ChunkPos::new(cx, layer, cz));
                    if layer < *span.start() {
                        assert!(
                            LocalPos::all().all(|pos| !chunk.get(pos).is_air()),
                            "seed {seed} ({cx}, {cz}) layer {layer} below the span {span:?}"
                        );
                    } else if layer > *span.end() {
                        assert!(
                            LocalPos::all().all(|pos| chunk.get(pos).is_air()),
                            "seed {seed} ({cx}, {cz}) layer {layer} above the span {span:?}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn no_layer_outside_the_span_meshes_to_anything() {
        // Every visible face (water surfaces and trees included) lies inside the span: the
        // layers outside it, meshed with their generated neighbours, have no quads.
        let textures = crate::mesh::identity_textures();
        let mut mesh = crate::ChunkMesh::default();
        for seed in 0..4 {
            for (cx, cz) in interesting_columns(seed) {
                let span = column_span(seed, cx, cz);
                let layers = layers_to_check(&span);
                let chunk_at = |dx: i32, dz: i32, layer: i32| {
                    generate_chunk(seed, ChunkPos::new(cx + dx, layer, cz + dz))
                };
                for &layer in &layers {
                    if span.contains(&layer) {
                        continue;
                    }
                    let own = chunk_at(0, 0, layer);
                    let above = chunk_at(0, 0, layer + 1);
                    // Below the lowest layer: the bedrock stand-in, like the world's floor.
                    let below = generate_chunk(
                        seed,
                        ChunkPos::new(cx, (layer - 1).max(LOWEST_CHUNK_LAYER - 1), cz),
                    );
                    let sides =
                        [(1, 0), (-1, 0), (0, 1), (0, -1)].map(|(dx, dz)| chunk_at(dx, dz, layer));
                    let neighbours = Neighbours::new([
                        Some(&sides[0]),
                        Some(&sides[1]),
                        Some(&above),
                        Some(&below),
                        Some(&sides[2]),
                        Some(&sides[3]),
                    ]);
                    crate::build_mesh_with_neighbours_into(&own, &neighbours, &textures, &mut mesh);
                    assert_eq!(
                        mesh.quad_count(),
                        0,
                        "seed {seed} column ({cx}, {cz}) layer {layer}, span {span:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn a_generated_column_equals_its_chunks_one_by_one() {
        for (seed, cx, cz) in [(1, 0, 0), (3, -2, 5), (273, 0, 0), (2, 40, -40)] {
            let layers = LOWEST_CHUNK_LAYER..=HIGHEST_CHUNK_LAYER + 1;
            let column = generate_column(seed, cx, cz, layers.clone(), &TEST_BLOCKS);
            assert_eq!(column.len(), layers.clone().count());
            for (layer, chunk) in layers.zip(&column) {
                let single = generate_chunk(seed, ChunkPos::new(cx, layer, cz));
                assert!(
                    same_blocks(chunk, &single),
                    "seed {seed} ({cx}, {cz}) layer {layer}"
                );
            }
        }
    }

    #[test]
    fn no_trunk_stands_at_the_spawn_column() {
        for seed in 0..40 {
            let [x, y, z] = spawn_point(seed);
            let (pos, local) = ChunkPos::containing([x, y, z]);
            let chunk = generate_chunk(seed, pos);
            assert_eq!(chunk.get(local), BlockId::AIR, "seed {seed}");
            // The column above the spawn holds no log.
            for dy in 0..9 {
                let (above, local) = ChunkPos::containing([x, y + dy, z]);
                assert_ne!(
                    generate_chunk(seed, above).get(local),
                    LOG,
                    "seed {seed} +{dy}"
                );
            }
        }
    }

    #[test]
    fn same_seed_and_far_position_give_identical_chunk() {
        let far = ChunkPos::new(3, 2, -2);
        assert!(same_blocks(
            &generate_chunk(5, far),
            &generate_chunk(5, far)
        ));
    }

    #[test]
    fn different_positions_give_different_chunks() {
        let spawn = spawn_chunk(1);
        let here = generate_chunk(1, spawn);
        for (x, z) in [(1, 0), (-1, 0), (0, 1), (0, -1), (3, -2)] {
            assert!(
                !same_blocks(
                    &here,
                    &generate_chunk(1, ChunkPos::new(spawn.x + x, spawn.y, spawn.z + z))
                ),
                "chunk ({x}, {z})"
            );
        }
    }

    #[test]
    fn faces_of_neighbouring_columns_meet_at_chunk_borders() {
        // A chunk meshed with its real neighbours has fewer quads than alone: the border
        // columns of neighbours agree, which is what hides the faces between them.
        let spawn = spawn_chunk(1);
        let alone = build_mesh(&generate_chunk(1, spawn), &crate::mesh::identity_textures());
        let [x, y, z] = [spawn.x, spawn.y, spawn.z];
        let around: [Chunk; Face::COUNT] =
            Face::ALL.map(|face| generate_chunk(1, spawn.neighbour(face)));
        let neighbours =
            Neighbours::new(Face::ALL.map(|face| Some(&around[usize::from(face.index())])));
        let mut mesh = crate::ChunkMesh::default();
        crate::build_mesh_with_neighbours_into(
            &generate_chunk(1, ChunkPos::new(x, y, z)),
            &neighbours,
            &crate::mesh::identity_textures(),
            &mut mesh,
        );
        assert!(mesh.quad_count() < alone.quad_count());
    }
}
