//! Demo terrain: rolling hills from deterministic value noise, generated per chunk.
//!
//! Everything here is integer hashing plus plain `f32` arithmetic (`+`, `-`, `*`, `/`
//! are exactly specified by IEEE 754), with no `sin` or other libm functions, so a seed
//! produces the same chunk on every platform. The noise is sampled at world coordinates,
//! so chunks next to each other fit together. It is neither optimized nor measured beyond
//! `docs/benchmarks.md`.

use std::ops::RangeInclusive;

use crate::block::BlockId;
use crate::bounds::{
    CHUNK_SIZE_I32, HIGHEST_CHUNK_LAYER, LOWEST_CHUNK_LAYER, WORLD_FLOOR_Y, chunk_layer_of,
};
use crate::chunk::{CHUNK_SIZE, Chunk, LocalPos};
use crate::chunk_pos::ChunkPos;

/// The blocks the terrain generator places, as ids.
///
/// The generator names no content: whoever owns the [`BlockRegistry`](crate::BlockRegistry)
/// looks the four blocks up by name and passes their ids in (decision 0013).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TerrainBlocks {
    /// The top block of every column.
    pub grass: BlockId,
    /// The layer right below the grass.
    pub dirt: BlockId,
    /// Everything below the dirt.
    pub stone: BlockId,
    /// The row at [`WORLD_FLOOR_Y`] (and everything below it, which is never meshed).
    pub bedrock: BlockId,
}

/// Ids 1 to 3 as the game's registry hands out, and 4 for bedrock (a test-only choice: the
/// game's bedrock id is later in the blocks file), for tests in this crate.
#[cfg(test)]
pub(crate) const TEST_BLOCKS: TerrainBlocks = TerrainBlocks {
    grass: BlockId(1),
    dirt: BlockId(2),
    stone: BlockId(3),
    bedrock: BlockId(4),
};

/// Lowest possible surface height (the `y` of the grass block).
pub const MIN_SURFACE: usize = 8;
/// Highest possible surface height (the `y` of the grass block).
pub const MAX_SURFACE: usize = 24;

// The columns are filled up to `MAX_SURFACE`, so it must lie inside the chunk.
const _: () = assert!(MAX_SURFACE < CHUNK_SIZE);

// The surface lies above the bedrock row, with room for the dirt layer, so the bedrock row
// is always inside the stone.
const _: () = assert!(MIN_SURFACE as i32 - DIRT_DEPTH as i32 > WORLD_FLOOR_Y);

// `smooth_noise` splits coordinates with a shift and a mask, which needs power-of-two cells.
const _: () = assert!(cells_are_powers_of_two());

/// Number of dirt blocks between the grass and the stone.
const DIRT_DEPTH: usize = 3;

/// Cell size in blocks and weight of each noise octave.
/// Large hills first, finer detail on top with less influence.
const OCTAVES: [(i32, f32); 3] = [(16, 4.0), (8, 2.0), (4, 1.0)];

/// Sum of the octave weights, the divisor that normalizes the noise to `0.0..=1.0`.
/// Computed from [`OCTAVES`], so the two cannot drift apart.
const WEIGHT_SUM: f32 = weight_sum();

const fn cells_are_powers_of_two() -> bool {
    let mut index = 0;
    while index < OCTAVES.len() {
        if OCTAVES[index].0.count_ones() != 1 {
            return false;
        }
        index += 1;
    }
    true
}

const fn weight_sum() -> f32 {
    let mut sum = 0.0;
    let mut index = 0;
    while index < OCTAVES.len() {
        sum += OCTAVES[index].1;
        index += 1;
    }
    sum
}

/// [`DIRT_DEPTH`] as a world height difference.
const DIRT_DEPTH_I32: i32 = DIRT_DEPTH as i32;

/// The block `depth` blocks below the surface of a column (`depth == 0` is the grass block,
/// a negative depth is above it: air). This is the layer rule of [`fill_column`] for one
/// block, for callers that classify single blocks (level-of-detail cells); a test checks
/// that both agree.
pub(crate) const fn block_at_depth(depth: i32, blocks: &TerrainBlocks) -> BlockId {
    if depth < 0 {
        BlockId::AIR
    } else if depth == 0 {
        blocks.grass
    } else if depth <= DIRT_DEPTH_I32 {
        blocks.dirt
    } else {
        blocks.stone
    }
}

