//! The work of one streaming job: generate the layers of a chunk column that can show a face and
//! mesh them, or generate and mesh one level-of-detail tile; and the same meshing for a chunk of
//! the game's world after an edit.
//!
//! A job depends on nothing but the [`Terrain`] and the column, so jobs can run in any order on
//! any thread and a mesh is final when it is built, until a neighbour is edited (decisions 0010,
//! 0011 and 0015). The price is that each chunk is generated five times over the whole area,
//! which is cheap next to meshing.

use vox_world::{
    Chunk, ChunkMesh, ChunkPos, ChunkVertex, Face, FaceTextures, Neighbours, TerrainBlocks,
    TilePos, World, build_mesh_with_neighbours_into, build_tile_mesh_into, column_span,
    generate_chunk, generate_column, generate_lod_chunk, solid_chunk,
};

use super::column::{Column, ColumnSpans, Span};

/// Everything a job needs to know about the terrain: the seed, the blocks the generator
/// places, the texture layer of each block face for the mesher and the solid chunk that stands
/// in for everything below a column's span. Built once and shared by every worker and by the
/// game's remeshing and chunk creation.
#[derive(Debug)]
pub struct Terrain {
    seed: u32,
    blocks: TerrainBlocks,
    /// The texture layer of every face of every block, read once per quad by the mesher.
    textures: FaceTextures,
    /// The stand-in for everything below a column's span: a chunk of solid bedrock, so the
    /// underside of the terrain, which nobody can see, produces no faces.
    solid_below: Chunk,
}

impl Terrain {
    /// The terrain of `seed` made of `blocks`, meshed with the layers of `textures`.
    pub fn new(seed: u32, blocks: TerrainBlocks, textures: FaceTextures) -> Self {
        Self {
            seed,
            blocks,
            textures,
            solid_below: solid_chunk(blocks.bedrock),
        }
    }

    /// The blocks of the chunk at `pos` as the generator makes them, before any edit.
    pub fn generate(&self, pos: ChunkPos) -> Chunk {
        generate_chunk(self.seed, pos, &self.blocks)
    }

    /// The chunks of the layers `span` of `column`, lowest first: [`Self::generate`] for each, with
    /// the landscape of the column worked out once.
    pub fn generate_span(&self, column: Column, span: &Span) -> Vec<Chunk> {
        generate_column(self.seed, column.x, column.z, span.clone(), &self.blocks)
    }

    /// The chunk layers of `column` that can show a face (`vox_world::column_span`).
    pub fn span(&self, column: Column) -> Span {
        column_span(self.seed, column.x, column.z)
    }
}

/// Whether the neighbour on side `face` is generated for a job: the four horizontal ones.
/// The layers above and below come from the column itself.
fn is_horizontal(face: Face) -> bool {
    face.offset()[1] == 0
}

/// A generated chunk with its finished mesh. The chunk is moved on to the game's `World`, so
/// editing can read and change it; the mesh goes to the renderer.
#[derive(Debug)]
pub struct Built {
    /// The generated blocks.
    pub chunk: Chunk,
    /// The mesh of `chunk`; empty when nothing is visible.
    pub mesh: ChunkMesh,
}

/// A finished column job: the column's span and a chunk with its mesh for every layer of it.
#[derive(Debug)]
pub struct BuiltColumn {
    /// The chunk layers that can show a face; `layers` holds exactly these.
    pub span: Span,
    /// The chunks of the span with their meshes, lowest layer first.
    pub layers: Vec<(ChunkPos, Built)>,
}

impl BuiltColumn {
    /// How many of the column's meshes have something to draw: what the column costs of the
    /// upload budget. Empty meshes are never uploaded.
    pub fn mesh_count(&self) -> usize {
        self.layers
            .iter()
            .filter(|(_, built)| !built.mesh.indices.is_empty())
            .count()
    }
}

