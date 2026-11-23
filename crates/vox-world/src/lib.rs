//! World data: blocks, chunks and the world that holds them.
//!
//! This crate knows nothing about the graphics API, mods or networking. Other layers
//! depend on it, but it depends on none of them (see `CLAUDE.md`, "Architecture").

mod block;
mod chunk;
mod chunk_pos;
mod mesh;
mod raycast;
mod registry;
mod terrain;
mod world;

pub use block::BlockId;
pub use chunk::{CHUNK_SIZE, CHUNK_VOLUME, Chunk, LocalPos};
pub use chunk_pos::ChunkPos;
pub use mesh::{
    ChunkMesh, ChunkVertex, Face, INDICES_PER_QUAD, VERTICES_PER_QUAD, build_mesh, build_mesh_into,
    build_mesh_with_neighbours_into,
};
pub use raycast::{RayHit, raycast};
pub use registry::{AIR_NAME, BlockRegistry, FaceTextures, RegistryError, TextureLayer};
pub use terrain::{MAX_SURFACE, MIN_SURFACE, TerrainBlocks, generate_chunk, generate_terrain};
pub use world::{ChangedChunks, MAX_CHANGED_CHUNKS, Neighbours, World};
