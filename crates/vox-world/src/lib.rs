//! World data: blocks, chunks and the world that holds them.
//!
//! This crate knows nothing about the graphics API, mods or networking. Other layers
//! depend on it, but it depends on none of them (see `CLAUDE.md`, "Architecture").

mod block;
mod bounds;
mod chunk;
mod chunk_pos;
mod lod;
mod mesh;
mod raycast;
mod registry;
mod terrain;
mod world;

pub use block::BlockId;
pub use bounds::{
    HIGHEST_CHUNK_LAYER, LOWEST_CHUNK_LAYER, WORLD_CEILING_Y, WORLD_FLOOR_Y, contains_block_y,
    contains_chunk_layer,
};
pub use chunk::{CHUNK_SIZE, CHUNK_VOLUME, Chunk, LocalPos};
pub use chunk_pos::ChunkPos;
pub use lod::{
    MAX_LOD_LEVEL, TilePos, build_tile_mesh_into, cell_size, generate_lod_chunk, skirt_quads,
    solid_chunk,
};
pub use mesh::{
    ChunkMesh, ChunkVertex, Face, INDICES_PER_QUAD, VERTICES_PER_QUAD, build_mesh, build_mesh_into,
    build_mesh_with_neighbours_into,
};
pub use raycast::{RayHit, raycast, raycast_with};
pub use registry::{AIR_NAME, BlockRegistry, FaceTextures, RegistryError, TextureLayer};
pub use terrain::{
    MAX_SURFACE, MIN_SURFACE, TerrainBlocks, column_span, generate_chunk, generate_terrain,
};
pub use world::{ChangedChunks, MAX_CHANGED_CHUNKS, Neighbours, World, chunks_touching};
