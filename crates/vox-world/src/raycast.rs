//! Ray casting through the blocks of a world: which solid block does a ray hit first?
//!
//! Used to pick the block the player looks at, to break it or to place a block on the
//! face that was hit. The ray walks cell by cell with the Amanatides-Woo voxel traversal
//! ("A Fast Voxel Traversal Algorithm for Ray Tracing", 1987): it visits exactly the
//! blocks the ray passes through, in order, and never skips a corner.

use crate::block::BlockId;
use crate::chunk::{AXIS_X, AXIS_Y, AXIS_Z};
use crate::mesh::Face;
use crate::world::World;

/// Largest absolute coordinate of a ray origin, in blocks: 2^24, the limit up to which an
/// `f32` (24 bits of mantissa) represents every whole number. Beyond it the origin
/// is too coarse to pick a block, and it is far below `i32::MAX`, so the cell of the
/// origin never overflows.
const MAX_ORIGIN_COORDINATE: f32 = (1_u32 << 24) as f32;

/// The first solid block a ray hits.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RayHit {
    /// World position of the solid block that was hit.
    pub block: [i32; 3],
    /// What the block is. Never air.
    pub id: BlockId,
    /// The face of the block the ray entered through. The adjacent cell on that side is
    /// where a new block would be placed (see [`RayHit::adjacent`]).
    ///
    /// If the ray starts inside a solid block, there is no entered face: the face that
    /// looks against the ray's strongest direction component is reported (ties go to
    /// X, then Y, then Z) and [`RayHit::distance`] is zero.
    pub face: Face,
    /// Distance from the origin to the point where the ray enters the block, in blocks.
    pub distance: f32,
}

impl RayHit {
    /// World position of the cell next to the hit block on the side of [`RayHit::face`],
    /// where a block is placed. Saturates at the ends of the `i32` range.
    pub fn adjacent(&self) -> [i32; 3] {
        let offset = self.face.offset();
        [
            self.block[AXIS_X].saturating_add(i32::from(offset[AXIS_X])),
            self.block[AXIS_Y].saturating_add(i32::from(offset[AXIS_Y])),
            self.block[AXIS_Z].saturating_add(i32::from(offset[AXIS_Z])),
        ]
    }
}

/// Per-axis state of the traversal.
#[derive(Clone, Copy, Debug)]
struct AxisState {
    /// Block coordinate of the current cell along this axis.
    cell: i32,
    /// Direction of travel along this axis: -1, 0 or 1.
    step: i32,
    /// Ray distance at which the ray reaches the next cell border on this axis.
    /// Infinite if the ray does not move along the axis.
    next_border: f32,
    /// Ray distance between two cell borders on this axis. Infinite if the ray does
    /// not move along the axis.
    border_spacing: f32,
}

impl AxisState {
    /// `origin` and `direction` are along one axis; `direction` is part of a unit vector.
    fn new(origin: f32, direction: f32) -> Self {
        let floor = origin.floor();
        // The origin was checked against `MAX_ORIGIN_COORDINATE`, so the cast is exact.
        let cell = floor as i32;
        if direction > 0.0 {
            Self {
                cell,
                step: 1,
                next_border: (floor + 1.0 - origin) / direction,
                border_spacing: 1.0 / direction,
            }
        } else if direction < 0.0 {
            Self {
                cell,
                step: -1,
                next_border: (origin - floor) / -direction,
                border_spacing: -1.0 / direction,
            }
        } else {
            Self {
                cell,
                step: 0,
                next_border: f32::INFINITY,
                border_spacing: f32::INFINITY,
            }
        }
    }
}

/// The face of a block that a ray moving by `step` along `axis` enters through.
fn entry_face(axis: usize, step: i32) -> Face {
    match (axis, step > 0) {
        (AXIS_X, true) => Face::NegX,
        (AXIS_X, false) => Face::PosX,
        (AXIS_Y, true) => Face::NegY,
        (AXIS_Y, false) => Face::PosY,
        (_, true) => Face::NegZ,
        (_, false) => Face::PosZ,
    }
}

