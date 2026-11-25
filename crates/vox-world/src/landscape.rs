//! The shape of the world: for every world column `(x, z)` the height of the ground, the
//! level of the water above it and the biome (decision 0017).
//!
//! Everything is a pure function of the seed and the column, so any chunk, level-of-detail
//! tile or span scan that asks gets the same answer. The height comes from a few fractal
//! noise fields (continents, hills, mountains, rivers), lakes are carved from a hash-placed
//! bowl per lake cell, and the biome follows from the height, the water and two climate
//! fields (temperature and humidity).

use crate::bounds::WORLD_FLOOR_Y;
use crate::noise::{Fractal, cell_hash, lerp, ramp, smooth_noise, smoothstep};
use crate::trees::tree_stands_at;

/// World height of the sea surface: the highest water block of every sea and river.
pub const SEA_LEVEL: i32 = 64;

/// Lowest possible ground height (the `y` of the top solid block). The ocean floor is
/// clamped to it; it lies far above the world floor.
pub const MIN_SURFACE: i32 = 16;

/// Highest possible ground height (the `y` of the top solid block): the mountain peaks.
/// 160 is five chunk layers of mountains over the sea, so columns there have several layers.
pub const MAX_SURFACE: i32 = 160;

/// Number of filler blocks (dirt under grass, sand under sand) between the surface block and
/// the stone.
pub(crate) const FILLER_DEPTH: i32 = 3;

// The ground lies above the bedrock row, with room for the filler, so the bedrock row is
// always inside the stone.
const _: () = assert!(MIN_SURFACE - FILLER_DEPTH > WORLD_FLOOR_Y);
const _: () = assert!(MIN_SURFACE < SEA_LEVEL && SEA_LEVEL < MAX_SURFACE);

/// The biome of a column, which decides its surface block and whether trees grow.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Biome {
    /// Open sea: the ground is below [`SEA_LEVEL`] and no river or lake.
    Ocean,
    /// The bed of a river valley below the sea level.
    River,
    /// The bed of a lake (water above the ground, at the lake's own level).
    Lake,
    /// A strip of sand just above the sea level, also on low river banks.
    Beach,
    /// Grassland with a few trees.
    Plains,
    /// Grassland with many trees.
    Forest,
    /// Hot and dry: sand and no trees.
    Desert,
    /// High ground: bare stone, snow on the peaks.
    Mountains,
}

impl Biome {
    /// Whether trees grow here (on the grass of plains and forests only).
    pub const fn has_trees(self) -> bool {
        matches!(self, Self::Plains | Self::Forest)
    }
}

/// What the generator knows about one world column.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ColumnInfo {
    /// Height of the top solid block.
    pub ground: i32,
    /// Height of the highest water block that may fill the air above the ground. It is only
    /// water if it lies above `ground`: [`ColumnInfo::has_water`].
    pub water_level: i32,
    /// The biome.
    pub biome: Biome,
}

impl ColumnInfo {
    /// Whether the air right above the ground is filled with water.
    pub(crate) const fn has_water(&self) -> bool {
        self.water_level > self.ground
    }

    /// Height of the highest block of the terrain without trees: the water surface or the
    /// ground.
    pub(crate) const fn top(&self) -> i32 {
        if self.has_water() {
            self.water_level
        } else {
            self.ground
        }
    }
}

// ---- Height: continents, hills, mountains, rivers ----

/// Stream numbers of the noise fields, so they are unrelated for one seed.
const CONTINENT_STREAM: u32 = 1;
const HILLS_STREAM: u32 = 2;
const MOUNTAIN_MASK_STREAM: u32 = 3;
const RIDGE_STREAM: u32 = 4;
const RIVER_STREAM: u32 = 5;
const TEMPERATURE_STREAM: u32 = 6;
const HUMIDITY_STREAM: u32 = 7;
/// Stream of the lake hash and of the lake shape noise.
const LAKE_STREAM: u32 = 8;

