//! How far the player sees: the horizon the game can afford, the fog and the camera's far plane,
//! so the end of the drawn terrain is never visible (decision 0015, fog step and S7).
//!
//! The terrain is drawn in full detail within the view radius and as level-of-detail tiles out to
//! the horizon. The fog is complete before the nearest place where terrain may be missing, and
//! the fog colour is the sky colour, so the edge of the terrain melts into the sky. The far plane
//! lies just beyond the fog end: everything farther would be drawn fully fogged, that is
//! invisible, so the renderer's frustum test may skip it.

use std::f32::consts::SQRT_2;

use vox_render::Fog;
use vox_world::CHUNK_SIZE;

use crate::streaming::lod;

/// `CHUNK_SIZE` as a float, to turn distances in chunks into blocks.
const CHUNK_SIZE_F32: f32 = CHUNK_SIZE as f32;

/// How far inside the view radius the nearest missing terrain can lie, in chunks: the square
/// root of 2. A column is loaded when its offset from the camera's column, in whole chunks, is
/// within the radius (`streaming::area`). The camera may be anywhere in its column and a
/// missing column shows it its nearest corner, which brings that corner up to one chunk closer
/// on each horizontal axis, so at most `sqrt(1 + 1)` chunks closer than the column offset. A
/// test checks the bound against the real loaded set.
const MISSING_TERRAIN_INSET_CHUNKS: f32 = SQRT_2;

/// How far inside the horizon the nearest missing terrain can lie, in blocks: half the diagonal
/// of a column, about 22.6. The tiles cover the disc of the horizon around the centre of the
/// camera's column (`streaming::lod`), and the camera can be that far from the centre. A test
/// checks the bound against the real selection.
const HORIZON_INSET_BLOCKS: f32 = CHUNK_SIZE_F32 * SQRT_2 / 2.0;

/// Where the fog starts, as a fraction of where it ends: the value decision 0015 proposed,
/// kept after looking at screenshots at radius 8 and 16 (the start view and from 200 blocks
/// up): the nearer terrain stays crisp, and the fade over the last 40 percent shows no band
/// and no visible edge. Other values were not compared.
const FOG_START_FRACTION: f32 = 0.6;

/// The smallest fog end, in blocks: one chunk. Below a view radius of 2 the bound of
/// [`MISSING_TERRAIN_INSET_CHUNKS`] leaves no fog-free distance at all; the camera then sees
/// its own chunk fade out, and the far plane stays well beyond the near plane.
const MIN_FOG_END: f32 = CHUNK_SIZE_F32;

/// How far the far plane lies beyond the fog end, in blocks. The far plane clips by depth
/// along the view direction, which is never more than the straight-line distance the fog
/// uses, so the fog end alone would already clip only fully fogged faces; the margin keeps
/// rounding in the depth mapping from clipping a face whose last fragments are not quite
/// fully fogged yet.
const FAR_PLANE_MARGIN: f32 = 1.0;

/// What lowered the horizon below the one asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HorizonLimit {
    /// The mesh budget at this view radius ([`lod::max_horizon_for`]).
    MeshBudget,
    /// The depth buffer's precision (`DepthMode::max_horizon`, a 24-bit buffer).
    DepthBuffer,
}

/// The horizon the game uses and, if it is lower than the one asked for, why.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Horizon {
    /// The horizon in blocks; 0 draws no tiles.
    pub blocks: i32,
    /// What lowered it, or `None` when it is the one asked for.
    pub limited_by: Option<HorizonLimit>,
}

/// The horizon for a request of `requested` blocks at `view_radius`: the smallest of the
/// request, the mesh budget's [`lod::max_horizon_for`] the radius and `depth_limit` (the
/// renderer's `DepthMode::max_horizon`, `None` for no limit). A request of 0 or less stays 0
/// (no tiles). When two limits are equal the depth buffer is named, since it is the one the
/// player can not change with an option.
///
/// Runs a few tile selections (well under a millisecond): call it when an option changes.
pub fn effective_horizon(requested: i32, view_radius: i32, depth_limit: Option<u32>) -> Horizon {
    let requested = requested.min(lod::MAX_HORIZON_BLOCKS);
    if requested <= 0 {
        return Horizon {
            blocks: 0,
            limited_by: None,
        };
    }
    let budget = lod::max_horizon_for(view_radius);
    let depth = depth_limit.map_or(i32::MAX, |blocks| i32::try_from(blocks).unwrap_or(i32::MAX));
    let mut horizon = Horizon {
        blocks: requested,
        limited_by: None,
    };
    if budget < horizon.blocks {
        horizon = Horizon {
            blocks: budget,
            limited_by: Some(HorizonLimit::MeshBudget),
        };
    }
    if depth <= horizon.blocks && depth < requested {
        horizon = Horizon {
            blocks: depth,
            limited_by: Some(HorizonLimit::DepthBuffer),
        };
    }
    horizon
}

