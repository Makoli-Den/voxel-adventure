//! The bookkeeping of streaming: which keys (columns or tiles) to load, which are loading, which
//! are loaded.
//!
//! A pure state machine with no threads, no clock and no GPU, so every decision can be tested
//! with fake results. One tracker serves columns and another tiles; what is wanted and what is
//! kept comes from outside ([`super::plan::Plan`]). The one rule: a tracked key is forgotten
//! only when it is no longer kept. A key that is queued, in flight, waiting for upload or loaded
//! stays that way while the camera wanders, as long as it is kept.

use std::collections::{HashMap, HashSet, VecDeque};
use std::hash::Hash;

use vox_world::ChunkMesh;

use super::job::{BuiltColumn, mesh_bytes};
use super::plan::Near;

/// What a finished job costs once it is handed out: meshes to upload and their bytes.
pub trait Uploads {
    /// Non-empty meshes, each one upload; empty meshes are never uploaded.
    fn mesh_count(&self) -> usize;
    /// Bytes of vertices and indices of those meshes, for the memory estimate.
    fn mesh_bytes(&self) -> usize;
}

impl Uploads for BuiltColumn {
    fn mesh_count(&self) -> usize {
        BuiltColumn::mesh_count(self)
    }

    fn mesh_bytes(&self) -> usize {
        self.layers
            .iter()
            .map(|(_, built)| mesh_bytes(&built.mesh))
            .sum()
    }
}

impl Uploads for ChunkMesh {
    fn mesh_count(&self) -> usize {
        usize::from(!self.indices.is_empty())
    }

    fn mesh_bytes(&self) -> usize {
        mesh_bytes(self)
    }
}

/// What one tracker knows about its progress.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Progress {
    /// Keys whose result went to the game, including those with nothing to draw.
    pub loaded: usize,
    /// Keys wanted but not loaded yet: queued, in flight or waiting for upload.
    pub queued: usize,
    /// Bytes of the meshes of the loaded keys as they were streamed (an edit's remesh is not
    /// counted): the estimate of GPU mesh memory.
    pub mesh_bytes: usize,
}

/// Decides what to load, unload and upload of one kind of key `K` whose jobs produce `V`.
#[derive(Debug)]
pub struct Tracker<K, V> {
    /// Keys to load, nearest first. Rebuilt whenever the plan changes.
    queue: VecDeque<Near<K>>,
    /// Keys handed to a worker whose result has not arrived. A key that stopped being kept
    /// and came back while its first job still ran is sent again, so the real number of
    /// running jobs can briefly exceed this count; the second result is dropped.
    in_flight: HashSet<K>,
    /// Finished keys waiting to be handed out.
    ready: VecDeque<(K, V)>,
    /// The keys that were handed out, with the bytes of their meshes.
    loaded: HashMap<K, usize>,
    /// The sum of the bytes in `loaded`, kept up to date so the figures cost nothing per frame.
    loaded_bytes: usize,
}

impl<K: Copy + Eq + Hash, V: Uploads> Default for Tracker<K, V> {
    fn default() -> Self {
        Self {
            queue: VecDeque::new(),
            in_flight: HashSet::new(),
            ready: VecDeque::new(),
            loaded: HashMap::new(),
            loaded_bytes: 0,
        }
    }
}

impl<K: Copy + Eq + Hash, V: Uploads> Tracker<K, V> {
    /// Takes a new plan: forgets every tracked key that `kept` refuses and returns the loaded
    /// ones among them, to remove from the game's world and the renderer. Rebuilds the queue
    /// from `wanted` (nearest first) minus everything already tracked.
    pub fn retarget(&mut self, wanted: &[Near<K>], kept: impl Fn(K) -> bool) -> Vec<K> {
        let removed: Vec<K> = self
            .loaded
            .keys()
            .copied()
            .filter(|key| !kept(*key))
            .collect();
        for key in &removed {
            if let Some(bytes) = self.loaded.remove(key) {
                self.loaded_bytes -= bytes;
            }
        }
        self.in_flight.retain(|key| kept(*key));
        self.ready.retain(|(key, _)| kept(*key));

        let ready: HashSet<K> = self.ready.iter().map(|(key, _)| *key).collect();
        self.queue = wanted
            .iter()
            .copied()
            .filter(|(_, key)| {
                !self.loaded.contains_key(key)
                    && !self.in_flight.contains(key)
                    && !ready.contains(key)
            })
            .collect();
        removed
    }

