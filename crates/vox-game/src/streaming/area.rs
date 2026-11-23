//! Which chunk columns are wanted around the camera, as pure functions.
//!
//! The area is a circle of columns in the terrain layer. A column is loaded when it lies
//! within the view radius and unloaded only when it is a margin farther out, so a camera that
//! wanders over a chunk border does not load and unload a row each time.

use vox_world::{CHUNK_SIZE, ChunkPos};

/// The layer all terrain lies in (decision 0006).
pub const TERRAIN_LAYER: i32 = 0;

/// Extra chunks beyond the view radius before a chunk is unloaded (hysteresis). One chunk is
/// the cheapest margin that stops a row from being reloaded at every border crossing.
pub const UNLOAD_MARGIN: i32 = 1;

/// `CHUNK_SIZE` as a float, to turn a position in blocks into a chunk coordinate.
const CHUNK_SIZE_F32: f32 = CHUNK_SIZE as f32;

/// The chunk that contains a world position in blocks.
///
/// The division rounds down, so negative positions land in negative chunks. A position out of
/// the `i32` range saturates and NaN counts as 0 (the behaviour of `as`).
pub fn chunk_of(position: [f32; 3]) -> ChunkPos {
    let [x, y, z] = position.map(|block| (block / CHUNK_SIZE_F32).floor() as i32);
    ChunkPos::new(x, y, z)
}

/// Squared distance between two columns, in chunks. `i64` cannot overflow for any `i32` pair.
fn distance_squared(dx: i64, dz: i64) -> i64 {
    dx * dx + dz * dz
}

/// The columns within `radius` of `centre` (x and z only, `y = TERRAIN_LAYER`), nearest first.
/// Ties are ordered by (z, x), so the order is the same on every call. A negative radius counts
/// as 0.
pub fn wanted(centre: ChunkPos, radius: i32) -> Vec<ChunkPos> {
    let radius = radius.max(0);
    let limit = i64::from(radius).pow(2);
    let mut offsets: Vec<(i64, i32, i32)> = Vec::new();
    for dz in -radius..=radius {
        for dx in -radius..=radius {
            let distance = distance_squared(i64::from(dx), i64::from(dz));
            if distance <= limit {
                offsets.push((distance, dz, dx));
            }
        }
    }
    offsets.sort_unstable();
    offsets
        .into_iter()
        .map(|(_, dz, dx)| {
            ChunkPos::new(
                centre.x.saturating_add(dx),
                TERRAIN_LAYER,
                centre.z.saturating_add(dz),
            )
        })
        .collect()
}

/// Whether a loaded chunk at `pos` stays loaded: within `radius + UNLOAD_MARGIN` of `centre`.
/// The layer is ignored, only the column counts.
pub fn is_kept(centre: ChunkPos, pos: ChunkPos, radius: i32) -> bool {
    let reach = i64::from(radius.max(0)) + i64::from(UNLOAD_MARGIN);
    let dx = i64::from(pos.x) - i64::from(centre.x);
    let dz = i64::from(pos.z) - i64::from(centre.z);
    distance_squared(dx, dz) <= reach * reach
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    /// The radius the plan's numbers (197 columns) are given for.
    const RADIUS: i32 = 8;
    /// Columns of a circle of radius 8.
    const RADIUS_8_COLUMNS: usize = 197;

    #[test]
    fn radius_zero_is_only_the_centre() {
        let centre = ChunkPos::new(5, 3, -2);
        assert_eq!(wanted(centre, 0), vec![ChunkPos::new(5, 0, -2)]);
        assert_eq!(wanted(centre, -4), wanted(centre, 0));
    }

    #[test]
    fn radius_eight_has_197_unique_positions_inside_the_circle() {
        let centre = ChunkPos::new(10, 0, -7);
        let area = wanted(centre, RADIUS);
        assert_eq!(area.len(), RADIUS_8_COLUMNS);
        assert_eq!(area.iter().collect::<HashSet<_>>().len(), RADIUS_8_COLUMNS);
        for pos in &area {
            let (dx, dz) = (pos.x - centre.x, pos.z - centre.z);
            assert!(dx * dx + dz * dz <= RADIUS * RADIUS, "{pos:?}");
            assert_eq!(pos.y, TERRAIN_LAYER);
        }
        // Just outside the circle on a diagonal: (6, 6) has 72 > 64.
        assert!(!area.contains(&ChunkPos::new(centre.x + 6, 0, centre.z + 6)));
        assert!(area.contains(&ChunkPos::new(centre.x + RADIUS, 0, centre.z)));
    }

    #[test]
    fn the_order_is_nearest_first_and_the_same_every_time() {
        let centre = ChunkPos::ORIGIN;
        let area = wanted(centre, RADIUS);
        assert_eq!(area[0], centre);
        let distances: Vec<i32> = area.iter().map(|p| p.x * p.x + p.z * p.z).collect();
        assert!(distances.windows(2).all(|pair| pair[0] <= pair[1]));
        assert_eq!(area, wanted(centre, RADIUS));
        // The four nearest neighbours tie; they come in (z, x) order.
        assert_eq!(
            area[1..5],
            [
                ChunkPos::new(0, 0, -1),
                ChunkPos::new(-1, 0, 0),
                ChunkPos::new(1, 0, 0),
                ChunkPos::new(0, 0, 1),
            ]
        );
    }

    #[test]
    fn a_chunk_stays_kept_one_margin_beyond_the_radius() {
        let centre = ChunkPos::new(3, 0, 3);
        let on_axis = |distance: i32| ChunkPos::new(3 + distance, 0, 3);
        assert!(is_kept(centre, on_axis(RADIUS + UNLOAD_MARGIN), RADIUS));
        assert!(!is_kept(
            centre,
            on_axis(RADIUS + UNLOAD_MARGIN + 1),
            RADIUS
        ));
        assert!(is_kept(centre, ChunkPos::new(3, 7, 3 - RADIUS), RADIUS));
    }

    #[test]
    fn every_wanted_position_is_kept() {
        for radius in 0..=RADIUS {
            let centre = ChunkPos::new(-4, 0, 9);
            assert!(
                wanted(centre, radius)
                    .into_iter()
                    .all(|pos| is_kept(centre, pos, radius))
            );
        }
    }

    #[test]
    fn huge_radii_and_far_positions_do_not_overflow() {
        let far = ChunkPos::new(i32::MAX, 0, i32::MIN);
        assert!(is_kept(far, far, i32::MAX));
        assert!(!is_kept(ChunkPos::ORIGIN, far, 1));
    }

    #[test]
    fn positions_map_to_the_chunk_that_contains_them() {
        let chunk_x = |x: f32| chunk_of([x, 0.0, 0.0]).x;
        assert_eq!(chunk_x(0.0), 0);
        assert_eq!(chunk_x(31.9), 0);
        assert_eq!(chunk_x(32.0), 1);
        assert_eq!(chunk_x(-0.1), -1);
        assert_eq!(chunk_x(-32.0), -1);
        assert_eq!(chunk_of([0.0, 60.0, -40.0]), ChunkPos::new(0, 1, -2));
    }

    #[test]
    fn chunk_of_survives_nan_and_infinity() {
        assert_eq!(chunk_of([f32::NAN, 0.0, 0.0]).x, 0);
        assert_eq!(
            chunk_of([f32::INFINITY, 0.0, f32::NEG_INFINITY]).x,
            i32::MAX
        );
        assert_eq!(chunk_of([0.0, 0.0, f32::NEG_INFINITY]).z, i32::MIN);
    }
}
