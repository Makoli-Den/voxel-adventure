//! The game's blocks, read from `base:blocks.json` through the resource packs (step S4 of
//! decision 0013).
//!
//! The rest of the game never names a block id: it asks [`Content`] for the blocks the terrain
//! uses and for the block the player places, both resolved by name once at startup.
//!
//! The mesher writes a texture layer per face into the vertex (step S6a), but the real layers
//! from the file are used only from step S7: until then the palette in
//! `assets/core/shaders/chunk.vert` is indexed by that number, so the game meshes with
//! [`identity_face_textures`], where the layer equals the block id, and the ids of grass, dirt
//! and stone must stay 1, 2 and 3. A pack that reorders the blocks file would silently
//! mis-colour the world, so [`Content::new`] checks the ids ([`PALETTE_IDS`]) and stops with
//! [`ContentError::UnexpectedId`]. Remove the check and [`identity_face_textures`] together
//! with the palette in S7.

use std::error::Error;
use std::fmt;

use vox_content::{BlockTextures, Warning};
use vox_resources::PackStack;
use vox_world::{BlockId, BlockRegistry, Face, FaceTextures, TerrainBlocks, TextureLayer};

/// Registry name of the grass block.
pub const GRASS_NAME: &str = "base:grass";
/// Registry name of the dirt block.
pub const DIRT_NAME: &str = "base:dirt";
/// Registry name of the stone block.
pub const STONE_NAME: &str = "base:stone";

/// The ids the shader palette in `assets/core/shaders/chunk.vert` is indexed by, per block.
/// Temporary: removed with the palette in step S7 of decision 0013.
const PALETTE_IDS: [(&str, BlockId); 3] = [
    (GRASS_NAME, BlockId(1)),
    (DIRT_NAME, BlockId(2)),
    (STONE_NAME, BlockId(3)),
];

/// A face texture table in which every face of block `i` has layer `i`, for the `block_count`
/// blocks with ids `0..block_count`.
///
/// Temporary, until the shader samples real textures (step S7 of decision 0013): the palette
/// in `chunk.vert` is indexed by the number in the vertex, which used to be the block id.
///
/// `block_count` is a registry's `len()`: the registry stops at `u16::MAX + 1` blocks, so every
/// id below the count fits a layer (checked in debug builds).
pub fn identity_face_textures(block_count: usize) -> FaceTextures {
    debug_assert!(block_count <= usize::from(u16::MAX) + 1);
    let layers = (0..block_count)
        // The cast cannot truncate: see the assert above.
        .map(|id| [TextureLayer(id as u16); Face::COUNT])
        .collect();
    FaceTextures::from_layers(layers)
}

/// Name of the block the place key puts down. Temporary: the base game mod will provide an
/// inventory with a selected block.
pub const PLACE_BLOCK_NAME: &str = STONE_NAME;

/// Why the content could not be set up.
#[derive(Debug)]
pub enum ContentError {
    /// The blocks file could not be loaded; the message names the file.
    Load(vox_content::ContentError),
    /// A block the game needs is not in the registry.
    MissingBlock(&'static str),
    /// A block has an id the shader palette does not expect (temporary, until step S7).
    UnexpectedId {
        /// Registry name of the block.
        name: &'static str,
        /// The id the blocks file gave it.
        found: BlockId,
        /// The id the palette is indexed by.
        expected: BlockId,
    },
}

impl fmt::Display for ContentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Load(error) => write!(f, "{error}"),
            Self::MissingBlock(name) => write!(
                f,
                "the block `{name}` does not exist in base:blocks.json;                  check the packs given with --pack"
            ),
            Self::UnexpectedId {
                name,
                found,
                expected,
            } => write!(
                f,
                "the block `{name}` has id {} in base:blocks.json but id {} is required until                  textures are used (keep the order grass, dirt, stone first);                  check the packs given with --pack",
                found.0, expected.0
            ),
        }
    }
}

impl Error for ContentError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Load(error) => Some(error),
            Self::MissingBlock(_) | Self::UnexpectedId { .. } => None,
        }
    }
}

