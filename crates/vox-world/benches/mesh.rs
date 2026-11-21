//! Benchmarks of the chunk mesher and the terrain generator.
//!
//! Run with `cargo bench -p vox-world` (see `docs/benchmarks.md` for how to read the
//! numbers). Every case works on one 32x32x32 chunk, and the input chunk is built
//! outside the timed closure, so only `build_mesh` or `generate_terrain` is timed.
//! Before the timing starts, the program prints the size of each mesh once, because
//! criterion measures time only.

use std::hint::black_box;
use std::mem::size_of;

use criterion::{Criterion, Throughput};
use vox_world::{BlockId, CHUNK_VOLUME, Chunk, ChunkMesh, ChunkPos, ChunkVertex, Face, LocalPos};
use vox_world::{Neighbours, build_mesh, build_mesh_into, build_mesh_with_neighbours_into};
use vox_world::{generate_chunk, generate_terrain};

const STONE: BlockId = BlockId(1);
/// The seed of the realistic case. `terrain_mesh_has_the_pinned_number_of_quads` uses it too.
const SEED: u32 = 1;

/// A chunk of air only: the cost of walking the block array and finding nothing.
fn empty_chunk() -> Chunk {
    Chunk::new()
}

/// One block in the middle: the fixed cost of a mesh with almost no output.
fn single_block_chunk() -> Chunk {
    let mut chunk = Chunk::new();
    if let Some(pos) = LocalPos::new(16, 16, 16) {
        chunk.set(pos, STONE);
    }
    chunk
}

/// Every block solid: all the work is the neighbour test, only the outer shell is emitted.
fn solid_chunk() -> Chunk {
    chunk_where(|_| true)
}

/// Blocks where `x + y + z` is even: no two solid blocks touch, so almost every face
/// is visible. This is the worst case for the mesher.
fn checkerboard_chunk() -> Chunk {
    chunk_where(|pos| (pos.x() + pos.y() + pos.z()) % 2 == 0)
}

/// Fills the chunk with stone at every position where `solid` is true.
fn chunk_where(solid: impl Fn(LocalPos) -> bool) -> Chunk {
    let mut chunk = Chunk::new();
    for pos in LocalPos::all().filter(|&pos| solid(pos)) {
        chunk.set(pos, STONE);
    }
    chunk
}