/// Land against sea. The biggest cell is 512 blocks, so seas and continents are about a
/// thousand blocks wide; the finer octaves make bays and islands.
const CONTINENT: Fractal = Fractal::new(CONTINENT_STREAM, &[(512, 8.0), (128, 3.0), (32, 1.0)]);
/// Rolling hills, from broad swells to small bumps.
const HILLS: Fractal = Fractal::new(HILLS_STREAM, &[(64, 4.0), (16, 2.0), (8, 1.0)]);
/// Where mountains rise: ranges a few hundred blocks wide.
const MOUNTAIN_MASK: Fractal = Fractal::new(MOUNTAIN_MASK_STREAM, &[(256, 2.0), (128, 1.0)]);
/// The shape of the peaks inside a range (folded into ridges by [`Landscape::natural`]).
const RIDGE: Fractal = Fractal::new(RIDGE_STREAM, &[(64, 2.0), (32, 1.0)]);
/// Rivers run where this field is near the middle of its range.
const RIVER: Fractal = Fractal::new(RIVER_STREAM, &[(256, 3.0), (64, 1.0)]);
/// Hot against cold.
const TEMPERATURE: Fractal = Fractal::new(TEMPERATURE_STREAM, &[(256, 1.0), (128, 1.0), (32, 1.0)]);
/// Wet against dry.
const HUMIDITY: Fractal = Fractal::new(HUMIDITY_STREAM, &[(256, 1.0), (128, 1.0), (32, 1.0)]);

/// Value of [`CONTINENT`] where the land meets the sea. Below it is ocean, so about a third
/// of the world is sea (measured, see the tests).
const COAST: f32 = 0.45;
/// How far above the sea level the land rises from the coast to the highest continent value.
const LAND_RISE: f32 = 14.0;
/// How far below the sea level the ocean floor drops from the coast to the lowest continent value.
const OCEAN_DEPTH: f32 = 44.0;
/// Range of [`CONTINENT`] above the coast over which the land counts as inland (hills at full
/// height, mountains and rivers allowed). A flat coast makes room for beaches.
const INLAND_RANGE: f32 = 0.12;
/// Height range of the hills on land, in blocks (peak to peak).
const LAND_HILL_AMPLITUDE: f32 = 12.0;
/// Height range of the swells of the sea floor, in blocks.
const OCEAN_HILL_AMPLITUDE: f32 = 3.0;
/// Mountains start where [`MOUNTAIN_MASK`] passes this value and are full at [`MOUNTAIN_FULL`].
const MOUNTAIN_START: f32 = 0.52;
/// Value of [`MOUNTAIN_MASK`] from which a range has its full height.
const MOUNTAIN_FULL: f32 = 0.68;
/// Mountains need this much more continent than the coast, and have no steeper start than a
/// ramp over [`MOUNTAIN_INLAND_RANGE`] more.
const MOUNTAIN_INLAND_START: f32 = 0.05;
/// See [`MOUNTAIN_INLAND_START`].
const MOUNTAIN_INLAND_RANGE: f32 = 0.15;
/// Height every mountain range adds, in blocks, before the ridges.
const MOUNTAIN_BASE_RISE: f32 = 18.0;
/// Height the ridges add on top, in blocks: base and peaks together reach [`MAX_SURFACE`]
/// (the final clamp cuts what is above).
const MOUNTAIN_PEAK_RISE: f32 = 66.0;

/// A river valley is where the distance of [`RIVER`] from the middle of its range
/// (`0.0` there, `1.0` at the ends) is below this: the valley's width.
const RIVER_WIDTH: f32 = 0.05;
/// How far below the sea level the river bed lies, in blocks.
const RIVER_DEPTH: i32 = 3;

// ---- Biomes ----

/// The ground height from which a column is mountain, in blocks.
pub(crate) const MOUNTAIN_LINE: i32 = 96;
/// The ground height from which mountains are covered with snow, in blocks.
pub(crate) const SNOW_LINE: i32 = 128;
/// Coastal land up to this many blocks above the sea level is beach.
const BEACH_HEIGHT: i32 = 2;
/// How far above the coast value of the continent field land still counts as coastal, so
/// beaches lie along coasts only.
const BEACH_COAST_RANGE: f32 = 0.03;
/// Sea floor up to this many blocks below the sea level is sand, deeper it is gravel.
pub(crate) const SAND_DEPTH: i32 = 10;
/// Temperature from which dry land is desert.
const DESERT_TEMPERATURE: f32 = 0.55;
/// Humidity below which hot land is desert.
const DESERT_HUMIDITY: f32 = 0.47;
/// Humidity from which land is forest.
const FOREST_HUMIDITY: f32 = 0.535;

// ---- Lakes ----

