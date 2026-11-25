//! The decisions of streaming for columns and tiles together: what to unload when the camera
//! enters another column, which jobs to start next and what to hand out this frame (decision
//! 0015, step S7).
//!
//! Pure, like the trackers it drives: the workers and the clock stay in [`super::ChunkStreamer`].
//! Columns and tiles share one cap of jobs in flight and one upload budget:
//! - **Jobs, nearest first:** the next job is the nearer of the next column and the next tile
//!   (by [`super::lod::distance_squared`]), so the ground near the camera fills before the far
//!   rings, and the full-detail columns, which are nearest, go first in practice.
//! - **Uploads, columns first:** the columns' meshes take the budget first and the tiles get
//!   what is left. Columns carry the blocks physics and editing read, and finished tiles that
//!   wait count against the cap, so new column jobs are never starved for long.

use vox_world::{ChunkMesh, TilePos};

use super::column::Column;
use super::job::{BuiltColumn, Finished, Job};
use super::plan::Plan;
use super::tracker::Tracker;

/// What the trackers know about the progress, for the HUD and the fill log line.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StreamStats {
    /// Columns whose chunks went to the game, including those with nothing to draw.
    pub loaded: usize,
    /// Columns wanted but not loaded yet: queued, in flight or waiting for upload.
    pub queued: usize,
    /// Tiles of level 1 or more handed to the renderer, including those with nothing to draw.
    pub tiles_loaded: usize,
    /// Tiles wanted but not loaded yet. After the first fill, a tile here is a hole in the
    /// ground (a level change removes the old tile first, step S8 will close that) or a new
    /// tile at the fogged edge of the horizon.
    pub tiles_queued: usize,
    /// Bytes of vertices and indices of the loaded columns and tiles as streamed: the estimate
    /// of GPU mesh memory (an edit's remesh is not counted).
    pub mesh_bytes: usize,
}

impl StreamStats {
    /// Columns and tiles still to come together: zero once the area and the horizon are full.
    pub fn waiting(&self) -> usize {
        self.queued + self.tiles_queued
    }
}

/// What leaves the game's world and the renderer when the camera enters another column.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Removed {
    /// Columns no longer kept, every chunk of them.
    pub columns: Vec<Column>,
    /// Tiles that left the selection.
    pub tiles: Vec<TilePos>,
}

/// What is handed out in one frame: finished columns and tiles within the upload budget.
#[derive(Debug, Default)]
pub struct Taken {
    /// Finished columns, also those with nothing to draw.
    pub columns: Vec<(Column, BuiltColumn)>,
    /// Finished tiles, also those with nothing to draw.
    pub tiles: Vec<(TilePos, ChunkMesh)>,
}

/// Decides what to load, unload and upload of columns and tiles around a moving camera.
#[derive(Debug)]
pub struct Scheduler {
    /// The full-detail radius in chunks.
    radius: i32,
    /// The horizon in blocks; 0 selects no tiles.
    horizon: i32,
    /// At most this many columns and tiles together are in flight plus waiting for upload.
    cap: usize,
    /// The column the plan is centred on; `None` until the first [`Self::set_centre`].
    centre: Option<Column>,
    columns: Tracker<Column, BuiltColumn>,
    tiles: Tracker<TilePos, ChunkMesh>,
}

impl Scheduler {
    /// A scheduler for a full-detail `radius` in chunks and a `horizon` in blocks (0: no
    /// tiles) that keeps at most `cap` jobs in flight plus waiting for upload. The cap counts at
    /// least 1, or nothing would ever be sent.
    pub fn new(radius: i32, horizon: i32, cap: usize) -> Self {
        Self {
            radius,
            horizon,
            cap: cap.max(1),
            centre: None,
            columns: Tracker::default(),
            tiles: Tracker::default(),
        }
    }

    /// Moves the centre; nothing happens if it did not change. Builds the new plan, forgets
    /// what it does not keep and returns the loaded columns and tiles among that.
    pub fn set_centre(&mut self, centre: Column) -> Removed {
        if self.centre == Some(centre) {
            return Removed::default();
        }
        self.centre = Some(centre);
        let plan = Plan::new(centre, self.radius, self.horizon);
        Removed {
            columns: self
                .columns
                .retarget(plan.columns(), |column| plan.keeps_column(column)),
            tiles: self
                .tiles
                .retarget(plan.tiles(), |tile| plan.keeps_tile(tile)),
        }
    }

