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
- Texture layers per quad (2026-11-24, step S6a of decision 0013): `build_mesh*` now take a
  `FaceTextures` and look the layer up once per quad. One run of `--quick`, compared with the
  figures above: within noise (equal or slightly faster, the machine varied between sessions).
  `mesh/terrain_seed_1` 171.5 us (185.4), `mesh_reused/terrain_seed_1` 172.1 us (187.5),
  `mesh_reused/solid` 138.2 us (145.7), `mesh_reused/checkerboard` 1.62 ms (1.67),
  `mesh_neighbours/terrain_seed_1` 167.5 us (180.7). No same-session baseline was built.

### Frame time with greedy meshing (2026-11-24, release, standing still, same setup)

Command line for each row: `vox-game.exe --background --view-radius N` (release build, default
window size, 7 s, camera standing still).

| Radius | FPS before | FPS after | GPU before | GPU after | Triangles at radius 16 |
|---|---|---|---|---|---|
| 8 | 2059 | 3562 | 0.31 ms | 0.09 ms | |
| 16 | 745 | 1761 | 1.23 ms | 0.42 ms | 1.22 M to 0.26 M |
| 30 | 307 | 674 | 3.20 ms | 1.41 ms | |

The picture is identical to the per-face mesh (checked on a screenshot), as the coverage tests promise.

## Textured blocks (2026-11-24, step S7 of decision 0013)

The chunk shaders now sample the block texture array (one `texture()` call per fragment, UVs
derived from the position in the vertex shader) instead of a per-block colour. Command line:
`vox-game.exe --background --view-radius N` (release build, default window size, camera standing
still at the start position, 8 s per run, RTX 4070 SUPER). "Before" is `main` at the end of step S6
(palette colours), built from the same sources; before and after runs alternated, three each.
The figures are the run summaries' `gpu` (upper bound) and FPS.

| Radius | GPU before (3 runs) | GPU after (3 runs) | FPS before | FPS after |
|---|---|---|---|---|
| 8 (default) | 0.08, 0.09, 0.09 ms | 0.09, 0.09, 0.09 ms | 3596, 3545, 3545 | 3577, 3516, 3467 |
| 16 | 0.43, 0.43, 0.43 ms | 0.42, 0.42, 0.42 ms | 1746, 1735, 1740 | 1748, 1730, 1749 |

No measurable cost on this GPU: the differences are within the run-to-run spread, and the
summary prints the GPU time to 0.01 ms. The textures are 16x16 with five layers, so
they stay in the texture cache; a slow integrated GPU and larger packs (256x256 tiles) are not
measured yet, and distant terrain samples without mipmaps until step S8 (measured below).

### Mipmaps (2026-11-24, step S8 of decision 0013)

The block textures now have a full mip chain (built on the CPU with a 2x2 box filter),
sampled with `NEAREST` within a level and `LINEAR` between levels. Same command line,
machine and method as above; "before" is `main` at the end of step S7.

| Radius | GPU before (3 runs) | GPU after (3 runs) | FPS before | FPS after |
|---|---|---|---|---|
| 8 (default) | 0.09, 0.09, 0.09 ms | 0.09, 0.09, 0.09 ms | 3546, 3519, 3542 | 3512, 3538, 3579 |
| 16 | 0.42, 0.43, 0.42 ms | 0.43, 0.43, 0.42 ms | 1736, 1752, 1739 | 1737, 1728, 1717 |

No measurable cost on this GPU, although `LINEAR` between levels reads two texels instead of
one. A hypothesis, not measured: distant faces now read small levels, which are friendlier to
the texture cache, and that pays for the second read. The visible gain is the point: distant grass shows its average colour instead of a speckle of
single texels that changes as the camera moves. The chain adds a third to the texture
memory: for five 16x16 layers, 1700 bytes on top of the 5120-byte base level, built once
per upload. The slow-GPU caveat above still applies.

