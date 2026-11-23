# 0010. Chunk streaming around the camera

Status: accepted (open questions answered: circle shape is fine, self-contained jobs until blocks can change, gaps at the edge are fine until fog exists, streaming stays in vox-game).

## Context
At startup `world_loader::load_area` generates and meshes a fixed square of `(2R + 1)^2` chunk columns on all cores, then `App::start` uploads every mesh. At radius 8 this blocks for about 50 ms, and the world ends where the square ends. The camera flies at 40 blocks/s, so the world has to follow it: chunks near the camera load, chunks far away unload, and the frame rate must not stutter while that happens.

What the code already gives us:
- `generate_chunk(seed, pos)` is a pure function of its inputs, and any chunk can be generated alone, in any order and on any thread (decision 0006).
- `build_mesh_with_neighbours_into` needs the six neighbours. A mesh built while a neighbour was missing is wrong once the neighbour appears (decision 0006, risks).
- `Renderer::set_chunk_mesh` and `remove_chunk_mesh` add, replace and remove meshes without stalling; freed buffers are retired by fences (decision 0007). Each mesh is one device memory allocation, and only 4096 are guaranteed.
- All terrain lies in chunk layer `y = 0`; above is air, below is stone (decision 0006).
- Frame time grows with the number of drawn triangles, not with a fixed per-chunk overhead (`docs/benchmarks.md`).

## Decision
- **Where:** a new module `crates/vox-game/src/streaming/` in the game crate. It knows `vox-world` and nothing of `vox-render`: it returns plain data (`ChunkPos`, `ChunkMesh`) and `main.rs` hands it to the `Renderer`. `world_loader.rs` is removed when streaming works.
- **Area:** a circle of columns around the camera's chunk, in the terrain layer `y = 0` only. A column `(x, z)` is wanted when `dx^2 + dz^2 <= R^2` (R is `--view-radius`). It is unloaded only when `dx^2 + dz^2 > (R + UNLOAD_MARGIN)^2`, with `UNLOAD_MARGIN = 1` chunk (hysteresis). The camera's vertical position is ignored.
- **One self-contained job per chunk:** a worker thread generates the chunk and its four horizontal neighbours, meshes the chunk with those neighbours (the layer above is air, so a missing `PosY` is correct; `NegY` is the bedrock stand-in as today), and returns the mesh. Every mesh is final when it is built, so nothing is ever meshed twice. The generated chunks are dropped after meshing, as they are today: nothing in the game reads blocks yet.
- **Workers:** `available_parallelism - 1` plain `std::thread`s (at least 1), fed by an `mpsc` job channel whose receiver is shared behind a `Mutex`, and returning results on a second channel. No new dependency.
- **Nearest first:** the main thread keeps the queue, not the channel. When the camera enters another chunk, the queue is rebuilt from the pure area function, sorted by squared distance. At most `JOBS_PER_WORKER * workers` positions are in flight or waiting for upload at once, so a move of the camera leaves only a few stale jobs; their results are dropped when they arrive outside the keep range. In-flight jobs are never cancelled.
- **Per-frame upload budget:** at most `MAX_UPLOADS_PER_FRAME` non-empty meshes go to `set_chunk_mesh` per frame (a count, provisional value 4, fixed by measurement in step 7). Empty meshes are marked loaded without an upload. Removals have no budget (they only retire a buffer) and are applied before uploads, so allocations are freed first.
- **Testable core:** the decisions (what to load, unload, upload) live in pure code (`area.rs`, `tracker.rs`) with no threads and no GPU; threads live in `workers.rs`; `mod.rs` wires them.
- **Metrics:** the HUD gets one line, `chunks  loaded L  queued Q` (`queued` = waiting, in flight or waiting for upload). When the queue first becomes empty the game logs the fill time, the number of chunks, the average job time and the average upload time.