/// Edge of a lake cell in blocks, a power of two. Each cell holds at most one lake, whose
/// bowl and rim lie wholly inside the cell (checked below), so a column needs the lake of its
/// own cell only. It is a power of two so cells are found with a shift.
pub(crate) const LAKE_CELL: i32 = 256;
/// `log2(LAKE_CELL)`: the shift that turns a block coordinate into a lake cell.
const LAKE_CELL_SHIFT: u32 = LAKE_CELL.trailing_zeros();
/// Chance out of 256 that a lake cell holds a lake (before the land check).
const LAKE_CHANCE_PER_256: u32 = 110;
/// Radius of the smallest lake bowl in blocks, and how much bigger the biggest may be.
const LAKE_MIN_RADIUS: i32 = 10;
/// See [`LAKE_MIN_RADIUS`].
const LAKE_EXTRA_RADIUS: i32 = 14;
/// Width of the raised rim that holds the water in, in blocks.
const LAKE_RIM: f32 = 5.0;
/// The most a lake's shape noise changes its radius: the radius is scaled by
/// `1 +- LAKE_WARP / 2`.
const LAKE_WARP: f32 = 0.5;
/// Cell size of the lake shape noise, in blocks.
const LAKE_SHAPE_CELL: i32 = 16;
/// How much deeper than its shore the middle of a lake is, in blocks.
const LAKE_DEPTH: f32 = 4.0;
/// A lake needs this much ground above the sea level around it, so its level lies above the
/// sea and rivers cannot drain it.
const LAKE_MIN_GROUND_ABOVE_SEA: i32 = 4;
/// A lake needs the ground around it below this height (no lakes in the mountains).
const LAKE_MAX_GROUND: i32 = MOUNTAIN_LINE - 8;
/// Number of lake cells along one axis that a scanned area may touch. Areas are a chunk plus
/// the tree margin (36 blocks) or a level-of-detail tile (up to 512 blocks, aligned to its own
/// size), which touch two cells at most (checked in `lod.rs`, where the widest area is made).
pub(crate) const LAKE_CELLS_PER_AXIS: usize = 2;

// A lake and its rim, with the widest shape, fit in the middle half of a cell.
const _: () = assert!(
    ((LAKE_MIN_RADIUS + LAKE_EXTRA_RADIUS) as f32 * (1.0 + LAKE_WARP / 2.0) + LAKE_RIM)
        < (LAKE_CELL / 4) as f32
);

/// Stream of the second lake hash, which chooses the radius.
const LAKE_RADIUS_STREAM: u32 = 9;
/// Layout of the first lake hash: the chance is the lowest [`LAKE_FIELD_BITS`] bits, the centre's
/// x and z the two fields above it.
const LAKE_CHANCE_SHIFT: u32 = 0;
/// See [`LAKE_CHANCE_SHIFT`].
const LAKE_X_SHIFT: u32 = LAKE_CHANCE_SHIFT + LAKE_FIELD_BITS;
/// See [`LAKE_CHANCE_SHIFT`].
const LAKE_Z_SHIFT: u32 = LAKE_X_SHIFT + LAKE_FIELD_BITS;
/// Width of each field of the first lake hash. 8 bits give 256 values: the chance is out of 256
/// and the centre ranges are half a cell (a divisor of 256), so neither is biased.
const LAKE_FIELD_BITS: u32 = 8;
/// Width of the radius field of the second hash.
const LAKE_RADIUS_BITS: u32 = 16;

// The first hash holds three fields; the ranges divide 2^bits, so `pick` is exact.
const _: () = assert!(LAKE_Z_SHIFT + LAKE_FIELD_BITS <= u32::BITS);
const _: () = assert!((LAKE_CELL / 2) as u32 <= 1 << LAKE_FIELD_BITS);

/// The `bits` bits of `hash` from bit `shift` up.
const fn hash_field(hash: u32, shift: u32, bits: u32) -> u32 {
    (hash >> shift) & ((1 << bits) - 1)
}

/// Maps a field of `bits` bits to `0..range` without a division (multiply and shift). Exact when
/// `range` divides `2^bits`, otherwise off by less than `range / 2^bits` in share.
const fn pick(field: u32, bits: u32, range: i32) -> i32 {
    ((field as u64 * range as u64) >> bits) as i32
}

/// One lake: a round bowl of water that is carved into the ground, held in by a rim.
#[derive(Clone, Copy, Debug)]
struct Lake {
    /// Centre column in world blocks.
    x: i32,
    z: i32,
    /// Radius of the bowl in blocks before the shape noise.
    radius: i32,
    /// Height of the water surface.
    level: i32,
}

