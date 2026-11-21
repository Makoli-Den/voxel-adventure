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
| `mesh/terrain_seed_1` | `generate_terrain(1)` | The realistic case: hills, air above, solid below. |
| `mesh/solid` | every block solid | Maximum number of neighbour checks with almost no output (only the outer shell). |
| `mesh/checkerboard` | blocks where `x + y + z` is even | Worst case: no two solid blocks touch, almost every face is emitted. |
| `terrain/generate_seed_1` | seed 1 | Cost of generating the chunk (noise, column fill, allocating the chunk). |

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

## What this does and does not tell us

- One machine, one CPU (a desktop Ryzen with a large cache). An old low-end device, which is
  the real target, will behave differently, especially for the memory-heavy checkerboard case.
- Single thread. Chunk meshing is meant to run on worker threads; contention for memory
  bandwidth and for the allocator is not measured.
- One chunk size (32) and one terrain seed. Other seeds change the number of faces by tens of
  percent, not by orders of magnitude.
- No neighbour lookups across chunk borders: the mesher treats outside as air, so border
  faces are always emitted. A mesher that reads neighbouring chunks will be slower and these
  numbers will not carry over.
- The vertices are not uploaded to a GPU; the benchmark ends at the `Vec`s. Page faults and
  allocator work for the fresh vectors are included, because every iteration allocates a new mesh.
- The terrain generator benchmark includes allocating and zeroing the chunk (64 KiB).
- Noise between runs is around 5 to 10 percent, and up to about 45 percent for the checkerboard.