## Reasons
- **Self-contained jobs** are the simplest correct design: no dependency between jobs, no shared world, no second meshing pass, no seams that pop when a neighbour arrives. It works because terrain is a pure function of `(seed, pos)`. The price is four extra generations per chunk, about 4 x 28 us next to about 210 us of meshing (`docs/benchmarks.md`), roughly 1.5 times the CPU per chunk, on threads that are otherwise idle.
- **Circle, not square:** about 79 percent of the chunks for the same view distance (197 instead of 289 at R = 8), which is "generate less". It also keeps the allocation count safe: the square at R = 30 with a margin of 2 would keep 4,225 meshes, over the 4096 allocations Vulkan guarantees; the circle with margin 1 keeps about 3,020.
- **A margin of one chunk** stops a camera that wanders back and forth over a chunk border from unloading and reloading a row each time, and costs the least memory and allocations.
- **Queue on the main thread with a small in-flight cap** gives nearest-first relative to where the camera is now, not where it was when the jobs were sent, without a priority queue shared between threads.
- **Count budget:** terrain meshes are of similar size (a few thousand quads), so a count is predictable and deterministic in tests. The camera needs about 21 new chunks per second at R = 8 (one row of 17 per 0.8 s chunk crossing); 4 per frame at 120 Hz is 480 per second.
- **Streaming stays in the game crate** until a second user (a server) needs it; then the pure area and tracker code moves to the core.

## Rejected
- **Two-stage pipeline** (generate jobs, then mesh jobs once all neighbours are in a shared `World` of `Arc<Chunk>`): needed once chunks are edited, saved or expensive to generate, because then a neighbour can no longer be regenerated. Not needed today; see the trigger under risks.
- **Mesh at once with missing neighbours as air, remesh when they arrive:** meshes most chunks twice and shows walls between chunks for a few frames.
- **`rayon` or `crossbeam`:** a thread pool and an MPMC channel are 60 lines of `std`; not worth a dependency yet.
- **Re-planning every frame:** an `O(R^2)` scan and sort per frame for nothing; the area changes only when the camera's chunk changes.
- **A time-based upload budget:** adapts to the device, but is not deterministic in tests; adopt it if measurement shows uploads of very different cost.
- **Uploading from worker threads (transfer queue, staging):** would pull Vulkan into streaming or threads into the renderer trait. Staging belongs inside the renderer later, behind the same `set_chunk_mesh`.
- **Vertical streaming:** all content is in one layer. It comes with terrain that spans layers (caves, mountains).
- **Square area:** more chunks and the allocation problem above.

## Risks
- **Switch trigger:** the first feature that changes blocks after generation (mining, building, loading saves) or makes generation much slower than meshing breaks the self-contained job. Then move to the two-stage pipeline above.
- `--view-radius` now means a circle: chunk counts differ from the tables in `docs/benchmarks.md`, so old and new frame times are not directly comparable.
- The chunks at the edge of the loaded area have their outer walls hidden by neighbours that are never drawn. A camera that outruns the streaming can see into them through the gap. Acceptable at 40 blocks/s; fog will hide the edge later.
- Chunks appear one by one at startup and at the edge (pop-in), nearest first.
- A worker that panics loses its job; the streamer reports it as an error (it checks whether a worker thread has finished), but the chunk it held would otherwise never load.
- The upload cost of `set_chunk_mesh` (one `vkAllocateMemory` and a copy per mesh) is not measured yet; the budget of 4 is a guess until step 7.
- Each loaded mesh is still one allocation; near the maximum radius the count plus retired buffers comes close to 4096 (decision 0007 risk).

## Implementation plan

Every step ends with `cargo fmt`, `cargo clippy -- -D warnings`, `cargo test` passing and an independent review before the commit. Constants get names and doc comments (no magic values).

### Ownership and parallelism
Steps 1 to 6 touch one tightly coupled module (`streaming/mod.rs` declares all the others) and are done by **one writer** (`world-dev`), one commit per step. The reviewer reviews step N while the writer does step N + 1. Step 7 is the `tester`. No two writers ever share a file.

### Step 1. Pure area functions: `streaming/area.rs`
Files: `crates/vox-game/src/streaming/mod.rs` (new, module declarations only), `crates/vox-game/src/streaming/area.rs` (new), `crates/vox-game/src/main.rs` (one line, `mod streaming;`).

Interface:
```rust
/// The layer all terrain lies in (decision 0006).
pub const TERRAIN_LAYER: i32 = 0;
/// Extra chunks beyond the view radius before a chunk is unloaded.
pub const UNLOAD_MARGIN: i32 = 1;

/// The chunk that contains a world position in blocks (floor division; `as` saturates,
/// NaN goes to 0).
pub fn chunk_of(position: [f32; 3]) -> ChunkPos;
/// Columns within `radius` of `centre` (x and z only, `y = TERRAIN_LAYER`), nearest first;
/// ties ordered by (z, x) so the order is deterministic. A negative radius counts as 0.
pub fn wanted(centre: ChunkPos, radius: i32) -> Vec<ChunkPos>;
/// Whether a loaded chunk at `pos` stays loaded (within `radius + UNLOAD_MARGIN`).
pub fn is_kept(centre: ChunkPos, pos: ChunkPos, radius: i32) -> bool;
```
Distances are computed in `i64` so no radius overflows.

