//! Demo terrain: one chunk of rolling hills from deterministic value noise.
//!
//! Everything here is integer hashing plus plain `f32` arithmetic (`+`, `-`, `*`, `/`
//! are exactly specified by IEEE 754), with no `sin` or other libm functions, so a seed
//! produces the same chunk on every platform. It runs once per chunk for now and is
//! neither optimized nor measured.

use crate::block::BlockId;
use crate::chunk::{CHUNK_SIZE, Chunk, LocalPos};

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

/// Number of dirt blocks between the grass and the stone.
const DIRT_DEPTH: usize = 3;

/// Cell size in blocks and weight of each noise octave.
/// Large hills first, finer detail on top with less influence.
const OCTAVES: [(u32, f32); 3] = [(16, 4.0), (8, 2.0), (4, 1.0)];

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

/// Generates a chunk of rolling hills. The same `seed` always gives the same chunk.
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
    let mut chunk = Chunk::new();
    for z in 0..CHUNK_SIZE {
        for x in 0..CHUNK_SIZE {
            let height = surface_height(seed, x as u32, z as u32);
            fill_column(&mut chunk, x, z, height);
        }
    }
    chunk
}

/// Surface height of the column `(x, z)`, always in `MIN_SURFACE..=MAX_SURFACE`.
fn surface_height(seed: u32, x: u32, z: u32) -> usize {
    let span = (MAX_SURFACE - MIN_SURFACE) as f32;
    // Adding 0.5 and truncating rounds to nearest for non-negative values.
    let offset = (octave_sum(seed, x, z) * span + 0.5) as usize;
    (MIN_SURFACE + offset).min(MAX_SURFACE)
}

/// Writes grass, dirt and stone of one column; the rest stays air.
fn fill_column(chunk: &mut Chunk, x: usize, z: usize, height: usize) {
    for y in 0..=height {
        let block = if y == height {
            GRASS
        } else if y + DIRT_DEPTH >= height {
            DIRT
        } else {
            STONE
        };
        // `LocalPos::new` cannot fail here: `x` and `z` come from the chunk loops and
        // `height` is at most `MAX_SURFACE`, below `CHUNK_SIZE`. The library never
        // panics, so the impossible case is skipped instead of unwrapped.
        if let Some(pos) = LocalPos::new(x, y, z) {
            chunk.set(pos, block);
        }
    }
}

/// Weighted sum of noise octaves, normalized to `0.0..=1.0`.
fn octave_sum(seed: u32, x: u32, z: u32) -> f32 {
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
fn smooth_noise(seed: u32, x: u32, z: u32, cell: u32) -> f32 {
    let (cx, cz) = (x / cell, z / cell);
    // Integer remainders divided by the cell size are exact in f32 for these small values.
    let tx = smoothstep((x % cell) as f32 / cell as f32);
    let tz = smoothstep((z % cell) as f32 / cell as f32);
    let near = lerp(
        lattice_value(seed, cx, cz),
        lattice_value(seed, cx + 1, cz),
        tx,
    );
    let far = lerp(
        lattice_value(seed, cx, cz + 1),
        lattice_value(seed, cx + 1, cz + 1),
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
    use crate::mesh::build_mesh;

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

    #[test]
    fn octave_weights_are_normalized() {
        let total: f32 = OCTAVES.iter().map(|(_, weight)| weight).sum();
        assert_eq!(WEIGHT_SUM, total);
    }
}
