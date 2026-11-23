# Benchmarks

Baseline of the chunk mesher and the terrain generator, measured with `criterion`.
The point is to have numbers to compare against before any optimization (see
`docs/performance-notes.md`: we optimize by measurement). Re-run on the same machine after
a change and compare against the table below.

## How to run

```powershell
cargo bench -p vox-world                 # all cases, release-like `bench` profile
cargo bench -p vox-world --bench mesh    # same, without the (ignored) unit tests
cargo bench -p vox-world -- mesh/solid   # one case, filtered by name
```

The sizes of the meshes are printed once at the start, before the timings. Criterion keeps
the previous run in `target/criterion` and prints the change against it on the next run.
Benchmarks are not part of CI (CI runs fmt, clippy and tests only). Close other heavy
programs while measuring.

The bench profile inherits the release profile from the workspace `Cargo.toml`:
`lto = "thin"`, `codegen-units = 1`, default `opt-level = 3`, no `target-cpu` flags.

## What each case measures

All cases use one 32x32x32 chunk (32,768 blocks). The input chunk is built outside the timed
closure; inputs and outputs go through `std::hint::black_box`. Source:
`crates/vox-world/benches/mesh.rs`.

| Case | Input | What it tells us |
|---|---|---|
| `mesh/empty` | all air | Fixed cost of walking the block array: iterator, `get`, `is_air`. |
| `mesh/single_block` | one block in the middle | Same walk plus six emitted faces; shows the cost of a nearly empty mesh. |
| `mesh/terrain_seed_1` | `generate_terrain(1, &blocks)` | The realistic case: hills, air above, solid below. |
| `mesh/solid` | every block solid | Maximum number of neighbour checks with almost no output (only the outer shell). |
| `mesh/checkerboard` | blocks where `x + y + z` is even | Worst case: no two solid blocks touch, almost every face is emitted. |
| `terrain/generate_seed_1` | seed 1 | Cost of generating the chunk (noise, column fill, allocating the chunk). |
| `mesh_reused/terrain_seed_1`, `solid`, `checkerboard` | same chunks | `build_mesh_into` on one warmed-up mesh: the steady state of a worker, without allocation. |
| `mesh_neighbours/terrain_seed_1` | the origin terrain chunk with all six neighbouring terrain chunks present | `build_mesh_with_neighbours_into` on a warmed-up mesh: the cost of looking into neighbouring chunks, to compare with `mesh_reused/terrain_seed_1`. |
| `terrain/generate_chunk_far` | `generate_chunk(1, ChunkPos::new(3, 0, -2), &blocks)` | Chunk generation away from the origin, with a negative coordinate. |

Throughput is set to the number of blocks in the chunk, so "blocks per second" counts air
blocks too; it is a way to compare cases, not a count of solid blocks processed.

## Machine

| | |
|---|---|
| CPU | AMD Ryzen 7 7700 8-Core Processor (8 cores, 16 threads, 3.8 GHz base) |
| RAM | 47.1 GB |
| OS | Windows 11 Pro 10.0.26300 |
| Compiler | rustc 1.99.1 (58cf8f9c3 2026-10-13), cargo 1.99.1 |
| Criterion | 0.7.0 (0.8.2 needs Rust 1.86, the workspace minimum is 1.85) |
| Power plan | not recorded |

## Baseline

Time per chunk is the criterion median of the typical run out of three consecutive runs
(the median of the three medians). The spread column gives the lowest and highest median over
the three runs. Blocks per second is `32,768 / time`.

| Case | Median | Spread over 3 runs | Blocks/s | Quads | Vertices | Mesh bytes | Vertex cap/len | Index cap/len |
|---|---|---|---|---|---|---|---|---|
| `mesh/empty` | 10.96 us | 10.87 - 11.11 us | 2.99 G | 0 | 0 | 0 | 0 / 0 | 0 / 0 |
| `mesh/single_block` | 11.62 us | 11.43 - 12.39 us | 2.82 G | 6 | 24 | 288 | 32 / 24 | 48 / 36 |
| `mesh/terrain_seed_1` | 220.2 us | 206.6 - 227.6 us | 149 M | 4,848 | 19,392 | 232,704 | 32,768 / 19,392 | 49,152 / 29,088 |
| `mesh/solid` | 363.8 us | 344.6 - 366.9 us | 90.1 M | 6,144 | 24,576 | 294,912 | 32,768 / 24,576 | 49,152 / 36,864 |
| `mesh/checkerboard` | 1.97 ms | 1.91 - 2.82 ms | 16.6 M | 98,304 | 393,216 | 4,718,592 | 524,288 / 393,216 | 786,432 / 589,824 |
| `terrain/generate_seed_1` | 35.72 us | 35.70 - 35.89 us | 917 M | - | - | - | - | - |

