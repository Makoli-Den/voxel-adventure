//! Block editing decisions: what to do when the player breaks or places a block.
//!
//! Pure code with no window, renderer or threads: given what the ray hit and what the player
//! asked for, [`plan`] says which block to write where, [`create_chunks_for_edit`] puts the
//! chunks under the terrain that the edit needs into the world, and [`apply_streamed`] keeps
//! the game's world in step with the chunk streaming. [`player_edit`] writes the player's
//! block and makes the event the mods hear of it, [`apply_mod_edits`] writes what they asked
//! for, and both collect the chunks to rebuild in [`StaleChunks`], each once. The game
//! (`main.rs`) casts the ray and rebuilds the meshes (decisions 0011, 0014 and 0015).

use std::collections::HashSet;

use vox_mods::{BlockEdit, BlockEvent, BlockEventKind};
use vox_world::{
    BlockId, ChangedChunks, ChunkPos, LOWEST_CHUNK_LAYER, RayHit, WORLD_FLOOR_Y, World,
    chunks_touching, contains_block_y, contains_chunk_layer,
};

use crate::physics::Body;
use crate::streaming::{Column, ColumnSpans, LoadedColumn, Terrain};

/// How far the player reaches, in blocks, measured from the eye along the look direction.
/// About the reach of Minecraft's creative mode plus a margin: far enough to build without
/// walking up to every block, short enough that the block under the crosshair is clearly the
/// one meant.
pub const REACH: f32 = 6.0;

/// What the player asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditAction {
    /// Remove the block under the crosshair.
    Break,
    /// Put the selected block on the face of the block under the crosshair.
    Place,
}

/// One block to write.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Edit {
    /// World position of the block to write.
    pub position: [i32; 3],
    /// What to write there.
    pub block: BlockId,
}

/// Whether the engine forbids writing the block at `position`: the bedrock row at
/// [`WORLD_FLOOR_Y`] is the floor of the world and never changes (decision 0015). One rule for
/// the player's break ([`plan`]) and for every write of a mod ([`apply_mod_edits`]), so the two
/// cannot disagree. A mod may not even place into the row: it is bedrock already.
pub fn is_protected(position: [i32; 3]) -> bool {
    position[1] == WORLD_FLOOR_Y
}

/// Decides what `action` does when the player `body` looks at `hit`, or `None` if nothing may
/// happen.
///
/// - [`EditAction::Break`] writes air into the block that was hit, unless it lies in the
///   bedrock row at [`WORLD_FLOOR_Y`]: the floor of the world cannot be broken (an engine
///   rule, decision 0015). A refused break does nothing at all, so nothing else hears of it.
/// - [`EditAction::Place`] writes `block` into the cell in front of the hit face, unless that
///   cell lies outside the world's height bounds, overlaps the player's box (the player would
///   be stuck inside the block; this includes the cell of the eye), or the ray started inside
///   a solid block (`distance` is zero: there is no face in front of which to place anything).
pub fn plan(action: EditAction, hit: &RayHit, body: &Body, block: BlockId) -> Option<Edit> {
    match action {
        EditAction::Break => (!is_protected(hit.block)).then_some(Edit {
            position: hit.block,
            block: BlockId::AIR,
        }),
        EditAction::Place => {
            let position = hit.adjacent();
            let inside_a_block = hit.distance <= 0.0;
            let allowed =
                contains_block_y(position[1]) && !inside_a_block && !body.overlaps_cell(position);
            allowed.then_some(Edit { position, block })
        }
    }
}

/// Puts into `world` every chunk that an edit of the block at `position` touches
/// ([`chunks_touching`]) and that is missing. Chunks under a column's span are not streamed
/// (decision 0015), so the first edit that reaches one generates it here, before `set_block`,
/// and the walls the edit uncovers can be meshed. Returns how many chunks were created.
///
/// Only chunks inside the world's layers and in loaded columns (`spans`) are created: a chunk
/// outside the world would be stored for nothing, and one in a column that is not loaded would
/// never be unloaded. A chunk already in the world is never replaced.
pub fn create_chunks_for_edit(
    world: &mut World,
    terrain: &Terrain,
    spans: &ColumnSpans,
    position: [i32; 3],
) -> usize {
    let mut created = 0;
    for &pos in chunks_touching(position).as_slice() {
        let wanted = contains_chunk_layer(pos.y) && spans.contains(Column::of(pos));
        if wanted && !world.contains(pos) {
            world.insert(pos, terrain.generate(pos));
            created += 1;
        }
    }
    created
}

/// What the player's ray sees at the block `position`: the block of the world; air in a
/// chunk missing above a loaded column's span (the generator puts nothing there, decision 0015);
/// and `None`, which stops the ray, anywhere else: below a span the rock is solid, but its chunk
/// is not in the world, so it can not be edited until a dig reaches it, and a column that is not
/// loaded is unknown.
pub fn block_for_ray(world: &World, spans: &ColumnSpans, position: [i32; 3]) -> Option<BlockId> {
    if let Some(block) = world.block_at(position) {
        return Some(block);
    }
    let chunk = ChunkPos::containing(position).0;
    let span = spans.get(Column::of(chunk))?;
    (chunk.y > *span.end()).then_some(BlockId::AIR)
}

/// The chunks of the loaded columns that the player has written a block into.
///
/// Only these can differ from the generator, so only they can expose a wall of a column that
/// streams in next to them ([`create_chunks_beside_edits`]). Chunks that an edit or an
/// arriving column merely created hold generated rock and are not recorded, so creation never
/// triggers more creation. Entries leave with their column ([`apply_streamed`]).
#[derive(Debug, Default)]
pub struct EditedChunks {
    chunks: HashSet<ChunkPos>,
}

impl EditedChunks {
    /// Records that a block at world position `block` was written.
    pub fn record(&mut self, block: [i32; 3]) {
        self.chunks.insert(ChunkPos::containing(block).0);
    }

    /// Whether a block of the chunk at `pos` was written since its column was loaded.
    pub fn contains(&self, pos: ChunkPos) -> bool {
        self.chunks.contains(&pos)
    }

    /// Forgets every chunk of `column`.
    fn remove_column(&mut self, column: Column) {
        self.chunks.retain(|pos| Column::of(*pos) != column);
    }
}

/// The four columns next to `column`.
fn horizontal_neighbours(column: Column) -> [Column; 4] {
    [
        Column::new(column.x.saturating_add(1), column.z),
        Column::new(column.x.saturating_sub(1), column.z),
        Column::new(column.x, column.z.saturating_add(1)),
        Column::new(column.x, column.z.saturating_sub(1)),
    ]
}