/// The landscape of one area: the seed and the lakes of the lake cells the area touches, so
/// the per-column function does not look them up again.
pub(crate) struct Landscape {
    seed: u32,
    /// Lake cell coordinates of `lakes[0][0]`.
    first_cell: [i32; 2],
    /// The lakes of the cells `first_cell + (i, j)`, indexed `[j][i]`.
    lakes: [[Option<Lake>; LAKE_CELLS_PER_AXIS]; LAKE_CELLS_PER_AXIS],
}

/// The part of a column's height that comes before the lakes.
struct Natural {
    /// Ground height.
    ground: i32,
    /// Whether the column lies in a river valley on land.
    in_river: bool,
    /// Whether the column is within [`BEACH_COAST_RANGE`] of the coast: only there is low land a
    /// beach (and not a grassy riverbank or inland flat).
    coastal: bool,
}

impl Landscape {
    /// The landscape for columns with `min.x <= x <= max.x` and `min.z <= z <= max.z`. Asking
    /// for a column outside the area still answers, but lakes of cells beyond the area's first
    /// [`LAKE_CELLS_PER_AXIS`] per axis are missing.
    pub(crate) fn for_area(seed: u32, min: [i32; 2], max: [i32; 2]) -> Self {
        let first_cell = min.map(|c| c >> LAKE_CELL_SHIFT);
        let last_cell = max.map(|c| c >> LAKE_CELL_SHIFT);
        let mut lakes = [[None; LAKE_CELLS_PER_AXIS]; LAKE_CELLS_PER_AXIS];
        for (j, row) in lakes.iter_mut().enumerate() {
            for (i, slot) in row.iter_mut().enumerate() {
                let cell = [first_cell[0] + i as i32, first_cell[1] + j as i32];
                if cell[0] <= last_cell[0] && cell[1] <= last_cell[1] {
                    *slot = lake_in_cell(seed, cell);
                }
            }
        }
        Self {
            seed,
            first_cell,
            lakes,
        }
    }

    /// The ground, water and biome of the world column `(x, z)`.
    pub(crate) fn column(&self, x: i32, z: i32) -> ColumnInfo {
        // HOT PATH: runs once per block column of every generated chunk, tile and span scan;
        // the climate fields are only sampled where they decide something (dry land).
        let natural = natural(self.seed, x, z);
        let (ground, lake_level) = self.carve_lake(x, z, natural.ground);
        let water_level = lake_level.unwrap_or(SEA_LEVEL);
        let biome = if ground < water_level {
            match (lake_level, natural.in_river) {
                (Some(_), _) => Biome::Lake,
                (None, true) => Biome::River,
                (None, false) => Biome::Ocean,
            }
        } else {
            land_biome(self.seed, x, z, ground, natural.coastal)
        };
        ColumnInfo {
            ground,
            water_level,
            biome,
        }
    }

    /// The lake of the cell that holds the column, if any.
    fn lake_at(&self, x: i32, z: i32) -> Option<Lake> {
        let i = usize::try_from((x >> LAKE_CELL_SHIFT) - self.first_cell[0]).ok()?;
        let j = usize::try_from((z >> LAKE_CELL_SHIFT) - self.first_cell[1]).ok()?;
        self.lakes.get(j)?.get(i).copied().flatten()
    }

    /// Applies the lake to a column: inside the bowl the ground is dug out to the lake's
    /// shape and the lake's level is returned; on the rim the ground is at least one block
    /// above the level, so the water stays in.
    fn carve_lake(&self, x: i32, z: i32, ground: i32) -> (i32, Option<i32>) {
        let Some(lake) = self.lake_at(x, z) else {
            return (ground, None);
        };
        let (dx, dz) = ((x - lake.x) as f32, (z - lake.z) as f32);
        let distance_squared = dx * dx + dz * dz;
        let widest = lake.radius as f32 * (1.0 + LAKE_WARP / 2.0) + LAKE_RIM;
        if distance_squared > widest * widest {
            return (ground, None);
        }
        let shape = smooth_noise(self.seed ^ LAKE_STREAM, x, z, LAKE_SHAPE_CELL);
        let radius = lake.radius as f32 * (1.0 + (shape - 0.5) * LAKE_WARP);
        if distance_squared < radius * radius {
            // `u` is 0 in the middle and 1 on the shore: a parabolic bowl.
            let u_squared = distance_squared / (radius * radius);
            let depth = ((1.0 - u_squared) * LAKE_DEPTH) as i32;
            (ground.min(lake.level - 1 - depth), Some(lake.level))
        } else if distance_squared < (radius + LAKE_RIM) * (radius + LAKE_RIM) {
            (ground.max(lake.level + 1), None)
        } else {
            (ground, None)
        }
    }
}