/// The note printed when the horizon was lowered, or `None` when it was not.
pub fn horizon_note(requested: i32, view_radius: i32, horizon: Horizon) -> Option<String> {
    let reason = match horizon.limited_by? {
        HorizonLimit::MeshBudget => {
            format!("to keep the mesh budget at view radius {view_radius}")
        }
        HorizonLimit::DepthBuffer => {
            "because the GPU has no 32-bit depth buffer (reversed depth)".to_owned()
        }
    };
    Some(format!(
        "note: horizon lowered from {requested} to {} blocks {reason}",
        horizon.blocks
    ))
}

/// The fog for a view radius in chunks and a horizon in blocks (0: no tiles): complete just
/// before the nearest terrain that may be missing, starting at [`FOG_START_FRACTION`] of that.
///
/// The terrain drawn is the full-detail circle together with the disc of tiles, so the nearest
/// missing terrain is at least as far as the farther of the two bounds.
pub fn fog(view_radius: i32, horizon: i32) -> Fog {
    let radius = view_radius.max(0) as f32;
    let radius_end = ((radius - MISSING_TERRAIN_INSET_CHUNKS) * CHUNK_SIZE_F32).max(MIN_FOG_END);
    let end = if horizon > 0 {
        radius_end.max(horizon as f32 - HORIZON_INSET_BLOCKS)
    } else {
        radius_end
    };
    Fog {
        start: FOG_START_FRACTION * end,
        end,
    }
}