    /// Distance of the next key in the queue, or `None` when the queue is empty.
    pub fn next_distance(&self) -> Option<i64> {
        self.queue.front().map(|(distance, _)| *distance)
    }

    /// Takes the next key from the queue and marks it in flight. The caller sends its job.
    pub fn take_job(&mut self) -> Option<K> {
        let (_, key) = self.queue.pop_front()?;
        self.in_flight.insert(key);
        Some(key)
    }

    /// Keys in flight plus waiting for upload: what counts against the cap of jobs.
    pub fn busy(&self) -> usize {
        self.in_flight.len() + self.ready.len()
    }

    /// A worker finished `key`. The result is dropped if `key` is not in flight (it stopped
    /// being kept meanwhile, or an earlier job of the same key already delivered).
    pub fn finish(&mut self, key: K, value: V) {
        if self.in_flight.remove(&key) {
            self.ready.push_back((key, value));
        }
    }

    /// The keys that are ready, marked loaded, in order, with at most `budget` meshes to upload
    /// among them, and the number of uploads they take. A key with nothing to draw does not
    /// count against the budget, but it is returned too (a column's blocks go to the game's
    /// world). A key with more meshes than the whole budget is taken alone, as the first of a
    /// frame, so it cannot block the queue.
    pub fn take_loaded(&mut self, budget: usize) -> (Vec<(K, V)>, usize) {
        let mut taken = Vec::new();
        let mut uploads = 0;
        while let Some((key, value)) = self.ready.pop_front() {
            let meshes = value.mesh_count();
            let fits = uploads + meshes <= budget || (uploads == 0 && budget > 0);
            if meshes > 0 && !fits {
                self.ready.push_front((key, value));
                break;
            }
            uploads += meshes;
            let bytes = value.mesh_bytes();
            self.loaded_bytes += bytes;
            // A key is in flight only while it is not loaded, so there is no old entry; were
            // there one, its bytes leave the sum with it.
            if let Some(old) = self.loaded.insert(key, bytes) {
                self.loaded_bytes -= old;
            }
            taken.push((key, value));
        }
        (taken, uploads)
    }

    /// Progress figures.
    pub fn progress(&self) -> Progress {
        Progress {
            loaded: self.loaded.len(),
            queued: self.queue.len() + self.in_flight.len() + self.ready.len(),
            mesh_bytes: self.loaded_bytes,
        }
    }
}

#[cfg(test)]
mod tests {
    use vox_world::{Chunk, ChunkPos};

    use super::*;
    use crate::streaming::area;
    use crate::streaming::column::Column;
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

    type ColumnTracker = Tracker<Column, BuiltColumn>;

    /// Retargets `tracker` to the plain area of `RADIUS` around `centre`, the way the
    /// streamer does with no horizon.
    fn set_centre(tracker: &mut ColumnTracker, centre: Column) -> Vec<Column> {
        let wanted: Vec<Near<Column>> = area::wanted(centre, RADIUS)
            .into_iter()
            .map(|c| {
                let (dx, dz) = (i64::from(c.x - centre.x), i64::from(c.z - centre.z));
                (dx * dx + dz * dz, c)
            })
            .collect();
        tracker.retarget(&wanted, |column| area::is_kept(centre, column, RADIUS))
    }

    /// Takes jobs while fewer than `cap` keys are busy, the way the streamer does.
    fn next_jobs(tracker: &mut ColumnTracker, cap: usize) -> Vec<Column> {
        let mut jobs = Vec::new();
        while tracker.busy() < cap.max(1) {
            let Some(column) = tracker.take_job() else {
                break;
            };
            jobs.push(column);
        }
        jobs
    }

    fn taken(tracker: &mut ColumnTracker, budget: usize) -> Vec<(Column, BuiltColumn)> {
        tracker.take_loaded(budget).0
    }

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
    fn finish_all(tracker: &mut ColumnTracker, jobs: &[Column]) {
        for column in jobs {
            tracker.finish(*column, solid());
        }
    }

