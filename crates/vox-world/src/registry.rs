//! The block registry: the map between block names and the numbers chunks store.
//!
//! Chunks hold only [`BlockId`] numbers; names such as `base:stone` live here, so the
//! content (the game's blocks, later mods) can be described by data and a world does not depend
//! on how the code names things (architecture rule 5, decision 0013). Ids are handed out in
//! registration order, so the same list of blocks always gives the same ids.
//!
//! The registry also keeps, per block, the texture layer of each face ([`FaceTextures`]), which
//! is all the mesher needs to know about it.

use std::collections::HashMap;
use std::error::Error;
use std::fmt;

use crate::block::BlockId;
use crate::mesh::Face;

/// Index of one texture in the block texture array of the renderer.
///
/// A `u16` like [`BlockId`], because the mesher writes it into the two bytes of a vertex that
/// used to hold the block id. `#[repr(transparent)]` keeps the layout of a plain `u16`.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct TextureLayer(pub u16);

impl TextureLayer {
    /// The "missing texture" layer. Zero, so anything that has no texture (an unknown block,
    /// a texture that failed to load) falls back to it and is easy to spot.
    pub const MISSING: Self = Self(0);
}

/// The name of the block that always has id [`BlockId::AIR`].
pub const AIR_NAME: &str = "core:air";

/// The texture layer of each face of every block, indexed by block id.
///
/// A flat table, so the mesher does one array read per quad. A block it does not know gets
/// [`TextureLayer::MISSING`] on every face.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FaceTextures {
    layers: Vec<[TextureLayer; Face::COUNT]>,
}

impl FaceTextures {
    /// A table from the layers of each block, indexed by block id: entry `i` holds the six
    /// layers of block `i`, in the order of [`Face::ALL`].
    ///
    /// The registry builds its own table as blocks are registered; this is for a caller that
    /// wants another mapping, for example one where the layer equals the block id. The caller
    /// owns the mapping and nothing is checked: by convention entry 0 belongs to air (which
    /// is never drawn), a block id past the end of the table reads as
    /// [`TextureLayer::MISSING`], and an empty `layers` makes every block read as missing.
    ///
    /// # Example
    ///
    /// ```
    /// use vox_world::{BlockId, Face, FaceTextures, TextureLayer};
    ///
    /// let table = FaceTextures::from_layers(vec![[TextureLayer(7); Face::COUNT]]);
    /// assert_eq!(table.get(BlockId(0), Face::NegZ), TextureLayer(7));
    /// ```
    pub fn from_layers(layers: Vec<[TextureLayer; Face::COUNT]>) -> Self {
        Self { layers }
    }

    /// The layer of `face` of `block`, or [`TextureLayer::MISSING`] for an unknown block.
    ///
    /// # Example
    ///
    /// ```
    /// use vox_world::{BlockId, BlockRegistry, Face, TextureLayer};
    ///
    /// let mut registry = BlockRegistry::new();
    /// let layer = TextureLayer(4);
    /// let id = registry.register("base:stone", [layer; Face::COUNT]).unwrap();
    /// assert_eq!(registry.face_textures().get(id, Face::PosY), layer);
    /// assert_eq!(registry.face_textures().get(BlockId(99), Face::PosY), TextureLayer::MISSING);
    /// ```
    pub fn get(&self, block: BlockId, face: Face) -> TextureLayer {
        self.layers
            .get(usize::from(block.0))
            .map_or(TextureLayer::MISSING, |faces| {
                faces[usize::from(face.index())]
            })
    }
}

/// Why a block could not be registered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RegistryError {
    /// The name is empty.
    EmptyName,
    /// A block with this name is already registered.
    DuplicateName(String),
    /// Every id of [`BlockId`] is taken.
    TooManyBlocks,
}