Mesh bytes are vertices x 6 plus indices x 4. Capacity against length is read right after
`build_mesh` returns: `Vec` doubling leaves 25 to 41 percent of the reserved space unused
(terrain: 19,392 of 32,768 vertices, 59 percent used; the used mesh is 232,704 bytes, the
reserved memory 393,216 bytes).

### Noise

- Criterion confidence intervals of a single run are narrow (about 1 to 3 percent wide).
- Between runs the medians moved more than the intervals suggest: `mesh/terrain_seed_1`
  206.6, 220.2 and 227.6 us (about 10 percent from lowest to highest), `mesh/solid` 344.6 to
  366.9 us (6 percent). Criterion itself flagged these runs as changed (p < 0.05). A difference
  under about 10 percent between two runs is therefore not a real change.
- `mesh/checkerboard` is the noisiest: 1.97, 1.91 and then 2.82 ms in the third run (47 percent
  higher) with 5 to 10 percent outliers per run. We do not know the cause (allocator behaviour
  for multi-megabyte vectors, CPU clocks, background load). Treat the 1.9 to 2.0 ms figure as
  the typical one and re-run before drawing conclusions from this case.
- `mesh/empty`, `mesh/single_block` and `terrain/generate_seed_1` are stable within 2 to 7 percent.

## Reading the numbers

Fitting two unknowns to the `solid` and `checkerboard` cases (both subtract the 11 us walk)
gives roughly 1.2 ns per neighbour check and 19 ns per emitted quad. For the terrain chunk
this predicts about 118 us of neighbour checks, 91 us of quad emission and 11 us of walk,
which sums to the measured 220 us. This is a model fitted to three measurements, not a
profile; it says where the time probably goes, not by how much to the percent.

**The model was tested and its main claim did not hold** (see the first experiment below):
making the neighbour check cheaper did not speed up the case that consists almost only of
neighbour checks. Treat the 54 percent figure as wrong. The fit mixed the neighbour test with
everything else that scales with the number of solid blocks (reading the block, branching
per face), so the neighbour check alone is much cheaper than 1.2 ns, probably because the
compiler already removes most of its bounds work. Only a profiler can say where the time goes.

## Experiments

Each experiment is run with three benchmark runs before and three after on the same machine,
and the change is kept only if the whole range of the new runs lies outside the range of the old
ones. This section records the ones that were tried, including those that were rejected, so that
nobody repeats them without a new reason.

### 1. Neighbour test by flat index: rejected

Replaced the per-face `LocalPos::offset` (three `checked_add_signed` calls and a bounds check) by
index arithmetic on the flat block array: the neighbour is `index +- 1`, `+- 32` or `+- 1024`,
with a border test on the coordinate. The mesh was identical, vertex for vertex (a test compared
it with a plain reference mesher on 11 chunks), and the code was about 30 lines longer.

| Case | Before (3 runs, median) | After (3 runs, median) | Verdict |
|---|---|---|---|
| `mesh/terrain_seed_1` | 221.6 - 232.0 us | 194.7 - 217.1 us | faster by 2 to 12 percent, wide spread |
| `mesh/solid` | 356.9 - 372.5 us | 360.1 - 388.5 us | no gain, probably slightly slower |
| `mesh/checkerboard` | 1.94 - 2.09 ms | 2.02 - 2.79 ms | no gain (one noisy outlier) |

The decision rule needed both the terrain and the solid case to be clearly faster. `mesh/solid`
is almost nothing but neighbour checks, and it did not improve, so the cheap check is not where
the time goes. Not kept: the extra code buys nothing that can be measured reliably.

### 2. Reusing the output buffers: kept