/// The mesher cases in the order they appear in the report.
fn mesh_cases() -> [(&'static str, Chunk); 5] {
    [
        ("empty", empty_chunk()),
        ("single_block", single_block_chunk()),
        ("terrain_seed_1", generate_terrain(SEED)),
        ("solid", solid_chunk()),
        ("checkerboard", checkerboard_chunk()),
    ]
}

/// Prints quads, vertices, bytes and `Vec` capacity against length for each case.
fn print_mesh_stats() {
    println!("mesh sizes (printed once, not measured):");
    println!(
        "{:<16} {:>8} {:>9} {:>10} {:>10} {:>10} {:>10} {:>10}",
        "case", "quads", "vertices", "bytes", "vtx cap", "vtx len", "idx cap", "idx len"
    );
    for (name, chunk) in mesh_cases() {
        let mesh = build_mesh(&chunk);
        println!(
            "{:<16} {:>8} {:>9} {:>10} {:>10} {:>10} {:>10} {:>10}",
            name,
            mesh.indices.len() / 6,
            mesh.vertices.len(),
            mesh_bytes(&mesh),
            mesh.vertices.capacity(),
            mesh.vertices.len(),
            mesh.indices.capacity(),
            mesh.indices.len(),
        );
    }
    println!();
}

/// Size of the mesh data: vertices at 6 bytes plus indices at 4 bytes.
fn mesh_bytes(mesh: &ChunkMesh) -> usize {
    mesh.vertices.len() * size_of::<ChunkVertex>() + mesh.indices.len() * size_of::<u32>()
}

/// Times `build_mesh` on each case. Throughput is blocks per second, counting every
/// block of the chunk, air included, so the cases can be compared by the same unit.
fn bench_mesh(c: &mut Criterion) {
    let mut group = c.benchmark_group("mesh");
    group.throughput(Throughput::Elements(CHUNK_VOLUME as u64));
    for (name, chunk) in mesh_cases() {
        group.bench_function(name, |b| b.iter(|| build_mesh(black_box(&chunk))));
    }
    group.finish();
}

/// Times `build_mesh_into` on one mesh that is reused, as a worker thread would. The
/// mesh is warmed up first, so its capacity already has the final size and the timed
/// closure does no allocation. Compare each case with the same name in `mesh`.
fn bench_mesh_reused(c: &mut Criterion) {
    let mut group = c.benchmark_group("mesh_reused");
    group.throughput(Throughput::Elements(CHUNK_VOLUME as u64));
    let cases = [
        ("terrain_seed_1", generate_terrain(SEED)),
        ("solid", solid_chunk()),
        ("checkerboard", checkerboard_chunk()),
    ];
    for (name, chunk) in cases {
        let mut mesh = ChunkMesh::default();
        build_mesh_into(&chunk, &mut mesh);
        group.bench_function(name, |b| {
            b.iter(|| {
                build_mesh_into(black_box(&chunk), &mut mesh);
                black_box(&mesh);
            })
        });
    }
    group.finish();
}

/// Times `build_mesh_with_neighbours_into` on the origin terrain chunk with all six
/// neighbours present, on one reused mesh. Compare with `mesh_reused/terrain_seed_1`: the
/// difference is the cost of looking into the neighbouring chunks.
fn bench_mesh_neighbours(c: &mut Criterion) {
    let mut group = c.benchmark_group("mesh_neighbours");
    group.throughput(Throughput::Elements(CHUNK_VOLUME as u64));
    let chunk = generate_terrain(SEED);
    let around = Face::ALL.map(|face| generate_chunk(SEED, ChunkPos::ORIGIN.neighbour(face)));
    let neighbours = Neighbours::new(around.each_ref().map(Some));
    let mut mesh = ChunkMesh::default();
    build_mesh_with_neighbours_into(&chunk, &neighbours, &mut mesh);
    println!(
        "terrain_seed_1 with neighbours: {} quads",
        mesh.indices.len() / 6
    );
    group.bench_function("terrain_seed_1", |b| {
        b.iter(|| {
            build_mesh_with_neighbours_into(black_box(&chunk), black_box(&neighbours), &mut mesh);
            black_box(&mesh);
        })
    });
    group.finish();
}

/// Times `generate_terrain` alone (noise, column fill and the allocation of the chunk).
fn bench_terrain(c: &mut Criterion) {
    let mut group = c.benchmark_group("terrain");
    group.throughput(Throughput::Elements(CHUNK_VOLUME as u64));
    group.bench_function("generate_seed_1", |b| {
        b.iter(|| generate_terrain(black_box(SEED)))
    });
    // A chunk away from the origin, with a negative coordinate: the same work, other noise cells.
    group.bench_function("generate_chunk_far", |b| {
        b.iter(|| generate_chunk(black_box(SEED), black_box(ChunkPos::new(3, 0, -2))))
    });
    group.finish();
}

// `criterion_group!` generates a public function without a doc comment.
#[allow(missing_docs)]
mod groups {
    use super::{bench_mesh, bench_mesh_neighbours, bench_mesh_reused, bench_terrain};
    use criterion::criterion_group;
    criterion_group!(
        benches,
        bench_mesh,
        bench_mesh_reused,
        bench_mesh_neighbours,
        bench_terrain
    );
}

/// A custom `main` instead of `criterion_main!`, only to print the mesh sizes first.
fn main() {
    print_mesh_stats();
    groups::benches();
    Criterion::default().configure_from_args().final_summary();
}
