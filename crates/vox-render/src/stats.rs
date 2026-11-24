//! Frame statistics: what the renderer drew and how long the GPU took.
//!
//! The draw counts are summed here while a frame is recorded; this module is pure
//! bookkeeping so it is unit tested without a GPU. The GPU time is measured by the
//! backend and only carried here.

use std::time::Duration;

use vox_world::MAX_LOD_LEVEL;

/// Number of tile levels the renderer counts apart: level 0 (chunks) up to
/// [`MAX_LOD_LEVEL`], the length of [`RenderStats::triangles_per_level`].
pub const LOD_LEVEL_COUNT: usize = MAX_LOD_LEVEL as usize + 1;

/// Indices per triangle in a triangle list, the only topology the chunk pipeline uses.
pub(crate) const INDICES_PER_TRIANGLE: u32 = 3;

/// What the renderer drew and how long the GPU took.
///
/// Returned by [`crate::Renderer::stats`]. The counts cover the meshes (chunk or tile) only: the
/// overlay ([`crate::Renderer::set_overlay`]) adds no meshes, draw calls or triangles,
/// though its GPU work is inside `gpu_time`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RenderStats {
    /// Meshes (each the mesh of a chunk or of a level-of-detail tile) drawn in the most recently
    /// recorded frame.
    ///
    /// "Recorded" means [`crate::Renderer::render_frame`] built that frame's GPU
    /// commands. A call that records nothing (minimized window, swapchain being rebuilt,
    /// window image out of date) leaves the previous counts in place. Zero before the
    /// first frame and for a frame without any mesh.
    ///
    /// A mesh (chunk or tile) in view whose faces all look away from the camera
    /// (per-direction culling) gets no draw and is not counted.
    pub meshes_drawn: usize,
    /// Draw calls of meshes (chunk or tile) in that frame: one to three per mesh, one for each
    /// contiguous run of the face directions that can face the camera (per-direction
    /// culling); the overlay's draw is not counted.
    pub draw_calls: usize,
    /// Triangles in that frame: the sum over its draws of the index count divided by
    /// three (the meshes are triangle lists), saturating at `u64::MAX`. Only the face
    /// directions actually drawn count, not the whole mesh.
    pub triangles: u64,
    /// [`Self::triangles`] split by tile level: entry `k` counts the triangles of tiles of
    /// level `k` (entry 0 those of full-detail chunks). The entries add up to
    /// [`Self::triangles`] (each saturating the same way).
    pub triangles_per_level: [u64; LOD_LEVEL_COUNT],
    /// Time the GPU spent on the most recent frame whose result has been read, or `None`
    /// if the device cannot measure it or no frame has finished yet.
    ///
    /// The span runs from the start to the end of the frame's GPU commands; presenting
    /// the image is not included. Treat it as an **upper bound** of the work: the start
    /// timestamp is written at the top of the pipe, while the frame's color output waits
    /// for the swapchain image, so that wait counts; and when the GPU is the bottleneck a
    /// frame can start while the previous one still runs, so the spans overlap. When the
    /// GPU has idle time between frames (a frame limiter) the figure is close to the work.
    ///
    /// A frame's result is read once the renderer has proven that frame finished, which
    /// happens when a later frame reuses its resources: with frames in flight this lags a
    /// couple of frames behind the counts above. A measurement that cannot be read keeps the
    /// previous value, so the value repeats instead of vanishing.
    pub gpu_time: Option<Duration>,
}

impl RenderStats {
    /// Counts one mesh (chunk or tile) that has at least one draw in the frame.
    // HOT PATH: runs for every mesh drawn every frame; one increment.
    pub(crate) fn count_mesh(&mut self) {
        // Cannot overflow: the meshes live in memory.
        self.meshes_drawn += 1;
    }

    /// Counts one indexed draw with `index_count` indices of a mesh of a tile of `level`
    /// (0 for a chunk). A level above [`MAX_LOD_LEVEL`] counts as [`MAX_LOD_LEVEL`], as
    /// `vox_world::TilePos` clamps it.
    // HOT PATH: runs for every draw every frame; one increment and two saturating adds.
    pub(crate) fn count_draw(&mut self, level: u8, index_count: u32) {
        // Cannot overflow: a mesh has at most a few draws, and the meshes live in memory.
        self.draw_calls += 1;
        let triangles = u64::from(index_count / INDICES_PER_TRIANGLE);
        self.triangles = self.triangles.saturating_add(triangles);
        let entry = &mut self.triangles_per_level[usize::from(level.min(MAX_LOD_LEVEL))];
        *entry = entry.saturating_add(triangles);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_frame_without_draws_reports_zeros() {
        let stats = RenderStats::default();
        assert_eq!(
            (stats.meshes_drawn, stats.draw_calls, stats.triangles),
            (0, 0, 0)
        );
        assert_eq!(stats.gpu_time, None);
    }

    #[test]
    fn each_draw_counts_a_call_and_its_triangles_and_chunks_count_apart() {
        let mut stats = RenderStats::default();
        stats.count_mesh();
        stats.count_draw(0, 6); // one quad
        stats.count_mesh();
        stats.count_draw(0, 12); // two quads of a cube
        stats.count_draw(0, 6); // a third quad, apart from the other two
        assert_eq!(stats.meshes_drawn, 2);
        assert_eq!(stats.draw_calls, 3);
        assert_eq!(stats.triangles, 8);
        assert_eq!(stats.triangles_per_level, [8, 0, 0, 0, 0]);
    }

    #[test]
    fn triangles_are_split_by_level_and_add_up_to_the_total() {
        let mut stats = RenderStats::default();
        stats.count_draw(0, 6);
        stats.count_draw(2, 12);
        stats.count_draw(2, 6);
        stats.count_draw(MAX_LOD_LEVEL, 30);
        // A level past the maximum is the maximum, as `TilePos` clamps it.
        stats.count_draw(u8::MAX, 3);
        assert_eq!(stats.triangles_per_level, [2, 0, 6, 0, 11]);
        assert_eq!(
            stats.triangles_per_level.iter().sum::<u64>(),
            stats.triangles
        );
        assert_eq!(LOD_LEVEL_COUNT, 5);
    }

    #[test]
    fn the_largest_mesh_counts_all_its_triangles() {
        let mut stats = RenderStats::default();
        stats.count_draw(1, u32::MAX);
        assert_eq!(stats.triangles, u64::from(u32::MAX / 3));
        assert_eq!(stats.triangles_per_level[1], stats.triangles);
    }

    #[test]
    fn the_triangle_sum_saturates_instead_of_overflowing() {
        let mut stats = RenderStats {
            triangles: u64::MAX - 1,
            triangles_per_level: [u64::MAX - 1; LOD_LEVEL_COUNT],
            ..RenderStats::default()
        };
        stats.count_draw(3, u32::MAX);
        assert_eq!(stats.triangles, u64::MAX);
        assert_eq!(stats.triangles_per_level[3], u64::MAX);
    }
}
