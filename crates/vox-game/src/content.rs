//! The game's blocks, read from `base:blocks.json` through the resource packs (step S4 of
//! decision 0013).
//!
//! The rest of the game never names a block id: it asks [`Content`] for the blocks the terrain
//! uses and for the block the player places, both resolved by name once at startup.
//!
//! The mesher draws every face with the texture layer the blocks file gives it
//! ([`Content::face_textures`]), so a pack may order the blocks file freely.

use std::error::Error;
use std::fmt;

use vox_content::{BlockTextures, Warning};
use vox_resources::PackStack;
use vox_world::{BlockId, BlockRegistry, FaceTextures, TerrainBlocks};

/// Registry name of the grass block.
pub const GRASS_NAME: &str = "base:grass";
/// Registry name of the dirt block.
pub const DIRT_NAME: &str = "base:dirt";
/// Registry name of the stone block.
pub const STONE_NAME: &str = "base:stone";

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
}

impl fmt::Display for ContentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Load(error) => write!(f, "{error}"),
            Self::MissingBlock(name) => write!(
                f,
                "the block `{name}` does not exist in base:blocks.json;                  check the packs given with --pack"
            ),
        }
    }
}

impl Error for ContentError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Load(error) => Some(error),
            Self::MissingBlock(_) => None,
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
    /// The pixels of every block texture, one layer per texture; layer 0 is the missing
    /// picture.
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

    /// The texture layer of each face of every block, as the blocks file assigns them: the
    /// table the mesher draws with.
    pub fn face_textures(&self) -> FaceTextures {
        self.registry.face_textures().clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::packs::built_in_pack;
    use vox_world::{Face, TextureLayer};

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
    fn a_pack_may_reorder_the_blocks() {
        let json = r#"{ "format": 1, "blocks": [
            { "name": "dirt", "textures": { "all": "base:d.png" } },
            { "name": "grass", "textures": { "all": "base:g.png" } },
            { "name": "stone", "textures": { "all": "base:s.png" } } ] }"#;
        let (stack, dir) = stack_with_blocks("reorder", json);
        let result = Content::new(&stack);
        let _ = std::fs::remove_dir_all(dir);
        let content = result.unwrap();
        assert_eq!(content.terrain.dirt, BlockId(1));
        assert_eq!(content.terrain.grass, BlockId(2));
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

    /// Pins the ids of the built-in blocks file: the terrain fingerprints rely on them.
    #[test]
    fn the_base_blocks_get_the_ids_the_terrain_fingerprints_expect() {
        let content = content();
        assert_eq!(content.terrain.grass, BlockId(1));
        assert_eq!(content.terrain.dirt, BlockId(2));
        assert_eq!(content.terrain.stone, BlockId(3));
    }

    /// The mesher gets the layers of the blocks file: grass has its own top, its own side and
    /// the dirt texture underneath.
    #[test]
    fn grass_faces_get_the_layers_of_the_blocks_file() {
        let content = content();
        let table = content.face_textures();
        let grass = content.terrain.grass;
        let dirt = table.get(content.terrain.dirt, Face::PosY);
        let top = table.get(grass, Face::PosY);
        let side = table.get(grass, Face::PosX);
        assert_eq!(table.get(grass, Face::NegY), dirt);
        assert!(top != side && top != dirt && side != dirt);
        assert!(![top, side, dirt].contains(&TextureLayer::MISSING));
    }

    /// Every layer the mesher can write exists in the texture array. A layer past the end
    /// would not show the missing texture: the GPU clamps the array layer, so it would sample
    /// the last texture instead and hide the mistake.
    #[test]
    fn every_face_layer_is_within_the_texture_array() {
        let content = content();
        let table = content.face_textures();
        for id in 0..content.registry.len() {
            let block = BlockId(u16::try_from(id).unwrap());
            for face in Face::ALL {
                let layer = table.get(block, face);
                assert!(
                    u32::from(layer.0) < content.textures.layer_count,
                    "{block:?} {face:?} has layer {layer:?}"
                );
            }
        }
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