/// Bytes of a mesh's vertices and indices: what its GPU buffer holds, short of the few bytes
/// of alignment between the two parts. The estimate of GPU mesh memory adds these up.
pub fn mesh_bytes(mesh: &ChunkMesh) -> usize {
    mesh.vertices.len() * size_of::<ChunkVertex>() + mesh.indices.len() * size_of::<u32>()
}

/// An exact-size copy of `scratch`, or an empty mesh when nothing is visible.
fn finished(scratch: &ChunkMesh) -> ChunkMesh {
    if scratch.indices.is_empty() {
        ChunkMesh::default()
    } else {
        scratch.clone()
    }
}

/// Generates every layer of `column`'s span and meshes each with its four horizontal neighbours
/// of the same layer and the layers above and below it.
///
/// Inside the span the layers above and below are the span's own chunks; above the span is air
/// (no neighbour) and below it the solid stand-in, which is what the generator puts there. The
/// scratch buffers keep their size for the next job of the same worker, which is why each mesh
/// is a copy and not the scratch itself. The horizontal neighbours are dropped; the chunks are
/// moved out, not copied.
pub fn build_column(terrain: &Terrain, column: Column, scratch: &mut ChunkMesh) -> BuiltColumn {
    build_span(terrain, column, terrain.span(column), scratch)
}

/// [`build_column`] for a given `span`, so a span of several layers can be tested while the
/// generator's own spans are all one layer.
fn build_span(
    terrain: &Terrain,
    column: Column,
    span: Span,
    scratch: &mut ChunkMesh,
) -> BuiltColumn {
    // HOT PATH: runs for every streamed column on a worker thread. The meshes are built into the
    // worker's reused `scratch` (no growth after the first jobs) and only the final exact-size
    // copies are allocated; the solid stand-in is built once, not per job.
    let chunks = terrain.generate_span(column, &span);
    // The four neighbouring columns, each generated once for all the layers of the span.
    let side_columns: [Option<Vec<Chunk>>; Face::COUNT] = Face::ALL.map(|face| {
        is_horizontal(face).then(|| {
            let [dx, _, dz] = face.offset().map(i32::from);
            terrain.generate_span(Column::new(column.x + dx, column.z + dz), &span)
        })
    });
    let mut meshes = Vec::with_capacity(chunks.len());
    for (chunk, index) in chunks.iter().zip(0_usize..) {
        let sides: [Option<&Chunk>; Face::COUNT] = Face::ALL.map(|face| {
            side_columns[usize::from(face.index())]
                .as_ref()
                .and_then(|layers| layers.get(index))
        });
        let above = chunks.get(index + 1);
        let below = index
            .checked_sub(1)
            .and_then(|lower| chunks.get(lower))
            .unwrap_or(&terrain.solid_below);
        let neighbours = Neighbours::new(Face::ALL.map(|face| match face {
            Face::PosY => above,
            Face::NegY => Some(below),
            _ => sides[usize::from(face.index())],
        }));
        build_mesh_with_neighbours_into(chunk, &neighbours, &terrain.textures, scratch);
        meshes.push(finished(scratch));
    }
    let layers = span
        .clone()
        .zip(chunks.into_iter().zip(meshes))
        .map(|(layer, (chunk, mesh))| (column.chunk(layer), Built { chunk, mesh }))
        .collect();
    BuiltColumn { span, layers }
}

/// The lowest tile layer of every tile column, whose bottom is below all ground. A tile column
/// has the layers [`vox_world::tile_layers`]: a level-1 tile is 64 blocks tall, so the mountains
/// (up to block 160) need three of them, and each layer is its own tile and its own job
/// (decision 0017). The selection and the tracker know tile columns by their layer 0 tile.
pub const TILE_LAYER: i32 = 0;

/// One unit of work for a streaming worker.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Job {
    /// Generate and mesh the layers of a full-detail column ([`build_column`]).
    Column(Column),
    /// Generate and mesh one level-of-detail tile of level 1 or more ([`build_tile`]).
    Tile(TilePos),
}

