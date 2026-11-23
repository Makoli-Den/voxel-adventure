//! Chunk streaming: loads the terrain around the camera and unloads what is far away.
//!
//! It knows `vox-world` and nothing of the renderer: [`ChunkStreamer::update`] returns plain
//! data (positions and meshes) that the game hands to the renderer. The decisions live in pure
//! code ([`area`], [`tracker`]), the threads in [`workers`], the work of one chunk in [`job`];
//! this module wires them together (decision 0010).

pub mod area;
mod job;
mod tracker;
mod workers;

use std::num::NonZeroUsize;
use std::thread;
use std::time::Duration;

use vox_world::{ChunkMesh, ChunkPos};

pub use tracker::StreamStats;
pub use workers::StreamError;

use tracker::ChunkTracker;
use workers::Workers;

/// How many jobs each worker may have in flight or waiting for upload. Two keeps every worker
/// busy while the main thread uploads, and few enough that a camera move leaves only a few
/// stale jobs.
const JOBS_PER_WORKER: usize = 2;

/// The most non-empty meshes handed to the renderer in one frame. Provisional: it is fixed by
/// measuring the cost of an upload (step 7 of decision 0010).
pub const MAX_UPLOADS_PER_FRAME: usize = 4;

/// What the renderer has to do after one frame of streaming.
#[derive(Debug, Default)]
pub struct Changes {
    /// Chunks to remove from the renderer. Applied before the uploads, so the memory of
    /// a removed mesh is released first.
    pub removed: Vec<ChunkPos>,
    /// Meshes to hand to the renderer, at most [`MAX_UPLOADS_PER_FRAME`].
    pub uploads: Vec<(ChunkPos, ChunkMesh)>,
}

/// Loads and unloads chunks around a moving centre on background threads.
pub struct ChunkStreamer {
    tracker: ChunkTracker,
    workers: Workers,
    worker_count: usize,
    /// Jobs finished so far and the time they took together, for the average.
    jobs_done: u32,
    job_time: Duration,
}

/// Number of worker threads: one per available core minus one for the main thread, at least
/// one.
fn worker_count() -> usize {
    thread::available_parallelism()
        .map_or(1, NonZeroUsize::get)
        .saturating_sub(1)
        .max(1)
}

impl ChunkStreamer {
    /// Starts the workers for the terrain of `seed`, to load a circle of `radius` chunks.
    pub fn new(seed: u32, radius: i32) -> Self {
        let worker_count = worker_count();
        Self {
            tracker: ChunkTracker::new(radius, JOBS_PER_WORKER * worker_count),
            workers: Workers::spawn(worker_count, move |pos, scratch| {
                job::build_chunk_mesh(seed, pos, scratch)
            }),
            worker_count,
            jobs_done: 0,
            job_time: Duration::ZERO,
        }
    }

    /// One frame of streaming: moves the centre to `camera_chunk`, collects finished jobs,
    /// hands out at most [`MAX_UPLOADS_PER_FRAME`] meshes and sends new jobs.
    ///
    /// # Errors
    /// [`StreamError::WorkerStopped`] when a worker thread has died.
    pub fn update(&mut self, camera_chunk: ChunkPos) -> Result<Changes, StreamError> {
        let removed = self.tracker.set_centre(camera_chunk);
        self.workers.check()?;
        while let Some(result) = self.workers.try_recv()? {
            self.jobs_done = self.jobs_done.saturating_add(1);
            self.job_time += result.took;
            self.tracker.finish(result.pos, result.mesh);
        }
        // Uploads come before new jobs: they free the slots the new jobs take.
        let uploads = self.tracker.take_uploads(MAX_UPLOADS_PER_FRAME);
        for pos in self.tracker.next_jobs() {
            self.workers.send(pos)?;
        }
        Ok(Changes { removed, uploads })
    }

    /// How many chunks are loaded and how many are still to come.
    pub fn stats(&self) -> StreamStats {
        self.tracker.stats()
    }

    /// Average time of a job so far, or `None` before the first one finished.
    pub fn average_job_time(&self) -> Option<Duration> {
        (self.jobs_done > 0).then(|| self.job_time / self.jobs_done)
    }

    /// Number of worker threads, for the fill log line.
    pub fn worker_count(&self) -> usize {
        self.worker_count
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::time::Instant;

    use super::*;

    /// A small radius keeps the test fast: 13 columns.
    const RADIUS: i32 = 2;
    /// The seed of the tests' terrain.
    const SEED: u32 = 1;
    /// How long the test waits for the workers before it fails.
    const TIMEOUT: Duration = Duration::from_secs(10);
    /// Pause between two frames of the test.
    const FRAME: Duration = Duration::from_millis(1);

    /// Runs frames at `centre` until nothing is queued; returns what was uploaded and the
    /// removals of every frame.
    fn fill(
        streamer: &mut ChunkStreamer,
        centre: ChunkPos,
    ) -> (HashMap<ChunkPos, ChunkMesh>, Vec<ChunkPos>) {
        let started = Instant::now();
        let mut uploaded = HashMap::new();
        let mut removed = Vec::new();
        loop {
            let changes = streamer.update(centre).expect("workers run");
            assert!(changes.uploads.len() <= MAX_UPLOADS_PER_FRAME);
            removed.extend(changes.removed);
            for (pos, mesh) in changes.uploads {
                assert!(
                    uploaded.insert(pos, mesh).is_none(),
                    "{pos:?} uploaded twice"
                );
            }
            if streamer.stats().queued == 0 {
                return (uploaded, removed);
            }
            assert!(started.elapsed() < TIMEOUT, "timed out");
            thread::sleep(FRAME);
        }
    }

    #[test]
    fn the_area_fills_and_a_far_move_removes_it() {
        let mut streamer = ChunkStreamer::new(SEED, RADIUS);
        let centre = ChunkPos::ORIGIN;
        let (uploaded, removed) = fill(&mut streamer, centre);

        let mut scratch = ChunkMesh::default();
        let mut expected = HashMap::new();
        for pos in area::wanted(centre, RADIUS) {
            let mesh = job::build_chunk_mesh(SEED, pos, &mut scratch);
            if !mesh.indices.is_empty() {
                expected.insert(pos, mesh);
            }
        }
        assert_eq!(uploaded, expected);
        assert!(removed.is_empty());
        assert_eq!(streamer.stats().loaded, area::wanted(centre, RADIUS).len());
        assert!(streamer.average_job_time().is_some());

        // Beyond the keep range of every earlier position.
        let far = ChunkPos::new(2 * (RADIUS + area::UNLOAD_MARGIN) + 1, 0, 0);
        let (_, removed) = fill(&mut streamer, far);
        for pos in uploaded.keys() {
            assert!(removed.contains(pos), "{pos:?} was not removed");
        }
    }

    #[test]
    fn there_is_no_average_before_the_first_job() {
        let streamer = ChunkStreamer::new(SEED, RADIUS);
        assert!(streamer.average_job_time().is_none());
        assert!(streamer.worker_count() >= 1);
    }
}
