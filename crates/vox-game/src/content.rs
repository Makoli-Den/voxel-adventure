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

use crate::selection::{Placeable, Selection};

/// Registry name of the grass block.
pub const GRASS_NAME: &str = "base:grass";
/// Registry name of the dirt block.
pub const DIRT_NAME: &str = "base:dirt";
/// Registry name of the stone block.
pub const STONE_NAME: &str = "base:stone";

/// Name of the block that is selected at startup, before the player picks another.
pub const PLACE_BLOCK_NAME: &str = STONE_NAME;

/// Why the content could not be set up.
#[derive(Debug)]
pub enum ContentError {
    /// The blocks file could not be loaded; the message names the file.
    Load(vox_content::ContentError),
    /// A block the game needs is not in the registry.
    MissingBlock(&'static str),
    /// There is no block the player could place, or the block selected at startup is not one.
    NoPlaceableBlocks,
}

impl fmt::Display for ContentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Load(error) => write!(f, "{error}"),
            Self::MissingBlock(name) => write!(
                f,
                "the block `{name}` does not exist in base:blocks.json;                  check the packs given with --pack"
            ),
            Self::NoPlaceableBlocks => write!(
                f,
                "no block can be placed: base:blocks.json has no block besides air, \
                 or lacks the block selected at startup"
            ),
        }
    }
}

impl Error for ContentError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Load(error) => Some(error),
            Self::MissingBlock(_) | Self::NoPlaceableBlocks => None,
        }
    }
}

impl From<vox_content::ContentError> for ContentError {
    fn from(error: vox_content::ContentError) -> Self {
        Self::Load(error)
    }
}

/// Every registered block except air, in id order, with its name: what the player may place.
fn placeable_blocks(registry: &BlockRegistry) -> Vec<Placeable> {
    // Ids are handed out in registration order, so `0..len` covers them all; `try_from` only
    // stops at the registry's own u16 limit, which it can not exceed.
    (0..registry.len())
        .filter_map(|id| u16::try_from(id).ok().map(BlockId))
        .filter(|block| !block.is_air())
        .filter_map(|id| {
            let name = registry.name(id)?.to_owned();
            Some(Placeable { id, name })
        })
        .collect()
}

/// The registry and the blocks the game uses by name.
#[derive(Debug)]
pub struct Content {
    /// Every registered block.
    pub registry: BlockRegistry,
    /// The blocks the terrain generator places.
    pub terrain: TerrainBlocks,
    /// The blocks the player may place (every registered block except air) and the selected
    /// one, which the place key puts down.
    pub selection: Selection,
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
        let start = find(PLACE_BLOCK_NAME)?;
        let selection = Selection::new(placeable_blocks(&registry), start)
            .ok_or(ContentError::NoPlaceableBlocks)?;
        Ok(Self {
            registry,
            terrain,
            selection,
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
    fn stone_is_selected_at_startup() {
        let content = content();
        assert_eq!(content.selection.selected(), content.terrain.stone);
        assert!(!content.selection.selected().is_air());
    }

    #[test]
    fn every_block_but_air_is_placeable() {
        let content = content();
        let mut selection = content.selection.clone();
        let mut seen = Vec::new();
        for _ in 0..content.registry.len() - 1 {
            seen.push(selection.selected());
            selection.cycle(1);
        }
        assert!(!seen.contains(&BlockId::AIR));
        seen.sort_by_key(|block| block.0);
        seen.dedup();
        assert_eq!(seen.len(), content.registry.len() - 1);
        // The wheel is back where it started.
        assert_eq!(selection.selected(), content.terrain.stone);
    }

    #[test]
    fn the_new_base_blocks_are_placeable_by_name() {
        let content = content();
        let mut selection = content.selection.clone();
        for name in ["base:sand", "base:cobblestone", "base:planks", "base:log"] {
            let id = content.registry.id(name).unwrap();
            let before = selection.clone();
            // Walk the wheel until the block is selected; a block that is not placeable would
            // never come up and the loop would end with the starting block.
            for _ in 0..content.registry.len() {
                if selection.selected() == id {
                    break;
                }
                selection.cycle(1);
            }
            assert_eq!(selection.selected(), id, "{name}");
            selection = before;
        }
    }

    #[test]
    fn names_map_back_from_the_ids() {
        let content = content();
        assert_eq!(content.registry.name(content.terrain.dirt), Some(DIRT_NAME));
    }

    #[test]
    fn the_selected_name_is_the_registry_name() {
        let content = content();
        assert_eq!(content.selection.selected_name(), STONE_NAME);
    }

    #[test]
    fn errors_describe_themselves() {
        assert!(!ContentError::NoPlaceableBlocks.to_string().is_empty());
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
