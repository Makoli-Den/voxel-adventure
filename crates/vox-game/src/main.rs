//! Game entry point.
//!
//! This is where the layers are wired together. For now it is a stub: the window,
//! renderer and mod loader will arrive as separate crates when they are needed.

use vox_world::{BlockId, CHUNK_VOLUME, Chunk, LocalPos};

fn main() {
    let mut chunk = Chunk::new();
    if let Some(pos) = LocalPos::new(0, 0, 0) {
        chunk.set(pos, BlockId(1));
        println!(
            "vox-game: created a chunk of {CHUNK_VOLUME} blocks, block at the origin: {:?}",
            chunk.get(pos)
        );
    }
}
