# 0011. Block editing in the game

Status: accepted.

## Context
Decision 0010 made every streaming job self-contained and dropped the generated chunks after meshing, because nothing read blocks. Breaking and placing blocks is the first feature that changes blocks after generation, which was named there as the trigger to revisit that. `vox-world` already has `World::block_at`, `World::set_block` (which reports the chunks whose mesh went stale) and `raycast`.

## Decision
- **The main thread owns a `World`** of the loaded chunks. A streaming job returns the generated `Chunk` together with its mesh (`job::Built`, the chunk is moved, not copied). The tracker hands loaded chunks out, `Changes` has a new `loaded: Vec<(ChunkPos, Chunk)>` next to `uploads`, and unloaded positions are removed from the `World` (`World::remove`), so memory stays flat: unloaded chunks are dropped. Chunks with nothing to draw also go to the `World` (the player can build in air), but cost no upload budget.
- **The job stays self-contained.** It still generates the four horizontal neighbours itself and meshes without a shared world. The two-stage pipeline of 0010 is not needed: the `World` is only written by the main thread and workers never read it.
- **Editing:** a left click asks to break, the F key asks to place (the right mouse button stays "look"). The request is remembered until the next frame (`Controls::take_edit`). That frame casts one ray from the eye along the look direction, `REACH` = 6 blocks, with `vox_world::raycast`. The ray is cast only when an edit was asked for, because nothing highlights the target.
- **The rules are pure** (`editing.rs`): break writes air into the hit block; place writes `PLACE_BLOCK` (stone) into `hit.adjacent()`, unless that cell overlaps the player's box (decision 0012; this first version only protected the eye's block), or the ray started inside a solid block (no face to place on).
- **Remeshing on the main thread:** after `set_block`, every chunk in `ChangedChunks` is meshed again with `job::remesh` into one reused scratch mesh and given to `set_chunk_mesh` (an empty mesh removes the entry). `remesh` takes neighbours from the `World`; a horizontal neighbour that is not loaded is generated, exactly as in a job, so a chunk at the edge of the area gets the same mesh whether it is built by a job or after an edit. The test checks both against the job's mesh.
- **Streaming never overwrites edits.** The tracker accepts one result per position between unloads, so a loaded chunk gets no second result. `apply_streamed` asserts this in debug builds and inserts. An edited chunk is replaced only after it was unloaded and loaded again.
- **Crosshair:** four overlay rectangles (dark outline, white plus) in the window centre, always visible, rebuilt on resize.

## Reasons
- Keeping the chunks in the `World` is the smallest change that gives `raycast` and `set_block` something to work on, and a `Box<[BlockId]>` chunk moves in a pointer-sized copy.
- Remeshing on the main thread is the simplest thing that works: one edit touches at most 4 chunks. Its cost is not measured (see the limitations).
- Casting only on a request costs nothing per frame.

## Rejected
- **Sending edits to the workers / a shared `Arc<World>`:** needed only when meshing gets expensive or edits are many; a single edit is cheaper than the hand-over.
- **Replacing the loaded chunk when a result arrives:** would silently undo the player's edits.
- **Right mouse button to place:** it is the look button, and holding it while clicking is awkward.
- **A selection highlight:** it needs a renderer feature (a line or box pass); the crosshair is enough for now.

## Known limitations
- **Edits are not saved.** A chunk that streams out of range and back in is generated again and the edits are gone. Saving needs the world format (a later decision).
- **Seams at the edge:** a chunk that loads next to an already edited chunk is meshed by its job from the generated terrain, so its border faces ignore the neighbour's edits until it is edited itself. The same for a chunk that is reloaded next to an edited one. Fixing it means remeshing the loaded neighbours of a new chunk when they are edited.
- **No vertical streaming:** blocks can only be placed in the terrain layer `y = 0` (up to the top of that chunk); `set_block` returns `None` for an unloaded chunk and nothing happens.
- **The player's body** arrived with decision 0012; placement keeps clear of its box.
- **Remesh cost is unmeasured and may be several milliseconds** for a chunk at the edge of the area: each changed chunk generates up to 4 missing horizontal neighbours before meshing, and one edit can change 4 chunks. Measure it, then move remeshing to the workers if it causes frame hitches.
- **Future work:** a selection highlight of the targeted block, and remeshing the loaded neighbours of a chunk that arrives next to an edited one.
- **Not measured:** remeshing cost on the main thread in the game (only the benchmark mesh time is known), and a window test of clicking and placing.
