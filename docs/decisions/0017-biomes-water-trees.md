# 0017. Biomes, water and trees

Status: accepted. Replaces the 8..24 heightmap of decision 0015's terrain (the world bounds, spans and tiles stay).

## Context
The terrain was one value-noise heightmap with grass over dirt over stone. The owner asked for a world: seas and oceans, rivers, lakes, mountains, forests, plains, deserts and beaches, with water, grass and trees, and it must stay fast and deterministic. It is also the first terrain with columns that span several chunk layers, so decision 0015's `column_span` and the tile streaming meet real mountains for the first time.

## Decision
- **The landscape is a pure function of the seed and the column** (`vox-world/src/landscape.rs`; the value noise moved to `noise.rs`). For a world column it gives the ground height, the water level above it and a `Biome`. Everything is integer hashing and `f32` `+ - * /` (no libm), so a seed gives the same world on every platform.
  - *Continents:* a three-octave field (cells 512, 128, 32). Below `COAST` (0.45) the ground falls to the sea floor (up to 44 blocks below `SEA_LEVEL` = 64), above it the land rises 14 blocks. Seas are about a third of the world and a thousand blocks wide.
  - *Hills:* three octaves, 12 blocks peak to peak on land, 3 on the sea floor.
  - *Mountains:* a mask field (cells 256, 128) turns on ranges inland; inside them 18 blocks of base rise plus a ridged field (the noise folded at its middle, cells 64, 32) up to 66 more. The ground is clamped to `MAX_SURFACE` = 160, so a mountain column spans up to six chunk layers (blocks 64 to 160 are layers 2 to 5).
  - *Rivers:* the contour of a field at its middle, 5 percent of its range wide, cut down to 3 blocks under the sea level with a smooth bank, only on land and fading out in mountains. The cut ground is below the sea level, so water fills it.
  - *Lakes:* one candidate per 256 x 256 lake cell, its centre and radius (10 to 24 blocks, warped by noise) from the cell hash, entirely inside the cell, so a column only needs its own cell's lake. The lake level is one block under the lowest of five ground samples (middle and shore), the bowl is dug below it and a rim of 5 blocks is raised to at least one block above it, so the water cannot run out (tested). Lakes only on land between 4 blocks over the sea and the mountain line.
  - *Water* fills every air block above the ground up to the column's water level: `SEA_LEVEL`, or the lake's own level inside a lake. A column's top is `max(ground, water level)`.
  - *Biomes:* under water `Ocean` / `River` / `Lake`; dry ground at or above `MOUNTAIN_LINE` (96) `Mountains`; up to two blocks over the sea level `Beach`; else the climate (two noise fields, each sampled once per column): hot and dry `Desert`, wet `Forest`, rest `Plains`. Beaches only along coasts (within 0.03 of the coast value of the continent field); inland low ground is grass. Over several seeds the dry land is about 10 percent desert, 34 plains, 29 forest, 12 beach and 16 mountains (a test pins the ranges). Surface blocks by biome: grass over dirt (plains, forest), sand (beach, desert, shallow floors), gravel (sea floor deeper than 10 under the sea level), stone (mountains) with snow above 128; stone below.
- **Trees are hashed, not stored** (`trees.rs`): the world is cut into 4 x 4 tree cells with at most one trunk each, whose column the hash picks; forests keep 175 of 256 candidates, plains 15. A chunk asks for every tree cell within the leaf radius (2) of its columns and stamps what falls inside it, logs of all trees first, then leaves, and leaves only fill air: so chunks generate independently, agree at borders, and no tree replaces a log, water or ground, in any order. Trunk 4 to 7 logs, a round leaf blob of radius 2 centred on the top log. The fields of the hashes (offsets, chance, trunk height; lake chance, centre, radius) have named positions and widths; the lake radius has a hash of its own so its range reduction is not biased. Only on grass (plains and forest). Chunk layers far from the land skip the search.
- **One landscape per column of chunks:** `generate_column` evaluates the 1,024 block columns and the trees once for all layers of a span, and a column job generates its own column and its four neighbours that way (five evaluations whatever the number of layers, not five per layer).
- **Generating less:** a chunk entirely above `MAX_SURFACE` plus the tallest tree is air and a chunk entirely below the lowest ground minus the filler layer is stone, both without sampling any noise.
- **`column_span`** uses the top (water included) and covers trees: the lowest layer is the lowest top of the 34 x 34 ring, the highest is the highest top of the 32 x 32 columns or, where a tree may stand (grass in the 36 x 36 area), the ground plus 9. The tree part is a bound (the tallest tree wherever grass is), so a layer may be included without a tree in it. Tests: no layer outside the span meshes to anything with water and trees included; layers below are all non-air and above all air.
- **Level of detail:** `generate_lod_chunk` samples the same landscape at the cell centre. The cell between the ground cell and the water surface is water, so far tiles show seas, mountains and biome surface blocks; level 0 equals the tree-less chunk (tested); tiles ignore trees at every level. A level-`k` tile is `2^k` times taller than a chunk, so the mountains need several tile layers: `tile_layers(level)` (level 1: layers 0 to 2, level 2: 0 to 1, levels 3 and 4: 0). **Every layer of a tile column is a tile of its own** (its own key, job and mesh): the selection still picks tile columns, `Plan` expands them. A layer meshes alone with the solid stand-in below, so a bottom face is always hidden; a layer under another one has a few hidden top and wall quads (a uniform solid layer is five quads). **Layers under the sea are not built** (`tile_layers_in`): every column's top is at least the sea level, so every cell row below the one that holds it is solid with solid neighbours (tested), which spares level 1 its layer 0. Layers above flat land are still built and found empty by their job: a cheap bound for "no mountain in this tile" does not exist, because the lattice maximum of the mountain field is always above its threshold (the threshold is near the field's median); a per-tile height cache would be needed.
- **Water for the player:** the body walks and falls through water, sinks at most 3 blocks per second, wades at 60 percent of the walking speed and swims up at 4 blocks per second while the jump key is held (`Space { liquid }`, `is_liquid`). The edit ray passes through water (`editing::ray_sees`), so the player can dig and build under water; a block placed against the floor goes into the water above it. The player spawns on land: `spawn_point(seed)` searches square rings from the origin for a grassy column; the camera starts 30 blocks above it.
- **Data:** `blocks.json` gets `water`, `leaves`, `snow`, `gravel` appended after bedrock (ids 9 to 12; the earlier ids are unchanged) with generated 16 x 16 textures. `content.rs` resolves `sand`, `gravel`, `snow`, `water`, `log`, `leaves` by name; a pack that drops one is rejected like for grass.