**Choosing the filters.** Screenshots of the same frame with three sampler settings (temporary
overrides, removed afterwards): `NEAREST` within a level with `NEAREST` between levels (NN),
`NEAREST` with `LINEAR` between levels (NL, chosen) and `LINEAR` with `LINEAR` (LL). Two
views at radius 16: the start position looking at the horizon (pitch -0.03) and a camera 3
blocks above the highest terrain (height 27, pitch -0.12). The figure is the share of
terrain pixels (the window below the panel) whose per-channel difference, converted to grey,
is 8 or more out of 255 (Pillow `ImageChops.difference`, then `convert("L")`).

| Views compared | Horizon | Low camera |
|---|---|---|
| NN vs NL | 1.1% | 1.6% |
| NL vs LL | 6.2% | 7.7% |
| NN vs LL | 8.8% | 10.8% |

`LINEAR` within a level softens textures visibly at middle distance; the mip mode changes
little in a still frame. NL is kept because it hides the line where one level hands over to
the next while the camera moves (not measured, the frames are stills).

## Fog and camera-relative drawing (2026-11-24, step S1 of decision 0015)

The chunk fragment shader now fades faces into the sky colour with the radial distance from
the camera (`smoothstep` from 60 percent of the fog end to the fog end, which lies
`(radius - 1.41) * 32` blocks away), chunks are placed relative to the camera, and the far
plane lies 1 block beyond the fog end instead of at 3000 blocks. Command line:
`vox-game.exe --background --view-radius N` standing still at the start position for 8 s, or
with `--fly 8` (flies sideways for 8 s, then quits); release build, default window size, RTX
4070 SUPER. The figures are the run summaries' `gpu` (upper bound, printed to 0.01 ms) and FPS;
runs alternated, three each.

**The fog shader alone.** The same build with and without a pack that replaces `chunk.frag` by
the fog-free one of `main` (`--pack DIR`), so culling and the far plane are identical:

| Radius | GPU without fog | GPU with fog | FPS without fog | FPS with fog |
|---|---|---|---|---|
| 8 | 0.08, 0.08, 0.08 ms | 0.08, 0.08, 0.08 ms | 3713, 3781, 3801 | 3813, 3721, 3746 |
| 12 | 0.22, 0.22, 0.22 ms | 0.23, 0.22, 0.23 ms | 2508, 2539, 2530 | 2551, 2543, 2554 |
| 16 | 0.43, 0.43, 0.43 ms | 0.43, 0.42, 0.43 ms | 1775, 1753, 1736 | 1754, 1762, 1766 |

No measurable cost: at most 0.01 ms at radius 12, the printing resolution (decision 0015 asks
for at most 0.01 ms at 8 and 5 percent at 16). One `length`, a multiply-add, a clamp, the
smoothstep polynomial and a `mix` per fragment.

**The whole step against `main`** (the far plane now culls the fully fogged outer ring: at the
start view, radius 8 draws 59 chunks and 71 k triangles instead of 67 and 80 k):

| Case | GPU before | GPU after | FPS before | FPS after | Worst frame before | Worst frame after |
|---|---|---|---|---|---|---|
| radius 8, standing | 0.09, 0.09, 0.09 ms | 0.08, 0.08, 0.08 ms | 3517, 3403, 3581 | 3655, 3597, 3754 | 9.7, 10.0, 10.6 ms | 8.7, 10.2, 9.7 ms |
| radius 12, standing | 0.22, 0.22, 0.23 ms | 0.22, 0.22, 0.23 ms | 2497, 2491, 2551 | 2497, 2474, 2501 | 9.6, 9.4, 10.1 ms | 9.8, 8.8, 10.0 ms |
| radius 16, standing | 0.42, 0.42, 0.42 ms | 0.43, 0.43, 0.43 ms | 1763, 1749, 1765 | 1749, 1709, 1757 | 9.5, 15.7, 8.4 ms | 9.5, 9.4, 9.1 ms |
| radius 8, `--fly 8` | 0.10, 0.10, 0.10 ms | 0.08, 0.08, 0.08 ms | 3395, 3281, 3398 | 3646, 3548, 3584 | 10.0, 9.9, 11.4 ms | 9.4, 9.7, 10.1 ms |
| radius 12, `--fly 8` | 0.21, 0.21, 0.21 ms | 0.23, 0.23, 0.23 ms | 2181, 2350, 2396 | 2400, 2462, 2466 | 10.1, 10.5, 9.4 ms | 8.6, 9.2, 8.5 ms |
| radius 16, `--fly 8` | 0.46, 0.45, 0.46 ms | 0.44, 0.44, 0.44 ms | 1635, 1663, 1662 | 1617, 1680, 1674 | 9.2, 9.4, 9.7 ms | 8.1, 10.1, 8.8 ms |

