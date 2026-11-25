//! Deterministic value noise: the integer hash, the smooth lattice noise and the fractal sum
//! of octaves that the landscape is made of.
//!
//! Everything is integer hashing plus plain `f32` arithmetic (`+`, `-`, `*`, `/` are exactly
//! specified by IEEE 754), with no `sin` or other libm functions, so a seed produces the same
//! world on every platform. The noise is sampled at world coordinates, so chunks next to each
//! other fit together.

/// Value noise: lattice values every `cell` blocks, blended with smoothstep.
///
/// Coordinates may be negative: floor division puts the lattice cell and the position
/// inside it on the right side of zero, so the noise is continuous across the origin.
/// Cell sizes are powers of two, where the arithmetic shift is the floor division and the
/// mask is the euclidean remainder (a test checks both against `div_euclid` and
/// `rem_euclid`); the shift and mask are about 30 percent cheaper than a signed division
/// per chunk, measured. The lattice hash takes the cell as `u32` by two's complement.
///
/// `stream` is a ready hash stream (a mixed seed, see [`Fractal::sample`]). The row hash
/// `mix(z ^ stream)` is shared by the two lattice points of a row, so a sample costs seven
/// `mix` calls instead of twelve (measured with the terrain benchmark).
pub(crate) fn smooth_noise(stream: u32, x: i32, z: i32, cell: i32) -> f32 {
    let shift = cell.trailing_zeros();
    let (lx, lz) = ((x >> shift) as u32, (z >> shift) as u32);
    // Integer remainders times `1 / cell` are exact in f32 for these small values; a multiply
    // instead of a divide, 30 times per column.
    let inverse_cell = power_of_two_reciprocal(shift);
    let tx = smoothstep((x & (cell - 1)) as f32 * inverse_cell);
    let tz = smoothstep((z & (cell - 1)) as f32 * inverse_cell);
    let (row_near, row_far) = (mix(lz ^ stream), mix(lz.wrapping_add(1) ^ stream));
    let near = lerp(
        lattice_value(row_near, lx),
        lattice_value(row_near, lx.wrapping_add(1)),
        tx,
    );
    let far = lerp(
        lattice_value(row_far, lx),
        lattice_value(row_far, lx.wrapping_add(1)),
        tx,
    );
    lerp(near, far, tz)
}

/// The exponent bias of an `f32`: the stored exponent of `2^0`.
const F32_EXPONENT_BIAS: u32 = 127;
/// The number of mantissa bits of an `f32`: where the exponent field starts.
const F32_MANTISSA_BITS: u32 = 23;

/// `2^-shift` as an `f32`, exactly, built from its bits (the exponent field is
/// `bias - shift`, the mantissa zero). `shift` must be below the bias.
fn power_of_two_reciprocal(shift: u32) -> f32 {
    f32::from_bits((F32_EXPONENT_BIAS - shift) << F32_MANTISSA_BITS)
}

/// The most octaves a [`Fractal`] may have.
pub(crate) const MAX_OCTAVES: usize = 4;

/// A fractal sum of value-noise octaves, normalized to `0.0..=1.0`: large features first,
/// finer detail on top with less weight.
///
/// Each field has its own `stream` number, so two fields of one seed are unrelated, and each
/// octave its own hash stream, so the layers do not line up.
pub(crate) struct Fractal {
    /// Cell size in blocks (a power of two) and weight of each octave.
    octaves: &'static [(i32, f32)],
    /// Per octave: the salt that makes its hash stream. Computed from the stream number and
    /// the octave index once, at compile time.
    salts: [u32; MAX_OCTAVES],
    /// Sum of the octave weights: the divisor that normalizes the sum.
    weight_sum: f32,
}

impl Fractal {
    /// The field `stream` made of `octaves` (cell size, weight), at most [`MAX_OCTAVES`];
    /// cell sizes must be powers of two (checked at compile time when used in a `const`).
    pub(crate) const fn new(stream: u32, octaves: &'static [(i32, f32)]) -> Self {
        assert!(octaves.len() <= MAX_OCTAVES);
        let mut salts = [0; MAX_OCTAVES];
        let mut weight_sum = 0.0;
        let mut index = 0;
        while index < octaves.len() {
            assert!(octaves[index].0.count_ones() == 1);
            // The stream goes through `mix` before the index is added, so octave 1 of one
            // field is not octave 0 of the next.
            salts[index] = mix(mix(stream).wrapping_add(index as u32));
            weight_sum += octaves[index].1;
            index += 1;
        }
        Self {
            octaves,
            salts,
            weight_sum,
        }
    }

