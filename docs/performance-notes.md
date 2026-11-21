# Performance notes

Source of ideas: a breakdown of the video "My game is 262,000 times faster than Minecraft" (the author builds a voxel game in Unity), and of its follow-up "Making my game 10x faster (By starting over)" (the same game rewritten in C++ with Vulkan). The video never explains the 262,000 figure; it comes from the title.

## What we plan to apply
1. Blocks are stored in flat arrays, not in objects. One mesh per chunk.
2. Faces hidden by neighbouring blocks are not generated.
3. Frustum culling, and disabling mesh sides that face away from the camera.
4. Compact vertices: positions stored in bytes.
5. Cubic chunks, so the world is infinite in height as well.
6. Chunk generation and mesh building run off the main thread.
7. Chunk LOD: distant chunks at lower resolution, merged into larger meshes.
8. Greedy meshing.
9. Multiplayer: only the world seed and modified chunks go over the network.
10. GPU-driven rendering: indirect draw, push constants, GPU occlusion culling (depth pyramid).
11. Graphics (AO, volumetric light, shadow cascades, TAA) only after the world core is stable.

## Rule
Generate less, not faster. LOD and culling first, micro-optimizations after.

## Before choosing a solution
Check every speed decision with a measurement on a prototype: one chunk, generation time, mesh build time.