`build_mesh_into(chunk, &mut mesh)` clears the mesh (`Vec::clear` keeps the capacity) and fills
it; `build_mesh` is a wrapper around it with a fresh `ChunkMesh`. The new cases `mesh_reused/*`
call it on one mesh created and warmed up outside the timed closure, which is what a worker
thread that meshes chunks all day does. Compared inside the same three runs (medians of each run,
range lowest - highest, runs on 2026-11-21; this session the machine ran 5 to 10 percent slower
than at the baseline, which is why `mesh/*` here is higher than in the baseline table):

| Case | Fresh `mesh/X` (3 runs) | Reused `mesh_reused/X` (3 runs) | Change at the median |
|---|---|---|---|
| terrain_seed_1 | 249.1 / 238.1 / 224.9 us (224.9 - 249.1) | 212.1 / 210.1 / 212.4 us (210.1 - 212.4) | -11 percent, ranges do not overlap |
| solid | 383.6 / 394.7 / 385.8 us (383.6 - 394.7) | 360.4 / 359.6 / 359.0 us (359.0 - 360.4) | -7 percent, ranges do not overlap |
| checkerboard | 2.74 / 2.56 / 2.47 ms (2.47 - 2.74) | 0.92 / 0.93 / 0.98 ms (0.92 - 0.98) | -63 percent, ranges do not overlap |

The conductor repeated the measurement independently in three more runs of the same session,
with the same direction: terrain 234 / 252 / 235 us fresh against 212 / 210 / 211 us reused (10 to
16 percent faster, ranges apart), solid 397 / 399 / 394 against 366 / 360 / 354 us (8 to 10 percent),
checkerboard 2.04 / 2.86 / 2.02 ms against 0.98 / 1.01 / 0.92 ms (52 to 65 percent).

The terrain case passes the rule (more than 10 percent and outside the noise), though only just:
11 percent at the median, and the fresh case had a wide spread (225 to 252 us across the two
measurements). The fresh figure also differs between sessions: the baseline table has 209 to 232 us
for the same case, so against the earlier fresh figures the reused terrain case (about 211 us) would
look like no gain at all. Read the terrain gain as modest and the comparison as valid only within
one session. The reused runs were very stable (within 1 percent). The big winner is the checkerboard: with 4.7 MB of output,
allocating, growing and page-faulting fresh vectors was most of its time. The gain is the cost
of growing and touching new memory, so it only exists for a caller that keeps its buffers; a
caller that needs a fresh mesh each time gets nothing. Next steps that follow from this: reserve
a capacity estimate for the first call, and note that reused meshes of different sizes keep
the largest capacity they ever reached.

### 3. Chunks next to each other (2026-11-21)

`generate_chunk` samples the noise at world coordinates and `build_mesh_with_neighbours_into`
reads the neighbouring chunk for faces on the border (decision 0006). `build_mesh_into` is now
the same function called with no neighbours. Medians of each of three consecutive runs on a
quiet machine, before and after the change (same session; the machine was 5 to 15 percent
slower than at the baseline above, and drifted during the last run, see below):

| Case | Before (3 runs) | After (3 runs) |
|---|---|---|
| `mesh/empty` | 11.91 / 12.08 / 12.37 us | 11.56 / 12.24 / 13.74 us |
| `mesh/single_block` | 11.63 / 11.96 / 12.07 us | 11.96 / 13.10 / 14.19 us |
| `mesh/terrain_seed_1` | 254.3 / 262.1 / 262.8 us | 217.9 / 242.2 / 251.6 us |
| `mesh/solid` | 399.2 / 409.3 / 413.1 us | 407.0 / 429.9 / 442.1 us |
| `mesh/checkerboard` | 2.05 / 2.28 / 2.87 ms | 1.96 / 2.47 / 2.98 ms |
| `mesh_reused/terrain_seed_1` | 224.2 / 226.5 / 230.6 us | 206.9 / 208.9 / 233.5 us |
| `mesh_reused/solid` | 379.6 / 385.0 / 387.1 us | 353.5 / 370.6 / 375.7 us |
| `mesh_reused/checkerboard` | 0.98 / 1.04 / 1.34 ms | 0.95 / 0.96 / 1.18 ms |
| `mesh_neighbours/terrain_seed_1` | new case | 197.8 / 222.5 / 247.6 us (1,642 quads) |
| `terrain/generate_seed_1` | 38.7 / 39.1 / 40.3 us | 46.3 / 48.2 / 49.2 us |
| `terrain/generate_chunk_far` | new case | 46.1 / 49.8 / 51.9 us |

