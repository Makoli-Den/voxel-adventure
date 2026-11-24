# 0015. A deep world and a far horizon

Status: accepted.

## Context
The owner asks for three things at once: (1) a **deep** world, (2) a draw distance whose edge lies **far beyond the horizon**, not a visible cut-off, and (3) **many FPS on old devices** despite 1 and 2.

What the code does today:
- All terrain lies in chunk layer `y = 0` (32 blocks tall), surface height `MIN_SURFACE..=MAX_SURFACE` = 8..24 (`terrain.rs`). Layers `y < 0` are known to be all stone but are never streamed (`area::TERRAIN_LAYER`); meshes treat the side below as a solid "bedrock" stand-in (`job.rs`), so no downward faces exist. Flying high and looking sideways shows the open edge of the 32-block slab; flying down shows nothing at all.
- Streaming loads a circle of chunk columns, default radius 8 (256 blocks), `--view-radius` up to 30; no fog; `FAR` = 3000 (`camera.rs`); the clear colour is the sky (`SKY_COLOR` in `renderer.rs`).
- Measured on the development machine (RTX 4070 SUPER, 1280x720, start view, `docs/benchmarks.md`): about 3500 FPS at radius 8 (about 62 chunks and 80 k triangles drawn after frustum culling), about 1750 at 16, about 670 at 30. **Frame time is proportional to drawn triangles**; draw calls and overdraw were measured not to matter. Vertices are 6 bytes, greedy meshing merges coplanar faces, frustum culling is per chunk box, chunk jobs run on worker threads with 4 uploads per frame.
- Every mesh is one Vulkan allocation; only 4096 are guaranteed (decision 0007).
- Edits are kept in the game's `World` and lost when a chunk unloads (decision 0011). Physics treats an unloaded cell in or below layer 0 as solid (decision 0012).

Arithmetic that rules out "just raise the radius": full detail to 2048 blocks is radius 64, about 12,900 columns. That is three times the allocation limit, about 6 s of worker CPU to fill (0.5 ms per job), and, with a third of them in view at about 1,200 triangles each, about 5 M triangles, roughly 8 ms of GPU on the 4070 SUPER alone. Draw less, not faster: the far terrain needs fewer triangles per block of distance.

## Decision

### Targets in numbers
| Parameter (named constant) | Value | Meaning |
|---|---|---|
| `WORLD_FLOOR_Y` | -512 | lowest block (a bedrock row); 16 chunk layers below 0; aligned to a chunk border (const assertion) |
| `WORLD_CEILING_Y` | 512 | first block that can never be set (build limit); 16 layers above 0 |
| `DEFAULT_VIEW_RADIUS` | 8 chunks (256 blocks), re-measured in S1 and S7 *(12 chunks since S1, see "S1 as built")* | full-detail area, editable |
| `DEFAULT_HORIZON` | 2048 blocks | where terrain ends; reached with level of detail (LOD) |
| `MAX_HORIZON` | 4096 blocks | `--horizon` upper bound |
| `MAX_LOD_LEVEL` | 4 | coarsest cell is 16 blocks, coarsest tile 512 blocks |

