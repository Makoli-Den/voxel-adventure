//! Chunk streaming: loads the terrain around the camera and unloads what is far away.
//!
//! It knows `vox-world` and nothing of the renderer: [`ChunkStreamer::update`] returns plain
//! data (positions, chunks, spans and meshes) that the game hands to its world and the renderer.
//! The unit is a column of chunks, of which only the layers of its span are generated (decision
//! 0015). The decisions live in pure code ([`area`], [`tracker`]), the threads in [`workers`],
//! the work of one column in [`job`]; this module wires them together (decision 0010).

pub mod area;
pub mod column;
mod job;
mod tracker;
mod workers;

use std::num::NonZeroUsize;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use vox_world::{Chunk, ChunkMesh, ChunkPos};

pub use column::{Column, ColumnSpans, Span};
pub use job::{Terrain, remesh};
pub use tracker::StreamStats;
pub use workers::StreamError;

use job::BuiltColumn;

/// The column job, for tests outside streaming that build a loaded area by hand.
#[cfg(test)]
pub(crate) use job::build_column;
use tracker::ChunkTracker;
use workers::Workers;

/// How many jobs each worker may have in flight or waiting for upload. Two keeps every worker
/// busy while the main thread uploads, and few enough that a camera move leaves only a few
/// stale jobs.
const JOBS_PER_WORKER: usize = 2;

/// The most non-empty meshes handed to the renderer in one frame. Provisional: it is fixed by
/// measuring the cost of an upload (step 7 of decision 0010).
pub const MAX_UPLOADS_PER_FRAME: usize = 4;

/// A column that finished loading: its span and the generated chunks of the span.
#[derive(Debug)]
pub struct LoadedColumn {
    /// The column.
    pub column: Column,
    /// The chunk layers that can show a face; `chunks` holds exactly these.
    pub span: Span,
    /// The generated chunks of the span, including those with nothing to draw, lowest first.
    pub chunks: Vec<(ChunkPos, Chunk)>,
}

/// What the game's world and the renderer have to do after one frame of streaming.
#[derive(Debug, Default)]
pub struct Changes {
    /// Columns that are unloaded: every chunk of them ([`Column::chunks`]) leaves the game's
    /// world and the renderer, also chunks an edit created. Applied before the uploads, so the
    /// memory of a removed mesh is released first.
    pub removed: Vec<Column>,
    /// Freshly generated columns for the game's world. Each column appears at most once and
    /// never together with its removal in one frame.
    pub loaded: Vec<LoadedColumn>,
    /// Meshes to hand to the renderer: the non-empty meshes of the columns in `loaded`, at most
    /// [`MAX_UPLOADS_PER_FRAME`] unless one column alone has more.
    pub uploads: Vec<(ChunkPos, ChunkMesh)>,
}

/// Loads and unloads chunks around a moving centre on background threads.
pub struct ChunkStreamer {
    tracker: ChunkTracker,
    workers: Workers<BuiltColumn>,
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
    /// Starts the workers for `terrain`, to load a circle of columns `radius` chunks wide.
    pub fn new(terrain: Arc<Terrain>, radius: i32) -> Self {
        let worker_count = worker_count();
        Self {
            tracker: ChunkTracker::new(radius, JOBS_PER_WORKER * worker_count),
            workers: Workers::spawn(worker_count, move |column, scratch| {
                job::build_column(&terrain, column, scratch)
            }),
            worker_count,
            jobs_done: 0,
            job_time: Duration::ZERO,
        }
    }

    /// One frame of streaming: moves the centre to the column of `camera_chunk`, collects
    /// finished jobs, hands out at most [`MAX_UPLOADS_PER_FRAME`] meshes and sends new jobs.
    ///
    /// # Errors
    /// [`StreamError::WorkerStopped`] when a worker thread has died.
    pub fn update(&mut self, camera_chunk: ChunkPos) -> Result<Changes, StreamError> {
        let removed = self.tracker.set_centre(Column::of(camera_chunk));
        self.workers.check()?;
        while let Some(result) = self.workers.try_recv()? {
            self.jobs_done = self.jobs_done.saturating_add(1);
            self.job_time += result.took;
            self.tracker.finish(result.column, result.output);
        }
        // Uploads come before new jobs: they free the slots the new jobs take.
        let taken = self.tracker.take_loaded(MAX_UPLOADS_PER_FRAME);
        for column in self.tracker.next_jobs() {
            self.workers.send(column)?;
        }
        let mut loaded = Vec::with_capacity(taken.len());
        let mut uploads = Vec::new();
        for (column, built) in taken {
            let mut chunks = Vec::with_capacity(built.layers.len());
            for (pos, layer) in built.layers {
                if !layer.mesh.indices.is_empty() {
                    uploads.push((pos, layer.mesh));
                }
                chunks.push((pos, layer.chunk));
            }
            loaded.push(LoadedColumn {
                column,
                span: built.span,
                chunks,
            });
        }
        Ok(Changes {
            removed,
            loaded,
            uploads,
        })
    }

    /// How many columns are loaded and how many are still to come.
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

/// The blocks every streaming test uses, in one place.
#[cfg(test)]
pub(crate) mod test_blocks {
    use vox_world::{BlockId, Face, FaceTextures, TerrainBlocks, TextureLayer};