- **The existing mesher cases are not distinguishable from noise.** The `mesh_reused/*` ranges
  overlap or lie lower. `mesh/single_block` and `mesh/solid` look a little higher after (13.1 - 14.2
  against 11.6 - 12.1 us, and 407 - 442 against 399 - 413 us), but `mesh/empty`, which runs none of
  the changed code, also moved up in the same runs (13.7 us in the last run), and the interleaved
  `mesh_reused/solid`, the same work without allocation, was lower. With three runs and 5 to 15 percent
  of drift this cannot be settled either way. The common path (a neighbour inside the chunk) is the
  same code as before.
- **Neighbour-aware meshing costs nothing that can be measured here.** `mesh_neighbours/terrain_seed_1`
  (198 - 248 us) lies inside the range of `mesh_reused/terrain_seed_1` (207 - 234 us). The runs of
  the neighbour case are wide (the ranges are 25 percent apart), so a difference of a few percent
  would not show. Note that the two cases do not produce the same output: with its neighbours the
  chunk has 1,642 quads instead of 4,848 (its sides are mostly hidden by the stone next to it), so
  there is less to write while more border faces take the slower path. The two effects may cancel.
- **Terrain generation was about 20 percent slower right after this change** (38.7 - 40.3 to
  46.3 - 49.2 us, ranges do not overlap). The cause was a hypothesis until experiment 4 below
  measured it: the per-block work in `fill_column`, not the noise. A first version of the noise with
  `div_euclid` and `rem_euclid` took 61 - 74 us, so the shift and mask (valid because every cell
  size is a power of two, a const assertion guards this) were kept. The numbers in the table above
  are from before experiment 4; see there for the final ones.

### 4. Filling a column by layers: kept

`fill_column` first decided the block of every height with a four-way `block_for`, a saturating add
and a `LocalPos::new`. It now works out the layer boundaries once per column (stone below
`stone_end`, dirt up to `dirt_end`, one grass block), clamped to the chunk, and fills each layer with
a plain loop. Output is identical (all terrain tests and pinned numbers pass unchanged). Three runs
before and after in the same session (medians of each run, range lowest - highest):

| Case | Before (3 runs) | After (3 runs) | Verdict |
|---|---|---|---|
| `terrain/generate_seed_1` | 45.3 / 44.2 / 44.9 us (44.2 - 45.3) | 29.3 / 29.4 / 26.8 us (26.8 - 29.4) | 35 to 40 percent faster, ranges apart |
| `terrain/generate_chunk_far` | 44.8 / 45.8 / 47.8 us (44.8 - 47.8) | 29.2 / 30.1 / 27.8 us (27.8 - 30.1) | 33 to 42 percent faster, ranges apart |

Both cases pass the keep rule. The new figures are also lower than the 38.7 - 40.3 us of before the
multi-chunk change (measured an hour earlier, so only roughly comparable): the old code branched
per block as well.

## What this does and does not tell us

- One machine, one CPU (a desktop Ryzen with a large cache). An old low-end device, which is
  the real target, will behave differently, especially for the memory-heavy checkerboard case.
- Single thread. Chunk meshing is meant to run on worker threads; contention for memory
  bandwidth and for the allocator is not measured.
- One chunk size (32) and one terrain seed. Other seeds change the number of faces by tens of
  percent, not by orders of magnitude.
- `mesh/*` and `mesh_reused/*` mesh a chunk without neighbours (outside is air). Only
  `mesh_neighbours/terrain_seed_1` looks into neighbouring chunks, and only for one terrain
  chunk surrounded by terrain. A chunk at a different height, beside air or beside a chunk
  that is not loaded, takes other paths.
- The vertices are not uploaded to a GPU; the benchmark ends at the `Vec`s. Page faults and
  allocator work for the fresh vectors are included, because every iteration allocates a new mesh.
- The terrain generator benchmark includes allocating and zeroing the chunk (64 KiB).
- Noise between runs is around 5 to 10 percent, and up to about 45 percent for the checkerboard.

## Frame time against chunk count (2026-11-24)