    /// Runs the tracker until nothing is queued, uploading everything, and returns the
    /// uploaded columns.
    fn run_to_the_end(tracker: &mut ColumnTracker, cap: usize) -> Vec<Column> {
        let mut uploaded = Vec::new();
        while tracker.progress().queued > 0 {
            let jobs = next_jobs(tracker, cap);
            finish_all(tracker, &jobs);
            uploaded.extend(taken(tracker, usize::MAX).into_iter().map(|u| u.0));
        }
        uploaded
    }

    #[test]
    fn the_first_jobs_are_the_nearest() {
        let mut tracker = ColumnTracker::default();
        set_centre(&mut tracker, at(0, 0));
        let wanted = area::wanted(at(0, 0), RADIUS);
        assert_eq!(next_jobs(&mut tracker, CAP), wanted[..CAP]);
    }

    #[test]
    fn the_next_distance_is_that_of_the_front_of_the_queue() {
        let mut tracker = ColumnTracker::default();
        assert_eq!(tracker.next_distance(), None);
        tracker.retarget(&[(5, at(1, 0)), (9, at(2, 0))], |_| true);
        assert_eq!(tracker.next_distance(), Some(5));
        assert_eq!(tracker.take_job(), Some(at(1, 0)));
        assert_eq!(tracker.next_distance(), Some(9));
    }

    #[test]
    fn never_more_than_the_cap_in_flight_plus_ready() {
        let mut tracker = ColumnTracker::default();
        set_centre(&mut tracker, at(0, 0));
        let first = next_jobs(&mut tracker, CAP);
        assert_eq!(first.len(), CAP);
        assert!(next_jobs(&mut tracker, CAP).is_empty());

        // One finished job still counts until it is uploaded.
        tracker.finish(first[0], solid());
        assert!(next_jobs(&mut tracker, CAP).is_empty());
        taken(&mut tracker, usize::MAX);
        assert_eq!(next_jobs(&mut tracker, CAP).len(), 1);
    }

    #[test]
    fn a_stale_result_is_dropped() {
        let mut tracker = ColumnTracker::default();
        set_centre(&mut tracker, at(0, 0));
        let jobs = next_jobs(&mut tracker, CAP);
        let moved = at(FAR, 0);
        set_centre(&mut tracker, moved);
        // Every job of the first area is out of the keep range of the new centre.
        let stale = jobs
            .iter()
            .find(|column| !area::is_kept(moved, **column, RADIUS))
            .copied()
            .expect("some job is far from the new centre");
        tracker.finish(stale, solid());
        assert!(taken(&mut tracker, usize::MAX).is_empty());
        assert_eq!(tracker.progress().loaded, 0);
    }

    #[test]
    fn a_column_that_left_and_came_back_in_flight_is_accepted_once() {
        let mut tracker = ColumnTracker::default();
        set_centre(&mut tracker, at(0, 0));
        let jobs = next_jobs(&mut tracker, BIG_CAP);
        let target = at(-RADIUS, 0);
        assert!(jobs.contains(&target));

        set_centre(&mut tracker, at(FAR, 0));
        set_centre(&mut tracker, at(0, 0));
        // It was forgotten when it left, so it is queued and sent again.
        let again = next_jobs(&mut tracker, BIG_CAP);
        assert!(again.contains(&target));

        tracker.finish(target, solid());
        tracker.finish(target, solid());
        let uploads = taken(&mut tracker, usize::MAX);
        assert_eq!(uploads.iter().filter(|u| u.0 == target).count(), 1);
    }

    #[test]
    fn a_move_by_one_chunk_unloads_nothing_and_a_far_move_unloads_everything() {
        let mut tracker = ColumnTracker::default();
        set_centre(&mut tracker, at(0, 0));
        run_to_the_end(&mut tracker, BIG_CAP);
        let loaded = tracker.progress().loaded;
        assert_eq!(loaded, area::wanted(at(0, 0), RADIUS).len());

        assert!(set_centre(&mut tracker, at(1, 0)).is_empty());
        assert_eq!(tracker.progress().loaded, loaded);

        let mut removed = set_centre(&mut tracker, at(FAR, 0));
        removed.sort_by_key(|column| (column.z, column.x));
        let mut all = area::wanted(at(0, 0), RADIUS);
        all.sort_by_key(|column| (column.z, column.x));
        assert_eq!(removed, all);
        assert_eq!(tracker.progress().loaded, 0);
    }

