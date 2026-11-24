//! The bookkeeping of streaming: which chunks to load, which are loading, which are loaded.
//!
//! A pure state machine with no threads, no clock and no GPU, so every decision can be tested
//! with fake meshes. The one rule: a tracked position is forgotten only when it leaves the
//! keep range ([`area::is_kept`]). A position that is queued, in flight, waiting for upload or
//! loaded stays that way while the camera wanders near the border of the area.

use std::collections::{HashSet, VecDeque};

use vox_world::ChunkPos;

use super::area;
use super::job::Built;

/// What the tracker knows about the progress, for the HUD and the fill log line.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StreamStats {
    /// Chunks with a mesh in the renderer, or known to have nothing to draw.
    pub loaded: usize,
    /// Chunks wanted but not loaded yet: queued, in flight or waiting for upload.
    pub queued: usize,
}

/// Decides what to load, unload and upload around a moving centre.
#[derive(Debug)]
pub struct ChunkTracker {
    /// The view radius in chunks.
    radius: i32,
    /// At most this many positions are in flight plus waiting for upload.
    cap: usize,
    /// The chunk the area is centred on; `None` until the first [`Self::set_centre`].
    centre: Option<ChunkPos>,
    /// Positions to load, nearest first. Rebuilt whenever the centre changes.
    queue: VecDeque<ChunkPos>,
    /// Positions handed to a worker whose result has not arrived. A position that left the
    /// keep range and came back while its first job still ran is sent again, so the real
    /// number of running jobs can briefly exceed this count; the second result is dropped.
    in_flight: HashSet<ChunkPos>,
    /// Finished chunks waiting to be handed out.
    ready: VecDeque<(ChunkPos, Built)>,
    /// Positions that were handed out, including those with nothing to draw.
    loaded: HashSet<ChunkPos>,
}

/// Whether a built chunk has nothing to draw.
fn has_nothing_to_draw(built: &Built) -> bool {
    built.mesh.indices.is_empty()
}

impl ChunkTracker {
    /// Creates a tracker for the view `radius` that keeps at most `cap` positions in flight
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
    /// Forgets every tracked position outside the keep range and returns the loaded ones
    /// among them, to remove from the renderer. Rebuilds the queue from the wanted area minus
    /// everything already tracked.
    pub fn set_centre(&mut self, centre: ChunkPos) -> Vec<ChunkPos> {
        if self.centre == Some(centre) {
            return Vec::new();
        }
        self.centre = Some(centre);
        let radius = self.radius;
        let kept = |pos: &ChunkPos| area::is_kept(centre, *pos, radius);

        let removed: Vec<ChunkPos> = self
            .loaded
            .iter()
            .copied()
            .filter(|pos| !kept(pos))
            .collect();
        self.loaded.retain(kept);
        self.in_flight.retain(kept);
        self.ready.retain(|(pos, _)| kept(pos));

        let ready: HashSet<ChunkPos> = self.ready.iter().map(|(pos, _)| *pos).collect();
        self.queue = area::wanted(centre, radius)
            .into_iter()
            .filter(|pos| {
                !self.loaded.contains(pos) && !self.in_flight.contains(pos) && !ready.contains(pos)
            })
            .collect();
        removed
    }

    /// Takes positions from the front of the queue while in-flight plus ready is below the
    /// cap, and marks them in flight. The caller sends them to the workers.
    pub fn next_jobs(&mut self) -> Vec<ChunkPos> {
        let mut jobs = Vec::new();
        while self.in_flight.len() + self.ready.len() < self.cap {
            let Some(pos) = self.queue.pop_front() else {
                break;
            };
            self.in_flight.insert(pos);
            jobs.push(pos);
        }
        jobs
    }

    /// A worker finished `pos`. The result is dropped if `pos` is not in flight (it left the
    /// keep range meanwhile, or an earlier job of the same position already delivered).
    pub fn finish(&mut self, pos: ChunkPos, built: Built) {
        if self.in_flight.remove(&pos) {
            self.ready.push_back((pos, built));
        }
    }

