//! Builds the world the game shows: generates terrain chunks and meshes them on every core.
//!
//! The whole area is built once at startup. Streaming chunks in and out around the player
//! will replace this later; the pieces here (positions, parallel work, neighbour-aware
//! meshing) are the ones streaming will reuse.

use std::fmt;
use std::num::NonZeroUsize;
use std::thread;
use std::time::{Duration, Instant};

use vox_world::{
    Chunk, ChunkMesh, ChunkPos, Face, LocalPos, Neighbours, STONE, World,
    build_mesh_with_neighbours_into, generate_chunk,
};

/// The meshes of a loaded area, ready for the renderer, and how long building them took.
pub struct Loaded {
    /// The mesh of every chunk that has anything to draw. Empty chunks are left out.
    pub meshes: Vec<(ChunkPos, ChunkMesh)>,
    /// Counts and timings of the load.
    pub stats: LoadStats,
}

/// What a load did, for the log line the game prints.
#[derive(Clone, Copy, Debug)]
pub struct LoadStats {
    /// Chunks that were generated.
    pub chunks: usize,
    /// Quads in all meshes together.
    pub quads: usize,
    /// Worker threads used.
    pub threads: usize,
    /// Time to generate all terrain chunks.
    pub generating: Duration,
    /// Time to build all meshes.
    pub meshing: Duration,
}

impl fmt::Display for LoadStats {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} chunks, {} quads, generated in {:.1?} and meshed in {:.1?} on {} threads",
            self.chunks, self.quads, self.generating, self.meshing, self.threads
        )
    }
}

/// Generates the terrain of the square area of `(2 * radius + 1)` by `(2 * radius + 1)` chunks
/// around the origin chunk (one chunk layer high: all the terrain lies in layer 0) and builds
/// the mesh of every chunk, using all cores for both steps. A negative `radius` counts as 0,
/// which loads only the origin chunk.
///
/// Faces between neighbouring chunks are hidden. Below the area the world counts as solid
/// stone, so the underside of the terrain, which nobody can see, produces no faces. The sides
/// of the area stay open, so the edge of the world is visible.
pub fn load_area(seed: u32, radius: i32) -> Loaded {
    let threads = worker_count();
    let positions = area_positions(radius);

    let started = Instant::now();
    let generated = parallel_map(
        &positions,
        threads,
        || (),
        |(), &pos| (pos, generate_chunk(seed, pos)),
    );
    let mut world = World::new();
    for (pos, chunk) in generated {
        world.insert(pos, chunk);
    }
    let generating = started.elapsed();

    let started = Instant::now();
    let bedrock = bedrock_chunk();
    let meshes: Vec<(ChunkPos, ChunkMesh)> =
        parallel_map(&positions, threads, ChunkMesh::default, |scratch, &pos| {
            let chunk = world.get(pos)?;
            let neighbours = neighbours_over_bedrock(&world, &bedrock, pos);
            build_mesh_with_neighbours_into(chunk, &neighbours, scratch);
            // The scratch buffers keep their size for the next chunk of this worker, so the
            // mesh handed on is an exact-size copy.
            (!scratch.indices.is_empty()).then(|| (pos, scratch.clone()))
        })
        .into_iter()
        .flatten()
        .collect();
    let meshing = started.elapsed();

    let quads = meshes.iter().map(|(_, mesh)| mesh.quad_count()).sum();
    Loaded {
        meshes,
        stats: LoadStats {
            chunks: positions.len(),
            quads,
            threads,
            generating,
            meshing,
        },
    }
}

/// Number of worker threads: one per available core, at least one.
fn worker_count() -> usize {
    thread::available_parallelism().map_or(1, NonZeroUsize::get)
}

/// All chunk positions of the square area around the origin, in layer 0.
fn area_positions(radius: i32) -> Vec<ChunkPos> {
    let radius = radius.max(0);
    (-radius..=radius)
        .flat_map(|z| (-radius..=radius).map(move |x| ChunkPos::new(x, 0, z)))
        .collect()
}