    /// The jobs to send now, nearest first, while fewer than the cap are busy. Ties go to the
    /// column.
    pub fn next_jobs(&mut self) -> Vec<Job> {
        let mut jobs = Vec::new();
        while self.columns.busy() + self.tiles.busy() < self.cap {
            let job = match (self.columns.next_distance(), self.tiles.next_distance()) {
                (None, None) => break,
                (Some(column), Some(tile)) if tile < column => self.tiles.take_job().map(Job::Tile),
                (Some(_), _) => self.columns.take_job().map(Job::Column),
                (None, Some(_)) => self.tiles.take_job().map(Job::Tile),
            };
            // The distance said the queue was not empty, so a job is always there.
            let Some(job) = job else {
                break;
            };
            jobs.push(job);
        }
        jobs
    }

    /// Files a finished job; a result for something no longer in flight is dropped.
    pub fn finish(&mut self, finished: Finished) {
        match finished {
            Finished::Column(column, built) => self.columns.finish(column, built),
            Finished::Tile(tile, mesh) => self.tiles.finish(tile, mesh),
        }
    }

    /// The finished columns and tiles to hand out this frame with at most `budget` meshes to
    /// upload among them: the columns first, the tiles with the rest of the budget.
    pub fn take_loaded(&mut self, budget: usize) -> Taken {
        let (columns, used) = self.columns.take_loaded(budget);
        let (tiles, _) = self.tiles.take_loaded(budget.saturating_sub(used));
        Taken { columns, tiles }
    }