/// Creates the chunks under the spans of the freshly loaded `columns` in every layer where a
/// neighbouring column holds an edited chunk (`edited`), and returns them together with those
/// edited chunks: all of them must be remeshed.
///
/// An edited chunk next to this column may hold a dug shaft or tunnel along the border. Its
/// wall towards this column consists of this column's blocks, which were dropped when the
/// column was unloaded; without them the wall shows a hole. Only edited chunks trigger: a
/// neighbour may also hold unedited chunks under this column's span (created by an edit next
/// to them, by this function, or streamed because its own span reaches lower), which are
/// generated rock and expose nothing, so the result does not depend on the order in which
/// columns arrive. The new chunks are the generated rock (edits are not saved), and the
/// neighbour's chunk is remeshed too, since its faces towards a tunnel that ran into this
/// column are now covered. Above the span this column is air and holds no faces, so nothing is
/// created there.
pub fn create_chunks_beside_edits(
    world: &mut World,
    terrain: &Terrain,
    spans: &ColumnSpans,
    edited: &EditedChunks,
    columns: &[Column],
) -> Vec<ChunkPos> {
    let mut to_remesh = Vec::new();
    for &column in columns {
        let Some(span) = spans.get(column) else {
            continue;
        };
        for layer in LOWEST_CHUNK_LAYER..*span.start() {
            let own = column.chunk(layer);
            for neighbour in horizontal_neighbours(column) {
                let beside = neighbour.chunk(layer);
                if !edited.contains(beside) || !world.contains(beside) {
                    continue;
                }
                if !world.contains(own) {
                    world.insert(own, terrain.generate(own));
                    to_remesh.push(own);
                }
                if !to_remesh.contains(&beside) {
                    to_remesh.push(beside);
                }
            }
        }
    }
    to_remesh
}

/// The chunks whose mesh is out of date after one player edit and the mods' writes it caused,
/// each listed once, so every chunk is rebuilt once however many writes touched it.
///
/// A `Vec` searched linearly: it holds at most the chunks of one player edit and
/// `MAX_EDITS_PER_CALL` writes per mod, a few hundred positions at worst, and edits are rare.
#[derive(Debug, Default)]
pub struct StaleChunks {
    chunks: Vec<ChunkPos>,
}

impl StaleChunks {
    /// Adds the chunks one write changed, skipping those already listed.
    pub fn add(&mut self, changed: &ChangedChunks) {
        for pos in changed.as_slice() {
            if !self.chunks.contains(pos) {
                self.chunks.push(*pos);
            }
        }
    }

    /// The listed chunks, in the order they were first added.
    pub fn as_slice(&self) -> &[ChunkPos] {
        &self.chunks
    }
}

/// What one player edit did: the event for the mods and the chunks to rebuild.
#[derive(Debug)]
pub struct PlayerEdit {
    /// What the mods hear of the edit.
    pub event: BlockEvent,
    /// The chunks the player's write made out of date.
    pub stale: StaleChunks,
    /// How many chunks the edit created under the terrain ([`create_chunks_for_edit`]).
    pub created: usize,
}

/// The event the mods get after the player's `edit`, made by `action`; `previous` is the block
/// that was in the cell before the write. A broken block is reported as the block that was
/// there, a placed one as the block that is there now.
fn block_event(action: EditAction, edit: &Edit, previous: BlockId) -> BlockEvent {
    let (kind, block) = match action {
        EditAction::Break => (BlockEventKind::BlockBroken, previous),
        EditAction::Place => (BlockEventKind::BlockPlaced, edit.block),
    };
    BlockEvent {
        kind,
        position: edit.position,
        block,
    }
}

/// Carries out the player's `edit` (already approved by [`plan`], which refuses bedrock and
/// cells outside the world) and returns the event for the mods, made after the write, or
/// `None` when nothing happened: the chunk of the cell is not loaded, or the cell already held
/// the block. The chunks the edit needs under the terrain are created first
/// ([`create_chunks_for_edit`]) and a changed chunk is recorded in `edited`.
pub fn player_edit(
    world: &mut World,
    terrain: &Terrain,
    spans: &ColumnSpans,
    edited: &mut EditedChunks,
    action: EditAction,
    edit: &Edit,
) -> Option<PlayerEdit> {
    let created = create_chunks_for_edit(world, terrain, spans, edit.position);
    let previous = world.block_at(edit.position)?;
    let changed = world.set_block(edit.position, edit.block)?;
    if changed.is_empty() {
        return None;
    }
    edited.record(edit.position);
    let mut stale = StaleChunks::default();
    stale.add(&changed);
    Some(PlayerEdit {
        event: block_event(action, edit, previous),
        stale,
        created,
    })
}

/// Writes the mods' `edits` into `world` in their order, so a later write to the same cell
/// wins, adds the chunks they changed to `stale` and records them in `edited`: a mod's write
/// is an edit like the player's, so it must survive a neighbouring column's reload the same
/// way. A write to a protected cell ([`is_protected`]) is skipped, and so is one that would
/// not change the block (nothing is created or remeshed for it). Chunks under the terrain are
/// created as for the player ([`create_chunks_for_edit`]); a write into a column that is not
/// loaded or outside the world is skipped. No event is sent for these writes: mods do not
/// trigger each other (decision 0014). Returns how many chunks were created.
pub fn apply_mod_edits(
    world: &mut World,
    terrain: &Terrain,
    spans: &ColumnSpans,
    edited: &mut EditedChunks,
    edits: &[BlockEdit],
    stale: &mut StaleChunks,
) -> usize {
    let mut created = 0;
    for edit in edits {
        if is_protected(edit.position)
            || block_for_ray(world, spans, edit.position) == Some(edit.block)
        {
            continue;
        }
        created += create_chunks_for_edit(world, terrain, spans, edit.position);
        let Some(changed) = world.set_block(edit.position, edit.block) else {
            continue;
        };
        if !changed.is_empty() {
            edited.record(edit.position);
            stale.add(&changed);
        }
    }
    created
}

