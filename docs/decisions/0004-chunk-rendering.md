# 0004. Drawing a chunk mesh

Status: accepted; the interface, the buffers, the push constants and the freeing of replaced meshes were changed by decision 0007 (many chunk meshes).

## Context
The renderer drew a hard-coded triangle. The next slice draws a real chunk mesh from `vox-world` in 3D, which needs vertex and index buffers, a depth buffer, a camera and back-face culling, while staying on plain Vulkan 1.0 for old GPUs (decision 0002).

## Decision
- **Interface:** the `Renderer` trait gains `set_mesh(&ChunkMesh)` and `set_camera([[f32; 4]; 4])`. The matrix is a column-major view-projection that already produces Vulkan clip space (Y down, depth 0..1); the renderer applies it unchanged. Mesh coordinates are chunk-local blocks and there is no model matrix: one chunk at the origin.
- **Buffers:** each mesh gets one vertex and one index buffer, each with its own allocation in **host-visible, host-coherent** memory, filled through a mapped copy. `set_mesh` uploads the new mesh first, then waits for the device to go idle and frees the old buffers. Indices are checked against the vertex count before the upload, because the device has no `robustBufferAccess` and `ChunkMesh` is plain data any caller can build; bad data is a `RenderError::InvalidMesh`. Empty buffers are refused, since Vulkan forbids zero-size buffers.
- **Vertex layout:** 6 bytes per vertex, serialized explicitly (no `unsafe`): `[x, y, z, face, block_lo, block_hi]`, little-endian. Attribute 0 is `R8G8B8A8_UINT` at offset 0: it reads the position in `xyz` and the face index in `w`. Attribute 1 is `R16_UINT` at offset 4: the block id. Indices are `u32`. The layout is described once, in `vox-render/src/interface.rs`; the Vulkan vertex input state and the shader interface check are both built from it, so they cannot drift apart.
- **Depth buffer:** the first of `D32_SFLOAT`, `X8_D24_UNORM_PACK32`, `D24_UNORM_S8_UINT` that supports depth attachments with optimal tiling, chosen once at startup. One depth image per swapchain, created and destroyed with it; cleared to 1.0, never stored, compared with `LESS`. The render pass dependency at the start of the frame also covers the depth tests, because frames in flight share the depth image.
- **Camera:** a 64-byte push constant (`mat4`) for the vertex stage. No descriptor sets yet. (Decision 0007 grows it to 80 bytes: the matrix plus the chunk origin.)
- **Culling:** back faces are culled, `front_face = COUNTER_CLOCKWISE`. The mesher winds faces counter-clockwise seen from outside in a right-handed, Y-up world. The projection flips Y so the image is upright, which keeps the winding the viewer sees counter-clockwise, and Vulkan defines facing in framebuffer coordinates with Y down, where that is `COUNTER_CLOCKWISE`. Checked by eye: with `CLOCKWISE` the far inner walls and the underside of the chunk show instead.
- **Shading:** no textures and no lighting yet. The vertex shader picks a base color per block id from a small palette (magenta for unknown ids) and multiplies it by a fixed brightness per face direction.

## Reasons
- **Host-visible buffers:** the simplest upload path: no staging buffer, no transfer commands, no extra synchronization. Integrated GPUs (a large share of old devices) have only unified memory, where this is already the best choice. A staging copy into device-local memory pays off on discrete GPUs once many chunks are drawn; it is added when a benchmark shows the vertex fetch cost.
- **Explicit serialization:** independent of how the compiler lays out `ChunkVertex`, no `unsafe`, and testable byte by byte.
- **`R8G8B8A8_UINT`:** Vulkan 1.0 requires every implementation to support it as a vertex format; the 3-component 8-bit formats are optional. Packing the face index into the fourth byte means no byte is wasted.
- **Depth candidates:** Vulkan 1.0 guarantees that `X8_D24_UNORM_PACK32` or `D32_SFLOAT` supports depth attachments, but neither one alone. `D32_SFLOAT` is preferred for precision.
- **Push constants:** 64 bytes fit the 128 bytes every device guarantees, need no buffer, descriptor set or per-frame synchronization, and are the fastest way to change a small value per draw.

## Rejected
- **`R8G8B8_UINT` for the position:** not guaranteed as a vertex format in Vulkan 1.0.
- **The `gpu-allocator` crate (for now):** two kinds of allocations do not need a general allocator. Sub-allocation becomes necessary when there are many chunks (drivers allow as few as 4096 allocations); the choice between a crate and a small own allocator is made then.
- **A model matrix per chunk (for now):** only one chunk is drawn, at the origin. Chunk positions will come with multiple chunks, as a push constant or per-draw data.

## Risks
- Host-visible vertex memory is slower to read on discrete GPUs. Acceptable for one chunk; measured before many chunks are drawn.
- One allocation per buffer runs into the allocation limit with many chunks; see "Rejected".