    #[test]
    fn the_same_centre_changes_nothing() {
        let mut tracker = ColumnTracker::default();
        set_centre(&mut tracker, at(0, 0));
        let queued = tracker.progress().queued;
        next_jobs(&mut tracker, CAP);
        assert!(set_centre(&mut tracker, at(0, 0)).is_empty());
        assert_eq!(tracker.progress().queued, queued);
    }

    #[test]
    fn a_column_is_never_queued_twice() {
        let mut tracker = ColumnTracker::default();
        let mut seen: HashSet<Column> = HashSet::new();
        // Walk along a line, sending and finishing a few jobs on the way.
        for x in 0..6 {
            set_centre(&mut tracker, at(x, 0));
            let jobs = next_jobs(&mut tracker, CAP);
            for column in &jobs {
                assert!(seen.insert(*column), "{column:?} was sent twice");
            }
            finish_all(&mut tracker, &jobs);
            taken(&mut tracker, usize::MAX);
        }
    }

    /// How many meshes to upload the taken columns have.
    fn drawable(taken: &[(Column, BuiltColumn)]) -> usize {
        taken.iter().map(|(_, built)| built.mesh_count()).sum()
    }

    #[test]
    fn only_meshes_count_against_the_budget_and_empty_chunks_are_still_handed_out() {
        let mut tracker = ColumnTracker::default();
        set_centre(&mut tracker, at(0, 0));
        let jobs = next_jobs(&mut tracker, BIG_CAP);
        // Every second chunk has nothing to draw.
        for (index, column) in jobs.iter().enumerate() {
            let built = if index % 2 == 0 { solid() } else { invisible() };
            tracker.finish(*column, built);
        }
        let budget = 3;
        let (first, used) = tracker.take_loaded(budget);
        assert_eq!(drawable(&first), budget);
        assert_eq!(used, budget);
        // Three meshes, and the three empty chunks that lie before the fourth solid one.
        assert_eq!(first.len(), 6);
        assert_eq!(tracker.progress().loaded, 6);

        let mut handed_out = first.len();
        while handed_out < jobs.len() {
            let batch = taken(&mut tracker, budget);
            assert!(!batch.is_empty() && drawable(&batch) <= budget);
            handed_out += batch.len();
        }
        assert_eq!(handed_out, jobs.len());
        assert_eq!(tracker.progress().loaded, jobs.len());
        assert_eq!(tracker.progress().queued, 0);
    }

    #[test]
    fn an_empty_chunk_is_handed_out_even_with_a_zero_budget() {
        let mut tracker = ColumnTracker::default();
        set_centre(&mut tracker, at(0, 0));
        let column = next_jobs(&mut tracker, CAP)[0];
        tracker.finish(column, invisible());
        let taken = taken(&mut tracker, 0);
        assert_eq!(taken.len(), 1);
        assert_eq!(taken[0].0, column);
    }

    #[test]
    fn a_zero_budget_uploads_nothing_and_keeps_the_meshes() {
        let mut tracker = ColumnTracker::default();
        set_centre(&mut tracker, at(0, 0));
        let jobs = next_jobs(&mut tracker, CAP);
        finish_all(&mut tracker, &jobs);
        assert!(taken(&mut tracker, 0).is_empty());
        assert_eq!(taken(&mut tracker, usize::MAX).len(), jobs.len());
    }

    #[test]
    fn loaded_plus_queued_is_the_wanted_count() {
        let mut tracker = ColumnTracker::default();
        set_centre(&mut tracker, at(3, -2));
        let wanted = area::wanted(at(3, -2), RADIUS).len();
        let total = |t: &ColumnTracker| t.progress().loaded + t.progress().queued;
        assert_eq!(total(&tracker), wanted);
        let jobs = next_jobs(&mut tracker, CAP);
        assert_eq!(total(&tracker), wanted);
        finish_all(&mut tracker, &jobs);
        assert_eq!(total(&tracker), wanted);
        taken(&mut tracker, 2);
        assert_eq!(total(&tracker), wanted);
    }

