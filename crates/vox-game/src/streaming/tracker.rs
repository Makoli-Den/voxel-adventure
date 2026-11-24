//! The bookkeeping of streaming: which chunk columns to load, which are loading, which are loaded.
//!
//! A pure state machine with no threads, no clock and no GPU, so every decision can be tested
//! with fake meshes. The one rule: a tracked column is forgotten only when it leaves the
//! keep range ([`area::is_kept`]). A column that is queued, in flight, waiting for upload or
//! loaded stays that way while the camera wanders near the border of the area.

use std::collections::{HashSet, VecDeque};

use super::area;
use super::column::Column;
use super::job::BuiltColumn;

/// What the tracker knows about the progress, for the HUD and the fill log line.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StreamStats {
    /// Columns whose chunks went to the game, including those with nothing to draw.
    pub loaded: usize,
    /// Columns wanted but not loaded yet: queued, in flight or waiting for upload.
    pub queued: usize,
}

/// Decides what to load, unload and upload around a moving centre.
#[derive(Debug)]
pub struct ChunkTracker {
    /// The view radius in chunks.
    radius: i32,
    /// At most this many columns are in flight plus waiting for upload.
    cap: usize,
    /// The column the area is centred on; `None` until the first [`Self::set_centre`].
    centre: Option<Column>,
    /// Columns to load, nearest first. Rebuilt whenever the centre changes.
    queue: VecDeque<Column>,
    /// Columns handed to a worker whose result has not arrived. A column that left the
    /// keep range and came back while its first job still ran is sent again, so the real
    /// number of running jobs can briefly exceed this count; the second result is dropped.
    in_flight: HashSet<Column>,
    /// Finished columns waiting to be handed out.
    ready: VecDeque<(Column, BuiltColumn)>,
    /// The columns that were handed out, including those with nothing to draw.
    loaded: HashSet<Column>,
}

impl ChunkTracker {
    /// Creates a tracker for the view `radius` that keeps at most `cap` columns in flight
    /// plus waiting for upload. The cap counts at least 1, or nothing would ever be sent.
    pub fn new(radius: i32, cap: usize) -> Self {
        Self {
            radius,
            cap: cap.max(1),
            centre: None,
            queue: VecDeque::new(),
            in_flight: HashSet::new(),
            ready: VecDeque::new(),
            loaded: HashSet::new(),
        }
    }

    /// Moves the centre; nothing happens if it did not change.
    ///
    /// Forgets every tracked column outside the keep range and returns the loaded ones among
    /// them, to remove from the game's world and the renderer. Rebuilds the queue from the
    /// wanted area minus everything already tracked.
    pub fn set_centre(&mut self, centre: Column) -> Vec<Column> {
        if self.centre == Some(centre) {
            return Vec::new();
        }
        self.centre = Some(centre);
        let radius = self.radius;
        let kept = |column: &Column| area::is_kept(centre, *column, radius);

        let removed: Vec<Column> = self
            .loaded
            .iter()
            .copied()
            .filter(|column| !kept(column))
            .collect();
        self.loaded.retain(kept);
        self.in_flight.retain(kept);
        self.ready.retain(|(column, _)| kept(column));

        let ready: HashSet<Column> = self.ready.iter().map(|(column, _)| *column).collect();
        self.queue = area::wanted(centre, radius)
            .into_iter()
            .filter(|column| {
                !self.loaded.contains(column)
                    && !self.in_flight.contains(column)
                    && !ready.contains(column)
            })
            .collect();
        removed
    }

    /// Takes columns from the front of the queue while in-flight plus ready is below the
    /// cap, and marks them in flight. The caller sends them to the workers.
    pub fn next_jobs(&mut self) -> Vec<Column> {
        let mut jobs = Vec::new();
        while self.in_flight.len() + self.ready.len() < self.cap {
            let Some(column) = self.queue.pop_front() else {
                break;
            };
            self.in_flight.insert(column);
            jobs.push(column);
        }
        jobs
    }

    /// A worker finished `column`. The result is dropped if `column` is not in flight (it left the
    /// keep range meanwhile, or an earlier job of the same column already delivered).
    pub fn finish(&mut self, column: Column, built: BuiltColumn) {
        if self.in_flight.remove(&column) {
            self.ready.push_back((column, built));
        }
    }