/// Generates the chunk at the origin: rolling hills. The same `seed` always gives the same
/// chunk. It is [`generate_chunk`] at [`ChunkPos::ORIGIN`].
///
/// Every column has one `blocks.grass` block at a height in
/// [`MIN_SURFACE`]`..=`[`MAX_SURFACE`], `blocks.dirt` below it, `blocks.stone` under that and
/// air above.
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
///     bedrock: BlockId(4),
/// };
/// let chunk = generate_terrain(1, &blocks);
/// let has_grass = LocalPos::all().any(|pos| chunk.get(pos) == blocks.grass);
/// assert!(has_grass);
/// ```
pub fn generate_terrain(seed: u32, blocks: &TerrainBlocks) -> Chunk {
    generate_chunk(seed, ChunkPos::ORIGIN, blocks)
}

/// Generates the terrain of the chunk at `pos`. The same `seed` and `pos` always give the
/// same chunk.
///
/// The surface height of a column depends only on its world column `(x, z)`, never on the
/// chunk, so neighbouring chunks agree along their borders and the hills continue across
/// them. For a block at world height `y` and a column with surface height `h` (in
/// [`MIN_SURFACE`]`..=`[`MAX_SURFACE`]): above `h` is air, `y == h` is `blocks.grass`, the
/// three blocks below are `blocks.dirt`, everything deeper is `blocks.stone`.
///
/// Consequently every chunk with `pos.y >= 1` is all air (its lowest block is at height 32,
/// above [`MAX_SURFACE`]) and every chunk between the floor layer and layer 0 is all stone
/// (its highest block is at height -1, more than three below [`MIN_SURFACE`]), except for
/// the row at [`WORLD_FLOOR_Y`], which is `blocks.bedrock`.
///
/// Outside the world's layers ([`contains_chunk_layer`](crate::contains_chunk_layer)) the answer is a stand-in, so the
/// function stays total: above the highest layer the chunk is all air, below the lowest
/// layer it is all `blocks.bedrock`. A mesh of the lowest layer that is given a chunk of this
/// kind below it therefore has no bottom faces. Nothing is ever stored there.
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
///     bedrock: BlockId(4),
/// };
/// let chunk = generate_chunk(1, ChunkPos::new(-1, 0, 2), &blocks);
/// assert!(LocalPos::all().any(|pos| chunk.get(pos) == blocks.grass));
/// ```
pub fn generate_chunk(seed: u32, pos: ChunkPos, blocks: &TerrainBlocks) -> Chunk {
    let mut chunk = Chunk::new();
    if pos.y > HIGHEST_CHUNK_LAYER {
        return chunk;
    }
    let below_world = pos.y < LOWEST_CHUNK_LAYER;
    let [origin_x, origin_y, origin_z] = pos.origin();
    let floor_row = floor_row_in_chunk(origin_y);
    for z in 0..CHUNK_SIZE {
        for x in 0..CHUNK_SIZE {
            if below_world {
                fill_range(&mut chunk, x, z, 0..CHUNK_SIZE, blocks.bedrock);
                continue;
            }
            // Saturating, like `ChunkPos::origin`: far outside the usable world the
            // terrain degrades but nothing wraps or panics.
            let world_x = origin_x.saturating_add(x as i32);
            let world_z = origin_z.saturating_add(z as i32);
            let height = surface_height(seed, world_x, world_z);
            fill_column(&mut chunk, x, z, origin_y, height, blocks);
            if let Some(row) = floor_row {
                fill_range(&mut chunk, x, z, row..row + 1, blocks.bedrock);
            }
        }
    }
    chunk
}

/// The height inside a chunk whose lowest block is at `base_y` of the row at
/// [`WORLD_FLOOR_Y`], or `None` if the chunk does not hold that row.
fn floor_row_in_chunk(base_y: i32) -> Option<usize> {
    let row = WORLD_FLOOR_Y.checked_sub(base_y)?;
    (0..CHUNK_SIZE_I32)
        .contains(&row)
        .then(|| clamp_to_chunk(row))
}

/// How far past the 32 x 32 columns of a chunk column [`column_span`] looks for the lowest
/// height: one block, because a side face is exposed by the neighbour next to it.
const SPAN_BORDER: i32 = 1;