impl From<vox_content::ContentError> for ContentError {
    fn from(error: vox_content::ContentError) -> Self {
        Self::Load(error)
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
    /// The pixels of every block texture. Nothing draws them yet (steps S6 and S7).
    pub textures: BlockTextures,
    /// Textures that could not be used and were replaced by the missing picture.
    pub warnings: Vec<Warning>,
}

impl Content {
    /// Loads the blocks file from `stack` and resolves the blocks the game needs.
    ///
    /// # Errors
    /// [`ContentError`] if the blocks file is missing or broken, or a needed block is not in it.
    pub fn new(stack: &PackStack) -> Result<Self, ContentError> {
        let (registry, textures, warnings) = vox_content::load_content(stack)?;
        for (name, expected) in PALETTE_IDS {
            let found = registry.id(name).ok_or(ContentError::MissingBlock(name))?;
            if found != expected {
                return Err(ContentError::UnexpectedId {
                    name,
                    found,
                    expected,
                });
            }
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
            textures,
            warnings,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::packs::built_in_pack;

    /// A stack with the built-in pack and, above it, a pack whose blocks file is `json`.
    fn stack_with_blocks(label: &str, json: &str) -> (PackStack, std::path::PathBuf) {
        let dir =
            std::env::temp_dir().join(format!("vox-game-content-{label}-{}", std::process::id()));
        let file = dir.join("base").join("blocks.json");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, json).unwrap();
        let mut stack = PackStack::new();
        stack.push(built_in_pack());
        stack.push(vox_resources::DirPack::new(&dir));
        (stack, dir)
    }

    #[test]
    fn a_pack_that_reorders_the_blocks_is_rejected_naming_the_block() {
        let json = r#"{ "format": 1, "blocks": [
            { "name": "dirt", "textures": { "all": "base:d.png" } },
            { "name": "grass", "textures": { "all": "base:g.png" } },
            { "name": "stone", "textures": { "all": "base:s.png" } } ] }"#;
        let (stack, dir) = stack_with_blocks("reorder", json);
        let result = Content::new(&stack);
        let _ = std::fs::remove_dir_all(dir);
        let error = result.unwrap_err();
        assert!(
            matches!(
                error,
                ContentError::UnexpectedId {
                    name: GRASS_NAME,
                    found: BlockId(2),
                    expected: BlockId(1)
                }
            ),
            "{error}"
        );
        assert!(error.to_string().contains("--pack"));
    }

    #[test]
    fn a_pack_that_drops_a_needed_block_is_rejected() {
        let json = r#"{ "format": 1, "blocks": [
            { "name": "grass", "textures": { "all": "base:g.png" } } ] }"#;
        let (stack, dir) = stack_with_blocks("drop", json);
        let result = Content::new(&stack);
        let _ = std::fs::remove_dir_all(dir);
        assert!(matches!(result, Err(ContentError::MissingBlock(DIRT_NAME))));
    }

    fn content() -> Content {
        let mut stack = PackStack::new();
        stack.push(built_in_pack());
        Content::new(&stack).unwrap()
    }

    /// Pins the ids: the terrain fingerprints and the colour palette of `chunk.vert` rely on
    /// them, so a reordered registration must fail here.
    #[test]
    fn the_base_blocks_get_the_ids_the_terrain_fingerprints_expect() {
        let content = content();
        assert_eq!(content.terrain.grass, BlockId(1));
        assert_eq!(content.terrain.dirt, BlockId(2));
        assert_eq!(content.terrain.stone, BlockId(3));
    }

    #[test]
    fn the_identity_table_maps_every_face_of_every_block_to_its_id() {
        let content = content();
        let table = identity_face_textures(content.registry.len());
        for id in 0..content.registry.len() {
            let block = BlockId(u16::try_from(id).unwrap());
            for face in Face::ALL {
                assert_eq!(table.get(block, face), TextureLayer(block.0));
            }
        }
        // One past the last block is unknown.
        let past = BlockId(u16::try_from(content.registry.len()).unwrap());
        assert_eq!(table.get(past, Face::PosY), TextureLayer::MISSING);
    }

    #[test]
    fn the_place_block_is_stone() {
        let content = content();
        assert_eq!(content.place_block, content.terrain.stone);
        assert!(!content.place_block.is_air());
    }

    #[test]
    fn names_map_back_from_the_ids() {
        let content = content();
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
            Content::new(&PackStack::new())
                .unwrap_err()
                .to_string()
                .contains("base:blocks.json")
        );
    }
}