    /// The columns that are ready, marked loaded, in order, with at most `budget` meshes to
    /// upload among them. Empty meshes cost no upload, so a column with nothing to draw does
    /// not count against the budget, but it is returned too, because the game's world needs
    /// its blocks. A column with more meshes than the whole budget is taken alone, as the first
    /// of a frame, so it cannot block the queue.
    pub fn take_loaded(&mut self, budget: usize) -> Vec<(Column, BuiltColumn)> {
        let mut taken = Vec::new();
        let mut uploads = 0;
        while let Some((column, built)) = self.ready.pop_front() {
            let meshes = built.mesh_count();
            let fits = uploads + meshes <= budget || (uploads == 0 && budget > 0);
            if meshes > 0 && !fits {
                self.ready.push_front((column, built));
                break;
            }
            uploads += meshes;
            self.loaded.insert(column);
            taken.push((column, built));
        }
        taken
    }

    /// Progress figures.
    pub fn stats(&self) -> StreamStats {
        StreamStats {
            loaded: self.loaded.len(),
            queued: self.queue.len() + self.in_flight.len() + self.ready.len(),
        }
    }
}

#[cfg(test)]
mod tests {
    use vox_world::{Chunk, ChunkMesh, ChunkPos};

    use super::*;
    use crate::streaming::job::Built;

    /// A small radius keeps the tests readable: 13 columns.
    const RADIUS: i32 = 2;
    /// The cap used unless a test is about the cap.
    const CAP: usize = 4;
    /// A centre so far away that nothing of the area around the origin is kept: more than twice
    /// the keep range (radius plus margin) away.
    const FAR: i32 = 2 * (RADIUS + area::UNLOAD_MARGIN) + 1;
    /// A bigger cap, for runs that should not be limited by it.
    const BIG_CAP: usize = 1000;

    /// A column whose layers, from layer 0 up, have something to draw where `drawn` says so.
    fn built_column(drawn: &[bool]) -> BuiltColumn {
        let layers: Vec<(ChunkPos, Built)> = (0..)
            .zip(drawn)
            .map(|(layer, &drawn)| {
                let mesh = if drawn {
                    ChunkMesh {
                        vertices: Vec::new(),
                        indices: vec![0],
                        // Less than one quad of indices, so no quad in any direction.
                        face_quads: [0; vox_world::Face::COUNT],
                    }
                } else {
                    ChunkMesh::default()
                };
                let chunk = Chunk::new();
                (ChunkPos::new(0, layer, 0), Built { chunk, mesh })
            })
            .collect();
        let top = i32::try_from(drawn.len()).unwrap() - 1;
        BuiltColumn {
            span: 0..=top,
            layers,
        }
    }

    /// A column of one chunk with something to draw.
    fn solid() -> BuiltColumn {
        built_column(&[true])
    }

    /// A column of one chunk with nothing to draw.
    fn invisible() -> BuiltColumn {
        built_column(&[false])
    }

    fn at(x: i32, z: i32) -> Column {
        Column::new(x, z)
    }

    /// Finishes every job in `jobs` with a solid mesh.
    fn finish_all(tracker: &mut ChunkTracker, jobs: &[Column]) {
        for column in jobs {
            tracker.finish(*column, solid());
        }
    }

    /// Runs the tracker until nothing is queued, uploading everything, and returns the
    /// uploaded columns.
    fn run_to_the_end(tracker: &mut ChunkTracker) -> Vec<Column> {
        let mut uploaded = Vec::new();
        while tracker.stats().queued > 0 {
            let jobs = tracker.next_jobs();
            finish_all(tracker, &jobs);
            uploaded.extend(tracker.take_loaded(usize::MAX).into_iter().map(|u| u.0));
        }
        uploaded
    }

    #[test]
    fn the_first_jobs_are_the_nearest() {
        let mut tracker = ChunkTracker::new(RADIUS, CAP);
        tracker.set_centre(at(0, 0));
        let wanted = area::wanted(at(0, 0), RADIUS);
        assert_eq!(tracker.next_jobs(), wanted[..CAP]);
    }

    #[test]
    fn never_more_than_the_cap_in_flight_plus_ready() {
        let mut tracker = ChunkTracker::new(RADIUS, CAP);
        tracker.set_centre(at(0, 0));
        let first = tracker.next_jobs();
        assert_eq!(first.len(), CAP);
        assert!(tracker.next_jobs().is_empty());

        // One finished job still counts until it is uploaded.
        tracker.finish(first[0], solid());
        assert!(tracker.next_jobs().is_empty());
        tracker.take_loaded(usize::MAX);
        assert_eq!(tracker.next_jobs().len(), 1);
    }

