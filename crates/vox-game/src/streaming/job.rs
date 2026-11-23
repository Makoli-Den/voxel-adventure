//! The work of one streaming job: generate a chunk with its horizontal neighbours and mesh it,
//! and the same meshing for a chunk of the game's world after an edit.
//!
//! A job depends on nothing but `(seed, pos)`, so jobs can run in any order on any thread and
//! a mesh is final when it is built, until a neighbour is edited (decisions 0010 and 0011).
//! The price is that each chunk is
//! generated five times over the whole area, which is cheap next to meshing.

use std::sync::OnceLock;

use vox_world::{
    Chunk, ChunkMesh, ChunkPos, Face, LocalPos, Neighbours, STONE, World,
    build_mesh_with_neighbours_into, generate_chunk,
};

/// The stand-in for everything below the terrain: a chunk of solid stone, so the underside of
/// the terrain, which nobody can see, produces no faces.
fn bedrock() -> &'static Chunk {
    static BEDROCK: OnceLock<Chunk> = OnceLock::new();
    BEDROCK.get_or_init(|| {
        let mut chunk = Chunk::new();
        for pos in LocalPos::all() {
            chunk.set(pos, STONE);
        }
        chunk
    })
}

/// Whether the neighbour on side `face` is generated for a job: the four horizontal ones.
/// The layer above the terrain is air, so a missing `PosY` is correct, and `NegY` is the
/// bedrock stand-in.
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

/// Meshes `chunk`, which lies at `pos`, into `scratch`.
///
/// A neighbour is taken from `world` when it is loaded there. A horizontal neighbour that is
/// not loaded is generated from `seed`, exactly as a streaming job does, so the mesh hides the
/// same faces whether the chunk sits inside the loaded area or at its edge. The side above
/// the terrain stays open and the side below faces bedrock.
fn mesh_with_neighbours(
    seed: u32,
    chunk: &Chunk,
    pos: ChunkPos,
    world: Option<&World>,
    scratch: &mut ChunkMesh,
) {
    let loaded = |face: Face| world.and_then(|world| world.get(pos.neighbour(face)));
    let generated: [Option<Chunk>; Face::COUNT] = Face::ALL.map(|face| {
        (is_horizontal(face) && loaded(face).is_none())
            .then(|| generate_chunk(seed, pos.neighbour(face)))
    });
    let neighbours = Neighbours::new(Face::ALL.map(|face| {
        loaded(face)
            .or(generated[usize::from(face.index())].as_ref())
            .or_else(|| (face == Face::NegY).then(bedrock))
    }));
    build_mesh_with_neighbours_into(chunk, &neighbours, scratch);
}

/// Generates the chunk at `pos` and its four horizontal neighbours, and meshes it into
/// `scratch`. Returns the chunk and an exact-size copy of the mesh, or an empty mesh when
/// nothing is visible.
///
/// Faces towards a generated neighbour are hidden, the side above stays open and the side
/// below faces bedrock. The scratch buffers keep their size for the next job of the same
/// worker, which is why the result is a copy and not the scratch itself. The neighbours are
/// dropped; the chunk is moved out, not copied.
pub fn build_chunk(seed: u32, pos: ChunkPos, scratch: &mut ChunkMesh) -> Built {
    // HOT PATH: runs for every streamed chunk on a worker thread. The mesh is built into the
    // worker's reused `scratch` (no growth after the first jobs) and only the final exact-size
    // copy is allocated; the bedrock is built once, not per job.
    let chunk = generate_chunk(seed, pos);
    mesh_with_neighbours(seed, &chunk, pos, None, scratch);
    let mesh = if scratch.indices.is_empty() {
        ChunkMesh::default()
    } else {
        scratch.clone()
    };
    Built { chunk, mesh }
}

/// Rebuilds the mesh of the loaded chunk at `pos` into `scratch` after an edit, from the
/// blocks in `world`. Returns `false` (and leaves `scratch` alone) if the chunk is not loaded.
///
/// For a chunk nobody has edited the result equals [`build_chunk`]'s mesh. An empty `scratch`
/// afterwards means nothing is visible, which `Renderer::set_chunk_mesh` takes as a removal.
pub fn remesh(seed: u32, world: &World, pos: ChunkPos, scratch: &mut ChunkMesh) -> bool {
    let Some(chunk) = world.get(pos) else {
        return false;
    };
    mesh_with_neighbours(seed, chunk, pos, Some(world), scratch);
    true
}

#[cfg(test)]
mod tests {
    use vox_world::{BlockId, CHUNK_SIZE, CHUNK_VOLUME, build_mesh_into};

    use super::*;

    /// The seed of the tests' terrain.
    const SEED: u32 = 1;

    /// The mesh of `pos` built by hand from a world that holds the chunk and its four
    /// horizontal neighbours, with bedrock below.
    fn by_hand(pos: ChunkPos) -> ChunkMesh {
        let mut world = World::new();
        world.insert(pos, generate_chunk(SEED, pos));
        for face in Face::ALL.into_iter().filter(|face| is_horizontal(*face)) {
            let next = pos.neighbour(face);
            world.insert(next, generate_chunk(SEED, next));
        }
        let neighbours = Neighbours::new(Face::ALL.map(|face| {
            world
                .get(pos.neighbour(face))
                .or_else(|| (face == Face::NegY).then(bedrock))
        }));
        let mut mesh = ChunkMesh::default();
        let chunk = world.get(pos).expect("the chunk was inserted");
        build_mesh_with_neighbours_into(chunk, &neighbours, &mut mesh);
        mesh
    }