    #[test]
    fn a_full_run_ends_with_exactly_the_wanted_area_loaded() {
        let mut tracker = ColumnTracker::default();
        set_centre(&mut tracker, at(-1, 4));
        let mut uploaded = run_to_the_end(&mut tracker, CAP);
        let mut wanted = area::wanted(at(-1, 4), RADIUS);
        assert_eq!(tracker.progress().loaded, wanted.len());
        assert_eq!(tracker.progress().queued, 0);
        uploaded.sort_by_key(|column| (column.z, column.x));
        wanted.sort_by_key(|column| (column.z, column.x));
        assert_eq!(uploaded, wanted);
    }

    #[test]
    fn every_mesh_of_a_column_counts_against_the_budget() {
        let mut tracker = ColumnTracker::default();
        set_centre(&mut tracker, at(0, 0));
        let jobs = next_jobs(&mut tracker, BIG_CAP);
        // Two meshes and one empty layer each.
        for column in &jobs {
            tracker.finish(*column, built_column(&[true, false, true]));
        }
        let budget = 5;
        let first = taken(&mut tracker, budget);
        // Two columns are four meshes; a third would make six.
        assert_eq!(first.len(), 2);
        assert_eq!(drawable(&first), 4);
    }

    #[test]
    fn a_column_with_more_meshes_than_the_budget_is_taken_alone() {
        let mut tracker = ColumnTracker::default();
        set_centre(&mut tracker, at(0, 0));
        let jobs = next_jobs(&mut tracker, BIG_CAP);
        for column in &jobs {
            tracker.finish(*column, built_column(&[true, true, true]));
        }
        let budget = 2;
        for _ in 0..jobs.len() {
            assert_eq!(taken(&mut tracker, budget).len(), 1);
        }
        assert_eq!(tracker.progress().queued, 0);
    }

    #[test]
    fn a_finished_key_that_stops_being_kept_before_upload_is_not_handed_out() {
        let mut tracker: Tracker<u8, ChunkMesh> = Tracker::default();
        tracker.retarget(&[(0, 1), (1, 2)], |_| true);
        assert_eq!(tracker.take_job(), Some(1));
        assert_eq!(tracker.take_job(), Some(2));
        let drawn = || ChunkMesh {
            vertices: Vec::new(),
            indices: vec![0],
            face_quads: [0; vox_world::Face::COUNT],
        };
        tracker.finish(1, drawn());
        tracker.finish(2, drawn());
        // Key 1 leaves the selection while it waits for upload.
        assert!(tracker.retarget(&[(1, 2)], |key| key == 2).is_empty());
        let (taken, used) = tracker.take_loaded(usize::MAX);
        let keys: Vec<u8> = taken.iter().map(|(key, _)| *key).collect();
        assert_eq!((keys, used), (vec![2], 1));
        assert_eq!(tracker.progress().loaded, 1);
    }

    #[test]
    fn the_memory_estimate_follows_the_loaded_meshes() {
        let mut tracker: Tracker<u8, ChunkMesh> = Tracker::default();
        let mesh = |indices: usize| ChunkMesh {
            vertices: Vec::new(),
            indices: vec![0; indices],
            face_quads: [0; vox_world::Face::COUNT],
        };
        tracker.retarget(&[(0, 1), (1, 2)], |_| true);
        for key in [1, 2] {
            assert_eq!(tracker.take_job(), Some(key));
        }
        tracker.finish(1, mesh(3));
        tracker.finish(2, mesh(5));
        let (taken, used) = tracker.take_loaded(usize::MAX);
        assert_eq!((taken.len(), used), (2, 2));
        let index_bytes = std::mem::size_of::<u32>();
        assert_eq!(tracker.progress().mesh_bytes, 8 * index_bytes);
        assert_eq!(tracker.retarget(&[], |key| key == 2), vec![1]);
        assert_eq!(tracker.progress().mesh_bytes, 5 * index_bytes);
    }
}