impl fmt::Display for RegistryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyName => write!(f, "a block name must not be empty"),
            Self::DuplicateName(name) => write!(f, "the block `{name}` is registered twice"),
            Self::TooManyBlocks => write!(f, "no free block id is left"),
        }
    }
}

impl Error for RegistryError {}

/// All known blocks: names, ids and face textures.
///
/// A new registry holds only air, at [`BlockId::AIR`]. Every [`register`](Self::register)
/// call hands out the next id.
#[derive(Clone, Debug)]
pub struct BlockRegistry {
    /// Name of each block, indexed by id.
    names: Vec<String>,
    ids: HashMap<String, BlockId>,
    textures: FaceTextures,
}

impl Default for BlockRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl BlockRegistry {
    /// A registry that holds only `core:air` with id 0.
    pub fn new() -> Self {
        Self {
            names: vec![AIR_NAME.to_owned()],
            ids: HashMap::from([(AIR_NAME.to_owned(), BlockId::AIR)]),
            textures: FaceTextures {
                layers: vec![[TextureLayer::MISSING; Face::COUNT]],
            },
        }
    }

    /// Registers a block and returns its id, the next free one.
    ///
    /// `textures` holds the layer of each face in the order of [`Face::ALL`].
    ///
    /// # Errors
    /// [`RegistryError::EmptyName`], [`RegistryError::DuplicateName`] (also for
    /// [`AIR_NAME`]) and [`RegistryError::TooManyBlocks`] when the ids of `u16` are used up.
    /// A failed call changes nothing.
    pub fn register(
        &mut self,
        name: &str,
        textures: [TextureLayer; Face::COUNT],
    ) -> Result<BlockId, RegistryError> {
        if name.is_empty() {
            return Err(RegistryError::EmptyName);
        }
        if self.ids.contains_key(name) {
            return Err(RegistryError::DuplicateName(name.to_owned()));
        }
        let id = u16::try_from(self.names.len())
            .map(BlockId)
            .map_err(|_| RegistryError::TooManyBlocks)?;
        self.names.push(name.to_owned());
        self.ids.insert(name.to_owned(), id);
        self.textures.layers.push(textures);
        Ok(id)
    }

    /// The id of the block called `name`.
    pub fn id(&self, name: &str) -> Option<BlockId> {
        self.ids.get(name).copied()
    }

    /// The name of the block with `id`.
    pub fn name(&self, id: BlockId) -> Option<&str> {
        self.names.get(usize::from(id.0)).map(String::as_str)
    }

    /// Number of blocks, air included.
    pub fn len(&self) -> usize {
        self.names.len()
    }

    /// Always `false`: air is always registered. Here for the `len` convention.
    pub fn is_empty(&self) -> bool {
        false
    }