**Default radius.** Decision 0015 sets it to the largest of 8, 12 and 16 that keeps at least
2000 FPS standing and 1000 FPS flying here: radius 16 stands at about 1740 FPS, radius 12 at
about 2490 standing and 2440 flying, so `DEFAULT_VIEW_RADIUS` is now 12 (fog from about 203 to
339 blocks, 441 chunks loaded, filled in about 85 ms). On a slow GPU these figures do not hold;
step S9 measures one.

## Lua sandbox: build cost and state size (2026-11-24, step S4 of decision 0014)

`vox-mods` now depends on `mlua` 0.11.6 with Lua 5.4.8 compiled from source (`vendored`). The
game does not link `vox-mods` yet (`cargo tree -p vox-game -i vox-mods` finds nothing), so
the game's executable and its build are unchanged by this step. To see what the game will pay
in step S5, a tiny probe binary outside the workspace was built with the workspace's release
profile (`lto = "thin"`, `codegen-units = 1`) and the same `Cargo.lock`:

- **before:** `vox-mods` at `main` (S1), `main` calls `discover` on a folder;
- **after:** `vox-mods` of this step, `main` calls `discover`, then `ModHost::start`, and
  prints the statuses, the memory of each state and the messages.

Each build starts from an empty target folder with the crates already downloaded
(`cargo build --release --offline`); before and after alternated, three runs each.

| | Clean release build (3 runs) | Executable |
|---|---|---|
| before | 6.53, 6.45, 6.41 s | 333,824 bytes |
| after | 10.49, 10.39, 10.33 s | 743,936 bytes |

So Lua and `mlua` add about 3.9 s to a clean release build of a small binary and about
410 KB to the executable. After the security review the sandbox wraps most library functions
(and adds `memchr` for the plain `find`); one more clean build of the same probe took 11.07 s
(one run) and the executable grew to 813,056 bytes, so about 480 KB over the S1 probe. A clean build of the whole game takes far longer, so the share
will be smaller there; it is measured in S5, when the game links the crate. No CMake and no Lua
installation is needed: `cc` finds the MSVC C compiler that the Rust MSVC toolchain
requires, here without a developer command prompt (`cl` is not on the `PATH` of the shell
that built it). Not checked on a freshly installed machine.

**Memory of a state**, as counted by `mlua`'s allocator (`ModHost::used_memory`, the number
the `MAX_MOD_MEMORY` cap applies to), right after the entry script ran, no collection forced:

| Mod | Lua heap |
|---|---|
| empty entry script | 17,892 bytes (20,496 with the wrapped library of the first review) |
| the `hello` fixture (one `print`, one handler) | 19,073 bytes (21,741 with the wrapped library of the first review) |

This is the Lua heap only. The Rust side of each state (`mlua`'s bookkeeping, the hook and
log closures) is not counted; it is a few small allocations, not measured. Fifty mods would
need about 1 MB of Lua heap.

New crates in `Cargo.lock`: `mlua`, `mlua-sys`, `lua-src`, `luajit-src`, `which`, `bstr`,
`parking_lot`, `parking_lot_core`, `lock_api`, `scopeguard`, `rustc-hash`, `redox_syscall`.
`luajit-src` and `which` are build dependencies of `mlua-sys`'s `vendored` feature and are
compiled, but no LuaJIT is built; `redox_syscall` is only used on Redox and is not compiled
on Windows.

### Wall-clock overrun of single long-string instructions (2026-11-24, same machine)

The count hook checks the clock every `HOOK_INTERVAL` (1,000) instructions, so one call can run
past `MAX_CALL_WALL_TIME` (50 ms) by the time of that many instructions. That matters only for
single VM instructions whose cost grows with string length, which the count does not weigh.
Measured as one handler call after the strings were built at startup.