/// The lake of the lake cell `cell`, or `None` if the cell has none or the land there is
/// not fit for one. The lake's level is one block below the lowest of five ground samples
/// (the middle and four points on the shore), so the rim stays close to the natural ground.
fn lake_in_cell(seed: u32, cell: [i32; 2]) -> Option<Lake> {
    let hash = cell_hash(seed ^ LAKE_STREAM, cell[0], cell[1]);
    if hash_field(hash, LAKE_CHANCE_SHIFT, LAKE_FIELD_BITS) >= LAKE_CHANCE_PER_256 {
        return None;
    }
    let quarter = LAKE_CELL / 4;
    let middle = LAKE_CELL / 2;
    // The centre is inside the middle half of the cell.
    let x = (cell[0] << LAKE_CELL_SHIFT)
        + quarter
        + pick(
            hash_field(hash, LAKE_X_SHIFT, LAKE_FIELD_BITS),
            LAKE_FIELD_BITS,
            middle,
        );
    let z = (cell[1] << LAKE_CELL_SHIFT)
        + quarter
        + pick(
            hash_field(hash, LAKE_Z_SHIFT, LAKE_FIELD_BITS),
            LAKE_FIELD_BITS,
            middle,
        );
    // The radius has a range that does not divide a field, so it gets a hash of its own with
    // a wide field: the bias of the range reduction is below one in four thousand.
    let radius_hash = cell_hash(seed ^ LAKE_RADIUS_STREAM, cell[0], cell[1]);
    let radius = LAKE_MIN_RADIUS
        + pick(
            hash_field(radius_hash, 0, LAKE_RADIUS_BITS),
            LAKE_RADIUS_BITS,
            LAKE_EXTRA_RADIUS + 1,
        );
    let samples = [(0, 0), (radius, 0), (-radius, 0), (0, radius), (0, -radius)];
    let lowest = samples
        .iter()
        .map(|(dx, dz)| natural(seed, x + dx, z + dz).ground)
        .min()?;
    let level = lowest - 1;
    (SEA_LEVEL + LAKE_MIN_GROUND_ABOVE_SEA..=LAKE_MAX_GROUND)
        .contains(&level)
        .then_some(Lake {
            x,
            z,
            radius,
            level,
        })
}

/// The ground of a column from the continent, hills, mountains and rivers, without lakes.
fn natural(seed: u32, x: i32, z: i32) -> Natural {
    let continent = CONTINENT.sample(seed, x, z);
    let inland = ramp(continent, COAST, COAST + INLAND_RANGE);
    // The sea level is where the land meets the sea; land rises, the sea floor falls.
    let base = if continent >= COAST {
        SEA_LEVEL as f32 + (continent - COAST) / (1.0 - COAST) * LAND_RISE
    } else {
        SEA_LEVEL as f32 - (COAST - continent) / COAST * OCEAN_DEPTH
    };
    let amplitude = lerp(OCEAN_HILL_AMPLITUDE, LAND_HILL_AMPLITUDE, inland);
    let hills = (HILLS.sample(seed, x, z) - 0.5) * amplitude;
    let mut height = base + hills;

    // The fields that only matter on land are sampled only there: the mountain mask needs
    // inland ground and a river valley needs land at all, so most of the sea skips them.
    let inland_mountain = ramp(
        continent,
        COAST + MOUNTAIN_INLAND_START,
        COAST + MOUNTAIN_INLAND_START + MOUNTAIN_INLAND_RANGE,
    );
    let mut mountain = 0.0;
    if inland_mountain > 0.0 {
        let range = smoothstep(ramp(
            MOUNTAIN_MASK.sample(seed, x, z),
            MOUNTAIN_START,
            MOUNTAIN_FULL,
        ));
        mountain = range * inland_mountain;
    }
    if mountain > 0.0 {
        // Folding the noise at its middle makes sharp ridges instead of round bumps.
        let ridge = 1.0 - (2.0 * RIDGE.sample(seed, x, z) - 1.0).abs();
        height += mountain * (MOUNTAIN_BASE_RISE + ridge * MOUNTAIN_PEAK_RISE);
    }

    // A river valley is the contour of the river field at its middle. It is cut into land
    // that is not mountain and never lifts the ground.
    let mut in_river = false;
    if continent >= COAST {
        let distance = (2.0 * RIVER.sample(seed, x, z) - 1.0).abs();
        let valley = smoothstep(1.0 - ramp(distance, 0.0, RIVER_WIDTH)) * (1.0 - mountain);
        in_river = valley > 0.0;
        let bed = (SEA_LEVEL - RIVER_DEPTH) as f32;
        if in_river && height > bed {
            height -= (height - bed) * valley;
        }
    }

    // Adding 0.5 and truncating rounds to nearest for non-negative values.
    let ground = (height.max(MIN_SURFACE as f32) + 0.5) as i32;
    Natural {
        ground: ground.min(MAX_SURFACE),
        in_river,
        coastal: continent < COAST + BEACH_COAST_RANGE,
    }
}

