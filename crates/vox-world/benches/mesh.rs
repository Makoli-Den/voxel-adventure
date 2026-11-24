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
use vox_world::solid_chunk as solid_stand_in;
use vox_world::{
    BlockId, CHUNK_SIZE, CHUNK_VOLUME, Chunk, ChunkMesh, ChunkPos, ChunkVertex, Face, FaceTextures,
    LocalPos, TextureLayer,
};
use vox_world::{MAX_LOD_LEVEL, TilePos, build_tile_mesh_into, generate_lod_chunk, skirt_quads};
use vox_world::{Neighbours, build_mesh, build_mesh_into, build_mesh_with_neighbours_into};
use vox_world::{TerrainBlocks, column_span, generate_chunk, generate_terrain};

/// The terrain blocks of the benchmarks. Plain ids: the generator only writes numbers, so no
/// registry is needed to time it.
const TERRAIN: TerrainBlocks = TerrainBlocks {
    grass: BlockId(1),
    dirt: BlockId(2),
    stone: BlockId(3),
    bedrock: BlockId(4),
};
/// Blocks the benchmark table covers: air and every id up to bedrock, the highest terrain
/// block (checked below); the solid and checkerboard cases use ids among them.
const TABLE_BLOCKS: u16 = TERRAIN.bedrock.0 + 1;

const _: () = assert!(TERRAIN.grass.0 < TERRAIN.bedrock.0 && TERRAIN.dirt.0 < TERRAIN.bedrock.0);
const _: () = assert!(TERRAIN.stone.0 < TERRAIN.bedrock.0);

/// A table where the layer of every face equals the block id, one array read per quad.
fn textures() -> FaceTextures {
    FaceTextures::from_layers(
        (0..TABLE_BLOCKS)
            .map(|id| [TextureLayer(id); Face::COUNT])
            .collect(),
    )
}

/// Coordinate of the middle of the chunk along an axis: far from every border, so the
/// single block of `single_block_chunk` has all six neighbours inside the chunk.
const MIDDLE: usize = CHUNK_SIZE / 2;
/// The seed of the realistic case. `terrain_mesh_has_the_pinned_number_of_quads` uses it too.
const SEED: u32 = 1;

/// A chunk of air only: the cost of walking the block array and finding nothing.
fn empty_chunk() -> Chunk {
    Chunk::new()
}

/// One block in the middle: the fixed cost of a mesh with almost no output.
fn single_block_chunk() -> Chunk {
    let mut chunk = Chunk::new();
    if let Some(pos) = LocalPos::new(MIDDLE, MIDDLE, MIDDLE) {
        chunk.set(pos, TERRAIN.stone);
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
        chunk.set(pos, TERRAIN.stone);
    }
    chunk
}

/// Height of the stone layer of `slab_chunk`, in blocks: a quarter of the chunk.
const SLAB_HEIGHT: usize = CHUNK_SIZE / 4;

/// A flat layer of stone from the bottom up to `SLAB_HEIGHT`: the best case for greedy
/// meshing, since every side is one rectangle.
fn slab_chunk() -> Chunk {
    chunk_where(|pos| usize::from(pos.y()) < SLAB_HEIGHT)
}