/// The result of a [`Job`], carrying the key it was made for, so a result can not be filed
/// under another job.
#[derive(Debug)]
pub enum Finished {
    /// The chunks and meshes of a column.
    Column(Column, BuiltColumn),
    /// The mesh of a tile; the tile's cells were dropped on the worker.
    Tile(TilePos, ChunkMesh),
}

/// Runs `job` on a worker with its reused `scratch` mesh.
pub fn run(terrain: &Terrain, job: Job, scratch: &mut ChunkMesh) -> Finished {
    match job {
        Job::Column(column) => Finished::Column(column, build_column(terrain, column, scratch)),
        Job::Tile(tile) => Finished::Tile(tile, build_tile(terrain, tile, scratch)),
    }
}

/// Generates the cells of `tile` and meshes them alone, with the solid stand-in below
/// (decision 0015: the borders become the skirts). The cells are dropped here: a tile of level
/// 1 or more keeps no blocks, only its mesh. Returns an exact-size copy of the mesh, or an
/// empty mesh when nothing is visible.
pub fn build_tile(terrain: &Terrain, tile: TilePos, scratch: &mut ChunkMesh) -> ChunkMesh {
    // HOT PATH: runs for every streamed tile on a worker thread: one generation (1,024 height
    // samples) and one mesh into the worker's reused scratch; the cells are freed at the end.
    let cells = generate_lod_chunk(terrain.seed, tile, &terrain.blocks);
    build_tile_mesh_into(&cells, &terrain.solid_below, &terrain.textures, scratch);
    finished(scratch)
}

/// What stands in for a neighbour that is missing from the world.
enum Missing {
    /// All solid: a layer under its loaded column's span (or under the world).
    Solid,
    /// All air: a layer over its loaded column's span.
    Air,
    /// Not known without the generator: a chunk of a column that is not loaded.
    Generate,
}

/// What the missing chunk at `pos` holds, from the span of its column in `spans`.
fn missing(spans: &ColumnSpans, pos: ChunkPos) -> Missing {
    match spans.get(Column::of(pos)) {
        Some(span) if pos.y < *span.start() => Missing::Solid,
        Some(span) if pos.y > *span.end() => Missing::Air,
        // Inside a loaded span every layer is in the world; were one missing, the generator
        // still has the right answer.
        _ => Missing::Generate,
    }
}

/// Rebuilds the mesh of the chunk at `pos` into `scratch` after an edit, from the blocks in
/// `world`. Returns `false` (and leaves `scratch` alone) if the chunk is not in the world.
///
/// A neighbour is taken from `world` when it is there. One that is not is what the generator
/// has there, so the mesh hides the same faces whether the chunk sits inside the loaded area, at
/// its edge or under a column's span: in a loaded column (`spans`) that is the solid stand-in
/// below the span and air above it, as in a column job, and in a column that is not loaded the
/// neighbour is generated. For a chunk nobody has edited the result therefore equals
/// [`build_column`]'s mesh. An empty `scratch` afterwards means nothing is visible, which
/// `Renderer::set_tile_mesh` takes as a removal.
pub fn remesh(
    terrain: &Terrain,
    world: &World,
    spans: &ColumnSpans,
    pos: ChunkPos,
    scratch: &mut ChunkMesh,
) -> bool {
    // The spans spare the generation of every missing neighbour in a loaded column: an edit
    // under the terrain changes up to four chunks with up to five missing neighbours each, and
    // generating them cost about a third of the edit (measured, decision 0015 "S3b as built").
    let Some(chunk) = world.get(pos) else {
        return false;
    };
    let loaded = |face: Face| world.get(pos.neighbour(face));
    let missing_kind = Face::ALL.map(|face| {
        loaded(face)
            .is_none()
            .then(|| missing(spans, pos.neighbour(face)))
    });
    let generated: [Option<Chunk>; Face::COUNT] = Face::ALL.map(|face| {
        matches!(
            missing_kind[usize::from(face.index())],
            Some(Missing::Generate)
        )
        .then(|| terrain.generate(pos.neighbour(face)))
    });
    let neighbours = Neighbours::new(Face::ALL.map(|face| {
        let index = usize::from(face.index());
        match missing_kind[index] {
            None => loaded(face),
            Some(Missing::Solid) => Some(&terrain.solid_below),
            Some(Missing::Air) => None,
            Some(Missing::Generate) => generated[index].as_ref(),
        }
    }));
    build_mesh_with_neighbours_into(chunk, &neighbours, &terrain.textures, scratch);
    true
}