/// Casts a ray from `origin` along `direction` and returns the first solid block within
/// `max_distance` blocks, or `None`.
///
/// - Solid means any block that is not air.
/// - `direction` does not need to be a unit vector; only its direction counts.
/// - A ray that reaches a chunk that is not loaded stops there and returns `None`: what
///   lies beyond is unknown.
/// - Returns `None` for a zero or non-finite direction, a non-finite or negative
///   `max_distance`, and an origin that is not finite or lies beyond 2^24 blocks.
/// - The cell that contains the origin is always tested first, even if the origin lies
///   exactly on its border and the ray moves away from it.
/// - If the origin is inside a solid block, that block is the hit with distance zero
///   (see [`RayHit::face`]).
///
/// # Example
///
/// ```
/// use vox_world::{BlockId, Chunk, ChunkPos, Face, World, raycast};
///
/// let mut world = World::new();
/// world.insert(ChunkPos::ORIGIN, Chunk::new());
/// world.set_block([5, 0, 0], BlockId(1));
/// let hit = raycast(&world, [0.5, 0.5, 0.5], [1.0, 0.0, 0.0], 10.0).unwrap();
/// assert_eq!(hit.block, [5, 0, 0]);
/// assert_eq!(hit.face, Face::NegX);
/// assert_eq!(hit.adjacent(), [4, 0, 0]);
/// ```
pub fn raycast(
    world: &World,
    origin: [f32; 3],
    direction: [f32; 3],
    max_distance: f32,
) -> Option<RayHit> {
    if !max_distance.is_finite() || max_distance < 0.0 {
        return None;
    }
    if origin
        .iter()
        .any(|c| !c.is_finite() || c.abs() > MAX_ORIGIN_COORDINATE)
    {
        return None;
    }
    let unit = normalized(direction)?;
    let mut axes = [
        AxisState::new(origin[AXIS_X], unit[AXIS_X]),
        AxisState::new(origin[AXIS_Y], unit[AXIS_Y]),
        AxisState::new(origin[AXIS_Z], unit[AXIS_Z]),
    ];

    let start = [axes[AXIS_X].cell, axes[AXIS_Y].cell, axes[AXIS_Z].cell];
    let id = world.block_at(start)?;
    if !id.is_air() {
        let axis = strongest_axis(unit);
        return Some(RayHit {
            block: start,
            id,
            face: entry_face(axis, axes[axis].step.signum()),
            distance: 0.0,
        });
    }

    loop {
        let axis = nearest_border(&axes);
        let state = &mut axes[axis];
        let distance = state.next_border;
        if distance > max_distance {
            return None;
        }
        state.cell = state.cell.checked_add(state.step)?;
        state.next_border += state.border_spacing;

        let block = [axes[AXIS_X].cell, axes[AXIS_Y].cell, axes[AXIS_Z].cell];
        let id = world.block_at(block)?;
        if !id.is_air() {
            return Some(RayHit {
                block,
                id,
                face: entry_face(axis, axes[axis].step),
                distance,
            });
        }
    }
}

/// The unit vector along `direction`, or `None` if it is zero or not finite.
///
/// Dividing by the largest component first keeps every component in -1..=1, so squaring
/// them cannot overflow (1e20) or underflow to zero (1e-30) the way a direct length would.
fn normalized(direction: [f32; 3]) -> Option<[f32; 3]> {
    if direction.iter().any(|c| !c.is_finite()) {
        return None;
    }
    let largest = direction.iter().fold(0.0_f32, |max, c| max.max(c.abs()));
    if largest <= 0.0 {
        return None;
    }
    let scaled = direction.map(|c| c / largest);
    let length = scaled.iter().map(|c| c * c).sum::<f32>().sqrt();
    Some(scaled.map(|c| c / length))
}

/// The axis whose cell border the ray reaches next. Ties go to X, then Y, then Z, so
/// a ray through an exact corner is deterministic.
fn nearest_border(axes: &[AxisState; 3]) -> usize {
    let mut best = AXIS_X;
    for axis in [AXIS_Y, AXIS_Z] {
        if axes[axis].next_border < axes[best].next_border {
            best = axis;
        }
    }
    best
}