/// Brings `world`, `spans` and `edited` in step with one frame of streaming: drops every chunk
/// of the unloaded columns, also those an edit created, their spans and their edit records, and
/// adds the chunks and spans of the loaded ones. They change here and in the edit path only, so
/// they always agree.
///
/// Streaming never replaces a chunk that is already in the world: the present chunk wins, as
/// it may hold the player's edits. Returns the positions whose streamed chunk was dropped for
/// that reason; their streamed meshes must not be uploaded either, since the renderer has the
/// mesh of the present chunk. Edits are not saved: a column that is unloaded and loaded again
/// is generated anew (decision 0011).
pub fn apply_streamed(
    world: &mut World,
    spans: &mut ColumnSpans,
    edited: &mut EditedChunks,
    removed: &[Column],
    loaded: Vec<LoadedColumn>,
) -> Vec<ChunkPos> {
    for column in removed {
        spans.remove(*column);
        edited.remove_column(*column);
        for pos in column.chunks() {
            world.remove(pos);
        }
    }
    let mut kept = Vec::new();
    for column in loaded {
        spans.insert(column.column, column.span);
        for (pos, chunk) in column.chunks {
            if world.contains(pos) {
                kept.push(pos);
            } else {
                world.insert(pos, chunk);
            }
        }
    }
    kept
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::time::{Duration, Instant};

    use vox_world::{
        CHUNK_SIZE, Chunk, ChunkMesh, Face, LOWEST_CHUNK_LAYER, LocalPos, VERTICES_PER_QUAD,
        WORLD_CEILING_Y,
    };

    use super::*;
    use crate::streaming::test_blocks::{BLOCKS, face_textures};
    use crate::streaming::{build_column, remesh};

    /// The block the tests place and hit; its value is arbitrary, the code only moves it around.
    const PLACE_BLOCK: BlockId = BlockId(3);

    /// The eye of the tests: in the middle of block (0, 1, 0).
    const EYE: [f32; 3] = [0.5, 1.5, 0.5];

    /// The seed of the tests' terrain.
    const SEED: u32 = 1;

    fn body(eye: [f32; 3]) -> Body {
        Body::from_eye(eye)
    }

    fn hit(block: [i32; 3], face: Face, distance: f32) -> RayHit {
        RayHit {
            block,
            id: PLACE_BLOCK,
            face,
            distance,
        }
    }

    #[test]
    fn breaking_writes_air_into_the_hit_block() {
        let edit = plan(
            EditAction::Break,
            &hit([3, 4, 5], Face::PosY, 2.0),
            &body(EYE),
            PLACE_BLOCK,
        );
        assert_eq!(
            edit,
            Some(Edit {
                position: [3, 4, 5],
                block: BlockId::AIR
            })
        );
    }

    #[test]
    fn placing_writes_the_block_into_the_cell_in_front_of_the_face() {
        let edit = plan(
            EditAction::Place,
            &hit([3, 4, 5], Face::PosY, 2.0),
            &body(EYE),
            PLACE_BLOCK,
        );
        assert_eq!(
            edit,
            Some(Edit {
                position: [3, 5, 5],
                block: PLACE_BLOCK
            })
        );
    }

    #[test]
    fn placing_into_the_cell_of_the_eye_is_refused() {
        // The block at (0, 0, 0) is hit on its top face, and the eye is in the cell above it.
        let target = hit([0, 0, 0], Face::PosY, 0.5);
        assert_eq!(
            plan(EditAction::Place, &target, &body(EYE), PLACE_BLOCK),
            None
        );
        // Breaking it is fine.
        assert!(plan(EditAction::Break, &target, &body(EYE), PLACE_BLOCK).is_some());
    }

    #[test]
    fn placing_into_any_cell_of_the_box_is_refused() {
        // The box of the eye at EYE covers the cells y = -1 (feet), 0 and 1 above (0, _, 0).
        let player = body(EYE);
        for y in -1..=1 {
            let target = hit([0, y - 1, 0], Face::PosY, 1.0);
            assert_eq!(
                plan(EditAction::Place, &target, &player, PLACE_BLOCK),
                None,
                "cell y = {y}"
            );
        }
        // The cell above the head (the box ends at 1.7) and the cell beside the box are free.
        let above = hit([0, 1, 0], Face::PosY, 1.0);
        assert!(plan(EditAction::Place, &above, &player, PLACE_BLOCK).is_some());
        let beside = hit([0, 0, 0], Face::PosX, 1.0);
        assert!(plan(EditAction::Place, &beside, &player, PLACE_BLOCK).is_some());
    }

    #[test]
    fn the_box_is_found_with_floor_for_negative_coordinates() {
        // Eye at (-0.5, -0.5, -0.5) is in block (-1, -1, -1), not (0, 0, 0).
        let player = body([-0.5, -0.5, -0.5]);
        let target = hit([-1, -2, -1], Face::PosY, 0.5);
        assert_eq!(plan(EditAction::Place, &target, &player, PLACE_BLOCK), None);
        let beside = hit([0, -2, 0], Face::PosY, 0.5);
        assert!(plan(EditAction::Place, &beside, &player, PLACE_BLOCK).is_some());
    }

    #[test]
    fn placing_when_the_ray_starts_inside_a_block_is_refused() {
        let target = hit([9, 9, 9], Face::NegX, 0.0);
        assert_eq!(
            plan(EditAction::Place, &target, &body(EYE), PLACE_BLOCK),
            None
        );
        assert!(plan(EditAction::Break, &target, &body(EYE), PLACE_BLOCK).is_some());
    }

    #[test]
    fn the_placed_block_is_not_air() {
        assert!(!PLACE_BLOCK.is_air());
    }

    #[test]
    fn the_bedrock_floor_cannot_be_broken() {
        // Literal height: the floor row of decision 0015.
        let floor = hit([3, -512, -7], Face::PosY, 2.0);
        assert_eq!(
            plan(EditAction::Break, &floor, &body(EYE), PLACE_BLOCK),
            None
        );
        // The row above it can.
        let above = hit([3, -511, -7], Face::PosY, 2.0);
        assert!(plan(EditAction::Break, &above, &body(EYE), PLACE_BLOCK).is_some());
        // Placing on top of the floor is fine.
        assert!(plan(EditAction::Place, &floor, &body(EYE), PLACE_BLOCK).is_some());
    }

    #[test]
    fn placing_outside_the_height_bounds_is_refused() {
        // On the top face of the highest row the new cell is the ceiling row, outside.
        let top = hit([0, WORLD_CEILING_Y - 1, 40], Face::PosY, 2.0);
        assert_eq!(plan(EditAction::Place, &top, &body(EYE), PLACE_BLOCK), None);
        // On its side the cell is still inside.
        let side = hit([0, WORLD_CEILING_Y - 1, 40], Face::PosX, 2.0);
        assert!(plan(EditAction::Place, &side, &body(EYE), PLACE_BLOCK).is_some());
        // Under the floor row.
        let under = hit([0, WORLD_FLOOR_Y, 40], Face::NegY, 2.0);
        assert_eq!(
            plan(EditAction::Place, &under, &body(EYE), PLACE_BLOCK),
            None
        );
    }

    /// A chunk with a marker block at the origin, to tell chunks apart.
    fn marked(id: u16) -> Chunk {
        let mut chunk = Chunk::new();
        if let Some(origin) = LocalPos::new(0, 0, 0) {
            chunk.set(origin, BlockId(id));
        }
        chunk
    }

    fn marker(world: &World, pos: ChunkPos) -> Option<BlockId> {
        let size = CHUNK_SIZE as i32;
        world.block_at([pos.x * size, pos.y * size, pos.z * size])
    }

    /// A loaded column with one marked chunk per layer of `span`.
    fn loaded(column: Column, span: std::ops::RangeInclusive<i32>, id: u16) -> LoadedColumn {
        LoadedColumn {
            column,
            chunks: span
                .clone()
                .map(|y| (column.chunk(y), marked(id)))
                .collect(),
            span,
        }
    }

    #[test]
    fn streamed_columns_are_added_and_unloaded_ones_dropped_with_every_layer() {
        let first = Column::new(0, 0);
        let second = Column::new(1, 0);
        let mut world = World::new();
        let mut spans = ColumnSpans::default();
        let kept = apply_streamed(
            &mut world,
            &mut spans,
            &mut EditedChunks::default(),
            &[],
            vec![loaded(first, 0..=0, 1), loaded(second, -1..=0, 2)],
        );
        assert!(kept.is_empty());
        assert_eq!(marker(&world, second.chunk(-1)), Some(BlockId(2)));
        assert_eq!(world.len(), 3);
        assert_eq!(spans.get(second), Some(&(-1..=0)));

        // A chunk an edit created far down in the first column goes with it.
        world.insert(first.chunk(LOWEST_CHUNK_LAYER), marked(9));
        apply_streamed(
            &mut world,
            &mut spans,
            &mut EditedChunks::default(),
            &[first],
            Vec::new(),
        );
        assert!(spans.get(first).is_none() && spans.contains(second));
        assert!(!world.contains(first.chunk(0)));
        assert!(!world.contains(first.chunk(LOWEST_CHUNK_LAYER)));
        assert!(world.contains(second.chunk(-1)) && world.contains(second.chunk(0)));
        assert_eq!(world.len(), 2);
    }

    #[test]
    fn streaming_never_replaces_a_chunk_that_is_in_the_world() {
        let column = Column::new(4, -2);
        let mut world = World::new();
        world.insert(column.chunk(0), marked(7));
        let mut spans = ColumnSpans::default();
        let kept = apply_streamed(
            &mut world,
            &mut spans,
            &mut EditedChunks::default(),
            &[],
            vec![loaded(column, -1..=0, 1)],
        );
        assert_eq!(kept, vec![column.chunk(0)]);
        assert_eq!(marker(&world, column.chunk(0)), Some(BlockId(7)));
        assert_eq!(marker(&world, column.chunk(-1)), Some(BlockId(1)));
    }

    #[test]
    fn an_unloaded_and_reloaded_column_is_generated_anew() {
        let column = Column::new(0, 0);
        let mut world = World::new();
        let mut spans = ColumnSpans::default();
        apply_streamed(
            &mut world,
            &mut spans,
            &mut EditedChunks::default(),
            &[],
            vec![loaded(column, 0..=0, 1)],
        );
        world.set_block([0, 0, 0], BlockId(7));
        apply_streamed(
            &mut world,
            &mut spans,
            &mut EditedChunks::default(),
            &[column],
            vec![],
        );
        apply_streamed(
            &mut world,
            &mut spans,
            &mut EditedChunks::default(),
            &[],
            vec![loaded(column, 0..=0, 1)],
        );
        assert_eq!(world.block_at([0, 0, 0]), Some(BlockId(1)));
    }

    fn terrain() -> Terrain {
        Terrain::new(SEED, BLOCKS, face_textures())
    }

    /// Spans of the columns `-1..=1` in X and Z, all layer 0.
    fn spans_3x3() -> ColumnSpans {
        let mut spans = ColumnSpans::default();
        for z in -1..=1 {
            for x in -1..=1 {
                spans.insert(Column::new(x, z), 0..=0);
            }
        }
        spans
    }

    #[test]
    fn an_edit_creates_the_missing_chunks_it_touches_in_loaded_columns_only() {
        let terrain = terrain();
        let spans = spans_3x3();
        let mut world = World::new();
        // Block (-1, -32, -1) is local (31, 0, 31) of chunk (-1, -1, -1): it touches that chunk
        // and the chunks behind its +X, -Y and +Z borders, all in loaded columns.
        let created = create_chunks_for_edit(&mut world, &terrain, &spans, [-1, -32, -1]);
        assert_eq!(created, 4);
        for pos in [
            ChunkPos::new(-1, -1, -1),
            ChunkPos::new(0, -1, -1),
            ChunkPos::new(-1, -2, -1),
            ChunkPos::new(-1, -1, 0),
        ] {
            assert!(world.contains(pos), "{pos:?}");
        }
        // Block (-32, -32, -32) is local (0, 0, 0) of the same chunk: behind its -X and -Z
        // borders lie columns (-2, -1) and (-1, -2), which are not loaded, and the chunk below
        // it is there from the first edit: nothing is created.
        let created = create_chunks_for_edit(&mut world, &terrain, &spans, [-32, -32, -32]);
        assert_eq!(created, 0);
        assert!(!world.contains(ChunkPos::new(-2, -1, -1)));
        assert!(!world.contains(ChunkPos::new(-1, -1, -2)));
        assert_eq!(world.len(), 4);
        // The created chunks are the generated ones: all stone under the terrain layer.
        assert_eq!(world.block_at([-32, -32, -32]), Some(BLOCKS.stone));
        // A second call finds them all and creates nothing.
        assert_eq!(
            create_chunks_for_edit(&mut world, &terrain, &spans, [-32, -32, -32]),
            0
        );
    }

    #[test]
    fn an_edit_never_replaces_a_chunk_and_never_creates_one_outside_the_world() {
        let terrain = terrain();
        let mut spans = ColumnSpans::default();
        spans.insert(Column::new(0, 0), 0..=0);
        let mut world = World::new();
        let floor_layer = ChunkPos::new(0, LOWEST_CHUNK_LAYER, 0);
        world.insert(floor_layer, marked(5));
        // A block of the floor row touches the layer below the world: not created, and the
        // marked chunk stays.
        let created = create_chunks_for_edit(&mut world, &terrain, &spans, [5, WORLD_FLOOR_Y, 5]);
        assert_eq!(created, 0);
        assert_eq!(marker(&world, floor_layer), Some(BlockId(5)));
        assert_eq!(world.len(), 1);
        // The highest row touches the layer above the ceiling: only its own chunk is created.
        let created =
            create_chunks_for_edit(&mut world, &terrain, &spans, [5, WORLD_CEILING_Y - 1, 5]);
        assert_eq!(created, 1);
        assert_eq!(world.len(), 2);
    }

    /// Whether `mesh` (of the chunk holding `cell`) has a quad covering the face `face` of the
    /// block at world position `cell`.
    fn covers(mesh: &ChunkMesh, cell: [i32; 3], face: Face) -> bool {
        let (_, local) = ChunkPos::containing(cell);
        let local = [local.x(), local.y(), local.z()].map(i32::from);
        let normal = face
            .offset()
            .iter()
            .position(|&step| step != 0)
            .unwrap_or(0);
        let plane = local[normal] + i32::from(face.offset()[normal].max(0));
        mesh.vertices.chunks(VERTICES_PER_QUAD).any(|quad| {
            if quad[0].face != face.index() {
                return false;
            }
            let low = |axis: usize| quad.iter().map(|v| i32::from(v.position[axis])).min();
            let high = |axis: usize| quad.iter().map(|v| i32::from(v.position[axis])).max();
            (0..3).all(|axis| match (low(axis), high(axis)) {
                (Some(low), Some(high)) if axis == normal => low == plane && high == plane,
                (Some(low), Some(high)) => low <= local[axis] && local[axis] < high,
                _ => false,
            })
        })
    }

    /// The loaded 3 x 3 columns around the origin, built by the column job, with their meshes
    /// as the renderer would hold them.
    struct Loaded {
        world: World,
        spans: ColumnSpans,
        edited: EditedChunks,
        meshes: HashMap<ChunkPos, ChunkMesh>,
    }

    fn loaded_3x3(terrain: &Terrain) -> Loaded {
        let mut scratch = ChunkMesh::default();
        let mut world = World::new();
        let mut spans = ColumnSpans::default();
        let mut meshes = HashMap::new();
        for z in -1..=1 {
            for x in -1..=1 {
                let column = Column::new(x, z);
                let built = build_column(terrain, column, &mut scratch);
                spans.insert(column, built.span.clone());
                for (pos, layer) in built.layers {
                    if !layer.mesh.indices.is_empty() {
                        meshes.insert(pos, layer.mesh);
                    }
                    world.insert(pos, layer.chunk);
                }
            }
        }
        Loaded {
            world,
            spans,
            edited: EditedChunks::default(),
            meshes,
        }
    }

    /// Breaks the block at `position` the way the game does, and returns how many chunks the
    /// edit created and how long it took.
    fn break_block(
        loaded: &mut Loaded,
        terrain: &Terrain,
        position: [i32; 3],
    ) -> (usize, Duration) {
        let mut scratch = ChunkMesh::default();
        let started = Instant::now();
        let created = create_chunks_for_edit(&mut loaded.world, terrain, &loaded.spans, position);
        let changed = loaded
            .world
            .set_block(position, BlockId::AIR)
            .expect("the block's chunk is in the world");
        loaded.edited.record(position);
        for &pos in changed.as_slice() {
            assert!(remesh(
                terrain,
                &loaded.world,
                &loaded.spans,
                pos,
                &mut scratch
            ));
            if scratch.indices.is_empty() {
                loaded.meshes.remove(&pos);
            } else {
                loaded.meshes.insert(pos, scratch.clone());
            }
        }
        (created, started.elapsed())
    }

    #[test]
    fn a_shaft_dug_from_the_surface_to_the_floor_shows_every_wall() {
        let terrain = terrain();
        let mut loaded = loaded_3x3(&terrain);
        let streamed = loaded.world.len();
        // The shaft runs down the block column (0, _, 0): a corner of four chunk columns, so its
        // -X and -Z walls lie in the neighbouring columns, and it crosses every layer border.
        let top = (0..CHUNK_SIZE as i32)
            .rev()
            .find(|&y| loaded.world.block_at([0, y, 0]) != Some(BlockId::AIR))
            .expect("the surface lies in layer 0");
        let player = body([100.5, 100.5, 100.5]);
        for y in (WORLD_FLOOR_Y..=top).rev() {
            let target = hit([0, y, 0], Face::PosY, 1.0);
            let Some(edit) = plan(EditAction::Break, &target, &player, PLACE_BLOCK) else {
                assert_eq!(y, WORLD_FLOOR_Y, "only the floor refuses");
                continue;
            };
            break_block(&mut loaded, &terrain, edit.position);
        }

        // Every solid cell next to the shaft shows the face that looks into it.
        let mut faces = 0;
        for y in WORLD_FLOOR_Y + 1..=top {
            for face in [Face::PosX, Face::NegX, Face::PosZ, Face::NegZ] {
                let [dx, _, dz] = face.offset().map(i32::from);
                let wall = [dx, y, dz];
                // A wall missing from the world lies under the span, where all is rock: it must
                // have been created, so it counts as solid and its face is looked for.
                let solid = loaded.world.block_at(wall).is_none_or(|b| !b.is_air());
                let chunk = ChunkPos::containing(wall).0;
                if solid {
                    // The wall's face looks back at the shaft: the opposite direction.
                    let back = Face::ALL
                        .into_iter()
                        .find(|f| f.offset().map(i32::from) == [-dx, 0, -dz])
                        .expect("every direction has an opposite");
                    let mesh = loaded.meshes.get(&chunk);
                    assert!(
                        mesh.is_some_and(|mesh| covers(mesh, wall, back)),
                        "no face at {wall:?} looking {back:?}"
                    );
                    faces += 1;
                }
            }
        }
        // The bedrock at the bottom shows its top.
        let bottom = [0, WORLD_FLOOR_Y, 0];
        assert_eq!(loaded.world.block_at(bottom), Some(BLOCKS.bedrock));
        let floor_chunk = ChunkPos::containing(bottom).0;
        assert!(covers(&loaded.meshes[&floor_chunk], bottom, Face::PosY));
        assert!(faces > 0);

        // Memory: the shaft created its own chunk and the two neighbours behind its -X and -Z
        // walls in every layer below the terrain layer, nothing else.
        let layers_below = usize::try_from(-LOWEST_CHUNK_LAYER).unwrap_or(0);
        assert_eq!(loaded.world.len() - streamed, 3 * layers_below);
    }

    /// The height of the highest solid block of the block column `(x, z)` in layer 0.
    fn surface(world: &World, x: i32, z: i32) -> i32 {
        (0..CHUNK_SIZE as i32)
            .rev()
            .find(|&y| {
                world
                    .block_at([x, y, z])
                    .is_some_and(|block| !block.is_air())
            })
            .expect("the surface lies in layer 0")
    }

    #[test]
    fn a_wall_beside_a_shaft_survives_the_unload_and_reload_of_its_column() {
        // A shaft down the block column (0, _, 0) to height -40 (layer -2): its -X wall,
        // x = -1, lies in column (-1, 0), whose chunks under the span were created by the dig.
        const BOTTOM: i32 = -40;
        let terrain = terrain();
        let mut loaded = loaded_3x3(&terrain);
        let top = surface(&loaded.world, 0, 0);
        for y in (BOTTOM..=top).rev() {
            break_block(&mut loaded, &terrain, [0, y, 0]);
        }
        let neighbour = Column::new(-1, 0);

        // The neighbour column leaves and comes back, as streaming hands it out.
        apply_streamed(
            &mut loaded.world,
            &mut loaded.spans,
            &mut loaded.edited,
            &[neighbour],
            Vec::new(),
        );
        for pos in neighbour.chunks() {
            loaded.meshes.remove(&pos);
            assert!(!loaded.world.contains(pos));
        }
        let mut scratch = ChunkMesh::default();
        let built = build_column(&terrain, neighbour, &mut scratch);
        let mut chunks = Vec::new();
        for (pos, layer) in built.layers {
            if !layer.mesh.indices.is_empty() {
                loaded.meshes.insert(pos, layer.mesh);
            }
            chunks.push((pos, layer.chunk));
        }
        let arrived = LoadedColumn {
            column: neighbour,
            span: built.span,
            chunks,
        };
        let kept = apply_streamed(
            &mut loaded.world,
            &mut loaded.spans,
            &mut loaded.edited,
            &[],
            vec![arrived],
        );
        assert!(kept.is_empty());
        let to_remesh = create_chunks_beside_edits(
            &mut loaded.world,
            &terrain,
            &loaded.spans,
            &loaded.edited,
            &[neighbour],
        );
        // Layers -1 and -2 of the neighbour, and the shaft's chunks beside them.
        assert_eq!(to_remesh.len(), 4);
        for pos in to_remesh {
            assert!(remesh(
                &terrain,
                &loaded.world,
                &loaded.spans,
                pos,
                &mut scratch
            ));
            loaded.meshes.insert(pos, scratch.clone());
        }

        // Every wall block under the terrain layer shows its face towards the shaft again.
        for y in BOTTOM..0 {
            let wall = [-1, y, 0];
            assert_eq!(loaded.world.block_at(wall), Some(BLOCKS.stone), "y {y}");
            let chunk = ChunkPos::containing(wall).0;
            let mesh = loaded.meshes.get(&chunk);
            assert!(
                mesh.is_some_and(|mesh| covers(mesh, wall, Face::PosX)),
                "no face at {wall:?}"
            );
        }
        // And the wall can be dug: its chunk is in the world (the loop above found its stone).
        break_block(&mut loaded, &terrain, [-1, -20, 0]);
        assert_eq!(loaded.world.block_at([-1, -20, 0]), Some(BlockId::AIR));
    }

    /// Streams `columns` in, in this order, as the game does in one frame, and returns what
    /// `create_chunks_beside_edits` created and asks to remesh, sorted.
    fn arrive(loaded: &mut Loaded, terrain: &Terrain, columns: &[Column]) -> Vec<ChunkPos> {
        let mut scratch = ChunkMesh::default();
        let arrived = columns
            .iter()
            .map(|&column| {
                let built = build_column(terrain, column, &mut scratch);
                LoadedColumn {
                    column,
                    span: built.span,
                    chunks: built
                        .layers
                        .into_iter()
                        .map(|(pos, layer)| (pos, layer.chunk))
                        .collect(),
                }
            })
            .collect();
        apply_streamed(
            &mut loaded.world,
            &mut loaded.spans,
            &mut loaded.edited,
            &[],
            arrived,
        );
        let mut to_remesh = create_chunks_beside_edits(
            &mut loaded.world,
            terrain,
            &loaded.spans,
            &loaded.edited,
            columns,
        );
        to_remesh.sort_by_key(|pos| (pos.x, pos.y, pos.z));
        to_remesh
    }

    #[test]
    fn only_edited_chunks_complete_an_arriving_column_in_any_order() {
        // An edit in column A = (0, 0) at height -5 (layer -1) on its -X border; column
        // B = (-1, 0) next to it and C = (-2, 0) next to B arrive in the same frame.
        let terrain = terrain();
        let a = Column::new(0, 0);
        let b = Column::new(-1, 0);
        let c = Column::new(-2, 0);
        let mut results = Vec::new();
        for order in [[b, c], [c, b]] {
            let mut loaded = loaded_3x3(&terrain);
            break_block(&mut loaded, &terrain, [0, -5, 0]);
            apply_streamed(
                &mut loaded.world,
                &mut loaded.spans,
                &mut loaded.edited,
                &[b],
                Vec::new(),
            );
            let to_remesh = arrive(&mut loaded, &terrain, &order);
            // B gets its layer -1 back beside A's edited chunk; C, next to B's new chunk only,
            // gets nothing.
            assert_eq!(to_remesh, vec![b.chunk(-1), a.chunk(-1)], "order {order:?}");
            assert!(!loaded.world.contains(c.chunk(-1)), "order {order:?}");
            results.push(loaded.world.len());
        }
        assert_eq!(results[0], results[1]);
    }

    #[test]
    fn unedited_chunks_under_a_lower_neighbour_span_create_nothing() {
        // Column X = (0, 0) streams with a span reaching down to layer -2, so it holds chunks
        // under the span 0..=0 of Y = (1, 0); they are not edited, so Y gets nothing.
        let terrain = terrain();
        let x = Column::new(0, 0);
        let y = Column::new(1, 0);
        let mut world = World::new();
        let mut spans = ColumnSpans::default();
        let mut edited = EditedChunks::default();
        let streamed = |column: Column, span: std::ops::RangeInclusive<i32>| LoadedColumn {
            column,
            chunks: span
                .clone()
                .map(|layer| (column.chunk(layer), terrain.generate(column.chunk(layer))))
                .collect(),
            span,
        };
        apply_streamed(
            &mut world,
            &mut spans,
            &mut edited,
            &[],
            vec![streamed(x, -2..=0), streamed(y, 0..=0)],
        );
        let before = world.len();
        let created = create_chunks_beside_edits(&mut world, &terrain, &spans, &edited, &[y]);
        assert!(created.is_empty());
        assert_eq!(world.len(), before);
        // Once a block of X's layer -1 is written, Y gets its layer -1.
        edited.record([5, -10, 5]);
        let created = create_chunks_beside_edits(&mut world, &terrain, &spans, &edited, &[y]);
        assert_eq!(created, vec![y.chunk(-1), x.chunk(-1)]);
    }

    #[test]
    fn edit_records_leave_with_their_column() {
        let mut world = World::new();
        let mut spans = ColumnSpans::default();
        let mut edited = EditedChunks::default();
        edited.record([5, -10, 5]);
        edited.record([40, -10, 5]);
        apply_streamed(
            &mut world,
            &mut spans,
            &mut edited,
            &[Column::new(0, 0)],
            Vec::new(),
        );
        assert!(!edited.contains(ChunkPos::new(0, -1, 0)));
        assert!(edited.contains(ChunkPos::new(1, -1, 0)));
    }

    #[test]
    fn nothing_is_created_beside_columns_without_edits() {
        let terrain = terrain();
        let mut loaded = loaded_3x3(&terrain);
        let before = loaded.world.len();
        let columns: Vec<Column> = (-1..=1).map(|x| Column::new(x, 0)).collect();
        let created = create_chunks_beside_edits(
            &mut loaded.world,
            &terrain,
            &loaded.spans,
            &loaded.edited,
            &columns,
        );
        assert!(created.is_empty());
        assert_eq!(loaded.world.len(), before);
    }

    #[test]
    fn a_ray_from_the_flying_height_reaches_the_terrain() {
        // The eye at height 60 lies in layer 1, which no column of the area holds.
        let terrain = terrain();
        let loaded = loaded_3x3(&terrain);
        let eye = [5.5, 60.5, 5.5];
        let down = [0.0, -1.0, 0.0];
        let reach = 100.0;
        let lookup = |block| block_for_ray(&loaded.world, &loaded.spans, block);
        let hit = vox_world::raycast_with(lookup, eye, down, reach).expect("the ray hits");
        assert_eq!(hit.block, [5, surface(&loaded.world, 5, 5), 5]);
        assert_eq!(hit.face, Face::PosY);
        // The world alone stops at the missing chunk.
        assert!(vox_world::raycast(&loaded.world, eye, down, reach).is_none());
    }

    #[test]
    fn the_ray_sees_air_above_a_span_and_stops_below_it_and_outside_the_area() {
        let terrain = terrain();
        let loaded = loaded_3x3(&terrain);
        let at = |position| block_for_ray(&loaded.world, &loaded.spans, position);
        assert_eq!(at([5, 40, 5]), Some(BlockId::AIR));
        assert_eq!(at([5, WORLD_CEILING_Y + 100, 5]), Some(BlockId::AIR));
        assert_eq!(at([5, -5, 5]), None);
        assert_eq!(at([500, 40, 5]), None);
        assert_eq!(at([5, 0, 5]), loaded.world.block_at([5, 0, 5]));
    }

    #[test]
    #[ignore = "a timing, run with --release -- --ignored --nocapture"]
    fn the_cost_of_an_edit_that_creates_four_chunks() {
        // The block (0, -32, 0) is the lower corner of chunk (0, -1, 0): its own chunk and the
        // chunks behind its -X, -Y and -Z borders are all under the terrain and missing.
        const RUNS: usize = 20;
        let terrain = terrain();
        let mut times = Vec::with_capacity(RUNS);
        for _ in 0..RUNS {
            let mut loaded = loaded_3x3(&terrain);
            let (created, took) = break_block(&mut loaded, &terrain, [0, -32, 0]);
            assert_eq!(created, 4);
            times.push(took);
        }
        times.sort_unstable();
        println!(
            "edit creating 4 chunks: median {:?}, worst {:?} over {RUNS} runs",
            times[RUNS / 2],
            times[RUNS - 1]
        );
    }

    /// A world of the chunks at `positions`, all air, and spans of `0..=top_layer` for the
    /// columns (0, 0) and (1, 0).
    fn empty_world(positions: &[ChunkPos], top_layer: i32) -> (World, ColumnSpans) {
        let mut world = World::new();
        for pos in positions {
            world.insert(*pos, Chunk::new());
        }
        let mut spans = ColumnSpans::default();
        spans.insert(Column::new(0, 0), 0..=top_layer);
        spans.insert(Column::new(1, 0), 0..=top_layer);
        (world, spans)
    }

    /// The edit writing `block` at `position`.
    fn edit_at(position: [i32; 3], block: BlockId) -> Edit {
        Edit { position, block }
    }

    #[test]
    fn breaking_reports_the_old_block_and_records_the_edit() {
        let terrain = terrain();
        let (mut world, spans) = empty_world(&[ChunkPos::ORIGIN], 0);
        world.set_block([4, 4, 4], BlockId(9));
        let mut edited = EditedChunks::default();
        let edit = edit_at([4, 4, 4], BlockId::AIR);
        let done = player_edit(
            &mut world,
            &terrain,
            &spans,
            &mut edited,
            EditAction::Break,
            &edit,
        )
        .unwrap();
        assert_eq!(
            done.event,
            BlockEvent {
                kind: BlockEventKind::BlockBroken,
                position: [4, 4, 4],
                block: BlockId(9)
            }
        );
        assert_eq!(done.stale.as_slice(), [ChunkPos::ORIGIN]);
        assert!(edited.contains(ChunkPos::ORIGIN));
        // The event is made after the write.
        assert_eq!(world.block_at([4, 4, 4]), Some(BlockId::AIR));
    }

    #[test]
    fn placing_reports_the_new_block() {
        let terrain = terrain();
        let (mut world, spans) = empty_world(&[ChunkPos::ORIGIN], 0);
        let mut edited = EditedChunks::default();
        let edit = edit_at([4, 4, 4], PLACE_BLOCK);
        let done = player_edit(
            &mut world,
            &terrain,
            &spans,
            &mut edited,
            EditAction::Place,
            &edit,
        )
        .unwrap();
        assert_eq!(
            done.event,
            BlockEvent {
                kind: BlockEventKind::BlockPlaced,
                position: [4, 4, 4],
                block: PLACE_BLOCK
            }
        );
        assert_eq!(world.block_at([4, 4, 4]), Some(PLACE_BLOCK));
    }

    #[test]
    fn a_write_that_changes_nothing_sends_no_event_and_records_no_edit() {
        let terrain = terrain();
        let (mut world, spans) = empty_world(&[ChunkPos::ORIGIN], 0);
        let mut edited = EditedChunks::default();
        let edit = edit_at([4, 4, 4], BlockId::AIR);
        let done = player_edit(
            &mut world,
            &terrain,
            &spans,
            &mut edited,
            EditAction::Break,
            &edit,
        );
        assert!(done.is_none());
        assert!(!edited.contains(ChunkPos::ORIGIN));
    }

    #[test]
    fn an_edit_in_a_column_that_is_not_loaded_sends_no_event() {
        let terrain = terrain();
        let (mut world, spans) = empty_world(&[ChunkPos::ORIGIN], 0);
        let mut edited = EditedChunks::default();
        let far = CHUNK_SIZE as i32 * 5;
        let edit = edit_at([far, 4, 4], PLACE_BLOCK);
        let done = player_edit(
            &mut world,
            &terrain,
            &spans,
            &mut edited,
            EditAction::Place,
            &edit,
        );
        assert!(done.is_none());
        assert_eq!(world.len(), 1);
    }

    #[test]
    fn a_refused_plan_sends_no_event() {
        // The bedrock row cannot be broken and the cell above the ceiling cannot be built in:
        // `plan` refuses, so no edit exists to hand to `player_edit` and nothing is written.
        let floor = hit([0, WORLD_FLOOR_Y, 0], Face::PosY, 2.0);
        assert!(plan(EditAction::Break, &floor, &body(EYE), PLACE_BLOCK).is_none());
        let top = hit([0, WORLD_CEILING_Y - 1, 0], Face::PosY, 2.0);
        assert!(plan(EditAction::Place, &top, &body(EYE), PLACE_BLOCK).is_none());
        // An edit outside the world that reaches `player_edit` anyway is refused by the world.
        let terrain = terrain();
        let (mut world, spans) = empty_world(&[ChunkPos::ORIGIN], 0);
        let mut edited = EditedChunks::default();
        let outside = edit_at([0, WORLD_CEILING_Y, 0], PLACE_BLOCK);
        let done = player_edit(
            &mut world,
            &terrain,
            &spans,
            &mut edited,
            EditAction::Place,
            &outside,
        );
        assert!(done.is_none());
    }

    #[test]
    fn the_player_edit_and_the_mod_edits_remesh_each_chunk_once() {
        let terrain = terrain();
        let origin = ChunkPos::ORIGIN;
        let east = ChunkPos::new(1, 0, 0);
        let up = ChunkPos::new(0, 1, 0);
        let (mut world, spans) = empty_world(&[origin, east, up], 1);
        let mut edited = EditedChunks::default();
        let last = CHUNK_SIZE as i32 - 1;
        // The player's write in the middle of the origin chunk.
        let player = edit_at([4, 4, 4], PLACE_BLOCK);
        let mut stale = player_edit(
            &mut world,
            &terrain,
            &spans,
            &mut edited,
            EditAction::Place,
            &player,
        )
        .unwrap()
        .stale;
        let write = |position, block| BlockEdit { position, block };
        let edits = [
            // Same chunk again, twice on one cell.
            write([5, 4, 4], PLACE_BLOCK),
            write([5, 4, 4], BlockId(7)),
            // On the east border: the origin chunk and its east neighbour.
            write([last, 4, 4], PLACE_BLOCK),
            // Inside the east chunk.
            write([last + 2, 4, 4], PLACE_BLOCK),
            // Inside the chunk above.
            write([4, last + 3, 4], PLACE_BLOCK),
            // A column that is not loaded: skipped.
            write([-4, 4, 4], PLACE_BLOCK),
        ];
        apply_mod_edits(
            &mut world,
            &terrain,
            &spans,
            &mut edited,
            &edits,
            &mut stale,
        );
        assert_eq!(stale.as_slice(), [origin, east, up]);
        // Applied in order: the later write to (5, 4, 4) won.
        assert_eq!(world.block_at([5, 4, 4]), Some(BlockId(7)));
        assert_eq!(world.block_at([last + 2, 4, 4]), Some(PLACE_BLOCK));
        assert_eq!(world.block_at([-4, 4, 4]), None);
        // Mod writes are edits: the chunks they wrote into are recorded.
        for pos in [origin, east, up] {
            assert!(edited.contains(pos), "{pos:?}");
        }
        assert!(!edited.contains(ChunkPos::new(-1, 0, 0)));
    }

    #[test]
    fn a_mod_write_into_a_missing_layer_under_the_span_creates_it() {
        let terrain = terrain();
        let (mut world, spans) = empty_world(&[ChunkPos::ORIGIN], 0);
        let mut edited = EditedChunks::default();
        let mut stale = StaleChunks::default();
        let below = ChunkPos::new(0, -1, 0);
        let edits = [BlockEdit {
            position: [4, -4, 4],
            block: BlockId::AIR,
        }];
        let created = apply_mod_edits(
            &mut world,
            &terrain,
            &spans,
            &mut edited,
            &edits,
            &mut stale,
        );
        assert_eq!(created, 1);
        // The generated rock there was solid, so the air write changed it.
        assert!(world.contains(below));
        assert_eq!(world.block_at([4, -4, 4]), Some(BlockId::AIR));
        assert!(stale.as_slice().contains(&below));
        assert!(edited.contains(below));
    }

    #[test]
    fn a_mod_write_that_changes_nothing_remeshes_and_records_nothing() {
        let terrain = terrain();
        let (mut world, spans) = empty_world(&[ChunkPos::ORIGIN], 0);
        let mut edited = EditedChunks::default();
        let mut stale = StaleChunks::default();
        let air = BlockEdit {
            position: [1, 1, 1],
            block: BlockId::AIR,
        };
        apply_mod_edits(
            &mut world,
            &terrain,
            &spans,
            &mut edited,
            &[air],
            &mut stale,
        );
        assert!(stale.as_slice().is_empty());
        assert!(!edited.contains(ChunkPos::ORIGIN));
    }

    #[test]
    fn a_mod_cannot_write_the_bedrock_row() {
        let terrain = terrain();
        let mut spans = ColumnSpans::default();
        spans.insert(Column::new(0, 0), 0..=0);
        let mut world = World::new();
        let floor_chunk = ChunkPos::new(0, LOWEST_CHUNK_LAYER, 0);
        world.insert(floor_chunk, terrain.generate(floor_chunk));
        let floor = [4, WORLD_FLOOR_Y, 4];
        let bedrock = world.block_at(floor);
        assert_eq!(bedrock, Some(BLOCKS.bedrock));
        let mut edited = EditedChunks::default();
        let mut stale = StaleChunks::default();
        let edits = [
            BlockEdit {
                position: floor,
                block: BlockId::AIR,
            },
            BlockEdit {
                position: floor,
                block: PLACE_BLOCK,
            },
        ];
        let created = apply_mod_edits(
            &mut world,
            &terrain,
            &spans,
            &mut edited,
            &edits,
            &mut stale,
        );
        assert_eq!(created, 0);
        assert_eq!(world.block_at(floor), bedrock);
        assert!(stale.as_slice().is_empty());
        assert!(!edited.contains(floor_chunk));
        // The row above it is not protected.
        assert!(!is_protected([4, WORLD_FLOOR_Y + 1, 4]));
    }

    #[test]
    fn a_mod_write_that_would_not_change_the_block_creates_nothing() {
        let terrain = terrain();
        let (mut world, spans) = empty_world(&[ChunkPos::ORIGIN], 1);
        let mut edited = EditedChunks::default();
        let mut stale = StaleChunks::default();
        // Air into the air of the present chunk and of the missing chunk above the span.
        let edits = [
            BlockEdit {
                position: [1, 1, 1],
                block: BlockId::AIR,
            },
            BlockEdit {
                position: [1, CHUNK_SIZE as i32 * 4, 1],
                block: BlockId::AIR,
            },
        ];
        let created = apply_mod_edits(
            &mut world,
            &terrain,
            &spans,
            &mut edited,
            &edits,
            &mut stale,
        );
        assert_eq!(created, 0);
        assert_eq!(world.len(), 1);
    }

    #[test]
    fn the_documented_write_limits_match_the_constants() {
        // `docs/mod-api.md` and decision 0014 say 64 per call and 256 per event.
        assert_eq!(vox_mods::MAX_EDITS_PER_CALL, 64);
        assert_eq!(vox_mods::MAX_EDITS_PER_EVENT, 256);
    }
}
