//! Frame statistics: what the renderer drew and how long the GPU took.
//!
//! The draw counts are summed here while a frame is recorded; this module is pure
//! bookkeeping so it is unit tested without a GPU. The GPU time is measured by the
//! backend and only carried here.

use std::time::Duration;

/// Indices per triangle in a triangle list, the only topology the chunk pipeline uses.
pub(crate) const INDICES_PER_TRIANGLE: u32 = 3;

/// What the renderer drew and how long the GPU took.
///
/// Returned by [`crate::Renderer::stats`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RenderStats {
    /// Chunk meshes drawn in the most recently recorded frame.
    ///
    /// "Recorded" means [`crate::Renderer::render_frame`] built that frame's GPU
    /// commands. A call that records nothing (minimized window, swapchain being rebuilt,
    /// window image out of date) leaves the previous counts in place. Zero before the
    /// first frame and for a frame without any mesh.
    pub chunks_drawn: usize,
    /// Draw calls in that frame (one per chunk today).
    pub draw_calls: usize,
    /// Triangles in that frame: the sum over its draws of the index count divided by
    /// three (the meshes are triangle lists), saturating at `u64::MAX`.
    pub triangles: u64,
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
    /// Counts one indexed draw of a chunk mesh with `index_count` indices.
    // HOT PATH: runs for every chunk every frame; two increments and one saturating add.
    pub(crate) fn count_chunk_draw(&mut self, index_count: u32) {
        // Cannot overflow: there is one draw per mesh, and the meshes live in memory.
        self.chunks_drawn += 1;
        self.draw_calls += 1;
        let triangles = u64::from(index_count / INDICES_PER_TRIANGLE);
        self.triangles = self.triangles.saturating_add(triangles);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_frame_without_draws_reports_zeros() {
        let stats = RenderStats::default();
        assert_eq!(
            (stats.chunks_drawn, stats.draw_calls, stats.triangles),
            (0, 0, 0)
        );
        assert_eq!(stats.gpu_time, None);
    }

    #[test]
    fn each_draw_counts_a_chunk_a_call_and_its_triangles() {
        let mut stats = RenderStats::default();
        stats.count_chunk_draw(6); // one quad
        stats.count_chunk_draw(36); // one cube
        assert_eq!(stats.chunks_drawn, 2);
        assert_eq!(stats.draw_calls, 2);
        assert_eq!(stats.triangles, 14);
    }

    #[test]
    fn the_largest_mesh_counts_all_its_triangles() {
        let mut stats = RenderStats::default();
        stats.count_chunk_draw(u32::MAX);
        assert_eq!(stats.triangles, u64::from(u32::MAX / 3));
    }

    #[test]
    fn the_triangle_sum_saturates_instead_of_overflowing() {
        let mut stats = RenderStats {
            triangles: u64::MAX - 1,
            ..RenderStats::default()
        };
        stats.count_chunk_draw(u32::MAX);
        assert_eq!(stats.triangles, u64::MAX);
    }
}