/// A chunk of solid stone, the stand-in for everything below the loaded area.
fn bedrock_chunk() -> Chunk {
    let mut chunk = Chunk::new();
    for pos in LocalPos::all() {
        chunk.set(pos, STONE);
    }
    chunk
}

/// The neighbours of the chunk at `pos`: the loaded chunks next to it. Only the side below
/// (`NegY`) gets `bedrock` where nothing is loaded; the other five sides stay open, so a
/// missing chunk there counts as air.
fn neighbours_over_bedrock<'a>(
    world: &'a World,
    bedrock: &'a Chunk,
    pos: ChunkPos,
) -> Neighbours<'a> {
    Neighbours::new(Face::ALL.map(|face| {
        world
            .get(pos.neighbour(face))
            .or_else(|| (face == Face::NegY).then_some(bedrock))
    }))
}

/// Applies `work` to every item on `threads` worker threads and returns the results in the
/// order of the items.
///
/// The items are cut into one contiguous part per thread. Each thread creates its own state
/// with `init` and passes it to every call of `work`, which lets a worker reuse buffers.
/// A panic in a worker is passed on to the caller.
fn parallel_map<T, R, S>(
    items: &[T],
    threads: usize,
    init: impl Fn() -> S + Sync,
    work: impl Fn(&mut S, &T) -> R + Sync,
) -> Vec<R>
where
    T: Sync,
    R: Send,
{
    let per_thread = items.len().div_ceil(threads.max(1)).max(1);
    thread::scope(|scope| {
        let handles: Vec<_> = items
            .chunks(per_thread)
            .map(|part| {
                let (init, work) = (&init, &work);
                scope.spawn(move || {
                    let mut state = init();
                    part.iter()
                        .map(|item| work(&mut state, item))
                        .collect::<Vec<R>>()
                })
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|handle| match handle.join() {
                Ok(results) => results,
                Err(payload) => std::panic::resume_unwind(payload),
            })
            .collect()
    })
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use vox_world::{CHUNK_SIZE, VERTICES_PER_QUAD, build_mesh_into};

    use super::*;

    #[test]
    fn area_has_one_chunk_per_cell_around_the_origin() {
        let positions = area_positions(2);
        assert_eq!(positions.len(), 25);
        assert_eq!(positions.iter().collect::<HashSet<_>>().len(), 25);
        assert!(positions.contains(&ChunkPos::ORIGIN));
        assert!(
            positions
                .iter()
                .all(|p| p.y == 0 && p.x.abs() <= 2 && p.z.abs() <= 2)
        );
    }

    #[test]
    fn a_negative_radius_counts_as_zero() {
        assert_eq!(area_positions(-3), vec![ChunkPos::ORIGIN]);
    }

    #[test]
    fn parallel_map_keeps_the_order_for_any_thread_count() {
        let items: Vec<u32> = (0..37).collect();
        for threads in [0, 1, 2, 3, 8, 100] {
            let doubled = parallel_map(&items, threads, || (), |(), &x| x * 2);
            assert_eq!(doubled, items.iter().map(|x| x * 2).collect::<Vec<_>>());
        }
    }

    #[test]
    fn parallel_map_of_nothing_is_nothing() {
        let none: [u32; 0] = [];
        assert!(parallel_map(&none, 4, || (), |(), &x| x).is_empty());
    }

    #[test]
    fn every_worker_gets_its_own_state_exactly_once() {
        let created = AtomicUsize::new(0);
        let items: Vec<u32> = (0..10).collect();
        let sums = parallel_map(
            &items,
            4,
            || {
                created.fetch_add(1, Ordering::SeqCst);
                0_u32
            },
            |sum, &x| {
                *sum += x;
                *sum
            },
        );
        // 10 items over 4 threads: parts of 3, 3, 3 and 1, so 4 states, each a running sum.
        assert_eq!(created.load(Ordering::SeqCst), 4);
        assert_eq!(sums, vec![0, 1, 3, 3, 7, 12, 6, 13, 21, 9]);
    }

    #[test]
    fn at_least_one_worker() {
        assert!(worker_count() >= 1);
    }

    #[test]
    fn loading_matches_a_plain_single_threaded_build() {
        let loaded = load_area(1, 1);
        assert_eq!(loaded.stats.chunks, 9);
        assert_eq!(loaded.meshes.len(), 9);

        // The same thing, built by hand on one thread.
        let mut world = World::new();
        for pos in area_positions(1) {
            world.insert(pos, generate_chunk(1, pos));
        }
        let bedrock = bedrock_chunk();
        for (pos, mesh) in &loaded.meshes {
            let mut expected = ChunkMesh::default();
            let chunk = world.get(*pos).expect("the area contains this chunk");
            let neighbours = neighbours_over_bedrock(&world, &bedrock, *pos);
            build_mesh_with_neighbours_into(chunk, &neighbours, &mut expected);
            assert_eq!(mesh, &expected, "chunk {pos:?}");
        }
        let total: usize = loaded.meshes.iter().map(|(_, m)| m.quad_count()).sum();
        assert_eq!(loaded.stats.quads, total);
    }

    #[test]
    fn the_underside_of_the_terrain_produces_no_faces() {
        let loaded = load_area(1, 1);
        let bottom = Face::NegY.index();
        for (pos, mesh) in &loaded.meshes {
            assert!(
                mesh.vertices.iter().all(|v| v.face != bottom),
                "chunk {pos:?} has a downward face"
            );
        }
    }

    #[test]
    fn without_bedrock_the_underside_would_be_drawn() {
        // The reason for the bedrock: a lone terrain chunk shows 1024 faces on its bottom.
        let chunk = generate_chunk(1, ChunkPos::ORIGIN);
        let mut mesh = ChunkMesh::default();
        build_mesh_into(&chunk, &mut mesh);
        let bottom = Face::NegY.index();
        let down = mesh.vertices.iter().filter(|v| v.face == bottom).count() / VERTICES_PER_QUAD;
        assert_eq!(down, CHUNK_SIZE * CHUNK_SIZE);
    }

    /// The number of downward faces of the chunk at `pos`, meshed with the neighbours the
    /// loader would give it.
    fn downward_faces(world: &World, bedrock: &Chunk, pos: ChunkPos) -> usize {
        let chunk = world.get(pos).expect("the chunk is in the world");
        let mut mesh = ChunkMesh::default();
        build_mesh_with_neighbours_into(
            chunk,
            &neighbours_over_bedrock(world, bedrock, pos),
            &mut mesh,
        );
        let bottom = Face::NegY.index();
        mesh.vertices.iter().filter(|v| v.face == bottom).count() / VERTICES_PER_QUAD
    }

    #[test]
    fn bedrock_replaces_only_a_missing_chunk_below() {
        let bedrock = bedrock_chunk();
        let above = ChunkPos::new(0, 1, 0);

        // Nothing below: the bedrock hides the underside of the solid chunk.
        let mut alone = World::new();
        alone.insert(above, bedrock_chunk());
        assert_eq!(downward_faces(&alone, &bedrock, above), 0);

        // An air chunk below is loaded and wins over the bedrock: the underside shows.
        let mut stacked = World::new();
        stacked.insert(ChunkPos::ORIGIN, Chunk::new());
        stacked.insert(above, bedrock_chunk());
        assert_eq!(
            downward_faces(&stacked, &bedrock, above),
            CHUNK_SIZE * CHUNK_SIZE
        );
    }

    #[test]
    fn faces_between_loaded_chunks_are_hidden() {
        let loaded = load_area(1, 1);
        let centre = loaded
            .meshes
            .iter()
            .find(|(pos, _)| *pos == ChunkPos::ORIGIN)
            .map(|(_, mesh)| mesh)
            .expect("the centre chunk has terrain");
        let isolated = {
            let mut mesh = ChunkMesh::default();
            build_mesh_into(&generate_chunk(1, ChunkPos::ORIGIN), &mut mesh);
            mesh
        };
        // The isolated chunk shows its four side walls and its bottom, the loaded one does not.
        assert!(centre.indices.len() < isolated.indices.len());
    }
}