## Reasons
- A pure column function lets the chunk generator, the tile generator, the span scan and the spawn search share one definition of the world, so they cannot disagree (a test compares every block of a chunk with the rule).
- Hash-placed trees need no stored state and no ordering between chunks; two passes make the result order-independent.
- Putting every lake inside its own cell keeps a column's cost to one lake lookup; the generator needs no neighbour cells.
- Tile layers as separate tiles reuse the tracker, scheduler and renderer keys unchanged (`TilePos` already has a `y`).

## Measured (release, criterion `--quick`, same machine, noisy by 10 to 20 percent)
| case | before | after |
| --- | --- | --- |
| `terrain/generate_seed_1` (a surface chunk; after: with trees) | 46 us | 157 us |
| `terrain/generate_chunk_far` | 43 us | 131 us |
| `terrain/generate_lod_level_1 / 2 / 4` | 53 / 47 / 40 us | 73-92 / 118 / 112 us |
| `terrain/column_span` | 38 us | 135 us |
| `mesh/terrain_seed_1` (greedy, 760 / 757 quads) | 171 us | 160 us |
| `mesh_reused/terrain_seed_1` | 207 us | 158 us |

The generator costs about 3 times a chunk for 15 noise octaves per column against 3, mostly the hash (`mix`); the row hash shared by two lattice points and a multiply for the cell scale made no measurable change. In the game (default settings, radius 8, horizon 2048, one quiet window run each, 800 x 600): start view 1827 fps and 196 k triangles before, 1696 fps and 247 k after; a high view 1480 fps with 196 k triangles (sea in view). The average column job went from 0.56 to 1.52 ms (mountain columns have several layers; each layer is generated five times), the tile job went from 0.21 to 0.25 ms; meshes loaded rose from 927 to 1357 (tile layers).

After the review round (tile layers under the sea dropped, one landscape per column job, the biome retune): 1,081 tiles and 1,125 meshes loaded at the defaults (from 1,312 and 1,357), average column job 1.43 ms; a high view over green land with a lake and snow: 1,579 fps, 279 k triangles.

## Known limits and deferred
- **Water is opaque** (decision 0013 has no alpha): the sea floor is not seen through it, and the water surface is a flat blue top. Transparency needs a second draw pass and sorting.
- **Plants and flowers** need models that are not cubes; tall grass, flowers and saplings are deferred.
- **Caves** and overhangs are not generated: terrain is still a heightmap (the span scan relies on it).
- **Saving:** edits are still not saved; a regenerated chunk has its trees again.
- **Biome-dependent tint** of far tiles (grass is not tinted per biome) is deferred; far tiles use the block textures.
- A shallow river or lake rounds away at tile levels 3 and 4 (its water and ground fall in one cell row), so far away only seas and big lakes show water.
- The seam bound of decision 0015's first terrain (adjacent columns differ by at most 3) no longer holds: mountain slopes and river banks are steeper; nothing relies on it now (the span scan looks at every column).
- The generation cost is three times higher; a coarse sampling of the slow fields (continent, mask, climate) or a cheaper hash are the measured candidates if it ever limits streaming.
- **The mesh budget counts meshes, not leaves.** `MAX_LEAVES` (3000) is the budget of meshes (decision 0015); a tile of level 1 or 2 is two meshes now (`tile_layer_bound`), so `max_horizon_for` counts `mesh_cost` (one per level-0 leaf, the layer bound per tile) and the horizon at radius 16 is clamped lower than before. A test sums the real layers of a plan: at most 3000 at radius 16 with its clamped horizon, at most 1500 at the defaults (radius 8, horizon 2048). The budget is an upper bound: layers over flat land are built but have no mesh.
- Tile layers over flat land are still jobs (a generation each, no mesh); a per-tile height cache would skip them.
- **Breaking a block under water leaves an air pocket:** there is no water flow, so the hole stays dry until something fills it; placing a water block refills it by hand.
- The swimmer climbs onto a bank one block over the water (with the head out of the water a stroke is a jump); a bank two blocks over is out of reach.

## Rejected
- *Water level = sea level everywhere (lakes only where the terrain dips below it):* no lakes in the highlands, and rivers could not hold water above the sea.
- *One tile layer of a taller tile (scaling the cells vertically):* a tile is 32 cells tall in the mesher's `u8` positions; stretching cells would break the one-to-one cell grid.
- *Trees as stored blocks of generated chunks (each chunk generates its own and the neighbours' trees):* needs the neighbours' state or duplicate work per chunk; the hash cell search is stateless.