/// The mesher cases in the order they appear in the report.
fn mesh_cases() -> Vec<(&'static str, Chunk)> {
    vec![
        ("empty", empty_chunk()),
        ("single_block", single_block_chunk()),
        ("terrain_seed_1", generate_terrain(SEED, &TERRAIN)),
        ("slab", slab_chunk()),
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
        let mesh = build_mesh(&chunk, &textures());
        println!(
            "{:<16} {:>8} {:>9} {:>10} {:>10} {:>10} {:>10} {:>10}",
            name,
            mesh.quad_count(),
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
    let textures = textures();
    for (name, chunk) in mesh_cases() {
        group.bench_function(name, |b| {
            b.iter(|| build_mesh(black_box(&chunk), black_box(&textures)))
        });
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
        ("terrain_seed_1", generate_terrain(SEED, &TERRAIN)),
        ("solid", solid_chunk()),
        ("checkerboard", checkerboard_chunk()),
    ];
    let textures = textures();
    for (name, chunk) in cases {
        let mut mesh = ChunkMesh::default();
        build_mesh_into(&chunk, &textures, &mut mesh);
        group.bench_function(name, |b| {
            b.iter(|| {
                build_mesh_into(black_box(&chunk), &textures, &mut mesh);
                black_box(&mesh);
            })
        });
    }
    group.finish();
}

/// The level of the benchmarked tile mesh: cells of 4 blocks, a tile of 128 blocks, the middle
/// of the range a quadtree uses.
const LOD_MESH_LEVEL: u8 = 2;

/// The levels whose generation is timed: fine, middle and coarsest (`MAX_LOD_LEVEL`).
const LOD_GENERATE_LEVELS: [u8; 3] = [1, LOD_MESH_LEVEL, MAX_LOD_LEVEL];

/// Converts a share (0 to 1) to percent for printing.
const PERCENT: f64 = 100.0;

/// The tile position of the LOD cases: the origin tile of its level, where the terrain lies.
fn lod_tile(level: u8) -> TilePos {
    TilePos::new(level, ChunkPos::ORIGIN)
}

/// Times a LOD tile meshed alone (skirts on every side, solid bedrock below), on one reused
/// mesh like `mesh_reused`. Prints the quads and the skirt share once.
fn bench_lod_mesh(c: &mut Criterion) {
    let mut group = c.benchmark_group("mesh_reused");
    group.throughput(Throughput::Elements(CHUNK_VOLUME as u64));
    let tile = generate_lod_chunk(SEED, lod_tile(LOD_MESH_LEVEL), &TERRAIN);
    let below = solid_stand_in(TERRAIN.bedrock);
    let textures = textures();
    let mut mesh = ChunkMesh::default();
    build_tile_mesh_into(&tile, &below, &textures, &mut mesh);
    let skirts = skirt_quads(&mesh);
    println!(
        "lod_tile_level_{LOD_MESH_LEVEL}: {} quads, {} skirt quads ({:.1} percent)",
        mesh.quad_count(),
        skirts,
        PERCENT * skirts as f64 / mesh.quad_count() as f64
    );
    group.bench_function(format!("lod_tile_level_{LOD_MESH_LEVEL}"), |b| {
        b.iter(|| {
            build_tile_mesh_into(black_box(&tile), &below, &textures, &mut mesh);
            black_box(&mesh);
        })
    });
    group.finish();
}

/// Times `build_mesh_with_neighbours_into` on the origin terrain chunk with all six
/// neighbours present, on one reused mesh. Compare with `mesh_reused/terrain_seed_1`: the
/// difference is the cost of looking into the neighbouring chunks.
fn bench_mesh_neighbours(c: &mut Criterion) {
    let mut group = c.benchmark_group("mesh_neighbours");
    group.throughput(Throughput::Elements(CHUNK_VOLUME as u64));
    let chunk = generate_terrain(SEED, &TERRAIN);
    let around =
        Face::ALL.map(|face| generate_chunk(SEED, ChunkPos::ORIGIN.neighbour(face), &TERRAIN));
    let neighbours = Neighbours::new(around.each_ref().map(Some));
    let textures = textures();
    let mut mesh = ChunkMesh::default();
    build_mesh_with_neighbours_into(&chunk, &neighbours, &textures, &mut mesh);
    println!(
        "terrain_seed_1 with neighbours: {} quads",
        mesh.quad_count()
    );
    group.bench_function("terrain_seed_1", |b| {
        b.iter(|| {
            build_mesh_with_neighbours_into(
                black_box(&chunk),
                black_box(&neighbours),
                &textures,
                &mut mesh,
            );
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
        b.iter(|| generate_terrain(black_box(SEED), &TERRAIN))
    });
    // A chunk away from the origin, with a negative coordinate: the same work, other noise cells.
    group.bench_function("generate_chunk_far", |b| {
        b.iter(|| {
            generate_chunk(
                black_box(SEED),
                black_box(ChunkPos::new(3, 0, -2)),
                &TERRAIN,
            )
        })
    });
    // LOD tiles: 1,024 samples and one cell fill each, about the cost of one chunk.
    for level in LOD_GENERATE_LEVELS {
        group.bench_function(format!("generate_lod_level_{level}"), |b| {
            b.iter(|| generate_lod_chunk(black_box(SEED), lod_tile(level), &TERRAIN))
        });
    }
    // The column span: 34 x 34 height samples, the worker's extra cost per column (decision 0015).
    group.bench_function("column_span", |b| {
        b.iter(|| column_span(black_box(SEED), black_box(3), black_box(-2)))
    });
    group.finish();
}

// `criterion_group!` generates a public function without a doc comment.
#[allow(missing_docs)]
mod groups {
    use super::{
        bench_lod_mesh, bench_mesh, bench_mesh_neighbours, bench_mesh_reused, bench_terrain,
    };
    use criterion::criterion_group;
    criterion_group!(
        benches,
        bench_mesh,
        bench_mesh_reused,
        bench_lod_mesh,
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
