//! Demo terrain: rolling hills from deterministic value noise, generated per chunk.
//!
//! Everything here is integer hashing plus plain `f32` arithmetic (`+`, `-`, `*`, `/`
//! are exactly specified by IEEE 754), with no `sin` or other libm functions, so a seed
//! produces the same chunk on every platform. The noise is sampled at world coordinates,
//! so chunks next to each other fit together. It is neither optimized nor measured beyond
//! `docs/benchmarks.md`.

use crate::block::BlockId;
use crate::chunk::{CHUNK_SIZE, Chunk, LocalPos};
use crate::chunk_pos::ChunkPos;

/// Grass, the top block of every column.
///
/// The `GRASS`, `DIRT` and `STONE` ids are a temporary palette until the block registry
/// exists, where blocks get their names and properties.
pub const GRASS: BlockId = BlockId(1);
/// Dirt, the layer right below the grass. See [`GRASS`] for the palette note.
pub const DIRT: BlockId = BlockId(2);
/// Stone, everything below the dirt. See [`GRASS`] for the palette note.
pub const STONE: BlockId = BlockId(3);

/// Lowest possible surface height (the `y` of the grass block).
pub const MIN_SURFACE: usize = 8;
/// Highest possible surface height (the `y` of the grass block).
pub const MAX_SURFACE: usize = 24;

// The columns are filled up to `MAX_SURFACE`, so it must lie inside the chunk.
const _: () = assert!(MAX_SURFACE < CHUNK_SIZE);

// `smooth_noise` splits coordinates with a shift and a mask, which needs power-of-two cells.
const _: () = assert!(OCTAVES[0].0.count_ones() == 1);
const _: () = assert!(OCTAVES[1].0.count_ones() == 1);
const _: () = assert!(OCTAVES[2].0.count_ones() == 1);

/// Number of dirt blocks between the grass and the stone.
const DIRT_DEPTH: usize = 3;

/// Cell size in blocks and weight of each noise octave.
/// Large hills first, finer detail on top with less influence.
const OCTAVES: [(i32, f32); 3] = [(16, 4.0), (8, 2.0), (4, 1.0)];

/// Sum of the octave weights, the divisor that normalizes the noise to `0.0..=1.0`.
/// Computed from [`OCTAVES`], so the two cannot drift apart.
const WEIGHT_SUM: f32 = weight_sum();

const fn weight_sum() -> f32 {
    let mut sum = 0.0;
    let mut index = 0;
    while index < OCTAVES.len() {
        sum += OCTAVES[index].1;
        index += 1;
    }
    sum
}

/// `CHUNK_SIZE` as the integer type of world coordinates (the size is 32, so it always fits).
const CHUNK_SIZE_I32: i32 = CHUNK_SIZE as i32;

/// [`DIRT_DEPTH`] as a world height difference.
const DIRT_DEPTH_I32: i32 = DIRT_DEPTH as i32;

/// Generates the chunk at the origin: rolling hills. The same `seed` always gives the same
/// chunk. It is [`generate_chunk`] at [`ChunkPos::ORIGIN`].
///
/// Every column has one [`GRASS`] block at a height in
/// [`MIN_SURFACE`]`..=`[`MAX_SURFACE`], [`DIRT`] below it, [`STONE`] under that and air above.
///
/// # Example
///
/// ```
/// use vox_world::{generate_terrain, LocalPos, GRASS};
///
/// let chunk = generate_terrain(1);
/// let has_grass = LocalPos::all().any(|pos| chunk.get(pos) == GRASS);
/// assert!(has_grass);
/// ```
pub fn generate_terrain(seed: u32) -> Chunk {
    generate_chunk(seed, ChunkPos::ORIGIN)
}

/// Generates the terrain of the chunk at `pos`. The same `seed` and `pos` always give the
/// same chunk.
///
/// The surface height of a column depends only on its world column `(x, z)`, never on the
/// chunk, so neighbouring chunks agree along their borders and the hills continue across
/// them. For a block at world height `y` and a column with surface height `h` (in
/// [`MIN_SURFACE`]`..=`[`MAX_SURFACE`]): above `h` is air, `y == h` is [`GRASS`], the three
/// blocks below are [`DIRT`], everything deeper is [`STONE`].
///
/// Consequently every chunk with `pos.y >= 1` is all air (its lowest block is at height 32,
/// above [`MAX_SURFACE`]) and every chunk with `pos.y < 0` is all stone (its highest block
/// is at height -1, more than three below [`MIN_SURFACE`]).
///
/// # Example
///
/// ```
/// use vox_world::{generate_chunk, ChunkPos, LocalPos, GRASS};
///
/// let chunk = generate_chunk(1, ChunkPos::new(-1, 0, 2));
/// assert!(LocalPos::all().any(|pos| chunk.get(pos) == GRASS));
/// ```
pub fn generate_chunk(seed: u32, pos: ChunkPos) -> Chunk {
    let [origin_x, origin_y, origin_z] = pos.origin();
    let mut chunk = Chunk::new();
    for z in 0..CHUNK_SIZE {
        for x in 0..CHUNK_SIZE {
            // Saturating, like `ChunkPos::origin`: far outside the usable world the
            // terrain degrades but nothing wraps or panics.
            let world_x = origin_x.saturating_add(x as i32);
            let world_z = origin_z.saturating_add(z as i32);
            let height = surface_height(seed, world_x, world_z);
            fill_column(&mut chunk, x, z, origin_y, height);
        }
    }
    chunk
}