    /// The face textures of all blocks, the part of the registry the mesher reads.
    pub fn face_textures(&self) -> &FaceTextures {
        &self.textures
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STONE_LAYER: TextureLayer = TextureLayer(5);

    fn solid(layer: TextureLayer) -> [TextureLayer; Face::COUNT] {
        [layer; Face::COUNT]
    }

    #[test]
    fn a_new_registry_holds_only_air_at_id_zero() {
        let registry = BlockRegistry::new();
        assert_eq!(registry.len(), 1);
        assert_eq!(registry.id(AIR_NAME), Some(BlockId::AIR));
        assert_eq!(registry.name(BlockId::AIR), Some(AIR_NAME));
    }

    #[test]
    fn ids_follow_registration_order() {
        let mut registry = BlockRegistry::new();
        let layer = TextureLayer(1);
        assert_eq!(
            registry.register("base:grass", solid(layer)),
            Ok(BlockId(1))
        );
        assert_eq!(registry.register("base:dirt", solid(layer)), Ok(BlockId(2)));
        assert_eq!(
            registry.register("base:stone", solid(layer)),
            Ok(BlockId(3))
        );
        assert_eq!(registry.len(), 4);
    }

    #[test]
    fn lookups_work_both_ways() {
        let mut registry = BlockRegistry::new();
        let dirt = registry.register("base:dirt", solid(TextureLayer(1)));
        assert_eq!(registry.id("base:dirt"), dirt.ok());
        assert_eq!(registry.name(BlockId(1)), Some("base:dirt"));
        assert_eq!(registry.id("base:nothing"), None);
        assert_eq!(registry.name(BlockId(2)), None);
    }

    #[test]
    fn an_empty_name_is_rejected() {
        let mut registry = BlockRegistry::new();
        assert_eq!(
            registry.register("", solid(TextureLayer(1))),
            Err(RegistryError::EmptyName)
        );
        assert_eq!(registry.len(), 1);
    }

    #[test]
    fn a_duplicate_name_is_rejected_and_changes_nothing() {
        let mut registry = BlockRegistry::new();
        let first = registry.register("base:dirt", solid(TextureLayer(1)));
        assert_eq!(
            registry.register("base:dirt", solid(TextureLayer(2))),
            Err(RegistryError::DuplicateName("base:dirt".to_owned()))
        );
        assert_eq!(registry.len(), 2);
        assert_eq!(registry.id("base:dirt"), first.ok());
        assert_eq!(
            registry.face_textures().get(BlockId(1), Face::PosX),
            TextureLayer(1)
        );
    }

    #[test]
    fn air_cannot_be_registered_again() {
        let mut registry = BlockRegistry::new();
        assert_eq!(
            registry.register(AIR_NAME, solid(TextureLayer(1))),
            Err(RegistryError::DuplicateName(AIR_NAME.to_owned()))
        );
    }

    #[test]
    fn running_out_of_ids_is_an_error() {
        let mut registry = BlockRegistry::new();
        // Air took id 0, so ids 1 ..= u16::MAX are free.
        for number in 1..=u16::MAX {
            let id = registry.register(&number.to_string(), solid(TextureLayer::MISSING));
            assert_eq!(id, Ok(BlockId(number)));
        }
        assert_eq!(
            registry.register("one-too-many", solid(TextureLayer::MISSING)),
            Err(RegistryError::TooManyBlocks)
        );
        assert_eq!(registry.len(), usize::from(u16::MAX) + 1);
        assert_eq!(registry.id("one-too-many"), None);
    }

    #[test]
    fn face_textures_are_kept_per_face() {
        let mut registry = BlockRegistry::new();
        let layers = [1, 2, 3, 4, 5, 6].map(TextureLayer);
        let id = registry.register("base:odd", layers).unwrap();
        for face in Face::ALL {
            assert_eq!(
                registry.face_textures().get(id, face),
                layers[usize::from(face.index())]
            );
        }
    }

    #[test]
    fn unknown_blocks_and_air_get_the_missing_layer() {
        let mut registry = BlockRegistry::new();
        registry.register("base:stone", solid(STONE_LAYER)).unwrap();
        let textures = registry.face_textures();
        for face in Face::ALL {
            assert_eq!(textures.get(BlockId::AIR, face), TextureLayer::MISSING);
            assert_eq!(textures.get(BlockId(2), face), TextureLayer::MISSING);
            assert_eq!(textures.get(BlockId(u16::MAX), face), TextureLayer::MISSING);
            assert_eq!(textures.get(BlockId(1), face), STONE_LAYER);
        }
    }

    #[test]
    fn an_empty_table_returns_the_missing_layer() {
        let textures = FaceTextures::default();
        assert_eq!(textures.get(BlockId(1), Face::NegY), TextureLayer::MISSING);
    }

    #[test]
    fn errors_describe_themselves() {
        assert!(RegistryError::EmptyName.to_string().contains("empty"));
        assert!(
            RegistryError::DuplicateName("a:b".to_owned())
                .to_string()
                .contains("a:b")
        );
        assert!(RegistryError::TooManyBlocks.to_string().contains("id"));
    }
}
