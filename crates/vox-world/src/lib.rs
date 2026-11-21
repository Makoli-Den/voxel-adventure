//! World data: blocks and chunks.
//!
//! This crate knows nothing about the graphics API, mods or networking. Other layers
//! depend on it, but it depends on none of them (see `CLAUDE.md`, "Architecture").

mod block;
mod chunk;
mod mesh;
mod terrain;

pub use block::BlockId;
pub use chunk::{CHUNK_SIZE, CHUNK_VOLUME, Chunk, LocalPos};
pub use mesh::{ChunkMesh, ChunkVertex, Face, build_mesh, build_mesh_into};
pub use terrain::{DIRT, GRASS, MAX_SURFACE, MIN_SURFACE, STONE, generate_terrain};