/// The chunk layers (chunk `y` coordinates, inclusive) of the column of chunks `(cx, cz)` that
/// can hold a visible face. Every layer below the range is all solid and every layer above it
/// is all air, so they have no faces and need not be generated, stored or meshed.
///
/// With `lowest` the lowest surface height among the column's 32 x 32 blocks and the
/// [`SPAN_BORDER`] ring around them, and `highest` the highest of the 32 x 32 blocks: a top
/// face sits at a block column's own height, and a side face lies above the lower of two
/// neighbouring heights, so every face is in `lowest..=highest`. The range is the layers of
/// those two heights, clamped to the world ([`contains_chunk_layer`](crate::contains_chunk_layer)). It is derived from
/// the generator's own heights, so it stays right when [`MIN_SURFACE`] or [`MAX_SURFACE`]
/// change; it is only valid while terrain is a heightmap (no caves or overhangs).
///
/// Costs one noise sample per block column of the 34 x 34 area, about one chunk generation.
///
/// # Example
///
/// ```
/// use vox_world::column_span;
///
/// // Today the surface lies inside layer 0 everywhere.
/// assert_eq!(column_span(1, 5, -3), 0..=0);
/// ```
pub fn column_span(seed: u32, cx: i32, cz: i32) -> RangeInclusive<i32> {
    column_span_with(|x, z| surface_height(seed, x, z), cx, cz)
}

/// [`column_span`] for any heightmap `height(x, z)` (world block columns), so the scan can be
/// tested with a surface that has a pit or a peak exactly where it matters.
fn column_span_with(height: impl Fn(i32, i32) -> i32, cx: i32, cz: i32) -> RangeInclusive<i32> {
    let origin_x = cx.saturating_mul(CHUNK_SIZE_I32);
    let origin_z = cz.saturating_mul(CHUNK_SIZE_I32);
    let inner = 0..CHUNK_SIZE_I32;
    let mut lowest = i32::MAX;
    let mut highest = i32::MIN;
    for dz in -SPAN_BORDER..CHUNK_SIZE_I32 + SPAN_BORDER {
        for dx in -SPAN_BORDER..CHUNK_SIZE_I32 + SPAN_BORDER {
            let sample = height(origin_x.saturating_add(dx), origin_z.saturating_add(dz));
            lowest = lowest.min(sample);
            if inner.contains(&dx) && inner.contains(&dz) {
                highest = highest.max(sample);
            }
        }
    }
    span_of(lowest, highest)
}

/// The chunk layers of the surface heights `lowest..=highest`, clamped to the world.
fn span_of(lowest: i32, highest: i32) -> RangeInclusive<i32> {
    let layer = |height: i32| chunk_layer_of(height).clamp(LOWEST_CHUNK_LAYER, HIGHEST_CHUNK_LAYER);
    layer(lowest)..=layer(highest)
}

/// World height of the grass block in the world column `(x, z)`, always in
/// `MIN_SURFACE..=MAX_SURFACE`.
pub(crate) fn surface_height(seed: u32, x: i32, z: i32) -> i32 {
    let span = (MAX_SURFACE - MIN_SURFACE) as f32;
    // Adding 0.5 and truncating rounds to nearest for non-negative values.
    let offset = (octave_sum(seed, x, z) * span + 0.5) as i32;
    (MIN_SURFACE as i32 + offset).min(MAX_SURFACE as i32)
}