#[cfg(test)]
mod tests {
    use vox_world::{BlockId, CHUNK_SIZE, CHUNK_VOLUME, LocalPos, build_mesh_into};

    use super::*;
    use crate::streaming::test_blocks::{BLOCKS, SEED, TERRAIN_LAYER, face_textures};

    fn terrain() -> Terrain {
        Terrain::new(SEED, BLOCKS, face_textures())
    }

    /// Shadows the imported function with the tests' seed and blocks filled in.
    fn generate_chunk(pos: ChunkPos) -> Chunk {
        vox_world::generate_chunk(SEED, pos, &BLOCKS)
    }

    /// The one layer of a column job on the tests' land (every span is [`TERRAIN_LAYER`]), with
    /// its position.
    fn only_layer(column: Column) -> (ChunkPos, Built) {
        let mut scratch = ChunkMesh::default();
        let built = build_column(&terrain(), column, &mut scratch);
        assert_eq!(
            built.span,
            TERRAIN_LAYER..=TERRAIN_LAYER,
            "column {column:?}"
        );
        let mut layers = built.layers;
        assert_eq!(layers.len(), 1);
        layers.remove(0)
    }

    /// The mesh of `pos` built by hand from a world that holds the chunk and its four
    /// horizontal neighbours, with bedrock below.
    fn by_hand(pos: ChunkPos) -> ChunkMesh {
        let terrain = terrain();
        let mut world = World::new();
        world.insert(pos, generate_chunk(pos));
        for face in Face::ALL.into_iter().filter(|face| is_horizontal(*face)) {
            let next = pos.neighbour(face);
            world.insert(next, generate_chunk(next));
        }
        let neighbours = Neighbours::new(Face::ALL.map(|face| {
            world
                .get(pos.neighbour(face))
                .or_else(|| (face == Face::NegY).then_some(&terrain.solid_below))
        }));
        let mut mesh = ChunkMesh::default();
        let chunk = world.get(pos).expect("the chunk was inserted");
        build_mesh_with_neighbours_into(chunk, &neighbours, &terrain.textures, &mut mesh);
        mesh
    }

    /// The chunk at column (0, 0) in the terrain layer.
    fn terrain_origin() -> ChunkPos {
        ChunkPos::new(0, TERRAIN_LAYER, 0)
    }

    /// The height of the top row of the terrain layer.
    fn top_row_y() -> i32 {
        (TERRAIN_LAYER + 1) * CHUNK_SIZE as i32 - 1
    }

    fn area_3x3() -> impl Iterator<Item = Column> {
        (-1..=1).flat_map(|z| (-1..=1).map(move |x| Column::new(x, z)))
    }

    /// A world holding the generated 3 x 3 area in the terrain layer.
    fn world_3x3() -> World {
        let mut world = World::new();
        for column in area_3x3() {
            world.insert(
                column.chunk(TERRAIN_LAYER),
                generate_chunk(column.chunk(TERRAIN_LAYER)),
            );
        }
        world
    }

    #[test]
    fn the_job_matches_a_mesh_built_by_hand() {
        for column in area_3x3() {
            let (pos, built) = only_layer(column);
            assert_eq!(pos, column.chunk(TERRAIN_LAYER));
            assert_eq!(built.mesh, by_hand(pos), "column {column:?}");
        }
    }

    #[test]
    fn the_job_returns_the_generated_chunk() {
        let column = Column::new(2, -1);
        let (pos, built) = only_layer(column);
        let expected = generate_chunk(pos);
        assert!(LocalPos::all().all(|local| built.chunk.get(local) == expected.get(local)));
    }

