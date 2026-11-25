//! Chunk streaming: loads the terrain around the camera and unloads what is far away, in full
//! detail near the camera and as level-of-detail tiles out to the horizon.
//!
//! It knows `vox-world` and nothing of the renderer: [`ChunkStreamer::update`] returns plain
//! data (positions, chunks, spans and meshes) that the game hands to its world and the renderer.
//! The unit of full detail is a column of chunks, of which only the layers of its span are
//! generated (decision 0015); far away the unit is a tile of level 1 or more, which keeps only
//! its mesh. The decisions live in pure code ([`area`], [`lod`], `plan`, `tracker`,
//! `scheduler`), the threads in `workers`, the work of one job in `job`; this module wires
//! them together (decisions 0010 and 0015).

pub mod area;
pub mod column;
mod job;
pub mod lod;
mod plan;
mod scheduler;
mod tracker;
mod workers;

use std::num::NonZeroUsize;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use vox_world::{Chunk, ChunkMesh, ChunkPos, TilePos};

pub use column::{Column, ColumnSpans, Span};
pub use job::{Terrain, remesh};
pub use scheduler::StreamStats;
pub use workers::StreamError;

use job::{Finished, Job};

/// The column job, for tests outside streaming that build a loaded area by hand.
#[cfg(test)]
pub(crate) use job::build_column;
use scheduler::Scheduler;
use workers::Workers;

/// How many jobs each worker may have in flight or waiting for upload. Two keeps every worker
/// busy while the main thread uploads, and few enough that a camera move leaves only a few
/// stale jobs.
const JOBS_PER_WORKER: usize = 2;

/// The most non-empty meshes handed to the renderer in one frame, columns and tiles together.
/// Provisional: it is fixed by measuring the cost of an upload (step 7 of decision 0010).
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
    /// Meshes to hand to the renderer: the non-empty meshes of the columns in `loaded`.
    pub uploads: Vec<(ChunkPos, ChunkMesh)>,
    /// Tiles of level 1 or more that left the selection: their meshes leave the renderer,
    /// before the uploads.
    pub removed_tiles: Vec<TilePos>,
    /// Non-empty meshes of tiles of level 1 or more for the renderer. Together with `uploads`
    /// at most [`MAX_UPLOADS_PER_FRAME`], unless one column alone has more.
    pub tile_uploads: Vec<(TilePos, ChunkMesh)>,
}

/// Time spent in finished jobs of one kind, for the averages in the fill log line.
#[derive(Clone, Copy, Debug, Default)]
struct JobTimes {
    count: u32,
    total: Duration,
}

impl JobTimes {
    fn add(&mut self, took: Duration) {
        self.count = self.count.saturating_add(1);
        self.total += took;
    }

    fn average(self) -> Option<Duration> {
        (self.count > 0).then(|| self.total / self.count)
    }
}

/// Loads and unloads chunk columns and tiles around a moving centre on background threads.
pub struct ChunkStreamer {
    scheduler: Scheduler,
    workers: Workers<Job, Finished>,
    worker_count: usize,
    column_times: JobTimes,
    tile_times: JobTimes,
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
    /// Starts the workers for `terrain`, to load a circle of columns `radius` chunks wide and
    /// tiles out to `horizon` blocks (0: no tiles). Keep the horizon at or below
    /// [`lod::max_horizon_for`] the radius, or the mesh budget is not kept.
    pub fn new(terrain: Arc<Terrain>, radius: i32, horizon: i32) -> Self {
        let worker_count = worker_count();
        Self {
            scheduler: Scheduler::new(radius, horizon, JOBS_PER_WORKER * worker_count),
            workers: Workers::spawn(worker_count, move |job, scratch| {
                job::run(&terrain, job, scratch)
            }),
            worker_count,
            column_times: JobTimes::default(),
            tile_times: JobTimes::default(),
        }
    }

