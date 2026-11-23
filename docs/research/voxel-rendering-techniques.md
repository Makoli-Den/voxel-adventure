# Voxel rendering techniques: what gives the thousands-of-FPS results

Status: research note, read-only investigation. No engine code was changed.

## 1. Our situation

- Chunks are 32^3, one vertex+index buffer per chunk, one draw call per chunk, no culling.
- Measured: frame time is linear in chunk count, about 5 us per chunk.
  - 289 chunks, 507k quads: 1.9 ms GPU on RTX 4070 SUPER (about 1750 quads per chunk).
  - 3721 chunks: 19 ms.
- Arithmetic (ours): at 5 us per chunk, a 1000 FPS budget (1 ms) allows about 200 chunks of work. Thousands of FPS therefore needs both fewer chunks drawn (culling) and fewer quads per chunk (better meshing, LOD).

**Inference to check first:** 507k quads is about 1M triangles. A modern mid-range GPU should draw about that many triangles in well under 1 ms unless fill-rate, overdraw or per-draw overhead dominates. So the 5 us per chunk may be partly CPU submission or GPU starvation, not vertex work. This is a hypothesis; see section 5 for how to test it.

## 2. The two reference videos

Both transcripts are YouTube auto-generated captions (English only; the Russian track returned HTTP 429). They contain transcription errors (for example "Vulcan" for Vulkan, "frustm" for frustum). Timestamps below are approximate, converted from the caption times. The videos are personal dev logs. Their claims ("10 times faster", "1500 FPS") are the authors' own, with no hardware given. Jokes in the captions are ignored.

### Video A: CJ94gOzKqsM (Unity, block game, "4 trillion blocks")

Source: https://www.youtube.com/watch?v=CJ94gOzKqsM

| Approx. time | Technique described |
|---|---|
| 0:48 | Blocks stored in a data array, one mesh per chunk instead of one object per block |
| 0:59 | Emit four vertices only for faces not hidden by solid neighbours (face culling) |
| 1:28 | Frustum culling (faces outside the view are dropped) |
| 1:35 | Split the mesh into one piece per side so faces pointing away from the camera can be disabled |
| 2:01 | Compact vertex data: whole-number positions stored in the smallest integer type |
| 3:18 | Burst compiler and rewrite of hot code; author reports chunk generation and rendering "10 times faster" |
| 4:13 | LOD: distant chunks use coarser voxels (one block per 8 m^3), far chunks merged into fewer objects |
| 4:33 | Greedy meshing (merging adjacent faces into rectangles); author says it was hard to implement, took code from Reddit |
| 5:32 | Chunk generation moved off the main thread (job system) |
| 9:20 onward | Ambient occlusion, volumetric light, cascaded shadows, TAA (visual, not speed) |

### Video B: Bmx7kup3gpo (C++ / Vulkan, "binary greedy meshing")

Source: https://www.youtube.com/watch?v=Bmx7kup3gpo

| Approx. time | Content |
|---|---|
| 0:00 | Title: "binary greedy meshing" |
| 4:26 | Vulkan start: swapchain, command buffers, synchronisation |
| 5:50 | First triangle after about 2000 lines of code |
| 7:25 | Matrices, perspective projection, camera |
| 8:13 | Depth testing |
| 9:40 | Optimisation phase: "Vulkan indirect draw indexed, push constants, LOD, depth pyramid occlusion culling"; author reports "over 1500 FPS with the same render distance" compared with his Unity version (no hardware given) |
| 10:04 | Author notes that further speed-ups have diminishing returns in absolute frame time |

The video itself does not explain the algorithms in detail; the sources in section 3 do.

## 3. Techniques, ranked by gain/effort

Gains are taken from cited sources where they exist. "Our estimate" marks my inference. Figures from GitHub READMEs and blogs were read through a summarising fetch; check the original before quoting.

### 3.1 Skip empty and invisible chunks (frustum culling, empty-chunk skip)