    #[test]
    fn a_dirty_scratch_does_not_change_the_result() {
        let mut scratch = ChunkMesh::default();
        let mesh_of = |built: BuiltColumn| built.layers.into_iter().map(|(_, b)| b.mesh).collect();
        let first: Vec<ChunkMesh> =
            mesh_of(build_column(&terrain(), Column::new(0, 0), &mut scratch));
        build_column(&terrain(), Column::new(3, 3), &mut scratch);
        let again: Vec<ChunkMesh> =
            mesh_of(build_column(&terrain(), Column::new(0, 0), &mut scratch));
        assert_eq!(again, first);
    }

    #[test]
    fn no_mesh_has_a_downward_face() {
        let bottom = Face::NegY.index();
        for column in area_3x3() {
            let (pos, built) = only_layer(column);
            assert!(
                !built.mesh.indices.is_empty(),
                "terrain chunk {pos:?} has faces"
            );
            assert!(
                // Leaves hang over air, so a face may look down inside the chunk; none lies on
                // the chunk's floor plane, where the stand-in (or the chunk below) hides it.
                built
                    .mesh
                    .vertices
                    .iter()
                    .all(|v| v.face != bottom || v.position[1] != 0),
                "chunk {pos:?} has a downward face on its floor"
            );
        }
    }

    #[test]
    fn neighbours_hide_the_walls_between_chunks() {
        let (_, joined) = only_layer(Column::new(0, 0));
        let mut alone = ChunkMesh::default();
        build_mesh_into(
            &generate_chunk(terrain_origin()),
            &terrain().textures,
            &mut alone,
        );
        assert!(joined.mesh.quad_count() < alone.quad_count());
    }

    #[test]
    fn only_the_layers_of_the_span_are_built_and_counted() {
        let mut scratch = ChunkMesh::default();
        let terrain = terrain();
        // A span reaching one layer under and one over the terrain: the layer under is all
        // stone and the layer over all air, so both have empty meshes that cost no upload.
        let built = build_span(
            &terrain,
            Column::new(0, 0),
            TERRAIN_LAYER - 1..=TERRAIN_LAYER + 1,
            &mut scratch,
        );
        let layers: Vec<i32> = built.layers.iter().map(|(pos, _)| pos.y).collect();
        assert_eq!(
            layers,
            [TERRAIN_LAYER - 1, TERRAIN_LAYER, TERRAIN_LAYER + 1]
        );
        assert!(built.layers[0].1.mesh.indices.is_empty());
        assert!(built.layers[2].1.mesh.indices.is_empty());
        assert_eq!(built.mesh_count(), 1);
        // The terrain layer looks the same as in a one-layer job: the chunks above and below
        // hide exactly what the air and the stand-in did.
        assert_eq!(built.layers[1].1.mesh, by_hand(terrain_origin()));
    }

    #[test]
    fn every_layer_of_a_deep_span_equals_a_remesh() {
        // A column job with a span of three layers against a remesh of its chunks: the job's
        // air above and stand-in below must hide the same faces as the generated neighbours
        // (no spans known) and as the remesh's own stand-ins (the column loaded).
        let terrain = terrain();
        let mut scratch = ChunkMesh::default();
        let column = Column::new(1, -2);
        let span = TERRAIN_LAYER - 2..=TERRAIN_LAYER;
        let built = build_span(&terrain, column, span.clone(), &mut scratch);
        let mut world = World::new();
        for (pos, layer) in &built.layers {
            world.insert(*pos, layer.chunk.clone());
        }
        let mut loaded = ColumnSpans::default();
        loaded.insert(column, span);
        for spans in [ColumnSpans::default(), loaded] {
            for (pos, layer) in &built.layers {
                assert!(remesh(&terrain, &world, &spans, *pos, &mut scratch));
                assert_eq!(scratch, layer.mesh, "layer {}", pos.y);
            }
        }
    }

