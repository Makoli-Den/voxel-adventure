# 0007. Drawing many chunk meshes

Status: accepted.

## Context
Decision 0004 drew one chunk mesh at the world origin and freed a replaced mesh after `vkDeviceWaitIdle`. A world is many chunks, each at its own place, and chunks are meshed and replaced all the time while the player moves. The renderer must draw many meshes, place each one, and swap meshes without stalling, while staying on plain Vulkan 1.0.

## Decision
- **Interface:** the `Renderer` trait replaces `set_mesh` by `set_chunk_mesh(ChunkPos, &ChunkMesh)` (adds or replaces; an empty mesh removes the entry) and `remove_chunk_mesh(ChunkPos)`. Mesh coordinates stay chunk-local (0..=32 per axis); the renderer places the chunk at `pos.origin()`. The renderer keeps a `HashMap<ChunkPos, GpuMesh>` and draws every entry each frame in any order; the depth buffer sorts it out. Per chunk: bind the buffer as vertex and index buffer, push the constants, one indexed draw. Without meshes only the sky is drawn.
- **Push constants:** 80 bytes, vertex stage, one range at offset 0: the column-major view-projection `mat4`, then a `vec4` with the chunk origin in blocks in `xyz` (converted from `i32`, exact up to 2^24 blocks) and 0 in `w`. The vertex shader adds the origin to the local position before the matrix. The layout lives in `vox-render/src/interface.rs`; the shader check rejects blocks larger than 80 bytes.
- **One buffer per chunk:** vertices first, then the `u32` indices from the next multiple of 4 (vertices are 6 bytes, so the vertex part often ends unaligned, and `vkCmdBindIndexBuffer` needs an offset aligned to the index size). The buffer has VERTEX and INDEX usage and is bound twice with the two offsets.
- **Deferred freeing:** a replaced or removed mesh goes to a `retired` list, stamped with the number of frames submitted so far (`s`): only frames `0..s` can use it, because it left the map before the next frame was recorded, and recording and submitting happen in one call. Before frame `n` is recorded, the fence of its slot is waited on; it was signaled by frame `n - FRAMES_IN_FLIGHT`, and each earlier frame's fence was waited on before an earlier recording, so frames `0..=n - FRAMES_IN_FLIGHT` are known finished. A retired mesh is freed when that count reaches its stamp. With two frames in flight a mesh replaced between frames `s - 1` and `s` is freed at the start of frame `s + 1`. The policy is a pure type (`vulkan/retired.rs`) with unit tests. At renderer drop, everything is freed after the existing `vkDeviceWaitIdle`.

## Reasons
- **One buffer per chunk:** each buffer has its own device memory allocation, and Vulkan guarantees only 4096 allocations (`maxMemoryAllocationCount`). Two per chunk would halve the number of chunks before sub-allocation exists.
- **Push constants for the origin:** a chunk origin is 16 bytes and changes per draw. Push constants need no buffer, no descriptor set and no per-frame synchronization, and 80 bytes fit the 128 every device guarantees, so no capability check is needed.
- **Deferred freeing:** `vkDeviceWaitIdle` on every change drains the GPU and stalls the CPU; with chunks streaming in, that would happen many times per second. The fences the frame loop already waits on tell exactly when a mesh is safe to free.

## Rejected
- **A model matrix per chunk through a uniform buffer:** needs descriptor sets, a per-frame buffer and its synchronization. A chunk only moves, so a translation is enough, and it fits in push constants.
- **Wait-idle on every update:** simple, but stalls the frame for every mesh change; see above.
- **Dynamic uniform buffer offsets:** one descriptor set with per-chunk offsets works, but adds descriptor sets, alignment rules (`minUniformBufferOffsetAlignment`) and a per-frame buffer for what 16 bytes of push constants already do.
- **One big buffer for all chunks now:** the right end state (fewer allocations, fewer binds, later indirect draws), but it needs a sub-allocator with free lists and defragmentation. It comes later, together with sub-allocation, once measurement shows the need.

## Risks
- Every mesh is in host-visible memory with its own allocation. Slower vertex fetch on discrete GPUs, and the allocation limit (4096 on some drivers) caps the number of loaded chunks until sub-allocation exists.
- One draw call (and two binds plus a push) per chunk. Fine for hundreds of chunks; thousands need merged buffers or indirect draws.
- No frustum culling yet: chunks behind the camera are drawn too.
- Retired meshes are freed only while frames are drawn. While the window is minimized nothing is freed, so changing many meshes then holds their old memory until drawing resumes.