First measurement, with the 16 MiB cap of the first S4 draft, three runs each, debug and
release alike:

| Loop | Stopped by | Time of the call |
|---|---|---|
| `while true do end` | instructions | 0.6 to 1.1 ms |
| `local t = s .. s`, `s` of 2 MB | clock | 104 to 110 ms |
| `local t = s .. s`, `s` of 4 MB | clock | 206 to 209 ms |

The second review found a much worse instruction: `<` on two long strings full of `\0` bytes,
which Lua compares piece by piece with `strcoll`. The cap was lowered to 4 MiB, which keeps
every string under about 2 MB (building a string copies it). With the 4 MiB cap, three runs
each:

| Loop | Stopped by | Release | Debug |
|---|---|---|---|
| `while true do end` | instructions | 0.7 to 0.8 ms | 1.0 to 1.2 ms |
| `local t = s .. s`, `s` of 1 MB | clock | 67 to 69 ms | 66 to 72 ms |
| one `Z < Z`, `Z` = 0.95 M times `'a\0'` | (no loop) | 3.5 to 3.9 ms | 6.6 to 7.1 ms |
| one `Z < Z`, `Z` = 1.9 M times `'\0'` | (no loop) | 6.5 to 6.7 ms | 11.5 to 12.8 ms |
| `while true do local _ = Z < Z end`, `'a\0'` | clock | 0.69 to 0.70 s | 1.31 to 1.33 s |
| `while true do local _ = Z < Z end`, `'\0'` | clock | **1.25 to 1.27 s** | 2.35 to 2.39 s |

So the worst measured stall is about 1.3 s in a release build on this machine, about 25 times
the limit, before the mod is disabled; an old CPU takes longer. It is a one-time stall (the mod
is then off). The overrun is about `HOOK_INTERVAL` / (instructions per loop iteration) such
instructions, so a lower `HOOK_INTERVAL` or a lower cap shrinks it in proportion. Documented
as a residual risk for S7, which decides on the low-end device.

## Face groups in the mesh (S2a of decision 0015)

`ChunkMesh::face_quads` is filled once per face in `build_mesh_with_neighbours_into`, from the
growth of the index buffer while that face was meshed. The quads were already emitted face by
face, so the vertices and indices are grouped by direction with no change to the loops, and
`push_quad` is unchanged.

Method: `cargo bench -p vox-world --bench mesh` (full run, no `--quick`), three runs of the
code before (main, a second worktree) and three of the code after, alternating, on the same
machine. Medians of the three medians per case:

| Case | Before | After | Change | Runs before | Runs after |
|---|---|---|---|---|---|
| `mesh/empty` | 1.33 us | 1.32 us | -1.1% | 1.40 / 1.33 / 1.33 | 1.32 / 1.32 / 1.34 |
| `mesh/single_block` | 6.84 us | 6.63 us | -3.1% | 6.84 / 6.54 / 7.54 | 6.26 / 6.63 / 6.72 |
| `mesh/terrain_seed_1` | 182.3 us | 168.8 us | -7.4% | 182.3 / 177.3 / 205.0 | 168.8 / 170.0 / 167.9 |
| `mesh/slab` | 84.6 us | 83.6 us | -1.2% | 84.6 / 82.2 / 85.8 | 83.6 / 82.7 / 85.4 |
| `mesh/solid` | 140.5 us | 139.4 us | -0.8% | 147.0 / 137.5 / 140.5 | 139.4 / 141.3 / 139.2 |
| `mesh/checkerboard` | 2.87 ms | 2.73 ms | -4.8% | 2.87 / 2.55 / 3.04 | 2.73 / 2.82 / 2.62 |
| `mesh_reused/terrain_seed_1` | 176.0 us | 167.6 us | -4.8% | 176.8 / 174.5 / 176.0 | 167.6 / 194.2 / 167.1 |
| `mesh_reused/solid` | 139.7 us | 148.8 us | +6.5% | 139.7 / 137.3 / 143.0 | 138.9 / 204.8 / 148.8 |
| `mesh_reused/checkerboard` | 1.63 ms | 1.79 ms | +9.4% | 1.63 / 1.73 / 1.58 | 1.61 / 2.54 / 1.79 |
| `mesh_neighbours/terrain_seed_1` | 175.7 us | 173.6 us | -1.2% | 176.3 / 172.9 / 175.7 | 166.6 / 186.7 / 173.6 |
| `terrain/generate_seed_1` (control, code unchanged) | 26.5 us | 26.6 us | +0.5% | 26.4 / 26.5 / 36.5 | 25.6 / 26.6 / 27.8 |

