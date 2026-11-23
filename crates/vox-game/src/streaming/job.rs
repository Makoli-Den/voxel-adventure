//! The work of one streaming job: generate a chunk with its horizontal neighbours and mesh it.
//!
//! A job depends on nothing but `(seed, pos)`, so jobs can run in any order on any thread and
//! every mesh is final when it is built (decision 0010). The price is that each chunk is
//! generated five times over the whole area, which is cheap next to meshing.

use std::sync::OnceLock;

use vox_world::{
    Chunk, ChunkMesh, ChunkPos, Face, LocalPos, Neighbours, STONE, build_mesh_with_neighbours_into,
    generate_chunk,
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

/// Generates the chunk at `pos` and its four horizontal neighbours, and meshes it into
/// `scratch`. Returns an exact-size copy, or an empty mesh when nothing is visible.
///
/// Faces towards a generated neighbour are hidden, the side above stays open and the side
/// below faces bedrock. The scratch buffers keep their size for the next job of the same
/// worker, which is why the result is a copy and not the scratch itself.
pub fn build_chunk_mesh(seed: u32, pos: ChunkPos, scratch: &mut ChunkMesh) -> ChunkMesh {
    // HOT PATH: runs for every streamed chunk on a worker thread. The mesh is built into the
    // worker's reused `scratch` (no growth after the first jobs) and only the final exact-size
    // copy is allocated; the bedrock is built once, not per job.
    let chunk = generate_chunk(seed, pos);
    let around: [Option<Chunk>; Face::COUNT] = Face::ALL
        .map(|face| is_horizontal(face).then(|| generate_chunk(seed, pos.neighbour(face))));
    let neighbours = Neighbours::new(Face::ALL.map(|face| {
        let generated = around[usize::from(face.index())].as_ref();
        generated.or_else(|| (face == Face::NegY).then(bedrock))
    }));
    build_mesh_with_neighbours_into(&chunk, &neighbours, scratch);
    if scratch.indices.is_empty() {
        ChunkMesh::default()
    } else {
        scratch.clone()
    }
}

#[cfg(test)]
mod tests {
    use vox_world::{CHUNK_VOLUME, World, build_mesh_into};

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

    #[test]
    fn the_job_matches_a_mesh_built_by_hand() {
        let mut scratch = ChunkMesh::default();
        for pos in area_3x3() {
            let mesh = build_chunk_mesh(SEED, pos, &mut scratch);
            assert_eq!(mesh, by_hand(pos), "chunk {pos:?}");
        }
    }

    #[test]
    fn a_dirty_scratch_does_not_change_the_result() {
        let mut scratch = ChunkMesh::default();
        let first = build_chunk_mesh(SEED, ChunkPos::ORIGIN, &mut scratch);
        build_chunk_mesh(SEED, ChunkPos::new(3, 0, 3), &mut scratch);
        assert_eq!(
            build_chunk_mesh(SEED, ChunkPos::ORIGIN, &mut scratch),
            first
        );
    }

    #[test]
    fn no_mesh_has_a_downward_face() {
        let mut scratch = ChunkMesh::default();
        let bottom = Face::NegY.index();
        for pos in area_3x3() {
            let mesh = build_chunk_mesh(SEED, pos, &mut scratch);
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
        let joined = build_chunk_mesh(SEED, ChunkPos::ORIGIN, &mut scratch);
        let mut alone = ChunkMesh::default();
        build_mesh_into(&generate_chunk(SEED, ChunkPos::ORIGIN), &mut alone);
        assert!(joined.quad_count() < alone.quad_count());
    }

    #[test]
    fn a_chunk_with_nothing_to_draw_gives_an_empty_mesh() {
        // Far above the terrain everything is air.
        let mut scratch = ChunkMesh::default();
        let mesh = build_chunk_mesh(SEED, ChunkPos::new(0, 5, 0), &mut scratch);
        assert_eq!(mesh, ChunkMesh::default());
    }

    #[test]
    fn the_bedrock_is_solid_all_through() {
        let solid = LocalPos::all().all(|pos| bedrock().get(pos) == STONE);
        assert!(solid);
        assert_eq!(LocalPos::all().count(), CHUNK_VOLUME);
    }
}