    /// One frame of streaming: moves the centre to the column of `camera_chunk`, collects
    /// finished jobs, hands out at most [`MAX_UPLOADS_PER_FRAME`] meshes and sends new jobs.
    ///
    /// # Errors
    /// [`StreamError::WorkerStopped`] when a worker thread has died.
    pub fn update(&mut self, camera_chunk: ChunkPos) -> Result<Changes, StreamError> {
        let removed = self.scheduler.set_centre(Column::of(camera_chunk));
        self.workers.check()?;
        while let Some(result) = self.workers.try_recv()? {
            match result.job {
                Job::Column(_) => self.column_times.add(result.took),
                Job::Tile(_) => self.tile_times.add(result.took),
            }
            self.scheduler.finish(result.output);
        }
        // Uploads come before new jobs: they free the slots the new jobs take.
        let taken = self.scheduler.take_loaded(MAX_UPLOADS_PER_FRAME);
        for job in self.scheduler.next_jobs() {
            self.workers.send(job)?;
        }
        let mut loaded = Vec::with_capacity(taken.columns.len());
        let mut uploads = Vec::new();
        for (column, built) in taken.columns {
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
        let tile_uploads = taken
            .tiles
            .into_iter()
            .filter(|(_, mesh)| !mesh.indices.is_empty())
            .collect();
        Ok(Changes {
            removed: removed.columns,
            loaded,
            uploads,
            removed_tiles: removed.tiles,
            tile_uploads,
        })
    }

    /// How many columns and tiles are loaded and how many are still to come.
    pub fn stats(&self) -> StreamStats {
        self.scheduler.stats()
    }

    /// Average time of a column job so far, or `None` before the first one finished.
    pub fn average_job_time(&self) -> Option<Duration> {
        self.column_times.average()
    }

    /// Average time of a tile job so far, or `None` before the first one finished.
    pub fn average_tile_time(&self) -> Option<Duration> {
        self.tile_times.average()
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

    /// The seed of the streaming tests' terrain. At this seed the columns around the origin
    /// (x -2 to 3, z -3 to 3) are land, plains and forest, every one with the span
    /// [`TERRAIN_LAYER`] alone (found by a scan, so the tests need no mountains or sea).
    pub const SEED: u32 = 273;
    /// The one chunk layer the tests' land lies in (blocks 64 to 95, just over the sea level).
    pub const TERRAIN_LAYER: i32 = 2;

    /// The ids of the tests' blocks: the first three are the ones the game's registry hands out.
    pub const BLOCKS: TerrainBlocks = TerrainBlocks {
        grass: BlockId(1),
        dirt: BlockId(2),
        stone: BlockId(3),
        sand: BlockId(4),
        gravel: BlockId(5),
        snow: BlockId(6),
        water: BlockId(7),
        log: BlockId(8),
        leaves: BlockId(9),
        bedrock: BlockId(10),
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
    use super::test_blocks::{BLOCKS, SEED, face_textures};

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
        tiles: HashMap<TilePos, ChunkMesh>,
        removed_tiles: Vec<TilePos>,
    }

    /// Runs frames at `centre` until nothing is queued.
    fn fill(streamer: &mut ChunkStreamer, centre: Column) -> Run {
        let started = Instant::now();
        let mut uploaded = HashMap::new();
        let mut removed = Vec::new();
        let mut loaded = Vec::new();
        let mut tiles = HashMap::new();
        let mut removed_tiles = Vec::new();
        loop {
            let changes = streamer.update(centre.chunk(0)).expect("workers run");
            assert!(changes.uploads.len() + changes.tile_uploads.len() <= MAX_UPLOADS_PER_FRAME);
            removed.extend(changes.removed);
            // A removed tile may have been loaded by an earlier run, or had nothing to draw.
            for tile in changes.removed_tiles {
                tiles.remove(&tile);
                removed_tiles.push(tile);
            }
            for (tile, mesh) in changes.tile_uploads {
                assert!(
                    tiles.insert(tile, mesh).is_none(),
                    "{tile:?} uploaded twice"
                );
            }
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
            if streamer.stats().waiting() == 0 {
                return Run {
                    uploaded,
                    loaded,
                    removed,
                    tiles,
                    removed_tiles,
                };
            }
            assert!(started.elapsed() < TIMEOUT, "timed out");
            thread::sleep(FRAME);
        }
    }

    #[test]
    fn the_area_fills_and_a_far_move_removes_it() {
        let mut streamer = ChunkStreamer::new(terrain(), RADIUS, 0);
        let centre = Column::new(0, 0);
        let Run {
            uploaded,
            loaded,
            removed,
            tiles,
            ..
        } = fill(&mut streamer, centre);
        assert!(tiles.is_empty(), "no horizon, no tiles");

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
        let streamer = ChunkStreamer::new(terrain(), RADIUS, 0);
        assert!(streamer.average_job_time().is_none());
        assert!(streamer.average_tile_time().is_none());
        assert!(streamer.worker_count() >= 1);
    }

    /// A horizon with tiles of level 1 and 2 at [`RADIUS`], small enough for a debug test.
    const TILE_HORIZON: i32 = 300;

    #[test]
    fn the_tiles_of_the_selection_arrive_as_the_tile_job_builds_them_and_leave_with_it() {
        let mut streamer = ChunkStreamer::new(terrain(), RADIUS, TILE_HORIZON);
        let centre = Column::new(-1, 2);
        let run = fill(&mut streamer, centre);
        let terrain = terrain();
        let mut scratch = ChunkMesh::default();
        let selected: Vec<TilePos> = lod::select_tiles(centre.chunk(0), RADIUS, TILE_HORIZON)
            .into_iter()
            .filter(|tile| tile.level() >= 1)
            // Every layer of a tile column is a tile of its own.
            .flat_map(|tile| {
                vox_world::tile_layers_in(tile.level()).map(move |layer| {
                    let pos = ChunkPos::new(tile.pos().x, layer, tile.pos().z);
                    TilePos::new(tile.level(), pos)
                })
            })
            .collect();
        assert!(!selected.is_empty());
        let mut expected = HashMap::new();
        for tile in &selected {
            let mesh = job::build_tile(&terrain, *tile, &mut scratch);
            if !mesh.indices.is_empty() {
                expected.insert(*tile, mesh);
            }
        }
        assert_eq!(run.tiles, expected);
        assert_eq!(streamer.stats().tiles_loaded, selected.len());
        assert!(streamer.stats().mesh_bytes > 0);
        assert!(streamer.average_tile_time().is_some());

        // Far away every old tile leaves the renderer.
        let far = Column::new(centre.x + 200, centre.z);
        let removed = fill(&mut streamer, far).removed_tiles;
        for tile in expected.keys() {
            assert!(removed.contains(tile), "{tile:?} was not removed");
        }
    }

    #[test]
    fn dropping_the_workers_with_tile_jobs_queued_stops_soon() {
        /// Jobs queued before the drop: level-4 tiles, the most expensive kind.
        const JOBS: i32 = 200;
        let terrain = terrain();
        let workers: Workers<Job, Finished> =
            Workers::spawn(1, move |job, scratch| job::run(&terrain, job, scratch));
        let one = Instant::now();
        workers
            .send(Job::Tile(TilePos::new(4, ChunkPos::new(-1, 0, 0))))
            .expect("workers run");
        while workers.try_recv().expect("workers run").is_none() {
            assert!(one.elapsed() < TIMEOUT, "timed out");
            thread::sleep(FRAME);
        }
        let job_time = one.elapsed();
        for x in 0..JOBS {
            let tile = TilePos::new(4, ChunkPos::new(x, 0, 0));
            workers.send(Job::Tile(tile)).expect("workers run");
        }
        let started = Instant::now();
        drop(workers);
        // Draining the queue would take about JOBS job times; stopping takes about one.
        let quarter = job_time * u32::try_from(JOBS).unwrap_or(u32::MAX) / 4;
        assert!(started.elapsed() < quarter, "{:?}", started.elapsed());
    }

    #[test]
    fn a_tile_job_is_the_generated_tile_meshed_alone_and_keeps_no_blocks() {
        let terrain = terrain();
        let mut scratch = ChunkMesh::default();
        for tile in [
            TilePos::new(1, ChunkPos::new(0, 0, 0)),
            TilePos::new(3, ChunkPos::new(-2, 0, 5)),
        ] {
            let Finished::Tile(key, mesh) = job::run(&terrain, Job::Tile(tile), &mut scratch)
            else {
                panic!("a tile job gives a tile");
            };
            assert_eq!(key, tile);
            let cells = vox_world::generate_lod_chunk(SEED, tile, &BLOCKS);
            let mut by_hand = ChunkMesh::default();
            vox_world::build_tile_mesh_into(
                &cells,
                &vox_world::solid_chunk(BLOCKS.bedrock),
                &face_textures(),
                &mut by_hand,
            );
            assert_eq!(mesh, by_hand);
            assert!(!mesh.indices.is_empty());
        }
    }
}