### (a) Deep world: vertical streaming by column span
- **The unit of streaming stays the column; the job loads only the layers that can show a face.** A new pure function in `vox-world` (`terrain.rs`), `column_span(seed, column) -> RangeInclusive<i32>` (chunk layers), samples the surface height of the column's 32 x 32 block columns plus a one-block border (34 x 34 samples, about the noise cost of one chunk). With `lo` the lowest of the 34 x 34 heights and `hi` the highest of the inner 32 x 32, every visible face of the column lies in `lo..=hi`: a top face sits at a column's own height, a side face above a neighbour's height. The span is the layers `floor(lo / 32)..=floor(hi / 32)`, clamped to the world bounds. Below the span every block is solid, above it every block is air, so those chunks have no faces by construction. Today every span is layer 0 only, so the loaded set and the picture do not change; the span grows with mountains and deeper valleys without another change.
- **The column job** (`streaming/job.rs`) computes the span, generates and meshes each layer in it with its four horizontal neighbours of the same layer, and uses the layer above and below from the same span (or air above and the solid stand-in below outside it). It returns the chunks, their meshes and the span. Empty meshes are never uploaded (as today) and do not count against the upload budget.
- **Underground chunks cost nothing until touched.** They are not generated, not stored and not meshed. They are created on the main thread only when an edit needs them: before `set_block`, the game makes sure the chunk of the block and every neighbour chunk across a border the block touches is in the `World` (`generate_chunk`, about 30 us each, at most 4 per edit), then remeshes as decision 0011 does. A dug shaft therefore shows its walls; a shaft 512 deep keeps about 50 chunks (3 MiB) in memory.
- **Streaming never replaces a chunk already in the `World`** (created by an edit before the column's job arrived): the present chunk wins, as with edits today.
- **Unloading a column removes every layer of it** from the `World` and the renderer (`WORLD_FLOOR_Y / 32 ..` up to the ceiling layer: 32 lookups per column, no index needed).
- **Physics** (`is_solid`): a cell below the floor is solid, at or above the ceiling air; a cell of an unloaded chunk is solid below its column's span and air above it; a cell of a column that is not loaded is solid (as today: the player stays in the loaded area). The streamer hands the spans to the game in `Changes` and keeps them per loaded column.
- **The bottom:** the generator places one row of a new data block `base:bedrock` (blocks file and a 16x16 texture, appended so ids 1 to 3 do not change) at `WORLD_FLOOR_Y`. Editing refuses to break any block at `y == WORLD_FLOOR_Y` (an engine rule, not a block property: properties come with a later content decision), and `World::set_block` refuses positions outside the bounds. Nothing below the floor is ever meshed (the stand-in is solid). The flying camera is clamped to `WORLD_FLOOR_Y..=WORLD_CEILING_Y + CHUNK_SIZE`, so it is never under the world.
- **Caveat: caves and overhangs.** The span rule holds only because the generator is a heightmap. A cave generator breaks "solid below the span"; it must widen the span to the cave layers and then cave culling (decision to come, research note 3.6) becomes worth measuring. In fly mode the camera inside solid rock sees no faces (back faces are culled, inner faces do not exist), so it sees sky colour; this is the same as spectator mode elsewhere and is accepted.

### (b) Far horizon: fog first, then level of detail

**Step 1: fog that ends where the terrain ends (shader only).** Per fragment, `f = smoothstep(fog_start, fog_end, distance)` with `distance` the radial distance from the camera, and the colour mixed towards the fog colour, which is the clear colour (`SKY_COLOR`, one constant for both, so a fully fogged pixel equals the sky and the edge disappears). Radial, not view depth: with depth the screen corners would be fogged less and show the edge when the camera turns. The game sets `fog_end` = loaded distance minus one chunk (the nearest missing column can be up to a chunk closer than the radius), `fog_start = FOG_START_FRACTION * fog_end` (0.6, tuned by eye), and the far plane to `fog_end` plus a margin, so the CPU frustum test also drops chunks that would be fully fogged. With radius 8 this hides the edge but the world looks small (fog from about 140 blocks); that is the reason for LOD.

**Step 2: LOD tiles from the generator, drawn by the same pipeline.**
- **Tile:** `TilePos { level: u8, pos: ChunkPos }` in `vox-world`; level 0 is a chunk. A level-k tile is a `Chunk` of 32 x 32 x 32 **cells**, each cell `2^k` blocks on a side, so the tile covers `32 * 2^k` blocks. The coordinates are in the level's own grid.
- **Generation (pure, `vox-world`):** `generate_lod_chunk(seed, tile, &TerrainBlocks) -> Chunk`. A cell column takes the surface height `h` sampled once at its centre; cells are solid up to `round(h / 2^k)`; a cell's block is the block the full-detail column has at the cell's top block (the surface cell is grass, cells below are dirt or stone by the same layer rule as `fill_column`). Point sampling is enough: tiles are fixed to the world grid and do not move with the camera, so the aliasing does not shimmer. Level 0 equals `generate_chunk` (tested). Cost: 1,024 noise samples plus one fill, about one chunk generation (about 30 us). Vertical tiles use `column_span` on the sampled heights; today every level needs one tile layer.
- **Meshing:** the unchanged greedy mesher, on the tile **alone** (no horizontal neighbours), with the solid stand-in below. Every border becomes a wall down to the tile's bottom: these are the **skirts** that close the cracks between levels. They need no neighbour or level information, so a tile mesh never depends on what is next to it and is never rebuilt because a neighbour changed level. Walls between same-level tiles that face away from the camera are back faces and are dropped by the per-direction test in (c); the rest are hidden by the depth test (overdraw, measured not to be the cost). Expected skirt overhead about 25 percent of a tile's triangles; measured in S4 (a quad count printed by the benchmark).
- **Selection: a quadtree over the horizon disc** (pure, `streaming/lod.rs`). Root tiles of level `MAX_LOD_LEVEL` cover the disc. A level-k tile (k >= 1) is split into its four children while its nearest point is closer than `SPLIT_RATIO * tile size` to the camera, where the camera column is snapped to the level-k grid, so a level changes only when the camera crosses a tile of that level. `SPLIT_RATIO = view_radius / 2` makes the level-0 leaves the full-detail area of about `view_radius` chunks, and each ring k then spans `256 * 2^(k-1) .. 256 * 2^k` blocks at radius 8. Leaves are drawn; leaves farther than the horizon are dropped. The selection is a pure function of the snapped camera position, tested for: no overlaps, full cover of the disc, neighbouring leaves differ by at most one level (if this fails, the ratio is raised).
- **Level 0 leaves are the full-detail columns** of (a), with blocks in the `World` and editable. Leaves of level 1 and up keep no blocks: the worker drops the cell chunk after meshing.
- **Screen error:** a cell of `2^k` blocks seen from at least `128 * 2^k` blocks subtends at most 1/128 radian, about 4 pixels at 720p with the 70 degree vertical field of view (8 at 1440p). Constant across rings; the full-detail radius sets it.
- **Textures, UVs, mipmaps:** unchanged shaders and vertex format. The vertex shader multiplies the cell position by the tile's cell size before deriving the UV, so the texture still repeats once per block and lines up across tiles of different levels. The mip level comes from the screen derivatives as for any far chunk, so distant tiles sample the small levels exactly as full-detail chunks at that distance would.
- **Counts at radius 8 (estimates, checked in S7):** each LOD ring holds about `0.75 * pi * 8^2`, about 150 tiles; the full-detail area about 200 columns. Horizon 2048: 3 rings, about 650 meshes; 4096: 4 rings, about 800. Worker time to fill: about 0.2 ms per tile (generation plus mesh), about 10 ms over 15 workers. GPU mesh memory: about 30 KB per tile, about 25 MB at 4096.
- **Depth precision:** with standard depth and `NEAR` = 0.1 the depth step at distance `d` is `d^2 * 2^-24 / NEAR` blocks: 0.16 at 512, 2.5 at 2048, 10 at 4096, so far hills behind hills and skirts would fight. LOD therefore comes with **reversed Z** on `D32_SFLOAT` (clear to 0, compare `GREATER`, projection with near and far swapped), which makes the step nearly proportional to distance. On a device without `D32_SFLOAT` (only `X8_D24`, where reversed Z gains nothing) the horizon is capped at `D24_MAX_HORIZON` = 1024 blocks.

### (c) Hidden-surface savings for a deep, far world
- **Per-direction culling at draw time.** The mesher already emits quads grouped by face direction in `Face::ALL` order. `ChunkMesh` gains `face_quads: [u32; Face::COUNT]` (quads per direction), and the renderer draws a group only if the camera can be on its front side: the `+X` group is skipped when `camera.x < box.min.x`, `-X` when `camera.x > box.max.x`, and so on. A chunk beside the camera draws at most three groups (three draws instead of one; draw calls were measured not to be the cost, re-checked on CPU time in S2). This drops the side faces turned away, the tops of everything above the camera, and the skirts facing outward. It is exact, so the picture does not change, and it applies to every tile level.
- **No direction is dropped at mesh time.** A mesh must stay valid wherever the camera goes; the draw-time test costs six comparisons per mesh.
- **Vertical culling** is the existing 3D frustum test per mesh box. For tiles, the box height is the mesh's real `y` range (computed once at upload from the vertices), not the 512-block tile, so tall empty tile boxes are not drawn.
- **Layers with nothing to see are never loaded** (span rule) and empty meshes never uploaded: the deep world adds no meshes and no triangles while nobody digs.

### (d) Renderer interface (the `Renderer` trait stays narrow; `vox-world` stays graphics-free)
- `set_camera(&CameraView { view_projection, position })`: the position is needed by fog, per-direction culling and camera-relative placement. The renderer pushes each mesh origin **relative to the camera** (computed on the CPU from the integer origin), with a matrix without the camera translation (`view_projection * translation(position)`, once per frame), so `f32` stays exact far from the origin.
- `set_fog(Fog { start, end })` in blocks; `end <= start` disables fog. The fog colour is the renderer's sky colour.
- `set_chunk_mesh`/`remove_chunk_mesh` become `set_tile_mesh(TilePos, &ChunkMesh)`/`remove_tile_mesh(TilePos)`: one map, one draw loop, one pipeline for all levels (callers use `TilePos::chunk(pos)`).
- **Push constants: 112 bytes** (128 are guaranteed), one range for vertex and fragment: `mat4` view-projection without translation (64), `vec4` mesh origin relative to the camera with the cell size in `w` (16; `w` was an unused 0), `vec4` fog start and end (16), `vec4` fog colour (16). The shader check limit goes from 80 to 112 bytes; the layout stays in `interface/chunk.rs`.
- **The projection contract** (`set_camera` doc) changes with reversed Z: near maps to depth 1, far to 0.
- **No second pipeline, no new vertex format, no new descriptor.** Shader packs keep one chunk shader pair; they get the fog inputs for free.
- `RenderStats` gains triangles per level (an array of `MAX_LOD_LEVEL + 1`), so the budgets below can be read from the run summary.

### (e) Budgets (development machine, 1280x720, start view, release; the low-end device check is S9)
| Budget | Default (radius 8, horizon 2048) | Maximum (radius 16, horizon 4096) |
|---|---|---|
| Triangles drawn, full-detail area | <= 100 k | <= 300 k |
| Triangles drawn, each LOD ring | <= 60 k | <= 120 k |
| Triangles drawn, total | <= 300 k | <= 700 k |
| FPS standing | >= 1500 | >= 600 |
| Flying (`--fly 8`): average FPS / worst frame | >= 1000 / <= 16.7 ms | >= 400 / <= 16.7 ms |
| Meshes in the renderer (allocations) | <= 1500 | <= 3000 (headroom under 4096 for retired meshes) |
| GPU mesh memory | <= 64 MiB | <= 160 MiB |
| Fill time to the horizon | <= 1 s | <= 3 s |
| Main-thread cost of one edit that creates chunks | <= 2 ms | same |

`MAX_VIEW_RADIUS` drops from 30 to 16 when LOD lands: at 30 a single LOD ring would hold about 2,100 tiles. 300 k triangles at 60 FPS is 18 M per second, a small fraction of what 2012-era integrated GPUs are rated for; this is an estimate until S9 measures one.

## Steps
Each step is one PR, leaves the game running, passes `cargo fmt`, `cargo clippy -- -D warnings` and `cargo test`, gets an independent review, and records its measurement in `docs/benchmarks.md` (three runs before and after, as there). Window checks follow the project rule (`--background`, hidden console, no topmost).

```
S1 -> S2b -> S5 -> S7 -> S8 -> S9
S2a -> S2b
S3a -> S3b ------> S7
S4 --------> S5
S6 ---------------> S7
```
S2b needs S1 (camera position) and S2a; S5 needs S2b (same renderer files) and S4 (`TilePos`); S7 needs S3b, S5 and S6. Parallel: S2a with S1 (mesh.rs against vox-render and camera); S3a with S1 and S2 (vox-world terrain and bounds against mesh.rs and vox-render); S4 with S3b (vox-world `lod.rs` against vox-game); S6 with S5 (`streaming/lod.rs` only, not wired). Every step that edits `vox-game/src/main.rs` (S1, S3b, S5, S7) is merged one after another, never written in parallel. `vox-world/src/lib.rs` exports change in S2a, S3a and S4: merged in order, the later author rebases.

**S1. Fog, camera position, default radius** (render-dev; `vox-render` trait, renderer, `interface/chunk.rs`, shader check, `assets/core/shaders/chunk.*`, `vox-game` `camera.rs`, `main.rs`). `CameraView`, camera-relative origins, `set_fog`, push layout of (d) without the level, far plane = fog end plus margin. Measure GPU time and FPS at radius 8 and 16 with and without fog, then set `DEFAULT_VIEW_RADIUS` to the largest of 8, 12, 16 that keeps >= 2000 FPS standing and >= 1000 flying. Done: shader tests for the new block; fog cost within the run spread (<= 0.01 ms at 8, <= 5 percent at 16); one window check from height 200 at the area edge shows no void; numbers and the chosen radius recorded.

**S2a. Face groups in the mesh** (world-dev; `vox-world/src/mesh.rs`). `ChunkMesh::face_quads`, filled by the mesher. Done: a test that the quads are grouped in `Face::ALL` order and the counts sum to `quad_count`; mesh benchmarks within noise.

**S2b. Per-direction culling** (render-dev; `vox-render` renderer). Up to three ranged draws per mesh by the box test of (c). Done: identical screenshot; drawn triangles at the start view at radius 8 and 16 at least 20 percent lower (expected 25 to 40); FPS not lower; CPU work per frame not more than 10 percent higher.

**S3a. World bounds and column span** (world-dev; `vox-world`: new `bounds.rs` with `WORLD_FLOOR_Y`, `WORLD_CEILING_Y`, layer helpers; `terrain.rs` `column_span` and the bedrock row (`TerrainBlocks::bedrock`); `world.rs` bounds check in `set_block`; base mod `blocks.json` and `bedrock.png`; `vox-game/src/content.rs` resolves `base:bedrock`). Done: tests that for many seeds and columns no layer outside the span meshes to anything (with generated neighbours) and that below the span all is solid and above all air; the floor row is bedrock; pinned fingerprints of layer-0 chunks unchanged; `set_block` outside the bounds returns `None`; benchmark `terrain/column_span` added.

**S3b. Vertical streaming in the game** (world-dev; `streaming/*`, `physics.rs`, `editing.rs`, `main.rs`). Column jobs, spans in `Changes`, whole-column unload, the physics rule, chunk creation before edits, unbreakable floor, flying camera clamp. Done: drawn chunks and triangles at the default view identical to S2b (the span is layer 0 everywhere today); a pure test digs a shaft from the surface to the floor through `World` and the job functions and every exposed face is meshed; no chunk is replaced by streaming; the edit cost is logged and within the 2 ms budget.

**S4. LOD tile generation** (world-dev; `vox-world`: `TilePos`, `generate_lod_chunk`, in a new `lod.rs`). Done: level 0 equals `generate_chunk`; cell heights follow the rounding rule; the surface cell's block is grass; benchmarks `terrain/generate_lod_level_{1,2,4}` and `mesh_reused/lod_tile_level_2` (quads, and the share of skirt quads).

**S5. Tiles and reversed Z in the renderer** (render-dev; `vox-render`, `vox-game/src/camera.rs`, the call sites in `main.rs`). `set_tile_mesh`, cell size in the push, mesh `y` range for the box, reversed Z with the `D24` horizon cap, triangles per level in `RenderStats`. Done: frustum and push-layout tests; the picture at level 0 identical; one window check of a hand-placed level-2 tile next to full-detail chunks (seams closed, texture aligned).

**S6. Tile selection** (world-dev; new `vox-game/src/streaming/lod.rs` and one line in `streaming/mod.rs`, not wired). The quadtree of (b). Done: the tests listed there, plus tile counts per level at radius 8 for horizons 2048 and 4096 within 20 percent of the estimates, and the cost of one selection (expected well under 1 ms) logged by a test.

**S7. LOD streaming** (world-dev; `streaming/*`, `main.rs`, `cli.rs`, `hud.rs`). Tracker keyed by `TilePos`, LOD jobs on the same workers sharing the upload budget, nearest first; `--horizon`, fog end from the horizon; `MAX_VIEW_RADIUS` 16. Done: the default and maximum columns of the budget table met and recorded (standing and `--fly 8`); one window check at the default settings from the ground and from height 300.

**S8. No holes during level changes** (world-dev; `streaming/tracker.rs`, `mod.rs`). A tile leaves the renderer only when the area it covered is fully drawn by uploaded tiles of the new selection (or it left the keep range). Done: a pure metric "uncovered area" in the stats, zero frames with uncovered area inside the horizon during `--fly 8` after the first fill; the meshes and memory budgets still met.

**S9. Low-end device** (tester; `docs/benchmarks.md`, a new `docs/minimum-hardware.md`). Run S1, S2b and S7 measurements on the reference old device; set the default horizon and radius for it if the budgets do not hold. Done when the document names the device and the measured settings.

## Benchmarks to add
- `vox-world` criterion: `terrain/column_span`, `terrain/generate_lod_level_{1,2,4}`, `mesh_reused/lod_tile_level_2` (prints quads and skirt quads). S2a adds no case; it must stay within noise of the existing mesh cases.
- In-game tables in `docs/benchmarks.md`: fog on/off GPU time (S1); drawn triangles before/after per-direction culling (S2b); for S7 and S8 per setting: FPS, 1% low, worst frame, triangles per level, meshes, mesh bytes, fill time, standing and `--fly 8`; edit cost with chunk creation (S3b).
- The same runs on the low-end device (S9).

## Reasons
- **Fog first:** one shader change removes the visible edge for every radius, costs a few instructions per fragment, and every later step needs it anyway.
- **Span per column instead of a vertical radius:** a vertical radius would load 16 all-stone layers per column for nothing; the span is exact for the heightmap generator, computed on the worker, and makes depth free.
- **Chunks created on edit instead of streamed underground:** nothing underground can be seen until someone digs, and an edit is the only thing that changes that.
- **LOD as coarse voxel tiles through the existing mesher and pipeline:** one generator, one mesher, one shader pair, one texture path; the blocky look stays consistent at every distance; shader packs and texture packs apply to the far terrain unchanged.
- **Skirts from meshing tiles alone:** cracks closed with zero coupling between tiles, so the selection can change without rebuilding neighbours. The cost is a bounded share of triangles, measured in S4, and the outward half is culled by (c).
- **Quadtree with snapping:** a fixed number of tiles per ring (constant screen error), no overlaps by construction, and a level changes only every `2^k` chunks of travel.
- **Reversed Z now, not later:** the depth step at 2048 blocks is a computed 2.5 blocks, not a guess.

## Rejected
- **Raising the full-detail radius to the horizon:** about 5 M triangles and 12,900 allocations at 2048 (see Context).
- **Heightfield raymarching in a full-screen shader for the far terrain:** per-pixel cost with many steps on old GPUs that are fill-rate bound, a heightmap texture to stream, a descriptor and a second, different-looking path; it cannot show blocks or texture packs.
- **Impostors or a panorama of distant terrain:** wrong parallax when flying at 160 blocks/s, rebuilt constantly.
- **Smooth LOD (transvoxel, dual contouring) and geomorphing:** a different look from blocks and much more code; popping at about 4 pixels of error is accepted.
- **A second pipeline or vertex format for LOD:** the cell size in the push does the job.
- **Skirts that know their neighbours' levels:** rebuilds when the selection changes; adopted only if S4 measures the skirt share above 40 percent.
- **Dropping face directions at mesh time** (no bottoms, no outward walls): a mesh would be wrong once the camera moves; the draw-time test is exact.
- **Indirect draws and one big buffer now:** draw calls were measured not to matter. Trigger: more than 3,000 meshes, or measured submission cost above a quarter of the frame on the low-end device.
- **Hi-Z occlusion culling and GPU culling now:** overdraw was measured not to matter, and LOD already cuts the far triangles that hills would hide. Trigger: terrain with mountains where an occlusion-query experiment shows more than half of drawn triangles hidden.
- **Cave culling (visibility graph):** no caves exist.
- **Loading all layers to the floor, or a uniform-chunk representation to make that cheap:** not needed while nothing underground is loaded; revisit with caves.
- **LOD data store on disk (Distant Horizons style):** the generator is a pure, cheap function; regenerate instead.

## Risks
- **Edits are invisible in LOD:** tiles come from the generator, so a build disappears beyond the full-detail area. Today edits are lost on unload anyway; when saves land, LOD tiles of edited areas must be built from saved chunks.
- **Generators must support coarse sampling.** When world generation becomes data or mods, the contract is "sample a column height (or a block) at any point cheaply"; a Lua generator per sample would be too slow for LOD.
- **The span breaks with caves or overhangs** (see (a)).
- **Popping** at ring borders and **brief holes** until S8.
- **Skirt share and three draws per mesh** may cost more than estimated on an old CPU or GPU; both are measured (S2b, S4, S9).
- **`D24`-only devices** get a 1024-block horizon.
- **Allocations:** about 3,000 meshes at the maximum settings is close to the limit with retired meshes; the sub-allocation trigger of decision 0007 fires if a later feature adds more.
- **Fog cost on fill-rate-bound GPUs** is measured only on the 4070 SUPER until S9.
- **Fly camera inside rock sees sky colour.**

## What must not be built yet
Caves and cave culling; uniform or palette chunk storage; the two-stage generation pipeline; indirect draws, a shared vertex buffer, Hi-Z, GPU-driven or mesh-shader culling; geomorphing; LOD from saved or edited chunks; a sky gradient, atmospheric scattering or an underground colour; block properties such as "unbreakable" in data; a time-based upload budget.

## S1 as built
- **`CameraView { view_projection, position }`, but the matrix comes without translation from the caller.** (d) planned `view_projection * translation(position)` in the renderer. A full `f32` view-projection already lost the precision that step is meant to keep: its translation column is rounded at the scale of the position, and the projection's small depth term drowns in it (at a million blocks the far plane moved from 500 to 625 blocks even with the product in `f64`, pinned by a test in `vox-render/src/view.rs`). So `view_projection` is the matrix of the camera placed at the origin (`look_at(ZERO, forward, Y)`); the renderer only subtracts the position from each integer chunk origin in `f64` and rounds once to `f32`. `position` is `[f32; 3]`, the type the game keeps its camera in. Frustum planes and boxes are both camera-relative.
- **Push constants: 112 bytes for the vertex and fragment stages** as in (d): matrix, origin relative to the camera with the cell size in `w` (1.0, chunk meshes are blocks), fog `vec4`, fog colour (`SKY_COLOR`, the clear value, one constant). The fog `vec4` is start, end, `1 / (end - start)` and 0, all zeros while fog is off: the shader computes `clamp((d - start) * z, 0, 1)` and its smoothstep polynomial, because GLSL `smoothstep(start, end, d)` is undefined for `end <= start`. The vertex shader passes the camera-relative position, the fragment shader takes its length (radial distance, exact per fragment). The shader check accepts push constants in every stage an interface lists.
- **Fog end `(radius - sqrt 2) * 32` blocks, not "minus one chunk".** A column is loaded when its offset in chunks is within the radius; the camera can be anywhere in its column and a missing column's nearest corner faces it, so missing terrain can be up to `sqrt 2` chunks inside the radius (along a diagonal). A test checks the bound against `area::wanted` for every radius from 2 to 30. The end is at least one chunk (radius 0 and 1). Start = 0.6 of the end; the far plane is the end plus 1 block. Code: `vox-game/src/view_distance.rs`.
- **Measured** (`docs/benchmarks.md`, "Fog and camera-relative drawing"): the fog shader costs nothing measurable (at most 0.01 ms at radius 12); the closer far plane draws 59 instead of 67 chunks at the default view of radius 8. Radius 16 stands at about 1740 FPS, so **`DEFAULT_VIEW_RADIUS` is 12** (about 2490 FPS standing, 2440 flying); the default column of the budget table (e) was written for radius 8 and is re-checked in S7.
- **Window check:** screenshots at the start view and from height 200 looking down towards the edge (a temporary camera pose, reverted): at radius 8 from that height everything beyond the fog end is sky, at radius 16 the terrain fades into the sky with no edge, where `main` shows the cut-off slab against the sky.

## S2b as built
- **The renderer copies the groups at upload.** `GpuMesh` keeps the seven boundaries of the six direction groups (`FaceGroups` in `vox-render/src/face_culling.rs`, taken from `ChunkMesh::face_index_range`), so the groups are contiguous and in `Face::ALL` order by construction. `set_chunk_mesh` refuses a mesh whose `face_quads` do not add up to its indices (six per quad) with `InvalidMesh`, like an index past the last vertex. A full-range fallback was rejected: the counts are the mesh's contract, and drawing such a mesh whole would hide a broken mesher together with the saving. Only the sum is checked (it keeps every draw inside the index buffer); that each group's quads really face its direction is the mesher's contract.
- **The test** per direction is one comparison on the chunk's camera-relative box (the same box the frustum test uses): `+X` is drawn when `min.x <= 0`, `-X` when `max.x >= 0`, and so on for Y and Z. The camera on a face plane counts as in front, NaN draws everything. The box comes from the same rounded relative origin the vertex shader adds the positions to, so rounding cannot move a plane across the camera. A shader pack that moves vertices out of the chunk box would break the assumption; the built-in shader does not.
- **Merging.** Opposite directions are neighbours in `Face::ALL` (pinned by a test), so the directions drawn for one axis are always one contiguous range; drawn groups that touch in the buffer (empty groups between them take no room) become one draw. A mesh needs one to three draws; a camera inside a chunk's box on every axis draws it in one. A mesh with nothing facing the camera is not drawn at all and not counted in `chunks_drawn`.
- **Stats.** `draw_calls` counts the ranged draws, `triangles` only the drawn ranges, so the HUD's "Drawn" line shows the saving.
- **Measured** (`docs/benchmarks.md`, "Per-direction culling"): the screenshots at the start view are pixel-identical to `main` at radius 8, 12 and 16 outside the HUD panel; drawn triangles drop by 27 to 29 percent (radius 8: 71 k to 52 k; 16: 262 k to 186 k); FPS rises by 2 to 25 percent (radius 16 standing about 1675 to 2090), GPU time at radius 16 by about a third, and the per-frame `work` falls. `work` includes waiting for the GPU, so the culling's CPU cost was timed apart (ignored test `per_chunk_cpu_cost`): about 2.1 ns more per mesh (8.2 instead of 6.1 ns), 0.4 us per frame for about 200 meshes, 0.2 percent of `work` at radius 8. The driver's cost of the extra draw calls (about 40 percent more draws) is not isolated: that part of the CPU criterion is bounded only by the falling `work` here and is deferred to S9.

## S3b must (found in the S3a review)
- Exclude `base:bedrock` from the placeable list (`placeable_blocks` in `vox-game/src/content.rs`; the wheel currently offers every block but air) and fix the README line about the base blocks.
- Filter chunk creation before an edit with `contains_chunk_layer`: `set_block` refuses out-of-bounds writes, but creating chunks outside the world first would store them for nothing.
- Make bedrock unbreakable (engine rule at `y == WORLD_FLOOR_Y`), as in (a).

## S4 as built
- **`vox-world/src/lod.rs`:** `TilePos { level, pos }` (`chunk`, `origin`, `extent`), `cell_size`, `MAX_LOD_LEVEL` = 4, `generate_lod_chunk`, and for meshing a tile alone `solid_chunk`, `build_tile_mesh_into` (solid stand-in below, no horizontal neighbours) and `skirt_quads`. A level above `MAX_LOD_LEVEL` counts as the maximum (no panic).
- **Rules:** the column samples its height at the cell's centre block (`cell / 2` blocks in from the corner); the surface row is `floor((h + cell / 2) / cell)` (nearest, ties up, floor-based for negative `h`); the top cell is grass, a cell below takes the layer rule (`terrain::block_at_depth`, tested against `fill_column`) at its highest block capped one below the surface, so the cell under the grass is dirt in most columns at every level (stone where it lies more than three blocks below) and deeper cells are stone; the cell row holding `WORLD_FLOOR_Y` and the rows below are bedrock; any tile layer works (above the surface air, below the floor bedrock). Level 0 equals `generate_chunk` (tested over seeds, layers, negative and far chunks).
- **Measured** (`docs/benchmarks.md`): a tile costs 33 to 45 us to generate (one chunk 37 us in the same run); level 2 meshes in 122 us. Skirt share of quads at levels 1 to 3: under 26 percent over 5 seeds and the 3 x 3 tiles around the origin, under the 40 percent limit, so the skirts stay neighbour-blind. Level 4 (23 to 45 quads, nearly all walls) is judged by its absolute cost, not by the share.
- **`TilePos` fields are private** (`level()`, `pos()`): `new` clamps the level, so equal tiles are equal keys.
- **For S5 to S7: layers outside the column span must not be meshed.** A tile meshed alone has a top and four walls, so a layer that lies fully inside the solid part would mesh a hidden top and four walls for nothing, and a solid layer above the surface layer (all stone, below the span) would do the same. The worker meshes only the layers of the span (selection in S6 and jobs in S7), as (a) says for chunks.

## Answers to the open questions
- The world bounds are -512 and 512 (named parameters, easy to change).
- The floor is a visible `base:bedrock` block (data plus a texture) that the engine refuses to break.
- The default horizon is 2048 blocks (maximum 4096); a level-of-detail change at about 4 pixels is acceptable.
- The reference low-end device is chosen when step S9 starts; until then the floors are set on the RTX 4070 SUPER.
- Edits beyond the full-detail area are not shown on coarse tiles until saves exist.
- A camera inside solid rock sees the sky colour; an underground colour is a later, separate change.