    /// The chunks that are ready, marked loaded: at most `budget` with a mesh to upload.
    /// Chunks with nothing to draw cost no upload, so they do not count against the budget,
    /// but they are returned too, because the game's world needs their blocks.
    pub fn take_loaded(&mut self, budget: usize) -> Vec<(ChunkPos, Built)> {
        let mut taken = Vec::new();
        let mut uploads = 0;
        while let Some((pos, built)) = self.ready.pop_front() {
            if !has_nothing_to_draw(&built) {
                if uploads == budget {
                    self.ready.push_front((pos, built));
                    break;
                }
                uploads += 1;
            }
            self.loaded.insert(pos);
            taken.push((pos, built));
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
    use vox_world::{Chunk, ChunkMesh};

    use super::*;

    /// A small radius keeps the tests readable: 13 columns.
    const RADIUS: i32 = 2;
    /// The cap used unless a test is about the cap.
    const CAP: usize = 4;
    /// A centre so far away that nothing of the area around the origin is kept: more than twice
    /// the keep range (radius plus margin) away.
    const FAR: i32 = 2 * (RADIUS + area::UNLOAD_MARGIN) + 1;
    /// A bigger cap, for runs that should not be limited by it.
    const BIG_CAP: usize = 1000;

    /// A chunk with something to draw.
    fn solid() -> Built {
        Built {
            chunk: Chunk::new(),
            mesh: ChunkMesh {
                vertices: Vec::new(),
                indices: vec![0],
                // Less than one quad of indices, so no quad in any direction.
                face_quads: [0; vox_world::Face::COUNT],
            },
        }
    }

    /// A chunk with nothing to draw.
    fn invisible() -> Built {
        Built {
            chunk: Chunk::new(),
            mesh: ChunkMesh::default(),
        }
    }

    fn at(x: i32, z: i32) -> ChunkPos {
        ChunkPos::new(x, area::TERRAIN_LAYER, z)
    }

    /// Finishes every job in `jobs` with a solid mesh.
    fn finish_all(tracker: &mut ChunkTracker, jobs: &[ChunkPos]) {
        for pos in jobs {
            tracker.finish(*pos, solid());
        }
    }

    /// Runs the tracker until nothing is queued, uploading everything, and returns the
    /// uploaded positions.
    fn run_to_the_end(tracker: &mut ChunkTracker) -> Vec<ChunkPos> {
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
            .find(|pos| !area::is_kept(moved, **pos, RADIUS))
            .copied()
            .expect("some job is far from the new centre");
        tracker.finish(stale, solid());
        assert!(tracker.take_loaded(usize::MAX).is_empty());
        assert_eq!(tracker.stats().loaded, 0);
    }

    #[test]
    fn a_position_that_left_and_came_back_in_flight_is_accepted_once() {
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
        removed.sort_by_key(|pos| (pos.z, pos.x));
        let mut all = area::wanted(at(0, 0), RADIUS);
        all.sort_by_key(|pos| (pos.z, pos.x));
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
    fn a_position_is_never_queued_twice() {
        let mut tracker = ChunkTracker::new(RADIUS, CAP);
        let mut seen = HashSet::new();
        // Walk along a line, sending and finishing a few jobs on the way.
        for x in 0..6 {
            tracker.set_centre(at(x, 0));
            let jobs = tracker.next_jobs();
            for pos in &jobs {
                assert!(seen.insert(*pos), "{pos:?} was sent twice");
            }
            finish_all(&mut tracker, &jobs);
            tracker.take_loaded(usize::MAX);
        }
    }

    /// How many of the taken chunks have a mesh to upload.
    fn drawable(taken: &[(ChunkPos, Built)]) -> usize {
        taken
            .iter()
            .filter(|(_, built)| !built.mesh.indices.is_empty())
            .count()
    }

    #[test]
    fn only_meshes_count_against_the_budget_and_empty_chunks_are_still_handed_out() {
        let mut tracker = ChunkTracker::new(RADIUS, BIG_CAP);
        tracker.set_centre(at(0, 0));
        let jobs = tracker.next_jobs();
        // Every second chunk has nothing to draw.
        for (index, pos) in jobs.iter().enumerate() {
            let built = if index % 2 == 0 { solid() } else { invisible() };
            tracker.finish(*pos, built);
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
        let pos = tracker.next_jobs()[0];
        tracker.finish(pos, invisible());
        let taken = tracker.take_loaded(0);
        assert_eq!(taken.len(), 1);
        assert_eq!(taken[0].0, pos);
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
        uploaded.sort_by_key(|pos| (pos.z, pos.x));
        wanted.sort_by_key(|pos| (pos.z, pos.x));
        assert_eq!(uploaded, wanted);
    }

    #[test]
    fn a_zero_cap_still_makes_progress() {
        let mut tracker = ChunkTracker::new(RADIUS, 0);
        tracker.set_centre(at(0, 0));
        assert_eq!(tracker.next_jobs().len(), 1);
    }
}