/// The axis along which the unit vector `direction` is longest. Ties go to X, then Y,
/// then Z.
fn strongest_axis(direction: [f32; 3]) -> usize {
    let mut best = AXIS_X;
    for axis in [AXIS_Y, AXIS_Z] {
        if direction[axis].abs() > direction[best].abs() {
            best = axis;
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunk::{CHUNK_SIZE, Chunk};
    use crate::chunk_pos::ChunkPos;

    const STONE: BlockId = BlockId(1);
    const FAR: f32 = 100.0;

    /// A world of empty chunks around the origin: chunk coordinates -2..=1 on every axis.
    fn empty_world() -> World {
        let mut world = World::new();
        for x in -2..=1 {
            for y in -2..=1 {
                for z in -2..=1 {
                    world.insert(ChunkPos::new(x, y, z), Chunk::new());
                }
            }
        }
        world
    }

    fn world_with(blocks: &[[i32; 3]]) -> World {
        let mut world = empty_world();
        for &block in blocks {
            assert!(world.set_block(block, STONE).is_some());
        }
        world
    }

    #[test]
    fn hits_a_block_straight_ahead_and_reports_the_entered_face() {
        let world = world_with(&[[5, 0, 0]]);
        let hit = raycast(&world, [0.5, 0.5, 0.5], [1.0, 0.0, 0.0], FAR).unwrap();
        assert_eq!(hit.block, [5, 0, 0]);
        assert_eq!(hit.id, STONE);
        assert_eq!(hit.face, Face::NegX);
        assert_eq!(hit.adjacent(), [4, 0, 0]);
        assert!((hit.distance - 4.5).abs() < 1e-5);
    }

    #[test]
    fn face_of_entry_for_every_axis_and_sign() {
        let cases = [
            ([1.0, 0.0, 0.0], [3, 0, 0], Face::NegX),
            ([-1.0, 0.0, 0.0], [-3, 0, 0], Face::PosX),
            ([0.0, 1.0, 0.0], [0, 3, 0], Face::NegY),
            ([0.0, -1.0, 0.0], [0, -3, 0], Face::PosY),
            ([0.0, 0.0, 1.0], [0, 0, 3], Face::NegZ),
            ([0.0, 0.0, -1.0], [0, 0, -3], Face::PosZ),
        ];
        for (direction, block, face) in cases {
            let world = world_with(&[block]);
            let hit = raycast(&world, [0.5, 0.5, 0.5], direction, FAR).unwrap();
            assert_eq!(hit.block, block, "{direction:?}");
            assert_eq!(hit.face, face, "{direction:?}");
        }
    }

    #[test]
    fn direction_length_does_not_matter() {
        let world = world_with(&[[0, 0, 7]]);
        let short = raycast(&world, [0.5, 0.5, 0.5], [0.0, 0.0, 0.001], FAR).unwrap();
        let long = raycast(&world, [0.5, 0.5, 0.5], [0.0, 0.0, 1000.0], FAR).unwrap();
        assert_eq!(short, long);
    }

    #[test]
    fn works_in_negative_coordinates() {
        let world = world_with(&[[-6, -1, -1]]);
        let hit = raycast(&world, [-0.5, -0.5, -0.5], [-1.0, 0.0, 0.0], FAR).unwrap();
        assert_eq!(hit.block, [-6, -1, -1]);
        assert_eq!(hit.face, Face::PosX);
        assert!((hit.distance - 4.5).abs() < 1e-5);
    }

    #[test]
    fn crosses_chunk_borders_both_ways() {
        let edge = CHUNK_SIZE as i32;
        let world = world_with(&[[edge + 3, 0, 0], [-edge - 3, 0, 0]]);
        let forward = raycast(&world, [0.5, 0.5, 0.5], [1.0, 0.0, 0.0], FAR).unwrap();
        assert_eq!(forward.block, [edge + 3, 0, 0]);
        let backward = raycast(&world, [0.5, 0.5, 0.5], [-1.0, 0.0, 0.0], FAR).unwrap();
        assert_eq!(backward.block, [-edge - 3, 0, 0]);
    }

    #[test]
    fn diagonal_ray_visits_the_right_cell() {
        // From the middle of cell (0,0,0) at 45 degrees in XZ: passes (1,0,1) corner-wise.
        let world = world_with(&[[3, 0, 3]]);
        let hit = raycast(&world, [0.5, 0.5, 0.5], [1.0, 0.0, 1.0], FAR).unwrap();
        assert_eq!(hit.block, [3, 0, 3]);
        // Ties go to X first, so the ray walks (2,0,2) -> (3,0,2) -> (3,0,3): last step is Z.
        assert_eq!(hit.face, Face::NegZ);
    }

    #[test]
    fn oblique_ray_hits_the_first_block_in_order() {
        let world = world_with(&[[2, 1, 0], [4, 2, 0]]);
        let hit = raycast(&world, [0.0, 0.0, 0.5], [2.0, 1.0, 0.0], FAR).unwrap();
        assert_eq!(hit.block, [2, 1, 0]);
    }

    #[test]
    fn ray_that_passes_beside_a_block_misses() {
        let world = world_with(&[[5, 1, 0]]);
        assert!(raycast(&world, [0.5, 0.5, 0.5], [1.0, 0.0, 0.0], FAR).is_none());
    }

    #[test]
    fn max_distance_limits_the_ray() {
        let world = world_with(&[[5, 0, 0]]);
        let origin = [0.5, 0.5, 0.5];
        let direction = [1.0, 0.0, 0.0];
        assert!(raycast(&world, origin, direction, 4.4).is_none());
        assert!(raycast(&world, origin, direction, 4.6).is_some());
        assert!(raycast(&world, origin, direction, 0.0).is_none());
    }

    #[test]
    fn invalid_directions_give_none() {
        let world = world_with(&[[0, 0, 0]]);
        let origin = [0.5, 0.5, 0.5];
        for direction in [
            [0.0, 0.0, 0.0],
            [f32::NAN, 0.0, 1.0],
            [1.0, f32::INFINITY, 0.0],
            [f32::NEG_INFINITY, 0.0, 0.0],
        ] {
            assert!(
                raycast(&world, origin, direction, FAR).is_none(),
                "{direction:?}"
            );
        }
    }

    #[test]
    fn huge_and_tiny_directions_still_work() {
        let world = world_with(&[[0, 0, 7]]);
        for scale in [1.0e20, 1.0e-30, 1.0e-40, f32::MAX] {
            let hit = raycast(&world, [0.5, 0.5, 0.5], [0.0, 0.0, scale], FAR).unwrap();
            assert_eq!(hit.block, [0, 0, 7], "{scale}");
            let hit = raycast(&world, [0.5, 0.5, 0.5], [scale, 0.0, 0.0], FAR);
            assert!(hit.is_none(), "{scale}");
        }
    }

    #[test]
    fn origin_limit_is_two_to_the_power_of_24() {
        let edge = 1 << 24;
        let mut world = World::new();
        let (chunk, local) = ChunkPos::containing([edge + 3, 0, 0]);
        let mut loaded = Chunk::new();
        loaded.set(local, STONE);
        world.insert(chunk, loaded);
        let direction = [1.0, 0.0, 0.0];
        let hit = raycast(&world, [edge as f32, 0.5, 0.5], direction, FAR).unwrap();
        assert_eq!(hit.block, [edge + 3, 0, 0]);
        assert!(raycast(&world, [edge as f32 + 2.0, 0.5, 0.5], direction, FAR).is_none());
    }

    #[test]
    fn invalid_origins_and_distances_give_none() {
        let world = world_with(&[[3, 0, 0]]);
        let direction = [1.0, 0.0, 0.0];
        assert!(raycast(&world, [f32::NAN, 0.5, 0.5], direction, FAR).is_none());
        assert!(raycast(&world, [f32::INFINITY, 0.5, 0.5], direction, FAR).is_none());
        assert!(raycast(&world, [3.0e7, 0.5, 0.5], direction, FAR).is_none());
        assert!(raycast(&world, [0.5, 0.5, 0.5], direction, f32::NAN).is_none());
        assert!(raycast(&world, [0.5, 0.5, 0.5], direction, f32::INFINITY).is_none());
        assert!(raycast(&world, [0.5, 0.5, 0.5], direction, -1.0).is_none());
    }

    #[test]
    fn origin_inside_a_solid_block_hits_it_at_distance_zero() {
        let world = world_with(&[[0, 0, 0]]);
        let hit = raycast(&world, [0.5, 0.5, 0.5], [0.0, -1.0, 0.2], FAR).unwrap();
        assert_eq!(hit.block, [0, 0, 0]);
        assert_eq!(hit.distance, 0.0);
        assert_eq!(hit.face, Face::PosY);
        let hit = raycast(&world, [0.5, 0.5, 0.5], [2.0, 1.0, -1.0], FAR).unwrap();
        assert_eq!(hit.face, Face::NegX);
    }

    #[test]
    fn unloaded_chunk_stops_the_ray() {
        let mut world = World::new();
        world.insert(ChunkPos::ORIGIN, Chunk::new());
        // The solid block is behind the unloaded chunk (1, 0, 0).
        let mut far = Chunk::new();
        far.set(crate::chunk::LocalPos::new(0, 0, 0).unwrap(), STONE);
        world.insert(ChunkPos::new(2, 0, 0), far);
        assert!(raycast(&world, [0.5, 0.5, 0.5], [1.0, 0.0, 0.0], 200.0).is_none());
    }

    #[test]
    fn unloaded_origin_chunk_gives_none() {
        let world = World::new();
        assert!(raycast(&world, [0.5, 0.5, 0.5], [1.0, 0.0, 0.0], FAR).is_none());
    }

    #[test]
    fn origin_exactly_on_a_cell_border_moving_negative_enters_the_previous_cell() {
        let world = world_with(&[[-1, 0, 0]]);
        let hit = raycast(&world, [0.0, 0.5, 0.5], [-1.0, 0.0, 0.0], FAR).unwrap();
        assert_eq!(hit.block, [-1, 0, 0]);
        assert_eq!(hit.face, Face::PosX);
        assert_eq!(hit.distance, 0.0);
    }

    #[test]
    fn adjacent_saturates_at_the_end_of_the_range() {
        let hit = RayHit {
            block: [i32::MAX, 0, 0],
            id: STONE,
            face: Face::PosX,
            distance: 1.0,
        };
        assert_eq!(hit.adjacent(), [i32::MAX, 0, 0]);
    }
}
