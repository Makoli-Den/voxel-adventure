//! How far the player sees: the fog and the camera's far plane, both derived from the view
//! radius, so the end of the loaded terrain is never visible (decision 0015, fog step).
//!
//! The fog is complete before the nearest place where terrain may be missing, and the fog
//! colour is the sky colour, so the edge of the loaded area melts into the sky. The far plane
//! lies just beyond the fog end: everything farther would be drawn fully fogged, that is
//! invisible, so the renderer's frustum test may skip it.

use std::f32::consts::SQRT_2;

use vox_render::Fog;
use vox_world::CHUNK_SIZE;

/// `CHUNK_SIZE` as a float, to turn distances in chunks into blocks.
const CHUNK_SIZE_F32: f32 = CHUNK_SIZE as f32;

/// How far inside the view radius the nearest missing terrain can lie, in chunks: the square
/// root of 2. A column is loaded when its offset from the camera's column, in whole chunks, is
/// within the radius (`streaming::area`). The camera may be anywhere in its column and a
/// missing column shows it its nearest corner, which brings that corner up to one chunk closer
/// on each horizontal axis, so at most `sqrt(1 + 1)` chunks closer than the column offset. A
/// test checks the bound against the real loaded set.
const MISSING_TERRAIN_INSET_CHUNKS: f32 = SQRT_2;

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

/// The fog for a view radius in chunks: complete just before the nearest terrain that may be
/// missing, starting at [`FOG_START_FRACTION`] of that.
pub fn fog(view_radius: i32) -> Fog {
    let radius = view_radius.max(0) as f32;
    let end = ((radius - MISSING_TERRAIN_INSET_CHUNKS) * CHUNK_SIZE_F32).max(MIN_FOG_END);
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

    use vox_world::ChunkPos;

    use super::*;
    use crate::cli::{DEFAULT_VIEW_RADIUS, MAX_VIEW_RADIUS};
    use crate::streaming::area;

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
            let loaded: HashSet<ChunkPos> =
                area::wanted(ChunkPos::ORIGIN, radius).into_iter().collect();
            let reach = radius + 2;
            let fog = fog(radius);
            for z in -reach..=reach {
                for x in -reach..=reach {
                    let column = ChunkPos::new(x, area::TERRAIN_LAYER, z);
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

    #[test]
    fn fog_is_on_for_every_radius_and_ends_before_the_far_plane() {
        for radius in 0..=MAX_VIEW_RADIUS {
            let fog = fog(radius);
            assert!(fog.is_enabled(), "radius {radius}");
            assert!(fog.end >= MIN_FOG_END, "radius {radius}");
            assert!(far_plane(fog) > fog.end, "radius {radius}");
        }
    }

    #[test]
    fn the_default_fog_starts_at_the_fraction_of_its_end() {
        let fog = fog(DEFAULT_VIEW_RADIUS);
        let expected_end = (DEFAULT_VIEW_RADIUS as f32 - SQRT_2) * CHUNK_SIZE_F32;
        assert!((fog.end - expected_end).abs() < 1e-3);
        assert!((fog.start - FOG_START_FRACTION * expected_end).abs() < 1e-3);
    }

    #[test]
    fn a_negative_radius_counts_as_zero() {
        assert_eq!(fog(-5), fog(0));
    }
}