/// The biome of a dry column at `ground` (not under water).
fn land_biome(seed: u32, x: i32, z: i32, ground: i32, coastal: bool) -> Biome {
    if ground >= MOUNTAIN_LINE {
        Biome::Mountains
    } else if coastal && ground <= SEA_LEVEL + BEACH_HEIGHT {
        Biome::Beach
    } else {
        // Each climate field once per column.
        let temperature = TEMPERATURE.sample(seed, x, z);
        let humidity = HUMIDITY.sample(seed, x, z);
        if temperature >= DESERT_TEMPERATURE && humidity < DESERT_HUMIDITY {
            Biome::Desert
        } else if humidity >= FOREST_HUMIDITY {
            Biome::Forest
        } else {
            Biome::Plains
        }
    }
}

/// The biome of the world column `(x, z)`.
///
/// # Example
///
/// ```
/// use vox_world::{biome_at, Biome, SEA_LEVEL};
///
/// // Somewhere in the world there is a sea; the biome is a pure function of the seed.
/// assert_eq!(biome_at(1, 100, -40), biome_at(1, 100, -40));
/// assert!(SEA_LEVEL > 0);
/// ```
pub fn biome_at(seed: u32, x: i32, z: i32) -> Biome {
    Landscape::for_area(seed, [x, z], [x, z]).column(x, z).biome
}

/// How far from the origin [`spawn_point`] looks for land, in steps of [`SPAWN_STEP`].
const SPAWN_SEARCH_RINGS: i32 = 200;
/// Distance in blocks between the columns [`spawn_point`] tries.
const SPAWN_STEP: i32 = 8;
/// A spawn column needs this much ground above the sea level, so it is clear of beaches
/// and shores.
const SPAWN_MIN_ABOVE_SEA: i32 = 6;