/// Writes one column of a chunk whose lowest block is at world height `base_y`.
///
/// The layer boundaries are worked out once, as heights inside the chunk, and each layer
/// is then filled by a plain loop: stone below `stone_end`, dirt up to `dirt_end`, one grass
/// block, and air above, which is left as it is. The boundaries are clamped to the chunk,
/// so a chunk that holds only part of the column (or none of it) gets an empty range.
fn fill_column(
    chunk: &mut Chunk,
    x: usize,
    z: usize,
    base_y: i32,
    height: i32,
    blocks: &TerrainBlocks,
) {
    let grass = height.saturating_sub(base_y);
    let stone_end = clamp_to_chunk(grass.saturating_sub(DIRT_DEPTH_I32));
    let dirt_end = clamp_to_chunk(grass);
    fill_range(chunk, x, z, 0..stone_end, blocks.stone);
    fill_range(chunk, x, z, stone_end..dirt_end, blocks.dirt);
    if (0..CHUNK_SIZE_I32).contains(&grass) {
        fill_range(chunk, x, z, dirt_end..dirt_end + 1, blocks.grass);
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

/// Weighted sum of noise octaves, normalized to `0.0..=1.0`.
fn octave_sum(seed: u32, x: i32, z: i32) -> f32 {
    let mut sum = 0.0;
    for (index, (cell, weight)) in OCTAVES.iter().enumerate() {
        // Each octave gets its own hash stream so the layers do not line up. The index
        // goes through `mix` first, so octave 1 of one seed is not octave 0 of the next.
        let octave_seed = mix(seed ^ mix(index as u32));
        sum += weight * smooth_noise(octave_seed, x, z, *cell);
    }
    (sum / WEIGHT_SUM).clamp(0.0, 1.0)
}

/// Value noise: lattice values every `cell` blocks, blended with smoothstep.
///
/// Coordinates may be negative: floor division puts the lattice cell and the position
/// inside it on the right side of zero, so the noise is continuous across the origin.
/// Cell sizes are powers of two, where the arithmetic shift is the floor division and the
/// mask is the euclidean remainder (a test checks both against `div_euclid` and
/// `rem_euclid`); the shift and mask are about 30 percent cheaper than a signed division
/// per chunk, measured. The
/// lattice hash takes the cell as `u32` by two's complement, which leaves the values for
/// non-negative coordinates exactly as they were before negative ones were supported.
fn smooth_noise(seed: u32, x: i32, z: i32, cell: i32) -> f32 {
    let shift = cell.trailing_zeros();
    let (lx, lz) = ((x >> shift) as u32, (z >> shift) as u32);
    // Integer remainders divided by the cell size are exact in f32 for these small values.
    let tx = smoothstep((x & (cell - 1)) as f32 / cell as f32);
    let tz = smoothstep((z & (cell - 1)) as f32 / cell as f32);
    let near = lerp(
        lattice_value(seed, lx, lz),
        lattice_value(seed, lx.wrapping_add(1), lz),
        tx,
    );
    let far = lerp(
        lattice_value(seed, lx, lz.wrapping_add(1)),
        lattice_value(seed, lx.wrapping_add(1), lz.wrapping_add(1)),
        tx,
    );
    lerp(near, far, tz)
}

/// Number of hash bits that become the noise value. 24 is the width of the `f32`
/// mantissa, so every such integer converts to `f32` exactly.
const LATTICE_BITS: u32 = 24;

/// `2^LATTICE_BITS` as `f32`: dividing a [`LATTICE_BITS`]-bit integer by it gives
/// `0.0..1.0`. A power of two, so the division is exact too.
const LATTICE_SCALE: f32 = (1_u32 << LATTICE_BITS) as f32;

/// The 32-bit golden-ratio multiplier (`2^32 / phi`, Knuth's multiplicative hashing). It
/// spreads the x coordinate over all bits before it is combined with z and the seed.
const GOLDEN_RATIO_32: u32 = 0x9E37_79B1;

/// Pseudo-random value in `0.0..1.0` for a lattice point.
fn lattice_value(seed: u32, x: u32, z: u32) -> f32 {
    let hash = mix(x.wrapping_mul(GOLDEN_RATIO_32) ^ mix(z ^ mix(seed)));
    // Keep the top bits: they are the best mixed ones.
    (hash >> (u32::BITS - LATTICE_BITS)) as f32 / LATTICE_SCALE
}

/// Shifts and multipliers of the `lowbias32` mix, in the order of [`mix`]. They are the
/// constants found by Chris Wellons' search for a 32-bit hash with good avalanche
/// (<https://nullprogram.com/blog/2018/07/31/>); other values would hash worse.
/// The three right shifts of the mix.
const MIX_SHIFTS: [u32; 3] = [16, 15, 16];
/// The two multipliers of the mix.
const MIX_MULTIPLIERS: [u32; 2] = [0x7FEB_352D, 0x846C_A68B];

/// The `lowbias32` integer mix by Chris Wellons: a cheap 32-bit hash with good avalanche.
fn mix(mut value: u32) -> u32 {
    value ^= value >> MIX_SHIFTS[0];
    value = value.wrapping_mul(MIX_MULTIPLIERS[0]);
    value ^= value >> MIX_SHIFTS[1];
    value = value.wrapping_mul(MIX_MULTIPLIERS[1]);
    value ^ (value >> MIX_SHIFTS[2])
}

/// Smooth ease curve for `t` in `0.0..=1.0`: zero slope at both ends.
fn smoothstep(t: f32) -> f32 {
    t * t * (3.0 - 2.0 * t)
}

/// Linear interpolation from `a` to `b`.
fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bounds::{HIGHEST_CHUNK_LAYER, LOWEST_CHUNK_LAYER};
    use crate::mesh::{Face, build_mesh};
    use crate::world::Neighbours;

    const BEDROCK: BlockId = TEST_BLOCKS.bedrock;
    const GRASS: BlockId = TEST_BLOCKS.grass;
    const DIRT: BlockId = TEST_BLOCKS.dirt;
    const STONE: BlockId = TEST_BLOCKS.stone;

    // The tests always use `TEST_BLOCKS` (ids 1 to 4, see there), so these
    // shadow the public functions with the blocks filled in.
    fn generate_terrain(seed: u32) -> Chunk {
        super::generate_terrain(seed, &TEST_BLOCKS)
    }

    fn generate_chunk(seed: u32, pos: ChunkPos) -> Chunk {
        super::generate_chunk(seed, pos, &TEST_BLOCKS)
    }

    fn block_at(chunk: &Chunk, x: usize, y: usize, z: usize) -> BlockId {
        LocalPos::new(x, y, z).map_or(BlockId::AIR, |pos| chunk.get(pos))
    }

    /// Surface height of a column read back from the chunk: the grass block's `y`.
    fn grass_heights(chunk: &Chunk, x: usize, z: usize) -> Vec<usize> {
        (0..CHUNK_SIZE)
            .filter(|&y| block_at(chunk, x, y, z) == GRASS)
            .collect()
    }

    fn same_blocks(a: &Chunk, b: &Chunk) -> bool {
        LocalPos::all().all(|pos| a.get(pos) == b.get(pos))
    }

    #[test]
    fn block_at_depth_agrees_with_fill_column() {
        for height in [-70, -3, 0, 5, 9, 31, 32, 40] {
            for base_y in [-64, -32, 0, 32] {
                let mut chunk = Chunk::new();
                fill_column(&mut chunk, 0, 0, base_y, height, &TEST_BLOCKS);
                for y in 0..CHUNK_SIZE {
                    let world_y = base_y + y as i32;
                    assert_eq!(
                        block_at(&chunk, 0, y, 0),
                        block_at_depth(height - world_y, &TEST_BLOCKS),
                        "height {height}, y {world_y}"
                    );
                }
            }
        }
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
    fn every_column_is_grass_over_dirt_over_stone_under_air() {
        for seed in [1, 2, 99] {
            let chunk = generate_terrain(seed);
            for z in 0..CHUNK_SIZE {
                for x in 0..CHUNK_SIZE {
                    let grass = grass_heights(&chunk, x, z);
                    assert_eq!(grass.len(), 1, "column ({x}, {z}) of seed {seed}");
                    assert_column_layers(&chunk, x, z, grass[0]);
                }
            }
        }
    }

    fn assert_column_layers(chunk: &Chunk, x: usize, z: usize, top: usize) {
        for y in 0..CHUNK_SIZE {
            let expected = if y > top {
                BlockId::AIR
            } else if y == top {
                GRASS
            } else if y + DIRT_DEPTH >= top {
                DIRT
            } else {
                STONE
            };
            assert_eq!(block_at(chunk, x, y, z), expected, "at ({x}, {y}, {z})");
        }
    }

    #[test]
    fn surface_height_stays_in_stated_range() {
        for seed in 0..20 {
            let chunk = generate_terrain(seed);
            for z in 0..CHUNK_SIZE {
                for x in 0..CHUNK_SIZE {
                    let top = grass_heights(&chunk, x, z)[0];
                    assert!((MIN_SURFACE..=MAX_SURFACE).contains(&top));
                }
            }
        }
    }

    #[test]
    fn terrain_is_not_flat() {
        let chunk = generate_terrain(1);
        let mut heights: Vec<usize> = (0..CHUNK_SIZE)
            .flat_map(|z| (0..CHUNK_SIZE).map(move |x| (x, z)))
            .map(|(x, z)| grass_heights(&chunk, x, z)[0])
            .collect();
        heights.sort_unstable();
        heights.dedup();
        assert!(
            heights.len() >= 3,
            "only {} distinct heights",
            heights.len()
        );
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
        // the hash or the layers on purpose, recompute these numbers (the failure prints them).
        assert_eq!(
            fingerprint(&generate_terrain(1)),
            14_581_174_898_471_107_366
        );
        assert_eq!(
            fingerprint(&generate_terrain(2)),
            13_175_094_009_649_080_753
        );
    }

    /// Quads of the greedy mesh of `generate_terrain(1)`.
    const GREEDY_QUADS: usize = 760;

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
        assert_eq!(naive.quad_count(), 4848);
        assert_eq!(
            build_mesh(&chunk, &crate::mesh::identity_textures()).quad_count(),
            GREEDY_QUADS
        );
    }

    /// The most the surface height can change between two adjacent columns. The noise has
    /// three octaves with weights 4, 2, 1 (sum 7) and cells 16, 8, 4. Value noise with a
    /// smoothstep blend changes by at most 1.5 times the lattice range (1.0) per cell, so
    /// per block an octave moves the normalized sum by `weight / 7 * 1.5 / cell`, which is
    /// 0.0536 for each of the three octaves, 0.161 in total. Times the height span 16 that
    /// is 2.57 blocks, and rounding to whole blocks adds less than 1 (the clamp only
    /// shrinks differences): below 3.57, so at most 3.
    const MAX_STEP: i32 = 3;

    fn chunk_at(seed: u32, x: i32, y: i32, z: i32) -> Chunk {
        generate_chunk(seed, ChunkPos::new(x, y, z))
    }

    /// World height of the grass of a column of the chunk at `pos`, read back from the blocks.
    fn world_grass(chunk: &Chunk, pos: ChunkPos, x: usize, z: usize) -> i32 {
        let heights = grass_heights(chunk, x, z);
        assert_eq!(heights.len(), 1, "column ({x}, {z}) of chunk {pos:?}");
        pos.origin()[1] + heights[0] as i32
    }

    fn assert_close(a: i32, b: i32, what: &str) {
        assert!((a - b).abs() <= MAX_STEP, "{what}: heights {a} and {b}");
    }

    #[test]
    fn origin_chunk_equals_generate_terrain() {
        for seed in [0, 1, 2, 99, u32::MAX] {
            assert!(same_blocks(
                &generate_chunk(seed, ChunkPos::ORIGIN),
                &generate_terrain(seed)
            ));
        }
    }

    /// Checks that the columns on the shared border of `first` and `second` (where
    /// `second` lies one chunk further along `face`) are at most `MAX_STEP` apart.
    fn assert_seam(seed: u32, first: ChunkPos, face: Face) {
        let second = first.neighbour(face);
        let (a, b) = (generate_chunk(seed, first), generate_chunk(seed, second));
        let last = CHUNK_SIZE - 1;
        for i in 0..CHUNK_SIZE {
            let ((ax, az), (bx, bz)) = match face {
                Face::PosX => ((last, i), (0, i)),
                _ => ((i, last), (i, 0)),
            };
            assert_close(
                world_grass(&a, first, ax, az),
                world_grass(&b, second, bx, bz),
                &format!("seed {seed}, {first:?} towards {face:?}, index {i}"),
            );
        }
    }

    #[test]
    fn heights_are_close_across_chunk_borders() {
        for seed in [1, 2, 3, 99, 12_345] {
            assert_seam(seed, ChunkPos::new(0, 0, 0), Face::PosX);
            assert_seam(seed, ChunkPos::new(-1, 0, 0), Face::PosX);
            assert_seam(seed, ChunkPos::new(0, 0, 0), Face::PosZ);
            assert_seam(seed, ChunkPos::new(0, 0, -1), Face::PosZ);
            assert_seam(seed, ChunkPos::new(-3, 0, -2), Face::PosX);
        }
    }

    /// Surface height the generator must use for the column `(x, z)` of the chunk at
    /// `pos`. The world column is computed here by hand (32 blocks per chunk), not with
    /// `ChunkPos::origin`, so the test does not share the code it checks.
    fn expected_height(seed: u32, pos: ChunkPos, x: usize, z: usize) -> i32 {
        surface_height(seed, pos.x * 32 + x as i32, pos.z * 32 + z as i32)
    }

    #[test]
    fn every_column_matches_the_height_of_its_world_column() {
        for seed in [1, 2, 3, 99] {
            for (px, pz) in [(0, 0), (1, 0), (-1, 0), (0, 1), (0, -1), (3, -2), (-4, -4)] {
                let pos = ChunkPos::new(px, 0, pz);
                let chunk = generate_chunk(seed, pos);
                for z in 0..CHUNK_SIZE {
                    for x in 0..CHUNK_SIZE {
                        assert_eq!(
                            world_grass(&chunk, pos, x, z),
                            expected_height(seed, pos, x, z),
                            "seed {seed}, chunk {pos:?}, column ({x}, {z})"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn border_columns_use_the_world_columns_on_both_sides_of_a_border() {
        // Literal world columns: chunk (1, 0, 0) starts at world x = 32, chunk (0, 0, 0)
        // ends at 31 and chunk (-1, 0, 0) ends at -1. A chunk origin that is off by one
        // column would put a different column in each of these places.
        for seed in [1, 2, 3, 99] {
            for i in 0..CHUNK_SIZE {
                let along = i as i32;
                let at = |x: i32, y: i32, z: i32, cx: usize, cz: usize| {
                    let pos = ChunkPos::new(x, y, z);
                    world_grass(&generate_chunk(seed, pos), pos, cx, cz)
                };
                let last = CHUNK_SIZE - 1;
                assert_eq!(at(0, 0, 0, last, i), surface_height(seed, 31, along));
                assert_eq!(at(1, 0, 0, 0, i), surface_height(seed, 32, along));
                assert_eq!(at(-1, 0, 0, last, i), surface_height(seed, -1, along));
                assert_eq!(at(0, 0, 0, i, last), surface_height(seed, along, 31));
                assert_eq!(at(0, 0, 1, i, 0), surface_height(seed, along, 32));
                assert_eq!(at(0, 0, -1, i, last), surface_height(seed, along, -1));
            }
        }
    }

    #[test]
    fn noise_is_continuous_across_zero_in_both_directions() {
        // A step of one block in world space moves the height by at most `MAX_STEP`,
        // also from -1 to 0, where floor division switches to the next lattice cell.
        for seed in [1, 2, 3, 99] {
            for z in -70..70 {
                for x in -70..70 {
                    let here = surface_height(seed, x, z);
                    assert_close(here, surface_height(seed, x + 1, z), "x step");
                    assert_close(here, surface_height(seed, x, z + 1), "z step");
                }
            }
        }
    }

    #[test]
    fn negative_chunks_have_valid_terrain() {
        for pos in [
            ChunkPos::new(-1, 0, -1),
            ChunkPos::new(-5, 0, 3),
            ChunkPos::new(2, 0, -7),
        ] {
            let chunk = generate_chunk(1, pos);
            for z in 0..CHUNK_SIZE {
                for x in 0..CHUNK_SIZE {
                    let grass = grass_heights(&chunk, x, z);
                    assert_eq!(grass.len(), 1, "column ({x}, {z}) of {pos:?}");
                    assert!((MIN_SURFACE..=MAX_SURFACE).contains(&grass[0]));
                    assert_column_layers(&chunk, x, z, grass[0]);
                }
            }
        }
    }

    #[test]
    fn chunks_above_the_surface_are_air_and_below_are_stone() {
        for seed in [1, 2] {
            let above = chunk_at(seed, 4, 1, -3);
            let below = chunk_at(seed, 4, -1, -3);
            let deep = chunk_at(seed, 0, LOWEST_CHUNK_LAYER + 1, 0);
            assert!(LocalPos::all().all(|pos| above.get(pos).is_air()));
            assert!(LocalPos::all().all(|pos| below.get(pos) == STONE));
            assert!(LocalPos::all().all(|pos| deep.get(pos) == STONE));
        }
    }

    #[test]
    fn the_floor_row_is_bedrock_and_the_rest_of_the_floor_layer_is_stone() {
        for seed in [1, 2, 99] {
            let chunk = chunk_at(seed, -3, LOWEST_CHUNK_LAYER, 4);
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

    /// A flat surface at height 40 (layer 1) with one block column changed.
    fn flat_with(spot: (i32, i32), height: i32) -> impl Fn(i32, i32) -> i32 {
        move |x, z| if (x, z) == spot { height } else { 40 }
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
        for spot in [(-1, 5), (32, 5), (5, -1), (5, 32)] {
            assert_eq!(
                column_span_with(flat_with(spot, peak), 0, 0),
                1..=1,
                "{spot:?}"
            );
        }
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
    fn the_span_is_layer_zero_today() {
        for seed in [0, 1, 2, 99, u32::MAX] {
            for (cx, cz) in [(0, 0), (-1, -1), (7, -3), (-20, 15)] {
                assert_eq!(column_span(seed, cx, cz), 0..=0, "seed {seed} ({cx}, {cz})");
            }
        }
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
    fn the_span_covers_exactly_the_layers_of_the_lowest_and_highest_surface() {
        // Brute force over the 34 x 34 area with literal world coordinates.
        for seed in [1, 2, 3, 99] {
            for (cx, cz) in [(0, 0), (1, 0), (-1, 0), (0, -1), (4, 9), (-6, -2)] {
                let heights = |range: std::ops::RangeInclusive<i32>| {
                    let (x0, z0) = (cx * 32, cz * 32);
                    let mut all = Vec::new();
                    for z in range.clone() {
                        for x in range.clone() {
                            all.push(surface_height(seed, x0 + x, z0 + z));
                        }
                    }
                    all
                };
                let outer = heights(-1..=32);
                let inner = heights(0..=31);
                let span = column_span(seed, cx, cz);
                assert_eq!(*span.start(), layer_of(*outer.iter().min().unwrap()));
                assert_eq!(*span.end(), layer_of(*inner.iter().max().unwrap()));
            }
        }
    }

    /// The blocks of the layers `LOWEST - 1..=HIGHEST + 1` of one column of chunks.
    fn column_layers(seed: u32, cx: i32, cz: i32) -> Vec<Chunk> {
        (LOWEST_CHUNK_LAYER - 1..=HIGHEST_CHUNK_LAYER + 1)
            .map(|y| generate_chunk(seed, ChunkPos::new(cx, y, cz)))
            .collect()
    }

    fn layer_index(layer: i32) -> usize {
        usize::try_from(layer - (LOWEST_CHUNK_LAYER - 1)).unwrap()
    }

    #[test]
    fn layers_outside_the_span_are_solid_below_and_air_above() {
        for seed in 0..6 {
            for (cx, cz) in [(0, 0), (3, -4), (-5, 2)] {
                let span = column_span(seed, cx, cz);
                for (layer, chunk) in (LOWEST_CHUNK_LAYER - 1..).zip(column_layers(seed, cx, cz)) {
                    if layer < *span.start() {
                        assert!(
                            LocalPos::all().all(|pos| !chunk.get(pos).is_air()),
                            "seed {seed} ({cx}, {cz}) layer {layer} below the span"
                        );
                    } else if layer > *span.end() {
                        assert!(
                            LocalPos::all().all(|pos| chunk.get(pos).is_air()),
                            "seed {seed} ({cx}, {cz}) layer {layer} above the span"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn no_layer_outside_the_span_meshes_to_anything() {
        let textures = crate::mesh::identity_textures();
        let mut mesh = crate::ChunkMesh::default();
        for seed in 0..8 {
            for (cx, cz) in [(0, 0), (1, 0), (-1, 3), (6, -6)] {
                let span = column_span(seed, cx, cz);
                let own = column_layers(seed, cx, cz);
                let sides = [(1, 0), (-1, 0), (0, 1), (0, -1)]
                    .map(|(dx, dz)| column_layers(seed, cx + dx, cz + dz));
                for layer in LOWEST_CHUNK_LAYER..=HIGHEST_CHUNK_LAYER {
                    if span.contains(&layer) {
                        continue;
                    }
                    let at = layer_index(layer);
                    let neighbours = Neighbours::new([
                        Some(&sides[0][at]),
                        Some(&sides[1][at]),
                        Some(&own[at + 1]),
                        Some(&own[at - 1]),
                        Some(&sides[2][at]),
                        Some(&sides[3][at]),
                    ]);
                    crate::build_mesh_with_neighbours_into(
                        &own[at],
                        &neighbours,
                        &textures,
                        &mut mesh,
                    );
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
    fn same_seed_and_far_position_give_identical_chunk() {
        let far = ChunkPos::new(3, 0, -2);
        assert!(same_blocks(
            &generate_chunk(5, far),
            &generate_chunk(5, far)
        ));
    }

    #[test]
    fn different_positions_give_different_chunks() {
        let here = chunk_at(1, 0, 0, 0);
        for (x, z) in [(1, 0), (-1, 0), (0, 1), (0, -1), (3, -2)] {
            assert!(
                !same_blocks(&here, &chunk_at(1, x, 0, z)),
                "chunk ({x}, {z})"
            );
        }
    }

    #[test]
    fn far_chunk_matches_pinned_fingerprint() {
        // Pins a chunk with negative Z, so a change of the floor division or of the way
        // the lattice hash takes negative cells shows up. Recompute on purpose only.
        assert_eq!(
            fingerprint(&chunk_at(1, 1, 0, -1)),
            13_859_822_351_980_690_189
        );
    }

    #[test]
    fn shift_and_mask_equal_floor_division_and_euclidean_remainder() {
        for (cell, _) in OCTAVES {
            let shift = cell.trailing_zeros();
            for x in -200_i32..200 {
                assert_eq!(x >> shift, x.div_euclid(cell), "x {x}, cell {cell}");
                assert_eq!(x & (cell - 1), x.rem_euclid(cell), "x {x}, cell {cell}");
            }
        }
    }

    #[test]
    fn octave_weights_are_normalized() {
        let total: f32 = OCTAVES.iter().map(|(_, weight)| weight).sum();
        assert_eq!(WEIGHT_SUM, total);
    }
}