    /// The value at the world column `(x, z)`, in `0.0..=1.0`.
    pub(crate) fn sample(&self, seed: u32, x: i32, z: i32) -> f32 {
        let mut sum = 0.0;
        for (index, (cell, weight)) in self.octaves.iter().enumerate() {
            sum += weight * smooth_noise(mix(seed ^ self.salts[index]), x, z, *cell);
        }
        (sum / self.weight_sum).clamp(0.0, 1.0)
    }
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

/// Pseudo-random 32-bit hash of a seed and a cell `(x, z)` of any grid (lattice points,
/// tree cells, lake cells). Coordinates are taken by two's complement.
pub(crate) fn cell_hash(seed: u32, x: i32, z: i32) -> u32 {
    lattice_hash(seed, x as u32, z as u32)
}

fn lattice_hash(seed: u32, x: u32, z: u32) -> u32 {
    lattice_point(mix(z ^ mix(seed)), x)
}

/// The hash of the lattice point `x` in the row whose row hash is `row` (`mix(z ^ stream)`).
fn lattice_point(row: u32, x: u32) -> u32 {
    mix(x.wrapping_mul(GOLDEN_RATIO_32) ^ row)
}

/// Pseudo-random value in `0.0..1.0` for a lattice point of the row with hash `row`.
fn lattice_value(row: u32, x: u32) -> f32 {
    // Keep the top bits: they are the best mixed ones.
    (lattice_point(row, x) >> (u32::BITS - LATTICE_BITS)) as f32 / LATTICE_SCALE
}

/// The three right shifts of the `lowbias32` mix, in the order of [`mix`]. They are the
/// constants found by Chris Wellons' search for a 32-bit hash with good avalanche
/// (<https://nullprogram.com/blog/2018/07/31/>); other values would hash worse.
const MIX_SHIFTS: [u32; 3] = [16, 15, 16];
/// The two multipliers of the mix.
const MIX_MULTIPLIERS: [u32; 2] = [0x7FEB_352D, 0x846C_A68B];

/// The `lowbias32` integer mix by Chris Wellons: a cheap 32-bit hash with good avalanche.
pub(crate) const fn mix(mut value: u32) -> u32 {
    value ^= value >> MIX_SHIFTS[0];
    value = value.wrapping_mul(MIX_MULTIPLIERS[0]);
    value ^= value >> MIX_SHIFTS[1];
    value = value.wrapping_mul(MIX_MULTIPLIERS[1]);
    value ^ (value >> MIX_SHIFTS[2])
}

/// Smooth ease curve for `t` in `0.0..=1.0`: zero slope at both ends.
pub(crate) fn smoothstep(t: f32) -> f32 {
    t * t * (3.0 - 2.0 * t)
}

/// Linear interpolation from `a` to `b`.
pub(crate) fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

/// Where `value` lies between `start` (gives 0) and `end` (gives 1), clamped to `0.0..=1.0`.
/// `start` and `end` must differ.
pub(crate) fn ramp(value: f32, start: f32, end: f32) -> f32 {
    ((value - start) / (end - start)).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CELLS: [i32; 6] = [4, 8, 16, 64, 256, 512];

    #[test]
    fn shift_and_mask_equal_floor_division_and_euclidean_remainder() {
        for cell in CELLS {
            let shift = cell.trailing_zeros();
            for x in -700_i32..700 {
                assert_eq!(x >> shift, x.div_euclid(cell), "x {x}, cell {cell}");
                assert_eq!(x & (cell - 1), x.rem_euclid(cell), "x {x}, cell {cell}");
            }
        }
    }

    #[test]
    fn value_noise_is_in_range_and_continuous_across_zero() {
        // Value noise with a smoothstep blend moves by at most 1.5 per cell.
        for cell in CELLS {
            let bound = 1.5 / cell as f32 + 1e-6;
            for x in -300..300 {
                let here = smooth_noise(7, x, -x, cell);
                assert!((0.0..=1.0).contains(&here));
                assert!((here - smooth_noise(7, x + 1, -x, cell)).abs() <= bound);
                assert!((here - smooth_noise(7, x, -x + 1, cell)).abs() <= bound);
            }
        }
    }

    const TEST_FIELD: Fractal = Fractal::new(1, &[(64, 2.0), (16, 1.0)]);
    const OTHER_FIELD: Fractal = Fractal::new(2, &[(64, 2.0), (16, 1.0)]);

    #[test]
    fn fractal_is_normalized_and_deterministic() {
        assert_eq!(TEST_FIELD.weight_sum, 3.0);
        for z in -50..50 {
            for x in -50..50 {
                let value = TEST_FIELD.sample(3, x * 7, z * 11);
                assert!((0.0..=1.0).contains(&value));
                assert_eq!(value, TEST_FIELD.sample(3, x * 7, z * 11));
            }
        }
    }

    #[test]
    fn streams_and_seeds_give_unrelated_fields() {
        let differs = |a: &dyn Fn(i32, i32) -> f32, b: &dyn Fn(i32, i32) -> f32| {
            (0..64).any(|i| a(i * 13, i * 5) != b(i * 13, i * 5))
        };
        assert!(differs(&|x, z| TEST_FIELD.sample(1, x, z), &|x, z| {
            OTHER_FIELD.sample(1, x, z)
        }));
        assert!(differs(&|x, z| TEST_FIELD.sample(1, x, z), &|x, z| {
            TEST_FIELD.sample(2, x, z)
        }));
    }

    #[test]
    fn the_reciprocal_of_a_power_of_two_is_exact() {
        for shift in 0..20 {
            assert_eq!(
                power_of_two_reciprocal(shift),
                1.0 / (1_u32 << shift) as f32
            );
        }
    }

    #[test]
    fn ramp_clamps_and_interpolates() {
        assert_eq!(ramp(0.0, 1.0, 3.0), 0.0);
        assert_eq!(ramp(2.0, 1.0, 3.0), 0.5);
        assert_eq!(ramp(9.0, 1.0, 3.0), 1.0);
    }
}