Measured with `--view-radius N` (release build, window 1280x720 default, static camera at the start
position, nothing culled: every chunk with a mesh is drawn each frame, frame limit off, 7 s per run,
RTX 4070 SUPER, one run each, so treat as rough). `gpu` is the upper-bound figure of the metrics.

| Radius | Chunks | Quads | FPS | 1% low | CPU work avg | GPU avg |
|---|---|---|---|---|---|---|
| 2 | 25 | 52 k | 3186 | 365 | 0.22 ms | 0.09 ms |
| 4 | 81 | 151 k | 1515 | 294 | 0.56 ms | 0.49 ms |
| 8 | 289 | 507 k | 518 | 160 | 1.68 ms | 1.86 ms |
| 16 | 1089 | 1.84 M | 163 | 97 | 5.91 ms | 6.11 ms |
| 30 | 3721 | 6.18 M | 52 | 31 | 18.97 ms | 19.35 ms |

Findings: the frame time grows linearly with the chunk count (about 5 us per chunk, about
3 ns per quad), so the cost is per drawn triangle, not a fixed overhead. "CPU work" tracks
"GPU" because the game waits for the GPU; the CPU is not the bottleneck. Hence the way to
more frames is to draw less: cull chunks outside the view, drop faces that cannot be seen,
and reduce detail with distance (see docs/performance-notes.md).

### After chunk frustum culling (same setup, same day)

| Radius | Chunks | FPS before | FPS after | GPU before | GPU after |
|---|---|---|---|---|---|
| 8 | 289 | 518 | 1051 | 1.86 ms | 0.79 ms |
| 16 | 1089 | 163 | 448 | 6.11 ms | 2.22 ms |
| 30 | 3721 | 52 | 168 | 19.35 ms | 5.89 ms |

Note: the tables above were measured with the square area that existed before chunk streaming; `--view-radius` now means a circle (about 79 percent as many chunks), so later figures are not directly comparable.

At radius 16 the start view draws 365 of 1089 chunks. The camera looks along one direction,
so a view that sees more of the area gains less; a flight around the world is not measured yet.
The remaining cost is still per drawn triangle (1.2 M triangles at radius 16): the next steps
are skipping faces that point away from the camera and reducing detail with distance.

## Streaming while flying (2026-11-24)