Done when tests show: radius 0 gives only the centre; radius 8 gives 197 positions, all unique, all within the circle; the order is non-decreasing in distance and the same on every call; `is_kept` is true at `R + 1` and false at `R + 2` on an axis; `chunk_of` maps 0.0, 31.9, 32.0, -0.1 and -32.0 to chunks 0, 0, 1, -1 and -1; every wanted position is kept (load range inside keep range).

### Step 2. The job: `streaming/job.rs`
Files: `crates/vox-game/src/streaming/job.rs` (new). `world_loader.rs` is not touched yet.

Interface:
```rust
/// Generates the chunk at `pos` and its four horizontal neighbours, and meshes it into
/// `scratch`. Returns an exact-size copy, or an empty mesh when nothing is visible.
pub fn build_chunk_mesh(seed: u32, pos: ChunkPos, scratch: &mut ChunkMesh) -> ChunkMesh;
```
The bedrock chunk and the neighbour helper move here (copied now, the originals are deleted in step 6).

Done when tests show: for every position of a 3 x 3 area, the mesh equals the one built by hand from a `World` holding the chunk and its four neighbours over bedrock; no mesh has a downward face; the centre mesh has fewer quads than the same chunk meshed alone.

### Step 3. The tracker: `streaming/tracker.rs`
Files: `crates/vox-game/src/streaming/tracker.rs` (new). Pure state machine, no threads, no clock.

Interface:
```rust
pub struct ChunkTracker { /* radius, cap, centre, queue: VecDeque, in_flight: HashSet,
                              ready: VecDeque<(ChunkPos, ChunkMesh)>, loaded: HashSet */ }

pub struct StreamStats {
    /// Chunks with a mesh in the renderer, or known to have nothing to draw.
    pub loaded: usize,
    /// Chunks wanted but not loaded yet: queued, in flight or waiting for upload.
    pub queued: usize,
}

impl ChunkTracker {
    pub fn new(radius: i32, cap: usize) -> Self;
    /// Moves the centre; nothing happens if it did not change. Forgets every tracked
    /// position outside the keep range and returns the loaded ones among them (to remove
    /// from the renderer). Rebuilds the queue from `area::wanted` minus everything tracked.
    pub fn set_centre(&mut self, centre: ChunkPos) -> Vec<ChunkPos>;
    /// Takes positions from the front of the queue while in-flight plus ready is below the
    /// cap, and marks them in flight.
    pub fn next_jobs(&mut self) -> Vec<ChunkPos>;
    /// A worker finished `pos`. Dropped if `pos` is not in flight or no longer kept.
    pub fn finish(&mut self, pos: ChunkPos, mesh: ChunkMesh);
    /// Up to `budget` non-empty meshes to upload, marked loaded. Empty ones are marked
    /// loaded on the way and do not count against the budget.
    pub fn take_uploads(&mut self, budget: usize) -> Vec<(ChunkPos, ChunkMesh)>;
    pub fn stats(&self) -> StreamStats;
}
```
The one rule to keep in mind: **a tracked position is forgotten only when it leaves the keep range.**

Done when deterministic tests (fake meshes, no workers) show: the first jobs are nearest first; never more than `cap` in flight plus ready; a stale result (moved away) is dropped; a result for a position that left and came back while in flight is accepted once; a move by one chunk unloads nothing (hysteresis) and a move by `R + 2` unloads everything that was loaded; positions are never queued twice; `take_uploads` respects the budget and skips empty meshes; `loaded + queued` equals the wanted count once the centre is fixed; a full simulated run (finish every job, upload everything) ends with `loaded` equal to `area::wanted` and `queued == 0`.

### Step 4. The worker pool: `streaming/workers.rs`
Files: `crates/vox-game/src/streaming/workers.rs` (new).