Reading: the control moved 0.5 percent at the median but one of its runs was 38 percent above
the others, so a spread of that size is present in this setup. Seven of the ten mesh cases are
faster after (the terrain cases by 1 to 7 percent, which cannot be an effect of the change, so
it is noise or placement). The two reused cases that are slower at the median each owe it to a
single bad run after (204.8 us and 2.54 ms); their other two runs are within 1 to 7 percent of
the runs before (lowest runs: 138.9 vs 137.3 us, 1.61 vs 1.58 ms, which is +1.2 and +1.4
percent). So the criterion "within noise" is met in the sense that no case shows a difference
larger than the run-to-run spread, but the data cannot prove a cost below that spread; the
added work is three instructions per face (six times per chunk), not per quad.

## Per-direction culling (2026-11-24, step S2b of decision 0015)

The renderer draws, of each mesh in view, only the face directions the camera can be in front
of (one to three ranged draws per mesh instead of one draw of the whole mesh). Before is
`main` at the end of S1 and S2a (exported with `git archive` and built separately), after is
this step; release builds, default window size, RTX 4070 SUPER, start position.

**Picture.** One screenshot of each build at the start view (`--background --view-radius N`,
captured with `PrintWindow` after 4 s), compared pixel by pixel outside the HUD panel (the
full width below y = 300 and the strip right of x = 615, 356,700 pixels):

| Radius | Pixels that differ |
|---|---|
| 8 | 0 |
| 12 (default) | 0 |
| 16 | 0 |

Only the HUD numbers differ. One of the first radius-16 shots of the new build showed the
camera somewhere else (a different view, 216 instead of 224 chunks, its worst frame 22 ms); a
second shot of the same build, and a repeat of the old one, matched exactly. The terrain in
that shot was right, seen from another place, so it is not a rendering difference.
Hypothesis, not confirmed: the `--background` window received the focus once, the game
entered playing mode on that first focus event, and mouse or keys moved the camera.

**Triangles drawn** at the start view (HUD "Drawn" line, same shots):

| Radius | Chunks | Triangles before | Triangles after | Change |
|---|---|---|---|---|
| 8 | 59 | 71,354 | 51,810 | -27.4% |
| 12 | 132 | 155,918 | 111,650 | -28.4% |
| 16 | 224 | 261,848 | 186,244 | -28.9% |

Decision 0015 asks for at least 20 percent (expected 25 to 40): met. The camera stands above
the terrain, so all bottoms and the side faces turned away drop out; tops stay.

**Frame figures.** 8 s per run (`--fly 8` flies sideways for 8 s), the run summary's FPS,
`work` average and `gpu`, three runs each, before and after alternating. Median, then the
three runs:

| Case | FPS before | FPS after | Work before | Work after | GPU before | GPU after |
|---|---|---|---|---|---|---|
| radius 8, standing | 3559 (3499, 3559, 3635) | 3901 (3908, 3784, 3901) | 0.20 (0.20, 0.20, 0.20) ms | 0.18 (0.17, 0.18, 0.18) ms | 0.08 ms | 0.06 ms |
| radius 16, standing | 1675 (1719, 1675, 1615) | 2089 (2150, 2054, 2089) | 0.50 (0.49, 0.50, 0.53) ms | 0.39 (0.37, 0.39, 0.39) ms | 0.43 (0.43, 0.43, 0.44) ms | 0.29 ms |
| radius 8, `--fly 8` | 3555 (3560, 3555, 3485) | 3626 (3626, 3700, 3570) | 0.20 (0.20, 0.21, 0.20) ms | 0.19 (0.19, 0.19, 0.19) ms | 0.08 ms | 0.06 ms |
| radius 16, `--fly 8` | 1597 (1597, 1630, 1563) | 1980 (1980, 2019, 1941) | 0.53 (0.53, 0.53, 0.50) ms | 0.40 (0.41, 0.40, 0.39) ms | 0.45 ms | 0.30 ms |