`--fly 8` flies the camera sideways at 160 blocks per second (5 chunks per second, 4 times the
player's speed) for 8 seconds, release build, upload budget 4 per frame, one run per radius.
The first fill is the same as standing still: 44 ms (radius 8), 193 ms (16), 1.5 s (30).

| Radius | Flying: FPS | 1% low | 0.1% low | Worst frame | Standing: FPS | 1% low |
|---|---|---|---|---|---|---|
| 8 | 1809 | 328 | 191 | 12.7 ms | 2059 | 365 |
| 16 | 684 | 203 | 145 | 10.1 ms | 745 | 236 |
| 30 | 285 | 78 | 71 | 14.2 ms | 307 | 141 |

Streaming costs 7 to 10 percent of the frame rate while flying fast, and no frame is longer than
14 ms, so the budget of 4 uploads per frame does not cause stutter at these speeds. The
average job is about 0.5 to 0.6 ms on one of 15 workers and an upload about 0.11 ms. Not yet
measured: smaller budgets, a slower machine, and how far the camera can outrun the loading.

## Greedy meshing (2026-11-24)

`build_mesh_with_neighbours_into` now merges visible faces of the same block and direction in
one plane into rectangles (performance notes, item 8). The keep rule set beforehand: keep it
only if it gives fewer quads (cheaper to render) and is not more than about 2x slower to mesh.
Three consecutive runs of `cargo bench -p vox-world --bench mesh` for each version, same
machine, nothing else running; the table shows the median of the three run medians and the
lowest and highest run median. The old numbers come from the same binary built before the
change, run just before.

| Case | Per-face mesher | Greedy mesher | Quads before / after |
|---|---|---|---|
| `mesh/terrain_seed_1` | 262.3 us (249.9 - 322.5) | 185.4 us (182.6 - 186.4) | 4,848 / 760 |
| `mesh_reused/terrain_seed_1` | 224.9 us (220.8 - 294.7) | 187.5 us (183.6 - 191.4) | 4,848 / 760 |
| `mesh_neighbours/terrain_seed_1` (six neighbours) | 209.0 us (205.0 - 266.5) | 180.7 us (179.0 - 192.7) | 1,642 / 610 |
| `mesh/slab` (stone up to y = 8, new case) | not run | 108.2 us (107.8 - 109.4) | 3,072 / 6 |
| `mesh/solid` | 387.9 us (387.6 - 495.2) | 147.8 us (146.6 - 148.4) | 6,144 / 6 |
| `mesh_reused/solid` | 370.0 us (363.8 - 497.3) | 145.7 us (145.7 - 148.5) | 6,144 / 6 |
| `mesh/checkerboard` | 1.97 ms (1.96 - 2.32) | 2.55 ms (2.53 - 2.56) | 98,304 / 98,304 |
| `mesh_reused/checkerboard` | 0.93 ms (0.93 - 1.26) | 1.67 ms (1.65 - 1.74) | 98,304 / 98,304 |
| `mesh/empty` | 15.1 us (14.9 - 21.9) | 1.39 us (1.39 - 1.40) | 0 / 0 |
| `mesh/single_block` | 15.8 us (15.5 - 22.7) | 6.72 us (6.67 - 6.76) | 6 / 6 |
| `terrain/generate_seed_1` | 28.1 us | 27.7 us | unchanged |

- Terrain: 6.4 times fewer quads (4,848 to 760; with neighbours 1,642 to 610, 2.7 times) and
  `mesh/terrain_seed_1` is 29 percent faster to build (262.3 to 185.4 us). The reused-buffer
  variant, which is the one a worker uses, is 17 percent faster (224.9 to 187.5 us). Mesh bytes
  drop from 232,704 to 36,480. The quad counts are exact; the old runs had one slow run each
  (10 to 25 percent above the others), so the size of the time gain is not precise, and the
  reused figure is the more honest one. The rows for `mesh/slab`, `mesh_reused/solid` and
  `mesh_neighbours` come from the earlier greedy runs, before the empty-chunk early-out below;
  they were not re-run.
- Keep rule: fewer quads everywhere or equal (checkerboard cannot merge). No case is above
  2x the time of the per-face mesher now. The slowest is the checkerboard, where nothing
  merges: `mesh_reused/checkerboard` is 1.80 times (0.93 to 1.67 ms; the first greedy runs
  gave 1.65 times) and `mesh/checkerboard` 1.3 times (1.97 to 2.55 ms, not re-run). Greedy is
  kept.
- Empty and sparse chunks: the greedy mesher first made an empty chunk 6 times slower
  (92 us against 15 us) because it filled the mask for all 6 x 32 slices (196,608 cells).
  `solid_slices` now makes one linear pass that records which slices hold a solid block; an
  all-air chunk returns right after it, and slices without a block are skipped. Empty is
  1.39 us (11 times faster than the per-face mesher, 15.1 us) and a single block 6.7 us
  (2.3 times faster than 15.8 us). Chunks full of blocks pay the extra pass (about a microsecond
  or two, inside the noise of the solid and terrain figures).
- What made the first version too slow: it built a `LocalPos` for every cell and called the
  visibility test through the position (820 us for terrain, 3.3 times the old mesher,
  540 us for an empty chunk). Reading the block array directly with strides, walking the
  slice in memory order (the inner mask axis is the one with the smaller stride), and using
  the position-based test only on the border slice, gave the numbers above.
- The effect on frame time is not measured here; it needs the renderer (`--fly` or the
  stationary benchmark) and will be recorded when someone runs it.

### Frame time with greedy meshing (2026-11-24, release, standing still, same setup)

Command line for each row: `vox-game.exe --background --view-radius N` (release build, default
window size, 7 s, camera standing still).

| Radius | FPS before | FPS after | GPU before | GPU after | Triangles at radius 16 |
|---|---|---|---|---|---|
| 8 | 2059 | 3562 | 0.31 ms | 0.09 ms | |
| 16 | 745 | 1761 | 1.23 ms | 0.42 ms | 1.22 M to 0.26 M |
| 30 | 307 | 674 | 3.20 ms | 1.41 ms | |

The picture is identical to the per-face mesh (checked on a screenshot), as the coverage tests promise.