- What: test each chunk's bounding box against the camera frustum on the CPU; do not draw chunks outside it, or with no quads.
- Source: Video A at 1:28; Ascendant GPU-driven engine uses per-chunk frustum planes in a compute shader (https://www.vkguide.dev/docs/ascendant/ascendant_geometry/).
- Gain: proportional to the share of chunks outside the view. Our estimate: for a 90 degree horizontal FOV, roughly 1/4 of a ring of chunks is visible, so 2 to 4 times fewer draws in typical views. Exact value depends on our data.
- Effort: low (AABB test per chunk, one loop).
- Vulkan: 1.0 only.

### 3.2 Hidden-face culling at chunk borders

- What: do not emit a face whose neighbour voxel is opaque, including neighbours in adjacent chunks.
- Source: Video A at 0:59; Vercidium's chunk mesher checks neighbour blocks at chunk borders (https://github.com/Vercidium/voxel-mesh-generation). 0fps reports interior-face culling reduces a solid 8^3 cube from 3072 quads (naive) to 384 (https://0fps.net/2012/06/30/meshing-in-a-minecraft-game/).
- Gain: large if we do not already do it. Verify first: 1750 quads per 32^3 chunk is in a range where hidden faces are probably removed already; check the mesher.
- Effort: low. Vulkan: 1.0.

### 3.3 Greedy meshing (binary greedy meshing)

- What: merge coplanar, same-type faces into maximal rectangles, so fewer quads and vertices. "Binary" variant uses 64-bit occupancy masks and bitwise operations to process 64 faces at once.
- Sources:
  - 0fps (greedy meshing article): on a demo, naive vs greedy quad counts were 690 vs 22 (cube-like shape), 4770 vs 2100 (sphere), 2198 vs 1670 (noise). Solid 8^3 cube: 384 (culled) vs 6 (greedy). The article gives no timing gains.
  - cgerikj binary greedy meshing (https://github.com/cgerikj/binary-greedy-meshing): reports about 50 to 200 us per chunk typical, and 74 us average single-threaded on a Ryzen 3800X for its screenshot chunks; version 2 "several times faster" than version 1 (no multiplier given). Quads are 8 bytes each (see 3.5).
  - Vercidium README (https://github.com/Vercidium/voxel-mesh-generation): a simpler X/Y-run merge gives "~20% more triangles than greedy meshing and runs ~390% faster" (author's claim; it chose this over full greedy because greedy was too slow for its case).
- Gain: fewer quads, so less vertex fetch and rasterisation. Ratio depends strongly on terrain: 1.3x (noise) to 30x+ (flat, cube-like). Our 1750 quads/chunk on terrain is likely in the range where greedy gives a clear gain. Measure on our own chunks.
- Effort: medium (the non-binary version is simple; binary greedy needs bit-twiddling and careful tests).
- Vulkan: 1.0.

### 3.4 Compact quad or vertex format

- What: pack each quad into a few bytes and decode in the shader.
- Sources:
  - cgerikj: 8 bytes per quad (6-bit x, y, z, width, height in 4 bytes, voxel type in the other 4) (https://github.com/cgerikj/binary-greedy-meshing).
  - Vercidium blog (Unity voxel engine): one uint per vertex (18-bit position, 5-bit texture, 4-bit health, 3-bit normal); the post gives no FPS figures (https://vercidium.com/blog/voxel-world-optimisations/).
  - Ascendant engine: a 32-bit packed block for cube voxels and 12-byte meshed vertices (16-bit positions, octahedral normal) (https://www.vkguide.dev/docs/ascendant/ascendant_geometry/).
  - Video A at 2:01: smallest integer types for whole-number positions.
- Gain: less memory traffic and VRAM. No source gives a GPU-time figure for this alone. Our estimate: a 2x to 4x reduction in vertex bytes is possible; the effect on GPU time is measurable only after the quad count is reduced.
- Effort: low to medium. Vertex pulling (storage buffer read in the vertex shader) is core in Vulkan 1.0; no feature needed for reads.

### 3.5 One big buffer and multi-draw indirect (one draw for all chunks)

- What: store all chunk meshes in one large vertex/index buffer (a "gigabuffer" with sub-allocation) and issue one indirect draw for all visible chunks, instead of one draw per chunk.
- Sources:
  - cgerikj: all chunks drawn in one call, glMultiDrawElementsIndirect.
  - Ascendant: 400 MB shared buffer with sub-allocation; per-chunk indirect command written by compute with atomicAdd; "indirect buffer + indirect count" reused for shadow and main pass.
  - Nvidium-vk (Minecraft mod, Vulkan): one indirect draw for all geometry. Its own benchmark: about 286 FPS (Vulkan Sodium) vs about 303 FPS (Nvidium-vk) with Distant Horizons; the author says the fixed per-frame cost does not pay off at render distance 16 (https://www.curseforge.com/minecraft/mc-mods/nvidium-vk; figures from its page via a summary, check before quoting).
  - Voxy (Minecraft): multi-draw indirect section renderer after GPU culling (https://deepwiki.com/MCRcortex/voxy, auto-generated page).
- Gain: removes per-chunk CPU binds and draw calls. Nvidium-vk's gain is small, but its game is CPU-limited. For us the gain is unknown until we know whether the 5 us per chunk is CPU submission (see section 5). Our estimate: likely a large part of 5 us per chunk, if the bottleneck is per-draw overhead.
- Effort: medium (allocator, indirect buffer, per-chunk offsets).
- Vulkan: `vkCmdDrawIndexedIndirect` with drawCount greater than 1 needs the optional `multiDrawIndirect` feature (core 1.0 `VkPhysicalDeviceFeatures`). Without it, loop over per-chunk `vkCmdDrawIndexedIndirect` calls (one per chunk, but no CPU rebinding). `vkCmdDrawIndexedIndirectCount` (GPU-chosen count) needs Vulkan 1.2 or `VK_KHR_draw_indirect_count` (https://docs.vulkan.org/spec/latest/chapters/features.html).

### 3.6 Visibility graph for caves (Tommo's cave culling)

- What: per chunk, a 15-bit table of which pairs of its six faces are connected through open space (flood fill of non-opaque voxels). The camera's reachable chunks are found by BFS through the graph; unreachable chunks are skipped.
- Source: Tommo, "The Advanced Cave Culling Algorithm" (https://tomcc.github.io/2014/08/31/visibility-1.html). Reports culling of 50% to 99% of geometry in caves; bit update about 0.1 to 0.2 ms, moved to a background thread; earlier wall-based culling removed only about 1 in 100 chunks in caves. Author rejects hardware occlusion queries (GPU stalls).
- Gain: large in caves and underground, small on open terrain.
- Effort: medium (flood fill per chunk, BFS per frame, rebuild on block change).
- Vulkan: 1.0 (CPU only).

### 3.7 Level of detail for distant chunks

- What: distant chunks use coarser voxels or merged chunks, so far regions have far fewer quads.
- Sources: Video A at 4:13 (one block per 8 m^3, then merging low-detail chunks); Voxy (hierarchical LOD, GPU culling) (https://github.com/MCRcortex/voxy); Aokana LOD and streaming (https://arxiv.org/html/2505.02017v1).
- Gain: large at long render distance, since the cost is per quad and far chunks dominate counts. The Aokana paper gives no FPS numbers for LOD alone.
- Effort: high (LOD meshes, seams between LOD levels, data flow).
- Vulkan: 1.0.

### 3.8 Hi-Z occlusion culling (depth pyramid)

- What: build a depth pyramid from the previous frame; test chunk bounding boxes against it; re-test culled items after the current frame's depth.
- Sources: Aokana (RTX 3060 Ti, Vulkan via Unity): Hi-Z culling of 8x8 tiles, re-test pass; the paper reports 6 ms average at 64K resolution but gives no culling-only gain (https://arxiv.org/html/2505.02017v1). Video B at 9:40 names "depth pyramid occlusion culling" with no figure. Tommo rejects hardware occlusion queries, not Hi-Z.
- Gain: unknown for us; large on dense terrain behind hills or walls, small in open views. Measure overdraw first (section 5).
- Effort: high (compute pass, pyramid, one-frame latency, sync).
- Vulkan: compute and images are core 1.0; needs explicit barriers.

### 3.9 GPU-driven culling (compute writes indirect commands)

- What: a compute shader tests chunks and writes indirect commands with an atomic counter.
- Source: Ascendant (https://www.vkguide.dev/docs/ascendant/ascendant_geometry/): "culling on CPU for such a high number is a non-starter" at about 400,000 chunks. For our 3721 chunks, CPU culling is almost certainly cheap enough (our estimate: well under 0.1 ms with a plain AABB loop). Do this when chunk counts rise far beyond 10k.
- Effort: medium.
- Vulkan: compute and storage buffers are 1.0; indirect count needs 1.2 or the KHR extension.

### 3.10 Threading of meshing (not a GPU-time gain)

- Sources: Video A at 5:32 (job system); Vercidium repo (per-thread meshing; 12-thread numbers, e.g. 799 chunks 421 ms single vs 110 ms multi-threaded) (https://github.com/Vercidium/voxel-mesh-generation). Video A also says chunk generation was a bottleneck, not rendering.
- Gain: removes load hitches, not frame GPU time.

## 4. Vulkan feature summary for a 1.0 fallback

| Technique | Needs beyond Vulkan 1.0 core? |
|---|---|
| Frustum/empty culling (CPU) | No |
| Border face culling | No |
| Greedy / binary greedy meshing | No |
| Compact quads, vertex pulling | No (storage buffer reads are core) |
| Per-chunk draws from one big buffer | No |
| Multi-draw indirect (drawCount > 1) | Optional feature `multiDrawIndirect` (core 1.0 struct); fallback: loop of per-chunk indirect draws |
| Indirect count (GPU-chosen count) | Vulkan 1.2 or `VK_KHR_draw_indirect_count` |
| Visibility graph (caves) | No (CPU) |
| LOD | No |
| Hi-Z / compute culling | No (compute and storage are core), but needs careful barriers |

Mesh shaders (used by Nvidium per Sportskeeda: https://sportskeeda.com/minecraft/minecraft-distant-horizons-vs-nvidium-how-different-two-render-mods) are NVIDIA-specific in that write-up; do not use them as the baseline.

## 5. Measuring true GPU time and presentation-bound vs GPU-bound

Sources: Vulkan Guide profiling page (https://docs.vulkan.org/guide/latest/profiling.html) and timestamp sample (https://docs.vulkan.org/samples/latest/samples/api/timestamp_queries/README.html).

- Placement: write one timestamp at the start of the measured work with `VK_PIPELINE_STAGE_TOP_OF_PIPE_BIT`, and one at the end with `VK_PIPELINE_STAGE_BOTTOM_OF_PIPE_BIT`. Reset the query pool first with `vkCmdResetQueryPool` in the same command buffer.
- Conversion: `(end - start) * timestampPeriod` gives nanoseconds (the guide divides by 1e6 for ms). `timestampPeriod` is in `VkPhysicalDeviceLimits`. The queue family must support timestamps (`timestampValidBits` nonzero) — this is from the spec, not from the fetched pages.
- Read back next frame (`VK_QUERY_RESULT_64_BIT`, wait only if needed), so the read does not stall.
- Limit: the Vulkan guide notes that timestamp-based timings "don't account for pipeline overlap" when compared with hardware counters. Use per-pass timestamps and a tool such as RenderDoc or Nsight Graphics to confirm per-draw costs.
- Presentation vs GPU bound:
  - Vulkan guide: confirm GPU-bound when frame time is dominated by GPU execution rather than CPU wait time.
  - Unity frame timing (https://docs.unity.com/en-us/engine/current/script-reference/unityengine/frametiming): classifies as GPU-bound when GPU time is close to frame time and CPU time is not.
  - Practical test (our inference): with present mode IMMEDIATE (uncapped) and GPU timestamps, if the GPU time is 1.9 ms but the frame interval is pinned at a refresh period, the frame is waiting on presentation; if the interval tracks the GPU time, it is GPU-bound. If GPU time is small but the frame is slow, look at CPU submission (fences, per-chunk binds).
  - Caveat: the sources above do not give an exact decision rule; the rule above is inference.

## 6. Recommended order for our engine

Ranked by gain/effort, assuming section 1 holds:

1. **Measure first (hours).** Check timestamp placement, that the 1.9 ms covers only the chunk pass, and that presentation mode is not limiting. Count draw calls and quads per frame. Decide whether the 5 us per chunk is CPU submission or GPU work.
2. **Confirm border face culling in the mesher** (3.2). Cheap if already present.
3. **CPU frustum and empty-chunk culling** (3.1). Low effort, direct gain, Vulkan 1.0.
4. **Greedy meshing** (3.3). Largest quad reduction in the cited sources; test on our real chunks and record the quad count before and after.
5. **Compact quad format** (3.4) once quads are fewer.
6. **One big buffer, one indirect draw, with a per-chunk fallback when `multiDrawIndirect` is missing** (3.5). Re-measure: this should be the main CPU-side gain if section 5 points to submission overhead.
7. **Visibility graph for caves** (3.6), when underground content matters.
8. **LOD** (3.7) for long render distance; this is needed for thousands of FPS at large view ranges, but it is the most expensive.
9. **Hi-Z / GPU culling** (3.8, 3.9) only if measurement shows overdraw or occlusion is the limit.

## 7. What is verified and what is not

Verified by a source (read through fetch or search, numbers as summarised):
- Greedy quad counts and face culling ratios (0fps); binary greedy timings and quad format (cgerikj README); Vercidium triangle and speed comparison (README); Tommo culling ratios and bit-update cost; Vulkan feature definitions (`multiDrawIndirect`, `drawIndirectFirstInstance`, indirect count in 1.2); timestamp query placement (Vulkan guide); Aokana frame time, VRAM and techniques (arXiv).

Not verified (our inference or missing):
- That our 5 us per chunk is CPU-side. No source gives a number for our hardware and data.
- The gain from frustum culling for our camera and view ranges.
- The gain from greedy meshing on our terrain (the cited ratios are for other data).
- The "thousands of FPS" claim in the videos: no hardware or scene is given, and the 1500 FPS figure is the author's own.
- Hi-Z gains: no source gives a culling-only number.
- Both Minecraft mods (Nvidium-vk, Voxy) are described from mod pages and an auto-generated wiki; treat their numbers as indicative only.

Sources used:
- https://www.youtube.com/watch?v=CJ94gOzKqsM (captions only)
- https://www.youtube.com/watch?v=Bmx7kup3gpo (captions only)
- https://github.com/cgerikj/binary-greedy-meshing
- https://0fps.net/2012/06/30/meshing-in-a-minecraft-game/
- https://github.com/Vercidium/voxel-mesh-generation
- https://vercidium.com/blog/voxel-world-optimisations/ (summary did not give numbers)
- https://tomcc.github.io/2014/08/31/visibility-1.html
- https://www.vkguide.dev/docs/ascendant/ascendant_geometry/
- https://arxiv.org/html/2505.02017v1 (Aokana)
- https://www.curseforge.com/minecraft/mc-mods/nvidium-vk
- https://deepwiki.com/MCRcortex/voxy
- https://github.com/MCRcortex/voxy
- https://sportskeeda.com/minecraft/minecraft-distant-horizons-vs-nvidium-how-different-two-render-mods
- https://docs.vulkan.org/guide/latest/profiling.html
- https://docs.vulkan.org/samples/latest/samples/api/timestamp_queries/README.html
- https://docs.unity.com/en-us/engine/current/script-reference/unityengine/frametiming
- https://docs.vulkan.org/spec/latest/chapters/features.html
