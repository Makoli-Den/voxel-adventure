//! World data: blocks and chunks.
//!
//! This crate knows nothing about the graphics API, mods or networking. Other layers
//! depend on it, but it depends on none of them (see `CLAUDE.md`, "Architecture").

mod block;
mod chunk;

pub use block::BlockId;
pub use chunk::{CHUNK_SIZE, CHUNK_VOLUME, Chunk, LocalPos};