    #[test]
    fn the_spans_stand_in_for_missing_chunks_exactly_as_the_generator() {
        // Dug blocks deep under a loaded 3 x 3 area, next to chunk borders on every side: the
        // remesh with spans (stand-ins) equals the remesh that generates every missing chunk.
        let terrain = terrain();
        let mut world = world_3x3();
        let mut spans = ColumnSpans::default();
        for column in area_3x3() {
            spans.insert(column, TERRAIN_LAYER..=TERRAIN_LAYER);
        }
        let pos = ChunkPos::new(0, TERRAIN_LAYER - 3, 0);
        world.insert(pos, generate_chunk(pos));
        let last = CHUNK_SIZE as i32 - 1;
        let [x, y, z] = pos.origin();
        for corner in [
            [x, y, z],
            [x + last, y + last, z + last],
            [x + 5, y, z + last],
        ] {
            world.set_block(corner, BlockId::AIR);
        }
        let mut with_spans = ChunkMesh::default();
        let mut generated = ChunkMesh::default();
        assert!(remesh(&terrain, &world, &spans, pos, &mut with_spans));
        assert!(remesh(
            &terrain,
            &world,
            &ColumnSpans::default(),
            pos,
            &mut generated
        ));
        assert!(!with_spans.indices.is_empty());
        assert_eq!(with_spans, generated);
    }

    /// Quads of `mesh` facing `face` on the chunk plane at `height` (local, 0..=32).
    fn quads_on_plane(mesh: &ChunkMesh, face: Face, height: u8) -> usize {
        mesh.vertices
            .chunks(vox_world::VERTICES_PER_QUAD)
            .filter(|quad| {
                quad[0].face == face.index() && quad.iter().all(|v| v.position[1] == height)
            })
            .count()
    }

    #[test]
    fn a_missing_layer_is_air_above_the_span_and_solid_below_it() {
        let terrain = terrain();
        let mut world = world_3x3();
        let mut spans = ColumnSpans::default();
        for column in area_3x3() {
            spans.insert(column, TERRAIN_LAYER..=TERRAIN_LAYER);
        }
        let top = CHUNK_SIZE as u8;
        // A block on the top row of the terrain layer under the missing layer above it, which
        // is air: its top face is drawn. A solid stand-in there would hide it.
        world.set_block([5, top_row_y(), 5], BLOCKS.stone);
        let mut scratch = ChunkMesh::default();
        assert!(remesh(
            &terrain,
            &world,
            &spans,
            terrain_origin(),
            &mut scratch
        ));
        assert_eq!(quads_on_plane(&scratch, Face::PosY, top), 1);
        // The bottom row of layer -1 lies on the missing layer -2, which is rock: no face
        // looks down. Air there would draw the whole underside.
        let below = ChunkPos::new(0, TERRAIN_LAYER - 1, 0);
        world.insert(below, generate_chunk(below));
        assert!(remesh(&terrain, &world, &spans, below, &mut scratch));
        assert_eq!(quads_on_plane(&scratch, Face::NegY, 0), 0);
        assert!(scratch.indices.is_empty());
    }

    #[test]
    fn the_stand_in_below_is_bedrock_all_through() {
        let solid = LocalPos::all().all(|pos| terrain().solid_below.get(pos) == BLOCKS.bedrock);
        assert!(solid);
        assert_eq!(LocalPos::all().count(), CHUNK_VOLUME);
    }

    #[test]
    fn the_filler_block_does_not_change_the_mesh() {
        // The mesher only asks whether the neighbour is solid, so a stone filler gives the same
        // mesh as the bedrock one. Compared with a hand-built stone chunk below.
        let terrain = terrain();
        let mut stone = Chunk::new();
        for pos in LocalPos::all() {
            stone.set(pos, BLOCKS.stone);
        }
        let pos = terrain_origin();
        let chunk = generate_chunk(pos);
        let sides: Vec<Chunk> = Face::ALL
            .into_iter()
            .filter(|face| is_horizontal(*face))
            .map(|face| generate_chunk(pos.neighbour(face)))
            .collect();
        let mesh_below = |below: &Chunk| {
            let mut sides = sides.iter();
            let neighbours = Neighbours::new(Face::ALL.map(|face| {
                if face == Face::NegY {
                    Some(below)
                } else if is_horizontal(face) {
                    sides.next()
                } else {
                    None
                }
            }));
            let mut mesh = ChunkMesh::default();
            build_mesh_with_neighbours_into(&chunk, &neighbours, &terrain.textures, &mut mesh);
            mesh
        };
        assert_eq!(mesh_below(&stone), mesh_below(&terrain.solid_below));
    }

