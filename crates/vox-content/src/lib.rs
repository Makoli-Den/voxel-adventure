//! Content loaded from resource packs, between the files and the core types: the world core
//! stays free of parsers and the renderer stays free of files (decision 0013).
//!
//! Today this is the blocks file ([`load_blocks`]); block textures follow in step S5.

mod blocks;
mod error;

pub use blocks::{BLOCKS_FILE, BLOCKS_FORMAT, MISSING_TEXTURE, load_blocks, load_blocks_from};
pub use error::ContentError;