    #[test]
    fn a_stale_result_is_dropped() {
        let mut tracker = ChunkTracker::new(RADIUS, CAP);
        tracker.set_centre(at(0, 0));
        let jobs = tracker.next_jobs();
        let moved = at(FAR, 0);
        tracker.set_centre(moved);
        // Every job of the first area is out of the keep range of the new centre.
        let stale = jobs
            .iter()
            .find(|column| !area::is_kept(moved, **column, RADIUS))
            .copied()
            .expect("some job is far from the new centre");
        tracker.finish(stale, solid());
        assert!(tracker.take_loaded(usize::MAX).is_empty());
        assert_eq!(tracker.stats().loaded, 0);
    }

    #[test]
    fn a_column_that_left_and_came_back_in_flight_is_accepted_once() {
        let mut tracker = ChunkTracker::new(RADIUS, BIG_CAP);
        tracker.set_centre(at(0, 0));
        let jobs = tracker.next_jobs();
        let target = at(-RADIUS, 0);
        assert!(jobs.contains(&target));

        tracker.set_centre(at(FAR, 0));
        tracker.set_centre(at(0, 0));
        // It was forgotten when it left, so it is queued and sent again.
        let again = tracker.next_jobs();
        assert!(again.contains(&target));

        tracker.finish(target, solid());
        tracker.finish(target, solid());
        let uploads = tracker.take_loaded(usize::MAX);
        assert_eq!(uploads.iter().filter(|u| u.0 == target).count(), 1);
    }

    #[test]
    fn a_move_by_one_chunk_unloads_nothing_and_a_far_move_unloads_everything() {
        let mut tracker = ChunkTracker::new(RADIUS, BIG_CAP);
        tracker.set_centre(at(0, 0));
        run_to_the_end(&mut tracker);
        let loaded = tracker.stats().loaded;
        assert_eq!(loaded, area::wanted(at(0, 0), RADIUS).len());

        assert!(tracker.set_centre(at(1, 0)).is_empty());
        assert_eq!(tracker.stats().loaded, loaded);

        let mut removed = tracker.set_centre(at(FAR, 0));
        removed.sort_by_key(|column| (column.z, column.x));
        let mut all = area::wanted(at(0, 0), RADIUS);
        all.sort_by_key(|column| (column.z, column.x));
        assert_eq!(removed, all);
        assert_eq!(tracker.stats().loaded, 0);
    }

    #[test]
    fn the_same_centre_changes_nothing() {
        let mut tracker = ChunkTracker::new(RADIUS, CAP);
        tracker.set_centre(at(0, 0));
        let queued = tracker.stats().queued;
        tracker.next_jobs();
        assert!(tracker.set_centre(at(0, 0)).is_empty());
        assert_eq!(tracker.stats().queued, queued);
    }

    #[test]
    fn a_column_is_never_queued_twice() {
        let mut tracker = ChunkTracker::new(RADIUS, CAP);
        let mut seen: HashSet<Column> = HashSet::new();
        // Walk along a line, sending and finishing a few jobs on the way.
        for x in 0..6 {
            tracker.set_centre(at(x, 0));
            let jobs = tracker.next_jobs();
            for column in &jobs {
                assert!(seen.insert(*column), "{column:?} was sent twice");
            }
            finish_all(&mut tracker, &jobs);
            tracker.take_loaded(usize::MAX);
        }
    }

    /// How many meshes to upload the taken columns have.
    fn drawable(taken: &[(Column, BuiltColumn)]) -> usize {
        taken.iter().map(|(_, built)| built.mesh_count()).sum()
    }

