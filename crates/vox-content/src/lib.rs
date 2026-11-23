//! Content loaded from resource packs, between the files and the core types: the world core
//! stays free of parsers and the renderer stays free of files (decision 0013).
//!
//! The blocks file ([`load_blocks`]) and the block textures ([`build_textures`]) come together in
//! [`load_content`].

mod blocks;
mod error;
mod textures;

pub use blocks::{BLOCKS_FILE, BLOCKS_FORMAT, MISSING_TEXTURE, load_blocks, load_blocks_from};
pub use error::ContentError;
pub use textures::{
    BYTES_PER_PIXEL, BlockTextures, DEFAULT_TILE_SIZE, MAX_LAYERS, MAX_TEXTURE_FILE_BYTES,
    MAX_TILE_SIZE, TextureProblem, Warning, build_textures, load_content,
};