    #[test]
    fn a_remesh_of_an_unedited_chunk_equals_the_job_mesh() {
        let mut scratch = ChunkMesh::default();
        // Alone in the world a chunk generates its missing neighbours; in the full area it
        // reads them. Both give the job's mesh.
        let full = world_3x3();
        for column in area_3x3() {
            let (pos, built) = only_layer(column);
            let mut lone = World::new();
            lone.insert(pos, generate_chunk(pos));
            assert!(remesh(
                &terrain(),
                &lone,
                &ColumnSpans::default(),
                pos,
                &mut scratch
            ));
            assert_eq!(scratch, built.mesh, "lone chunk {pos:?}");
            assert!(remesh(
                &terrain(),
                &full,
                &ColumnSpans::default(),
                pos,
                &mut scratch
            ));
            assert_eq!(scratch, built.mesh, "chunk {pos:?} in the area");
        }
    }

    #[test]
    fn a_remesh_of_a_missing_chunk_does_nothing() {
        let mut scratch = ChunkMesh::default();
        scratch.indices.push(7);
        assert!(!remesh(
            &terrain(),
            &World::new(),
            &ColumnSpans::default(),
            terrain_origin(),
            &mut scratch
        ));
        assert_eq!(scratch.indices, vec![7]);
    }

    #[test]
    fn a_remesh_shows_an_edit_and_the_wall_it_uncovers_next_door() {
        let mut scratch = ChunkMesh::default();
        let mut world = world_3x3();
        let (_, before) = only_layer(Column::new(0, 0));
        let neighbour = ChunkPos::new(1, TERRAIN_LAYER, 0);
        remesh(
            &terrain(),
            &world,
            &ColumnSpans::default(),
            neighbour,
            &mut scratch,
        );
        let neighbour_quads = scratch.quad_count();

        // Dig out the whole column at the +X border of the origin chunk: the origin's mesh
        // changes, and the neighbour on that side now shows a wall where the column was.
        let border_x = CHUNK_SIZE as i32 - 1;
        for y in 0..CHUNK_SIZE as i32 {
            world.set_block([border_x, TERRAIN_LAYER * 32 + y, 0], BlockId::AIR);
        }
        remesh(
            &terrain(),
            &world,
            &ColumnSpans::default(),
            terrain_origin(),
            &mut scratch,
        );
        assert_ne!(scratch, before.mesh);
        remesh(
            &terrain(),
            &world,
            &ColumnSpans::default(),
            neighbour,
            &mut scratch,
        );
        assert!(scratch.quad_count() > neighbour_quads);
    }

    #[test]
    fn a_remesh_of_an_underground_chunk_has_no_faces_until_it_is_dug() {
        // Layer -1 is all stone under layer 0: alone in the world it generates its six
        // neighbours (layer 0 above, stone around and below) and shows nothing.
        let mut scratch = ChunkMesh::default();
        let mut world = World::new();
        let pos = ChunkPos::new(0, -1, 0);
        world.insert(pos, generate_chunk(pos));
        assert!(remesh(
            &terrain(),
            &world,
            &ColumnSpans::default(),
            pos,
            &mut scratch
        ));
        assert!(scratch.indices.is_empty());
        // One block dug out in the middle shows the six walls around it.
        world.set_block([5, -10, 5], BlockId::AIR);
        assert!(remesh(
            &terrain(),
            &world,
            &ColumnSpans::default(),
            pos,
            &mut scratch
        ));
        assert_eq!(scratch.quad_count(), Face::COUNT);
    }
}