/// A place to start: the block `(x, y, z)` just above the ground of the grassy column nearest
/// to the origin (searched on square rings in steps of [`SPAWN_STEP`] blocks), so the player
/// starts on land. If nothing grassy lies within the search, the origin column is used.
///
/// # Example
///
/// ```
/// use vox_world::{spawn_point, SEA_LEVEL};
///
/// let [_, y, _] = spawn_point(1);
/// assert!(y > SEA_LEVEL);
/// ```
pub fn spawn_point(seed: u32) -> [i32; 3] {
    let above = |x: i32, z: i32| -> [i32; 3] {
        let info = Landscape::for_area(seed, [x, z], [x, z]).column(x, z);
        [x, info.top() + 1, z]
    };
    for ring in 0..SPAWN_SEARCH_RINGS {
        let reach = ring * SPAWN_STEP;
        for step in -ring..=ring {
            let offset = step * SPAWN_STEP;
            for (x, z) in [
                (offset, -reach),
                (offset, reach),
                (-reach, offset),
                (reach, offset),
            ] {
                let landscape = Landscape::for_area(seed, [x, z], [x, z]);
                let info = landscape.column(x, z);
                if info.biome.has_trees()
                    && info.ground >= SEA_LEVEL + SPAWN_MIN_ABOVE_SEA
                    && !tree_stands_at(&landscape, seed, x, z)
                {
                    return above(x, z);
                }
            }
        }
    }
    above(0, 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Counts of each biome over a grid of columns `step` blocks apart.
    fn census(seed: u32, half: i32, step: i32) -> std::collections::HashMap<Biome, usize> {
        let mut counts = std::collections::HashMap::new();
        let mut z = -half;
        while z < half {
            let mut x = -half;
            while x < half {
                *counts.entry(biome_at(seed, x, z)).or_insert(0) += 1;
                x += step;
            }
            z += step;
        }
        counts
    }

    #[test]
    fn ground_stays_in_its_stated_range() {
        for seed in 0..4 {
            for z in (-1500..1500).step_by(37) {
                for x in (-1500..1500).step_by(41) {
                    let info = Landscape::for_area(seed, [x, z], [x, z]).column(x, z);
                    assert!((MIN_SURFACE..=MAX_SURFACE).contains(&info.ground));
                    assert!(info.water_level <= LAKE_MAX_GROUND);
                }
            }
        }
    }

    #[test]
    fn a_wide_world_has_every_biome() {
        let counts = census(1, 6000, 60);
        for biome in [
            Biome::Ocean,
            Biome::River,
            Biome::Lake,
            Biome::Beach,
            Biome::Plains,
            Biome::Forest,
            Biome::Desert,
            Biome::Mountains,
        ] {
            assert!(
                counts.get(&biome).copied().unwrap_or(0) > 0,
                "no {biome:?} in {counts:?}"
            );
        }
    }

    #[test]
    fn the_sea_is_a_fair_share_of_the_world() {
        // The share of ocean and mountains over several seeds, in percent of the columns.
        let (mut total, mut ocean, mut mountains) = (0, 0, 0);
        for seed in 0..4 {
            for (biome, count) in census(seed, 6000, 60) {
                total += count;
                match biome {
                    Biome::Ocean => ocean += count,
                    Biome::Mountains => mountains += count,
                    _ => {}
                }
            }
        }
        let share = |count: usize| count * 100 / total;
        assert!((15..=45).contains(&share(ocean)), "ocean {}", share(ocean));
        assert!(
            (3..=30).contains(&share(mountains)),
            "mountains {}",
            share(mountains)
        );
    }

    #[test]
    fn mountain_peaks_reach_several_layers_above_the_sea() {
        let mut highest = 0;
        for z in (-3000..3000).step_by(16) {
            for x in (-3000..3000).step_by(16) {
                highest = highest.max(Landscape::for_area(1, [x, z], [x, z]).column(x, z).ground);
            }
        }
        assert!(highest >= 128, "highest ground {highest}");
    }

    #[test]
    fn water_is_above_the_ground_exactly_where_the_biome_is_water() {
        for z in (-2000..2000).step_by(23) {
            for x in (-2000..2000).step_by(29) {
                let info = Landscape::for_area(2, [x, z], [x, z]).column(x, z);
                let wet = matches!(info.biome, Biome::Ocean | Biome::River | Biome::Lake);
                assert_eq!(info.has_water(), wet, "({x}, {z}) {info:?}");
                assert!(info.top() >= info.ground);
            }
        }
    }

    #[test]
    fn the_lake_radius_covers_its_whole_range_evenly() {
        let mut counts = [0_u32; (LAKE_EXTRA_RADIUS + 1) as usize];
        for z in 0..400 {
            for x in 0..400 {
                let hash = cell_hash(1 ^ LAKE_RADIUS_STREAM, x, z);
                let radius = pick(
                    hash_field(hash, 0, LAKE_RADIUS_BITS),
                    LAKE_RADIUS_BITS,
                    LAKE_EXTRA_RADIUS + 1,
                );
                counts[radius as usize] += 1;
            }
        }
        let mean = 160_000 / counts.len() as u32;
        assert!(
            counts
                .iter()
                .all(|&c| c > mean * 9 / 10 && c < mean * 11 / 10),
            "{counts:?}"
        );
    }

    #[test]
    fn lake_surfaces_are_flat_and_above_the_sea() {
        // Over every lake of a stretch of cells: each lake column has the lake's level, which
        // is above the sea, and no column next to one shows less than that level (the water
        // cannot run out).
        let mut lake_columns = 0;
        for (seed, cell_z, cell_x) in
            (1..=4).flat_map(|seed| (-8..8).flat_map(move |z| (-8..8).map(move |x| (seed, z, x))))
        {
            {
                let Some(lake) = lake_in_cell(seed, [cell_x, cell_z]) else {
                    continue;
                };
                let reach = LAKE_MIN_RADIUS + LAKE_EXTRA_RADIUS + LAKE_RIM as i32 + 2;
                let landscape = Landscape::for_area(
                    seed,
                    [cell_x << LAKE_CELL_SHIFT, cell_z << LAKE_CELL_SHIFT],
                    [
                        (cell_x << LAKE_CELL_SHIFT) + LAKE_CELL - 1,
                        (cell_z << LAKE_CELL_SHIFT) + LAKE_CELL - 1,
                    ],
                );
                for z in lake.z - reach..=lake.z + reach {
                    for x in lake.x - reach..=lake.x + reach {
                        let here = landscape.column(x, z);
                        if here.biome != Biome::Lake {
                            continue;
                        }
                        lake_columns += 1;
                        assert_eq!(here.water_level, lake.level);
                        assert!(here.water_level > SEA_LEVEL);
                        for (nx, nz) in [(x + 1, z), (x - 1, z), (x, z + 1), (x, z - 1)] {
                            let next = landscape.column(nx, nz);
                            assert!(
                                next.top() >= here.water_level,
                                "water runs out at ({x}, {z}): {here:?} next to {next:?}"
                            );
                        }
                    }
                }
            }
        }
        assert!(lake_columns > 400, "only {lake_columns} lake columns");
    }

    #[test]
    fn a_landscape_for_one_column_agrees_with_one_for_its_whole_area() {
        let landscape = Landscape::for_area(5, [-40, 200], [-9, 231]);
        for z in 200..232 {
            for x in -40..-8 {
                assert_eq!(
                    landscape.column(x, z),
                    Landscape::for_area(5, [x, z], [x, z]).column(x, z),
                    "({x}, {z})"
                );
            }
        }
    }

    #[test]
    fn the_spawn_is_on_land_for_many_seeds() {
        for seed in 0..40 {
            let [x, y, z] = spawn_point(seed);
            let info = Landscape::for_area(seed, [x, z], [x, z]).column(x, z);
            assert!(info.biome.has_trees(), "seed {seed}: {info:?}");
            assert_eq!(y, info.ground + 1);
            assert!(info.ground >= SEA_LEVEL + SPAWN_MIN_ABOVE_SEA);
        }
    }

    #[test]
    fn the_spawn_is_deterministic() {
        assert_eq!(spawn_point(9), spawn_point(9));
    }

    #[test]
    fn the_widest_lake_fits_its_cell() {
        // The compile-time check of the same fact, with the numbers shown.
        let widest = (LAKE_MIN_RADIUS + LAKE_EXTRA_RADIUS) as f32 * (1.0 + LAKE_WARP / 2.0);
        assert!(widest + LAKE_RIM < (LAKE_CELL / 4) as f32);
    }

    #[test]
    fn the_land_is_a_balanced_mix_of_biomes() {
        // Shares of the dry land (beach, plains, forest, desert, mountains), over 4 seeds. The
        // owner's targets: desert 10 to 15 percent, plains 30 to 35, forest 25 to 30, mountains
        // by height, beaches only along coasts (a small share). The bounds are those targets
        // with a margin for the seeds.
        let mut counts = std::collections::HashMap::<Biome, usize>::new();
        for seed in 0..4 {
            for (biome, count) in census(seed, 6000, 60) {
                *counts.entry(biome).or_default() += count;
            }
        }
        let land_biomes = [
            Biome::Beach,
            Biome::Plains,
            Biome::Forest,
            Biome::Desert,
            Biome::Mountains,
        ];
        let land: usize = land_biomes.iter().map(|b| counts[b]).sum();
        let share = |biome: Biome| counts[&biome] * 100 / land;
        assert!(
            (8..=16).contains(&share(Biome::Desert)),
            "desert {}",
            share(Biome::Desert)
        );
        assert!(
            (28..=38).contains(&share(Biome::Plains)),
            "plains {}",
            share(Biome::Plains)
        );
        assert!(
            (24..=33).contains(&share(Biome::Forest)),
            "forest {}",
            share(Biome::Forest)
        );
        assert!(
            (10..=25).contains(&share(Biome::Mountains)),
            "mountains {}",
            share(Biome::Mountains)
        );
        assert!(share(Biome::Beach) <= 15, "beach {}", share(Biome::Beach));
    }
}