    /// Progress figures.
    pub fn stats(&self) -> StreamStats {
        let columns = self.columns.progress();
        let tiles = self.tiles.progress();
        StreamStats {
            loaded: columns.loaded,
            queued: columns.queued,
            tiles_loaded: tiles.loaded,
            tiles_queued: tiles.queued,
            mesh_bytes: columns.mesh_bytes + tiles.mesh_bytes,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use vox_world::{Chunk, Face};

    use super::*;
    use crate::streaming::job::{Built, TILE_LAYER};
    use crate::streaming::lod::{self, DEFAULT_HORIZON_BLOCKS};

    /// The game's default radius.
    const RADIUS: i32 = 8;
    /// A horizon that has tiles of levels 1 to 3 at radius 8 and keeps the tests quick.
    const HORIZON: i32 = 1024;
    /// A cap smaller than either queue, so the sharing shows.
    const CAP: usize = 6;
    /// More than any plan of the tests holds.
    const BIG_CAP: usize = 100_000;

    /// A mesh with something to draw: less than a quad of indices, so no quad per direction.
    fn drawn_mesh() -> ChunkMesh {
        ChunkMesh {
            vertices: Vec::new(),
            indices: vec![0],
            face_quads: [0; Face::COUNT],
        }
    }

    /// A fake result for `job`: one drawn mesh.
    fn fake(job: Job) -> Finished {
        match job {
            Job::Column(column) => Finished::Column(
                column,
                BuiltColumn {
                    span: 0..=0,
                    layers: vec![(
                        column.chunk(0),
                        Built {
                            chunk: Chunk::new(),
                            mesh: drawn_mesh(),
                        },
                    )],
                },
            ),
            Job::Tile(tile) => Finished::Tile(tile, drawn_mesh()),
        }
    }

    /// What the game holds, kept from the scheduler's output the way `main.rs` applies it.
    #[derive(Default)]
    struct Game {
        columns: HashSet<Column>,
        tiles: HashSet<TilePos>,
    }

    impl Game {
        fn apply_removed(&mut self, removed: Removed) {
            for column in removed.columns {
                assert!(
                    self.columns.remove(&column),
                    "{column:?} removed, not loaded"
                );
            }
            for tile in removed.tiles {
                assert!(self.tiles.remove(&tile), "{tile:?} removed, not loaded");
            }
        }

        fn apply_taken(&mut self, taken: Taken) {
            for (column, _) in taken.columns {
                assert!(self.columns.insert(column), "{column:?} loaded twice");
            }
            for (tile, _) in taken.tiles {
                assert!(self.tiles.insert(tile), "{tile:?} loaded twice");
            }
        }
    }

    /// Frames at `centre` until nothing waits: every job finishes at once.
    fn fill(scheduler: &mut Scheduler, game: &mut Game, centre: Column, budget: usize) {
        game.apply_removed(scheduler.set_centre(centre));
        while scheduler.stats().waiting() > 0 {
            for job in scheduler.next_jobs() {
                scheduler.finish(fake(job));
            }
            game.apply_taken(scheduler.take_loaded(budget));
        }
    }

    /// The coverage rule of `plan.rs`: every column whose square reaches into the horizon disc
    /// is drawn by exactly one of a loaded column or a loaded tile, and no column anywhere is
    /// drawn twice.
    fn assert_partition(game: &Game, centre: Column, horizon: i32) {
        let mut cover_count = std::collections::HashMap::<Column, usize>::new();
        // Every tile column is drawn by all its layers that can show a face, and counts once.
        let tile_columns: HashSet<(u8, i32, i32)> = game
            .tiles
            .iter()
            .map(|tile| (tile.level(), tile.pos().x, tile.pos().z))
            .collect();
        for tile in &game.tiles {
            assert!(
                tile.level() >= 1
                    && vox_world::tile_layers_in(tile.level()).contains(&tile.pos().y)
            );
        }
        for &(level, x, z) in &tile_columns {
            for layer in vox_world::tile_layers_in(level) {
                let pos = vox_world::ChunkPos::new(x, layer, z);
                assert!(game.tiles.contains(&TilePos::new(level, pos)));
            }
            let size = 1_i32 << level;
            for dz in 0..size {
                for dx in 0..size {
                    let column = Column::new(x * size + dx, z * size + dz);
                    *cover_count.entry(column).or_default() += 1;
                }
            }
        }
        for column in &game.columns {
            *cover_count.entry(*column).or_default() += 1;
        }
        for (column, count) in &cover_count {
            assert_eq!(
                *count, 1,
                "{column:?} drawn {count} times around {centre:?}"
            );
        }
        let reach = horizon / vox_world::CHUNK_SIZE as i32 + 1;
        let camera = centre.chunk(TILE_LAYER);
        let horizon_squared = i64::from(horizon) * i64::from(horizon);
        for z in centre.z - reach..=centre.z + reach {
            for x in centre.x - reach..=centre.x + reach {
                let column = Column::new(x, z);
                let tile = TilePos::chunk(column.chunk(TILE_LAYER));
                if lod::distance_squared(camera, tile) <= horizon_squared {
                    assert!(
                        cover_count.contains_key(&column),
                        "{column:?} is not drawn around {centre:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn every_place_is_drawn_exactly_once_at_steady_state_along_a_walk() {
        let mut scheduler = Scheduler::new(RADIUS, HORIZON, BIG_CAP);
        let mut game = Game::default();
        // A walk that crosses tile borders of every level, both signs, and a jump back.
        let mut centres: Vec<Column> = (-20..20).map(|x| Column::new(x, x / 3)).collect();
        centres.push(Column::new(-3, 40));
        centres.push(Column::new(0, 0));
        for centre in centres {
            fill(
                &mut scheduler,
                &mut game,
                centre,
                crate::streaming::MAX_UPLOADS_PER_FRAME,
            );
            assert_partition(&game, centre, HORIZON);
        }
    }

    #[test]
    fn the_partition_holds_at_the_default_horizon() {
        let mut scheduler = Scheduler::new(RADIUS, DEFAULT_HORIZON_BLOCKS, BIG_CAP);
        let mut game = Game::default();
        for centre in [Column::new(5, -9), Column::new(6, -9), Column::new(16, -9)] {
            fill(&mut scheduler, &mut game, centre, usize::MAX);
            assert_partition(&game, centre, DEFAULT_HORIZON_BLOCKS);
        }
        // Without the keep rule a column in the radius's keep range would stay under a tile:
        // show that the case occurs, so the rule is tested.
        let centre = Column::new(16, -9);
        let plan = Plan::new(centre, RADIUS, DEFAULT_HORIZON_BLOCKS);
        let reach = RADIUS + crate::streaming::area::UNLOAD_MARGIN;
        let under_a_tile = (-reach..=reach).any(|dz| {
            (-reach..=reach).any(|dx| {
                let column = Column::new(centre.x + dx, centre.z + dz);
                crate::streaming::area::is_kept(centre, column, RADIUS) && plan.covers(column)
            })
        });
        assert!(
            under_a_tile,
            "the test does not reach the case the rule is for"
        );
    }

    #[test]
    fn without_a_horizon_there_are_no_tiles() {
        let mut scheduler = Scheduler::new(RADIUS, 0, BIG_CAP);
        let mut game = Game::default();
        fill(&mut scheduler, &mut game, Column::new(0, 0), usize::MAX);
        assert!(game.tiles.is_empty());
        assert_eq!(
            game.columns.len(),
            crate::streaming::area::wanted(Column::new(0, 0), RADIUS).len()
        );
        assert_eq!(scheduler.stats().tiles_loaded, 0);
    }

    #[test]
    fn jobs_come_nearest_first_across_columns_and_tiles() {
        let mut scheduler = Scheduler::new(RADIUS, HORIZON, BIG_CAP);
        let centre = Column::new(2, 3);
        scheduler.set_centre(centre);
        let camera = centre.chunk(TILE_LAYER);
        let jobs = scheduler.next_jobs();
        let distance = |job: &Job| match job {
            Job::Column(column) => {
                lod::distance_squared(camera, TilePos::chunk(column.chunk(TILE_LAYER)))
            }
            Job::Tile(tile) => lod::distance_squared(camera, *tile),
        };
        assert!(jobs.windows(2).all(|w| distance(&w[0]) <= distance(&w[1])));
        assert_eq!(jobs[0], Job::Column(centre));
        assert!(jobs.iter().any(|job| matches!(job, Job::Tile(_))));
        // No job twice.
        let unique: HashSet<Job> = jobs.iter().copied().collect();
        assert_eq!(unique.len(), jobs.len());
    }

    #[test]
    fn the_cap_is_shared_and_a_zero_cap_still_makes_progress() {
        let mut scheduler = Scheduler::new(RADIUS, HORIZON, CAP);
        scheduler.set_centre(Column::new(0, 0));
        let first = scheduler.next_jobs();
        assert_eq!(first.len(), CAP);
        // The nearest jobs are columns, and they hold the cap the tiles share.
        assert!(first.iter().all(|job| matches!(job, Job::Column(_))));
        assert!(scheduler.stats().tiles_queued > 0);
        assert!(scheduler.next_jobs().is_empty());
        // A finished job still counts until it is handed out.
        scheduler.finish(fake(first[0]));
        assert!(scheduler.next_jobs().is_empty());
        scheduler.take_loaded(usize::MAX);
        assert_eq!(scheduler.next_jobs().len(), 1);

        let mut zero = Scheduler::new(RADIUS, HORIZON, 0);
        zero.set_centre(Column::new(0, 0));
        assert_eq!(zero.next_jobs().len(), 1);
    }

    #[test]
    fn ready_tiles_that_fill_the_cap_block_new_jobs() {
        let mut scheduler = Scheduler::new(RADIUS, HORIZON, CAP);
        scheduler.set_centre(Column::new(0, 0));
        // Run columns to the end, holding the tile jobs back.
        let mut tiles = Vec::new();
        while tiles.len() < CAP {
            let jobs = scheduler.next_jobs();
            assert!(!jobs.is_empty(), "the cap is free, so jobs come");
            for job in jobs {
                match job {
                    Job::Tile(_) => tiles.push(job),
                    Job::Column(_) => scheduler.finish(fake(job)),
                }
            }
            scheduler.take_loaded(usize::MAX);
        }
        // The tiles finish but are not handed out: they still hold the cap.
        for job in &tiles {
            scheduler.finish(fake(*job));
        }
        assert!(scheduler.next_jobs().is_empty());
        assert!(scheduler.stats().waiting() > tiles.len());
        assert_eq!(scheduler.take_loaded(usize::MAX).tiles.len(), CAP);
        assert!(!scheduler.next_jobs().is_empty());
    }

    #[test]
    fn at_the_same_distance_the_column_goes_first() {
        let mut scheduler = Scheduler::new(RADIUS, HORIZON, BIG_CAP);
        let column = Column::new(3, 0);
        let tile = TilePos::new(1, vox_world::ChunkPos::new(5, TILE_LAYER, 0));
        scheduler.columns.retarget(&[(7, column)], |_| true);
        scheduler.tiles.retarget(&[(7, tile)], |_| true);
        assert_eq!(
            scheduler.next_jobs(),
            vec![Job::Column(column), Job::Tile(tile)]
        );
    }

    #[test]
    fn the_upload_budget_is_shared_columns_first() {
        let mut scheduler = Scheduler::new(RADIUS, HORIZON, BIG_CAP);
        scheduler.set_centre(Column::new(0, 0));
        let jobs = scheduler.next_jobs();
        let tiles: Vec<Job> = jobs
            .iter()
            .copied()
            .filter(|job| matches!(job, Job::Tile(_)))
            .collect();
        let columns: Vec<Job> = jobs
            .iter()
            .copied()
            .filter(|job| matches!(job, Job::Column(_)))
            .collect();
        // Tiles finish first, then two columns: the columns still go first.
        for job in tiles.iter().chain(&columns[..2]) {
            scheduler.finish(fake(*job));
        }
        let budget = 4;
        let taken = scheduler.take_loaded(budget);
        assert_eq!(taken.columns.len(), 2);
        assert_eq!(taken.tiles.len(), budget - 2);
        let taken = scheduler.take_loaded(budget);
        assert!(taken.columns.is_empty());
        assert_eq!(taken.tiles.len(), budget);
        assert!(scheduler.take_loaded(0).tiles.is_empty());
    }

    #[test]
    fn a_tile_that_leaves_the_selection_is_removed_and_its_stale_result_dropped() {
        let mut scheduler = Scheduler::new(RADIUS, HORIZON, BIG_CAP);
        let mut game = Game::default();
        fill(&mut scheduler, &mut game, Column::new(0, 0), usize::MAX);
        let before = game.tiles.clone();
        // Far enough that every tile of the old selection leaves it.
        let far = Column::new(400, 0);
        game.apply_removed(scheduler.set_centre(far));
        assert!(game.tiles.is_empty(), "every old tile was removed");
        let jobs = scheduler.next_jobs();
        // A late result of an old tile is dropped.
        let old = *before.iter().next().expect("tiles were loaded");
        scheduler.finish(Finished::Tile(old, drawn_mesh()));
        for job in jobs {
            scheduler.finish(fake(job));
        }
        let taken = scheduler.take_loaded(usize::MAX);
        assert!(taken.tiles.iter().all(|(tile, _)| *tile != old));
        assert!(!taken.tiles.is_empty());
    }

    #[test]
    fn the_same_centre_changes_nothing_and_no_job_is_sent_twice() {
        let mut scheduler = Scheduler::new(RADIUS, HORIZON, CAP);
        let centre = Column::new(-5, 7);
        scheduler.set_centre(centre);
        let mut sent = HashSet::new();
        let mut held = Vec::new();
        while scheduler.stats().waiting() > 0 {
            assert_eq!(scheduler.set_centre(centre), Removed::default());
            // Every second job finishes a frame late, so jobs are in flight across frames.
            for (index, job) in scheduler.next_jobs().into_iter().enumerate() {
                assert!(sent.insert(job), "{job:?} sent twice");
                if index % 2 == 0 {
                    scheduler.finish(fake(job));
                } else {
                    held.push(job);
                }
            }
            for job in held.drain(..) {
                scheduler.finish(fake(job));
            }
            scheduler.take_loaded(crate::streaming::MAX_UPLOADS_PER_FRAME);
        }
        let stats = scheduler.stats();
        assert_eq!(sent.len(), stats.loaded + stats.tiles_loaded);
    }
}