/// The distance of the camera's far plane for `fog`, in blocks: just beyond its end.
pub fn far_plane(fog: Fog) -> f32 {
    fog.end + FAR_PLANE_MARGIN
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use vox_render::D24_MAX_HORIZON;
    use vox_world::{MAX_LOD_LEVEL, TilePos};

    use super::*;
    use crate::cli::{DEFAULT_VIEW_RADIUS, MAX_VIEW_RADIUS};
    use crate::streaming::area;
    use crate::streaming::column::Column;

    /// The horizontal distance in blocks from the nearest point of the camera's column to the
    /// nearest point of the column `offset` chunks away: the closest that column can come.
    fn closest_distance(offset: (i32, i32)) -> f32 {
        let gap = |chunks: i32| (chunks.abs() - 1).max(0) as f32 * CHUNK_SIZE_F32;
        gap(offset.0).hypot(gap(offset.1))
    }

    #[test]
    fn fog_is_complete_before_any_missing_terrain() {
        // From radius 2 on; below that the fog cannot end before its minimum.
        for radius in 2..=MAX_VIEW_RADIUS {
            let loaded: HashSet<Column> = area::wanted(Column::new(0, 0), radius)
                .into_iter()
                .collect();
            let reach = radius + 2;
            let fog = fog(radius, 0);
            for z in -reach..=reach {
                for x in -reach..=reach {
                    let column = Column::new(x, z);
                    if !loaded.contains(&column) {
                        let distance = closest_distance((x, z));
                        assert!(
                            fog.end <= distance,
                            "radius {radius}: column {x}, {z} at {distance} < {}",
                            fog.end
                        );
                    }
                }
            }
        }
    }

    /// Whether a column is drawn by the selection around `camera`: a level-0 leaf, a column of
    /// the radius's area, or inside a tile of level 1 or more.
    fn drawn(columns: &HashSet<Column>, tiles: &HashSet<TilePos>, column: Column) -> bool {
        columns.contains(&column)
            || (1..=MAX_LOD_LEVEL).any(|level| {
                let pos = vox_world::ChunkPos::new(column.x >> level, 0, column.z >> level);
                tiles.contains(&TilePos::new(level, pos))
            })
    }

    #[test]
    fn fog_with_a_horizon_is_complete_before_any_missing_terrain() {
        for (radius, horizon) in [(2, 400), (DEFAULT_VIEW_RADIUS, 700), (4, 100)] {
            let centre = Column::new(-3, 7);
            let leaves = lod::select_tiles(centre.chunk(0), radius, horizon);
            let mut columns: HashSet<Column> = area::wanted(centre, radius).into_iter().collect();
            columns.extend(
                leaves
                    .iter()
                    .filter(|t| t.level() == 0)
                    .map(|t| Column::of(t.pos())),
            );
            let tiles: HashSet<TilePos> = leaves.into_iter().filter(|t| t.level() > 0).collect();
            let fog = fog(radius, horizon);
            let reach = horizon / CHUNK_SIZE as i32 + radius + 2;
            for dz in -reach..=reach {
                for dx in -reach..=reach {
                    let column = Column::new(centre.x + dx, centre.z + dz);
                    if !drawn(&columns, &tiles, column) {
                        let distance = closest_distance((dx, dz));
                        assert!(
                            fog.end <= distance,
                            "r{radius} h{horizon}: {column:?} at {distance} < {}",
                            fog.end
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn the_horizon_fog_ends_the_inset_before_the_horizon() {
        let far_fog = fog(DEFAULT_VIEW_RADIUS, lod::DEFAULT_HORIZON_BLOCKS);
        let expected = lod::DEFAULT_HORIZON_BLOCKS as f32 - HORIZON_INSET_BLOCKS;
        assert!((far_fog.end - expected).abs() < 1e-3);
        assert!((HORIZON_INSET_BLOCKS - 22.627).abs() < 1e-3);
        assert!((far_fog.start - FOG_START_FRACTION * expected).abs() < 1e-3);
        assert!(far_plane(far_fog) > far_fog.end);
        // A horizon inside the full-detail circle does not pull the fog in.
        assert_eq!(fog(DEFAULT_VIEW_RADIUS, 50), fog(DEFAULT_VIEW_RADIUS, 0));
    }

    #[test]
    fn fog_is_on_for_every_radius_and_ends_before_the_far_plane() {
        for radius in 0..=MAX_VIEW_RADIUS {
            for horizon in [0, 1, lod::DEFAULT_HORIZON_BLOCKS, lod::MAX_HORIZON_BLOCKS] {
                let fog = fog(radius, horizon);
                assert!(fog.is_enabled(), "radius {radius}");
                assert!(fog.end >= MIN_FOG_END, "radius {radius}");
                assert!(far_plane(fog) > fog.end, "radius {radius}");
            }
        }
    }

    #[test]
    fn the_default_fog_without_a_horizon_starts_at_the_fraction_of_its_end() {
        let fog = fog(DEFAULT_VIEW_RADIUS, 0);
        let expected_end = (DEFAULT_VIEW_RADIUS as f32 - SQRT_2) * CHUNK_SIZE_F32;
        assert!((fog.end - expected_end).abs() < 1e-3);
        assert!((fog.start - FOG_START_FRACTION * expected_end).abs() < 1e-3);
    }

    #[test]
    fn a_negative_radius_counts_as_zero() {
        assert_eq!(fog(-5, 0), fog(0, 0));
    }

    fn horizon(blocks: i32, limited_by: Option<HorizonLimit>) -> Horizon {
        Horizon { blocks, limited_by }
    }

    #[test]
    fn the_effective_horizon_is_the_smallest_limit() {
        let depth = Some(D24_MAX_HORIZON);
        let d24 = D24_MAX_HORIZON as i32;
        let budget_16 = lod::max_horizon_for(MAX_VIEW_RADIUS);
        assert!(budget_16 < lod::MAX_HORIZON_BLOCKS && budget_16 > d24);
        // No tiles asked for: no tiles, whatever the limits.
        for limit in [None, depth] {
            assert_eq!(effective_horizon(0, 8, limit), horizon(0, None));
            assert_eq!(effective_horizon(-7, 16, limit), horizon(0, None));
        }
        // Below every limit: as asked.
        assert_eq!(effective_horizon(2048, 8, None), horizon(2048, None));
        assert_eq!(effective_horizon(900, 16, depth), horizon(900, None));
        // Exactly at a limit: as asked, nothing was lowered.
        assert_eq!(effective_horizon(d24, 8, depth), horizon(d24, None));
        assert_eq!(
            effective_horizon(budget_16, 16, None),
            horizon(budget_16, None)
        );
        // The mesh budget alone.
        assert_eq!(
            effective_horizon(4096, 16, None),
            horizon(budget_16, Some(HorizonLimit::MeshBudget))
        );
        assert_eq!(effective_horizon(4096, 8, None), horizon(4096, None));
        // The depth buffer alone.
        assert_eq!(
            effective_horizon(2048, 8, depth),
            horizon(d24, Some(HorizonLimit::DepthBuffer))
        );
        // Both: the lower one is named.
        assert_eq!(
            effective_horizon(4096, 16, depth),
            horizon(d24, Some(HorizonLimit::DepthBuffer))
        );
        assert_eq!(
            effective_horizon(4096, 16, Some(4000)),
            horizon(budget_16, Some(HorizonLimit::MeshBudget))
        );
        // Equal limits below the request: the depth buffer is named.
        assert_eq!(
            effective_horizon(4096, 16, Some(u32::try_from(budget_16).unwrap())),
            horizon(budget_16, Some(HorizonLimit::DepthBuffer))
        );
        // A request above the maximum counts as the maximum.
        assert_eq!(effective_horizon(i32::MAX, 8, None), horizon(4096, None));
    }

    #[test]
    fn a_note_is_printed_only_when_the_horizon_was_lowered() {
        assert_eq!(horizon_note(2048, 8, horizon(2048, None)), None);
        let budget =
            horizon_note(4096, 16, horizon(3328, Some(HorizonLimit::MeshBudget))).expect("a note");
        assert!(
            budget.contains("4096") && budget.contains("3328"),
            "{budget}"
        );
        assert!(budget.contains("radius 16"), "{budget}");
        let depth =
            horizon_note(2048, 8, horizon(1024, Some(HorizonLimit::DepthBuffer))).expect("a note");
        assert!(depth.contains("depth"), "{depth}");
    }
}