FPS is not lower in any case (+2 to +25 percent), and `work` is lower, not higher. The GPU
time drops by about a third at radius 16 (0.43 to 0.29 ms standing), a little more than the
triangle count, while radius 8 is near the printing resolution. The lows moved within their
run-to-run spread (radius 16 flying, 0.1% low: 196 to 222 before, 182 to 220 after).

**CPU cost of the culling itself.** `work` includes `render_frame`'s wait for the GPU, so
the lighter GPU load hides the CPU side. An ignored timing test measures it apart:
`cargo test -p vox-render --release -- --ignored per_chunk_cpu_cost --nocapture`
(`vox-render/src/face_culling.rs`). It runs the renderer's per-chunk path over 197 surface
chunks (a circle of radius 8, one layer, face groups shaped like terrain: many tops, sides in
every direction, almost no bottoms) from a camera at the start height looking slightly down;
69 are in view and get 99 draws. Old path: relative origin, box, frustum test. New path: the
same plus `visible_ranges` and summing the drawn indices. Fastest of 14 rounds of 20,000
frames, three runs:

| Path | ns per mesh (3 runs) |
|---|---|
| old: frustum test only | 6.07, 6.15, 6.21 |
| new: frustum test and per-direction ranges | 8.22, 8.22, 8.26 |

About 2.1 ns more per mesh, 0.4 us per frame for 197 meshes, 0.2 percent of the 0.20 ms
`work` at radius 8: far below the 10 percent the decision allows. Not covered by the test:
the driver's cost of the extra `vkCmdDrawIndexed` calls (99 draws instead of 69 here, about
43 percent more). That part is only bounded by the frame figures above (`work` fell); it is
re-measured on a CPU-bound low-end device in S9.

## Vertical streaming by column span (2026-11-24, step S3b of decision 0015)

Development machine (RTX 4070 SUPER), release builds of `main` (before) and this step (after),
start view, 1280x720 window as the game opens it, alternating runs of 8 s each.

**Unchanged picture.** Every span is layer 0 today, so the loaded set and the meshes must not
change. The screenshots at the start view are pixel-identical outside the HUD panel at radius 8
and 12 (0 of 356,700 pixels differ). Drawn at the start view, before and after: radius 8, 59
chunks and 51,810 triangles; radius 12, 132 chunks and 111,650 triangles; loaded 197 and 441.

**Frame rate at radius 12** (three runs each):

| Case | FPS before | FPS after | `work` before | `work` after |
|---|---|---|---|---|
| standing | 3053 (3032, 3075, 3053) | 3075 (3026, 3075, 3081) | 0.25 ms | 0.25 ms |
| `--fly 8` | 2787 (2787, 2784, 2884) | 2848 (2848, 2839, 2885) | 0.27 ms | 0.27 ms |

Within the run spread. The column job (one layer plus the span scan) averaged 0.629 ms in a
`--fly 8` run at radius 12 with 15 workers.

**Edit that creates chunks.** An ignored timing test,
`cargo test -p vox-game --release -- --ignored the_cost_of_an_edit_that_creates_four_chunks --nocapture`
(`vox-game/src/editing.rs`), breaks block (0, -32, 0) under a streamed 3 x 3 area: the corner of
chunk (0, -1, 0), so the edit creates that chunk and the three behind its -X, -Y and -Z borders,
writes the block and remeshes the four chunks, as the game does on the main thread. Four is a
deliberate upper bound: in the game an edit creates at most 3 chunks (the hit block's chunk is
always present), so the game's worst case is cheaper than this. Median and
worst of 20 runs, three runs:

| Remesh of a missing neighbour | median | worst |
|---|---|---|
| generated (first version) | 1.69, 1.64, 1.65 ms | 1.95, 2.63, 2.11 ms |
| solid or air from the column's span, generated only outside the loaded columns | 0.91, 0.92, 0.86 ms | 1.10, 1.37, 1.13 ms |

The budget of decision 0015 is 2 ms, checked here on the development machine only; S9
re-measures it on the low-end device. Generating the up to five missing neighbours of each of
the four remeshed chunks (about 14 generations) was nearly half of the edit; reading the span
instead keeps the edit near 0.9 ms. The game logs every edit that created chunks
(`Edit: N chunks created, M remeshed in T ms`).

## LOD tile generation (step S4 of decision 0015)

`generate_lod_chunk` fills a chunk of 32 x 32 x 32 cells (cells of `2^level` blocks): one noise
sample per cell column at its centre block, then one write per solid cell. A tile is meshed
alone (`build_tile_mesh_into`, solid bedrock chunk below), so its four sides become walls
(skirts).

Method: `cargo bench -p vox-world --bench mesh -- "generate_seed_1|generate_lod|lod_tile"`, one
full run (no `--quick`), nothing else running, seed 1, origin tile of the level. One run per
case, so there is no spread across runs; the control moved a lot against the older tables
(37 us here against 26 to 28 us in the sections above, same code), so only compare numbers
inside this table. The ratios to the control are indicative only: the control drifted by
about 35 percent against the older tables, which is larger than the differences between the
levels.

| Case | Median (range) | Notes |
|---|---|---|
| `terrain/generate_seed_1` (control, code unchanged) | 37.0 us (36.8 - 37.2) | one chunk, 1,024 samples |
| `terrain/generate_lod_level_1` | 45.4 us (45.2 - 45.5) | 1.23 x control |
| `terrain/generate_lod_level_2` | 39.5 us (39.3 - 39.6) | 1.07 x control |
| `terrain/generate_lod_level_4` | 32.6 us (32.5 - 32.8) | 0.88 x control |
| `mesh_reused/lod_tile_level_2` | 122.2 us (121.6 - 122.8) | 567 quads |

Reading: a tile costs about one chunk generation, as decision 0015 estimated. The noise cost is the
same 1,024 samples at every level; the finer levels fill more solid cells (level 1 reaches
rows up to about 12, level 4 only 0 and 1), which is a plausible reason why level 1 costs the
most (not isolated).
Generation plus mesh of a level-2 tile is about 160 us, near the 0.2 ms per tile of the
estimate.

Quads and skirts (the test `skirt_share_is_reported_and_below_the_adr_limit` prints them; a
skirt is a sideways quad on the tile's own border plane), seed 1, origin tile:

| Level | Quads | Skirt share |
|---|---|---|
| 1 | 787 | 18.9 percent |
| 2 | 567 | 14.3 percent (81 quads) |
| 3 | 320 | 17.5 percent |
| 4 | 31 | 96.8 percent |

Over 5 seeds and the 3 x 3 tiles around the origin the skirt share at levels 1 to 3 stays
under 26 percent (limit of the decision: 40), so neighbour-aware skirts are not needed. Level 4 is the exception in share only: with cells
of 16 blocks the whole tile is nearly flat (rows 0 and 1), so it is 23 to 45 quads over the same
tiles, and the share is meaningless; it is judged by its absolute cost. For comparison, a full
chunk of the same terrain is 760 quads. The outward half of the skirts is dropped at draw time
by the per-direction culling (S2b).

## Tile selection (step S6 of decision 0015)

One `select_tiles` call (the quadtree over the horizon disc, view radius 8), release build,
averaged over 49 cameras, three quiet runs read 25 to 29 us at 2048 and 51 to 52 us at 4096; one run during other work read 62 and 73 us (negative columns and tile borders included). Run with
`cargo test -p vox-game --release streaming::lod::tests::selection_cost -- --ignored --nocapture`.

| Horizon | Time per selection | Leaves |
|---|---|---|
| 2048 | 26 us | 927 |
| 4096 | 52 us | 1157 |

Far below the 1 ms the decision expected. The leaf counts are about 45 percent above the
estimate of the decision (650 and 800); see "S6 as built" in decision 0015.