    fn area_3x3() -> impl Iterator<Item = ChunkPos> {
        (-1..=1).flat_map(|z| (-1..=1).map(move |x| ChunkPos::new(x, 0, z)))
    }

    /// A world holding the generated 3 x 3 area.
    fn world_3x3() -> World {
        let mut world = World::new();
        for pos in area_3x3() {
            world.insert(pos, generate_chunk(SEED, pos));
        }
        world
    }

    #[test]
    fn the_job_matches_a_mesh_built_by_hand() {
        let mut scratch = ChunkMesh::default();
        for pos in area_3x3() {
            let mesh = build_chunk(SEED, pos, &mut scratch).mesh;
            assert_eq!(mesh, by_hand(pos), "chunk {pos:?}");
        }
    }

    #[test]
    fn the_job_returns_the_generated_chunk() {
        let mut scratch = ChunkMesh::default();
        let pos = ChunkPos::new(2, 0, -1);
        let built = build_chunk(SEED, pos, &mut scratch);
        let expected = generate_chunk(SEED, pos);
        assert!(LocalPos::all().all(|local| built.chunk.get(local) == expected.get(local)));
    }

    #[test]
    fn a_dirty_scratch_does_not_change_the_result() {
        let mut scratch = ChunkMesh::default();
        let first = build_chunk(SEED, ChunkPos::ORIGIN, &mut scratch).mesh;
        build_chunk(SEED, ChunkPos::new(3, 0, 3), &mut scratch);
        assert_eq!(
            build_chunk(SEED, ChunkPos::ORIGIN, &mut scratch).mesh,
            first
        );
    }

    #[test]
    fn no_mesh_has_a_downward_face() {
        let mut scratch = ChunkMesh::default();
        let bottom = Face::NegY.index();
        for pos in area_3x3() {
            let mesh = build_chunk(SEED, pos, &mut scratch).mesh;
            assert!(!mesh.indices.is_empty(), "terrain chunk {pos:?} has faces");
            assert!(
                mesh.vertices.iter().all(|v| v.face != bottom),
                "chunk {pos:?} has a downward face"
            );
        }
    }

    #[test]
    fn neighbours_hide_the_walls_between_chunks() {
        let mut scratch = ChunkMesh::default();
        let joined = build_chunk(SEED, ChunkPos::ORIGIN, &mut scratch).mesh;
        let mut alone = ChunkMesh::default();
        build_mesh_into(&generate_chunk(SEED, ChunkPos::ORIGIN), &mut alone);
        assert!(joined.quad_count() < alone.quad_count());
    }

    #[test]
    fn a_chunk_with_nothing_to_draw_gives_an_empty_mesh() {
        // Far above the terrain everything is air.
        let mut scratch = ChunkMesh::default();
        let mesh = build_chunk(SEED, ChunkPos::new(0, 5, 0), &mut scratch).mesh;
        assert_eq!(mesh, ChunkMesh::default());
    }

    #[test]
    fn the_bedrock_is_solid_all_through() {
        let solid = LocalPos::all().all(|pos| bedrock().get(pos) == STONE);
        assert!(solid);
        assert_eq!(LocalPos::all().count(), CHUNK_VOLUME);
    }

    #[test]
    fn a_remesh_of_an_unedited_chunk_equals_the_job_mesh() {
        let mut scratch = ChunkMesh::default();
        // Alone in the world a chunk generates its missing neighbours; in the full area it
        // reads them. Both give the job's mesh.
        let full = world_3x3();
        for pos in area_3x3() {
            let mut lone = World::new();
            lone.insert(pos, generate_chunk(SEED, pos));
            let expected = build_chunk(SEED, pos, &mut scratch).mesh;
            assert!(remesh(SEED, &lone, pos, &mut scratch));
            assert_eq!(scratch, expected, "lone chunk {pos:?}");
            assert!(remesh(SEED, &full, pos, &mut scratch));
            assert_eq!(scratch, expected, "chunk {pos:?} in the area");
        }
    }

    #[test]
    fn a_remesh_of_a_missing_chunk_does_nothing() {
        let mut scratch = ChunkMesh::default();
        scratch.indices.push(7);
        assert!(!remesh(SEED, &World::new(), ChunkPos::ORIGIN, &mut scratch));
        assert_eq!(scratch.indices, vec![7]);
    }

    #[test]
    fn a_remesh_shows_an_edit_and_the_wall_it_uncovers_next_door() {
        let mut scratch = ChunkMesh::default();
        let mut world = world_3x3();
        let before = build_chunk(SEED, ChunkPos::ORIGIN, &mut scratch).mesh;
        let neighbour = ChunkPos::new(1, 0, 0);
        remesh(SEED, &world, neighbour, &mut scratch);
        let neighbour_quads = scratch.quad_count();

        // Dig out the whole column at the +X border of the origin chunk: the origin's mesh
        // changes, and the neighbour on that side now shows a wall where the column was.
        let border_x = CHUNK_SIZE as i32 - 1;
        for y in 0..CHUNK_SIZE as i32 {
            world.set_block([border_x, y, 0], BlockId::AIR);
        }
        remesh(SEED, &world, ChunkPos::ORIGIN, &mut scratch);
        assert_ne!(scratch, before);
        remesh(SEED, &world, neighbour, &mut scratch);
        assert!(scratch.quad_count() > neighbour_quads);
    }
}
