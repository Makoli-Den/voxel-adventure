//! The game's blocks. Temporary: registered in code until the blocks file is read from the
//! resource packs (step S4 of decision 0013).
//!
//! The rest of the game never names a block id: it asks [`Content`] for the blocks the terrain
//! uses and for the block the player places, both resolved by name once at startup.

use std::error::Error;
use std::fmt;

use vox_world::{BlockId, BlockRegistry, Face, RegistryError, TerrainBlocks, TextureLayer};

/// Registry name of the grass block.
pub const GRASS_NAME: &str = "base:grass";
/// Registry name of the dirt block.
pub const DIRT_NAME: &str = "base:dirt";
/// Registry name of the stone block.
pub const STONE_NAME: &str = "base:stone";

/// Name of the block the place key puts down. Temporary: the base game mod will provide an
/// inventory with a selected block.
pub const PLACE_BLOCK_NAME: &str = STONE_NAME;

/// The blocks of the base game in registration order, so they get the ids 1, 2 and 3, each
/// with one texture layer on every face. The layer equals the id, because the mesher still
/// writes it into the vertex and the palette in `assets/core/shaders/chunk.vert` is indexed by
/// it; the order here must match that palette until textures are loaded (S6a and S7 of
/// decision 0013).
const BASE_BLOCKS: [(&str, TextureLayer); 3] = [
    (GRASS_NAME, TextureLayer(1)),
    (DIRT_NAME, TextureLayer(2)),
    (STONE_NAME, TextureLayer(3)),
];

/// Why the content could not be set up.
#[derive(Debug)]
pub enum ContentError {
    /// A block could not be registered.
    Register(RegistryError),
    /// A block the game needs is not in the registry.
    MissingBlock(&'static str),
}

impl fmt::Display for ContentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Register(error) => write!(f, "cannot register a block: {error}"),
            Self::MissingBlock(name) => write!(f, "the block `{name}` does not exist"),
        }
    }
}

impl Error for ContentError {}

impl From<RegistryError> for ContentError {
    fn from(error: RegistryError) -> Self {
        Self::Register(error)
    }
}

/// The registry and the blocks the game uses by name.
#[derive(Debug)]
pub struct Content {
    /// Every registered block.
    pub registry: BlockRegistry,
    /// The blocks the terrain generator places.
    pub terrain: TerrainBlocks,
    /// The block the place key puts down.
    pub place_block: BlockId,
}

impl Content {
    /// Registers the base blocks and resolves the blocks the game needs.
    ///
    /// # Errors
    /// [`ContentError`] if a registration fails or a needed block is missing.
    pub fn new() -> Result<Self, ContentError> {
        let mut registry = BlockRegistry::new();
        for (name, layer) in BASE_BLOCKS {
            registry.register(name, [layer; Face::COUNT])?;
        }
        let find = |name: &'static str| registry.id(name).ok_or(ContentError::MissingBlock(name));
        let terrain = TerrainBlocks {
            grass: find(GRASS_NAME)?,
            dirt: find(DIRT_NAME)?,
            stone: find(STONE_NAME)?,
        };
        let place_block = find(PLACE_BLOCK_NAME)?;
        Ok(Self {
            registry,
            terrain,
            place_block,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pins the ids: the terrain fingerprints and the colour palette of `chunk.vert` rely on
    /// them, so a reordered registration must fail here.
    #[test]
    fn the_base_blocks_get_the_ids_the_terrain_fingerprints_expect() {
        let content = Content::new().unwrap();
        assert_eq!(content.terrain.grass, BlockId(1));
        assert_eq!(content.terrain.dirt, BlockId(2));
        assert_eq!(content.terrain.stone, BlockId(3));
    }

    #[test]
    fn the_place_block_is_stone() {
        let content = Content::new().unwrap();
        assert_eq!(content.place_block, content.terrain.stone);
        assert!(!content.place_block.is_air());
    }

    #[test]
    fn names_map_back_from_the_ids() {
        let content = Content::new().unwrap();
        assert_eq!(content.registry.name(content.terrain.dirt), Some(DIRT_NAME));
    }

    #[test]
    fn errors_describe_themselves() {
        assert!(
            ContentError::MissingBlock("base:x")
                .to_string()
                .contains("base:x")
        );
        assert!(
            ContentError::from(RegistryError::EmptyName)
                .to_string()
                .contains("empty")
        );
    }
}
