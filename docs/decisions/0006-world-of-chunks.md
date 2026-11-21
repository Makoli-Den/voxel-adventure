# 0006. A world of many chunks

Status: accepted.

## Context
`vox-world` knew one chunk. The game needs many: terrain that continues from one chunk into the next, and a mesher that does not draw the faces two touching chunks hide from each other.

## Decision
- **`World` is a `HashMap<ChunkPos, Chunk>`** and nothing more: `insert`, `get`, `contains`, `len`, `iter` and `neighbours`. Generating, loading and meshing stay with the caller.
- **Cubic chunks.** `ChunkPos` has three coordinates, so the world has no height limit and the same code handles up, down and sideways.
- **Terrain is sampled at world coordinates.** `generate_chunk(seed, pos)` computes the surface height of a column from its world column `(x, z)`; the chunk only decides which part of the column it holds. Neighbouring chunks therefore agree along their borders by construction. The noise uses floor division for negative coordinates and feeds the lattice hash the cell as `u32` by two's complement, so the origin chunk is bit for bit what `generate_terrain` produced before (the pinned fingerprints did not change). The block rule per world height `y` and surface height `h` is: `y > h` air, `y == h` grass, `y + 3 >= h` dirt, else stone. As `h` is always in 8..=24, every chunk with `pos.y >= 1` is all air and every chunk with `pos.y < 0` is all stone.
- **Neighbour policy.** `build_mesh_with_neighbours_into(chunk, &Neighbours, mesh)` takes the six adjacent chunks as `Option<&Chunk>` per face. A border face is hidden only if the block it touches in the neighbouring chunk is solid. A missing neighbour counts as air, so the faces on the edge of the loaded area are drawn, which is what is wanted at the edge of the world. `build_mesh_into` is the same function with `Neighbours::none()`.
- **The common path stays cheap.** A face whose neighbour is inside the chunk takes the same test as before; only the faces on the border look into `Neighbours`.

## Reasons
- **HashMap:** no fixed size or shape, chunks are added and dropped as the player moves, and a lookup happens once per chunk, not per block.
- **World coordinates for terrain:** no seams and no second pass to stitch borders; any chunk can be generated alone and in any order, which is what worker threads and the network need.
- **`Neighbours` is a small `Copy` value of six references:** the mesher does not know about `World`, and tests and benchmarks can build one for chunks that live anywhere.
- **Missing means air:** treating it as solid would remove the faces on the edge of the loaded area, and the world would look hollow there.

## Rejected
- **Padded chunks** (a border layer of copied blocks around every chunk): about 12 percent more memory per chunk and the copies must be kept in sync on every edit.
- **A per-face lookup through the `World`:** a hash lookup for every border face, thousands per chunk, and the mesher would depend on the storage.

## Risks
- A chunk meshed before its neighbour exists has faces that the neighbour hides; it must be meshed again when the neighbour appears (and when a border block of a neighbour changes). Tracking that is the caller's job; the world does not remember which mesh was built from which neighbours.
- Far from the origin (beyond about 67 million chunks) the coordinates saturate instead of wrapping; the terrain there is not meaningful.