Interface:
```rust
pub struct Workers { /* job Sender, result Receiver, JoinHandles */ }

impl Workers {
    /// Starts `count` (at least 1) threads, each with its own scratch mesh, that run
    /// `work` on every position sent and report the result and how long it took.
    pub fn spawn(count: usize, work: impl Fn(ChunkPos, &mut ChunkMesh) -> ChunkMesh + Send + Sync + 'static) -> Self;
    pub fn send(&self, pos: ChunkPos) -> Result<(), StreamError>;
    /// A finished job, if any, without blocking.
    pub fn try_recv(&self) -> Result<Option<JobResult>, StreamError>;
    /// An error if any worker thread has stopped (it panicked).
    pub fn check(&self) -> Result<(), StreamError>;
}
// Drop: closes the job channel, then joins every thread (waits for at most one job each).
```
`StreamError` is a small error enum (`WorkerStopped`) implementing `Error` and `Display`.

Done when tests show: every sent position comes back exactly once (order free, collected with a timeout); a work function that panics is reported by `check`; dropping the pool returns (threads joined) with jobs still queued.

### Step 5. The streamer: `streaming/mod.rs`
Files: `crates/vox-game/src/streaming/mod.rs`.

Interface:
```rust
pub struct ChunkStreamer { tracker: ChunkTracker, workers: Workers, /* job time sums */ }

/// What the renderer has to do this frame.
pub struct Changes {
    pub removed: Vec<ChunkPos>,
    pub uploads: Vec<(ChunkPos, ChunkMesh)>,
}

impl ChunkStreamer {
    pub fn new(seed: u32, radius: i32) -> Self;
    /// One frame of streaming: move the centre, collect finished jobs, send new ones,
    /// and hand out at most `MAX_UPLOADS_PER_FRAME` meshes.
    pub fn update(&mut self, camera_chunk: ChunkPos) -> Result<Changes, StreamError>;
    pub fn stats(&self) -> StreamStats;
    /// Average job time so far, for the fill log line.
    pub fn average_job_time(&self) -> Option<Duration>;
}
```
Constants: `JOBS_PER_WORKER = 2`, `MAX_UPLOADS_PER_FRAME = 4` (provisional), worker count `available_parallelism - 1`, at least 1.

Done when a test with real workers and radius 2 calls `update` in a loop (1 ms sleep, 10 s timeout) until `queued == 0`, then checks that the uploaded positions are exactly the non-empty ones of `area::wanted`, each mesh equals `build_chunk_mesh`, and no frame had more than the budget; and a second phase moves the centre by `R + 2` and checks that every earlier position is reported in `removed`.

### Step 6. Wire into the game
Files: `crates/vox-game/src/main.rs`, `camera.rs` (add `pub fn position(&self) -> Vec3`), `hud.rs` (`lines` takes `&StreamStats`, one new line; check `LABEL_WIDTH` still covers the longest label), `cli.rs` (doc of `--view-radius` and `MAX_VIEW_RADIUS`: a circle now), delete `world_loader.rs`.

- `App::start` creates the `ChunkStreamer` instead of calling `load_area`; no meshes are uploaded there.
- In `redraw`, after `update_camera` and before `render_frame`: `update(chunk_of(camera.position()))`, then every `remove_chunk_mesh`, then every `set_chunk_mesh`. An error ends the game, as a failed upload at startup does today.
- While the queue is not empty for the first time, sum the upload time; when it first empties, print one line: chunks, fill time from start, average job time, average upload time, workers.
- `refresh_hud` passes the stream stats.

Done when the game starts without the blocking load, the world follows the camera, and the log line appears. One window test (with `--background`, hidden console, no topmost), reported as such.

### Step 7. Measure and tune (`tester`)
Files: `docs/benchmarks.md` only (plus temporary local instrumentation that is not committed).

Measure on the development machine and, when available, the low-end device:
1. Cost of one `set_chunk_mesh` (median and max over the fill), from the step 6 log line. Set `MAX_UPLOADS_PER_FRAME` so the uploads of one frame stay under about 1 ms on the low-end device.
2. Average job time against the prediction of about 350 us (5 generations and one mesh).
3. Fill time at radius 8 and 16 against today's blocking load (about 50 ms at radius 8).
4. Frame time and 1% low while flying straight at full speed for 10 s, against a static camera, for budgets 1, 2, 4 and 8. Stutter shows as a lower 1% low.
5. The highest number of loaded meshes at radius 30, against the 4096 allocation limit.

Done when the table is in `docs/benchmarks.md` and the budget constant carries the measured reason in its doc comment.