    /// The ids of the tests' blocks: the ones the game's registry hands out.
    pub const BLOCKS: TerrainBlocks = TerrainBlocks {
        grass: BlockId(1),
        dirt: BlockId(2),
        stone: BlockId(3),
        bedrock: BlockId(4),
    };

    /// Blocks the tests' texture table covers: air and every id up to the highest terrain
    /// block (bedrock, checked below).
    const BLOCK_COUNT: usize = BLOCKS.bedrock.0 as usize + 1;

    const _: () = assert!(
        BLOCKS.grass.0 < BLOCKS.bedrock.0
            && BLOCKS.dirt.0 < BLOCKS.bedrock.0
            && BLOCKS.stone.0 < BLOCKS.bedrock.0
    );

    /// The tests' texture table: every face of block `i` has layer `i`. The tests compare
    /// meshes built the same way, so any table would do; this one keeps the blocks apart.
    pub fn face_textures() -> FaceTextures {
        let layers = (0..BLOCK_COUNT)
            .map(|id| [TextureLayer(u16::try_from(id).unwrap()); Face::COUNT])
            .collect();
        FaceTextures::from_layers(layers)
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
    use super::test_blocks::{BLOCKS, face_textures};

    fn terrain() -> Arc<Terrain> {
        Arc::new(Terrain::new(SEED, BLOCKS, face_textures()))
    }
    /// How long the test waits for the workers before it fails.
    const TIMEOUT: Duration = Duration::from_secs(10);
    /// Pause between two frames of the test.
    const FRAME: Duration = Duration::from_millis(1);

    /// What a run of frames gave: the meshes, the loaded columns and the removals.
    struct Run {
        uploaded: HashMap<ChunkPos, ChunkMesh>,
        loaded: Vec<Column>,
        removed: Vec<Column>,
    }

    /// Runs frames at `centre` until nothing is queued.
    fn fill(streamer: &mut ChunkStreamer, centre: Column) -> Run {
        let started = Instant::now();
        let mut uploaded = HashMap::new();
        let mut removed = Vec::new();
        let mut loaded = Vec::new();
        loop {
            let changes = streamer.update(centre.chunk(0)).expect("workers run");
            assert!(changes.uploads.len() <= MAX_UPLOADS_PER_FRAME);
            removed.extend(changes.removed);
            for column in &changes.loaded {
                assert!(
                    !loaded.contains(&column.column) || removed.contains(&column.column),
                    "{:?} loaded twice",
                    column.column
                );
                // The chunks are exactly the span's layers of this column.
                let layers: Vec<ChunkPos> = column.chunks.iter().map(|(pos, _)| *pos).collect();
                let span: Vec<ChunkPos> = column
                    .span
                    .clone()
                    .map(|y| column.column.chunk(y))
                    .collect();
                assert_eq!(layers, span);
            }
            loaded.extend(changes.loaded.into_iter().map(|column| column.column));
            for (pos, mesh) in changes.uploads {
                assert!(
                    uploaded.insert(pos, mesh).is_none(),
                    "{pos:?} uploaded twice"
                );
            }
            if streamer.stats().queued == 0 {
                return Run {
                    uploaded,
                    loaded,
                    removed,
                };
            }
            assert!(started.elapsed() < TIMEOUT, "timed out");
            thread::sleep(FRAME);
        }
    }

    #[test]
    fn the_area_fills_and_a_far_move_removes_it() {
        let mut streamer = ChunkStreamer::new(terrain(), RADIUS);
        let centre = Column::new(0, 0);
        let Run {
            uploaded,
            loaded,
            removed,
        } = fill(&mut streamer, centre);

        let terrain = terrain();
        let mut scratch = ChunkMesh::default();
        let mut expected = HashMap::new();
        for column in area::wanted(centre, RADIUS) {
            for (pos, built) in job::build_column(&terrain, column, &mut scratch).layers {
                if !built.mesh.indices.is_empty() {
                    expected.insert(pos, built.mesh);
                }
            }
        }
        assert_eq!(uploaded, expected);
        assert!(removed.is_empty());
        // Every wanted column reaches the world, also those with nothing to draw.
        let mut wanted = area::wanted(centre, RADIUS);
        let mut loaded_sorted = loaded;
        wanted.sort_by_key(|pos| (pos.z, pos.x));
        loaded_sorted.sort_by_key(|pos| (pos.z, pos.x));
        assert_eq!(loaded_sorted, wanted);
        assert_eq!(streamer.stats().loaded, area::wanted(centre, RADIUS).len());
        assert!(streamer.average_job_time().is_some());

        // Beyond the keep range of every earlier position.
        let far = Column::new(2 * (RADIUS + area::UNLOAD_MARGIN) + 1, 0);
        let removed = fill(&mut streamer, far).removed;
        for pos in uploaded.keys() {
            assert!(
                removed.contains(&Column::of(*pos)),
                "{pos:?} was not removed"
            );
        }
    }

    #[test]
    fn there_is_no_average_before_the_first_job() {
        let streamer = ChunkStreamer::new(terrain(), RADIUS);
        assert!(streamer.average_job_time().is_none());
        assert!(streamer.worker_count() >= 1);
    }
}