    #[test]
    fn only_meshes_count_against_the_budget_and_empty_chunks_are_still_handed_out() {
        let mut tracker = ChunkTracker::new(RADIUS, BIG_CAP);
        tracker.set_centre(at(0, 0));
        let jobs = tracker.next_jobs();
        // Every second chunk has nothing to draw.
        for (index, column) in jobs.iter().enumerate() {
            let built = if index % 2 == 0 { solid() } else { invisible() };
            tracker.finish(*column, built);
        }
        let budget = 3;
        let first = tracker.take_loaded(budget);
        assert_eq!(drawable(&first), budget);
        // Three meshes, and the three empty chunks that lie before the fourth solid one.
        assert_eq!(first.len(), 6);
        assert_eq!(tracker.stats().loaded, 6);

        let mut handed_out = first.len();
        while handed_out < jobs.len() {
            let batch = tracker.take_loaded(budget);
            assert!(!batch.is_empty() && drawable(&batch) <= budget);
            handed_out += batch.len();
        }
        assert_eq!(handed_out, jobs.len());
        assert_eq!(tracker.stats().loaded, jobs.len());
        assert_eq!(tracker.stats().queued, 0);
    }

    #[test]
    fn an_empty_chunk_is_handed_out_even_with_a_zero_budget() {
        let mut tracker = ChunkTracker::new(RADIUS, CAP);
        tracker.set_centre(at(0, 0));
        let column = tracker.next_jobs()[0];
        tracker.finish(column, invisible());
        let taken = tracker.take_loaded(0);
        assert_eq!(taken.len(), 1);
        assert_eq!(taken[0].0, column);
    }

    #[test]
    fn a_zero_budget_uploads_nothing_and_keeps_the_meshes() {
        let mut tracker = ChunkTracker::new(RADIUS, CAP);
        tracker.set_centre(at(0, 0));
        let jobs = tracker.next_jobs();
        finish_all(&mut tracker, &jobs);
        assert!(tracker.take_loaded(0).is_empty());
        assert_eq!(tracker.take_loaded(usize::MAX).len(), jobs.len());
    }

    #[test]
    fn loaded_plus_queued_is_the_wanted_count() {
        let mut tracker = ChunkTracker::new(RADIUS, CAP);
        tracker.set_centre(at(3, -2));
        let wanted = area::wanted(at(3, -2), RADIUS).len();
        let total = |t: &ChunkTracker| t.stats().loaded + t.stats().queued;
        assert_eq!(total(&tracker), wanted);
        let jobs = tracker.next_jobs();
        assert_eq!(total(&tracker), wanted);
        finish_all(&mut tracker, &jobs);
        assert_eq!(total(&tracker), wanted);
        tracker.take_loaded(2);
        assert_eq!(total(&tracker), wanted);
    }

    #[test]
    fn a_full_run_ends_with_exactly_the_wanted_area_loaded() {
        let mut tracker = ChunkTracker::new(RADIUS, CAP);
        tracker.set_centre(at(-1, 4));
        let mut uploaded = run_to_the_end(&mut tracker);
        let mut wanted = area::wanted(at(-1, 4), RADIUS);
        assert_eq!(tracker.stats().loaded, wanted.len());
        assert_eq!(tracker.stats().queued, 0);
        uploaded.sort_by_key(|column| (column.z, column.x));
        wanted.sort_by_key(|column| (column.z, column.x));
        assert_eq!(uploaded, wanted);
    }

    #[test]
    fn every_mesh_of_a_column_counts_against_the_budget() {
        let mut tracker = ChunkTracker::new(RADIUS, BIG_CAP);
        tracker.set_centre(at(0, 0));
        let jobs = tracker.next_jobs();
        // Two meshes and one empty layer each.
        for column in &jobs {
            tracker.finish(*column, built_column(&[true, false, true]));
        }
        let budget = 5;
        let first = tracker.take_loaded(budget);
        // Two columns are four meshes; a third would make six.
        assert_eq!(first.len(), 2);
        assert_eq!(drawable(&first), 4);
    }

    #[test]
    fn a_column_with_more_meshes_than_the_budget_is_taken_alone() {
        let mut tracker = ChunkTracker::new(RADIUS, BIG_CAP);
        tracker.set_centre(at(0, 0));
        let jobs = tracker.next_jobs();
        for column in &jobs {
            tracker.finish(*column, built_column(&[true, true, true]));
        }
        let budget = 2;
        for _ in 0..jobs.len() {
            assert_eq!(tracker.take_loaded(budget).len(), 1);
        }
        assert_eq!(tracker.stats().queued, 0);
    }

    #[test]
    fn a_zero_cap_still_makes_progress() {
        let mut tracker = ChunkTracker::new(RADIUS, 0);
        tracker.set_centre(at(0, 0));
        assert_eq!(tracker.next_jobs().len(), 1);
    }
}