/// World height of the grass block in the world column `(x, z)`, always in
/// `MIN_SURFACE..=MAX_SURFACE`.
fn surface_height(seed: u32, x: i32, z: i32) -> i32 {
    let span = (MAX_SURFACE - MIN_SURFACE) as f32;
    // Adding 0.5 and truncating rounds to nearest for non-negative values.
    let offset = (octave_sum(seed, x, z) * span + 0.5) as i32;
    (MIN_SURFACE as i32 + offset).min(MAX_SURFACE as i32)
}

/// The block at world height `y` in a column whose grass is at `height`.
fn block_for(y: i32, height: i32) -> BlockId {
    if y > height {
        BlockId::AIR
    } else if y == height {
        GRASS
    } else if y.saturating_add(DIRT_DEPTH_I32) >= height {
        DIRT
    } else {
        STONE
    }
}

/// Writes one column of a chunk whose lowest block is at world height `base_y`.
/// Everything above the surface stays air, so only the part up to it is written.
fn fill_column(chunk: &mut Chunk, x: usize, z: usize, base_y: i32, height: i32) {
    let top = height.saturating_sub(base_y).min(CHUNK_SIZE_I32 - 1);
    // An empty range when the whole column of this chunk lies above the surface.
    for y in 0..=top {
        let block = block_for(base_y.saturating_add(y), height);
        // `LocalPos::new` cannot fail here: `x` and `z` come from the chunk loops and
        // `y` is in `0..CHUNK_SIZE`. The library never panics, so the impossible case is
        // skipped instead of unwrapped.
        if let Some(pos) = LocalPos::new(x, y as usize, z) {
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

/// Pseudo-random value in `0.0..1.0` for a lattice point.
fn lattice_value(seed: u32, x: u32, z: u32) -> f32 {
    let hash = mix(x.wrapping_mul(0x9E37_79B1) ^ mix(z ^ mix(seed)));
    // The top 24 bits fit the f32 mantissa exactly, and 2^24 is a power of two.
    (hash >> 8) as f32 / 16_777_216.0
}

/// The `lowbias32` integer mix by Chris Wellons: a cheap 32-bit hash with good avalanche.
fn mix(mut value: u32) -> u32 {
    value ^= value >> 16;
    value = value.wrapping_mul(0x7FEB_352D);
    value ^= value >> 15;
    value = value.wrapping_mul(0x846C_A68B);
    value ^ (value >> 16)
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
    use crate::mesh::{Face, build_mesh};

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

    #[test]
    fn terrain_mesh_has_the_pinned_number_of_quads() {
        // The exact count for seed 1, so a change in the terrain or the mesher shows up.
        // Recompute it when either changes on purpose.
        let mesh = build_mesh(&generate_terrain(1));
        assert_eq!(mesh.indices.len() / 6, 4848);
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

    #[test]
    fn two_chunks_match_one_height_function_over_the_whole_area() {
        for seed in [1, 2, 3, 99] {
            for pos in [
                ChunkPos::new(0, 0, 0),
                ChunkPos::new(1, 0, 0),
                ChunkPos::new(-1, 0, 0),
            ] {
                let chunk = generate_chunk(seed, pos);
                let [origin_x, _, origin_z] = pos.origin();
                for z in 0..CHUNK_SIZE {
                    for x in 0..CHUNK_SIZE {
                        let expected =
                            surface_height(seed, origin_x + x as i32, origin_z + z as i32);
                        assert_eq!(
                            world_grass(&chunk, pos, x, z),
                            expected,
                            "seed {seed}, chunk {pos:?}, column ({x}, {z})"
                        );
                    }
                }
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
            let deep = chunk_at(seed, 0, -40, 0);
            assert!(LocalPos::all().all(|pos| above.get(pos).is_air()));
            assert!(LocalPos::all().all(|pos| below.get(pos) == STONE));
            assert!(LocalPos::all().all(|pos| deep.get(pos) == STONE));
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
