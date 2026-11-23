//! The blocks file: block definitions as data (architecture rule 3, decision 0013).
//!
//! [`load_blocks`] reads `base:blocks.json` through the pack stack, resolves the texture keys
//! of every block to one texture per face and fills a [`BlockRegistry`]. Ids follow the order
//! of the file, so the same file always gives the same ids.

use std::collections::HashMap;

use serde::Deserialize;
use vox_resources::{NAMESPACE_SEPARATOR, PATH_SEPARATOR, PackStack, ResourceId};
use vox_world::{BlockRegistry, Face, TextureLayer};

use crate::error::ContentError;

/// Resource id of the blocks file. A pack that has this file replaces the whole list.
pub const BLOCKS_FILE: &str = "base:blocks.json";

/// The only `format` number of the blocks file this game reads. Raised when the file layout
/// changes in a way an old reader could not understand.
pub const BLOCKS_FORMAT: u32 = 1;

/// The name of the texture of layer 0, the "missing texture". The renderer generates that
/// picture in code (step S5), so no pack has a file by this id; it only fills index 0 of the
/// texture list so that an index in the list is a [`TextureLayer`].
pub const MISSING_TEXTURE: &str = "core:textures/missing.png";

/// Only the version number of a blocks file. Read first and without `deny_unknown_fields`, so
/// a file of a future format is reported as "unsupported format" and not as an unknown key.
#[derive(Deserialize)]
struct Header {
    format: u32,
}

/// The whole blocks file.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BlocksFile {
    /// Checked again after the full parse, see [`check_format`].
    format: u32,
    blocks: Vec<BlockDef>,
}

/// One block of the file.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BlockDef {
    /// Local name; the registry name is `<namespace of the file>:<name>`.
    name: String,
    textures: TextureKeys,
}

/// The texture keys of a block, as written. `side`, `top` and `bottom` override `all`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TextureKeys {
    all: Option<String>,
    side: Option<String>,
    top: Option<String>,
    bottom: Option<String>,
}

/// The texture keys of a block with their ids parsed.
struct ParsedKeys {
    all: Option<ResourceId>,
    side: Option<ResourceId>,
    top: Option<ResourceId>,
    bottom: Option<ResourceId>,
}

impl ParsedKeys {
    /// The texture `face` gets: `top` for [`Face::PosY`], `bottom` for [`Face::NegY`], `side`
    /// for the four horizontal faces, each falling back to `all`.
    fn for_face(&self, face: Face) -> Option<&ResourceId> {
        let specific = match face {
            Face::PosY => &self.top,
            Face::NegY => &self.bottom,
            Face::PosX | Face::NegX | Face::PosZ | Face::NegZ => &self.side,
        };
        specific.as_ref().or(self.all.as_ref())
    }
}

/// Hands out texture layers: layer 0 is the missing texture, then one layer per distinct
/// texture id in order of first use.
struct Layers {
    /// The texture id of each layer; the index is the layer.
    textures: Vec<ResourceId>,
    by_id: HashMap<ResourceId, TextureLayer>,
}

impl Layers {
    fn new() -> Result<Self, ContentError> {
        Ok(Self {
            textures: vec![ResourceId::parse(MISSING_TEXTURE)?],
            by_id: HashMap::new(),
        })
    }

    /// The layer of `id`, a new one if the texture is used for the first time. `None` when
    /// the layer numbers are used up.
    fn layer_of(&mut self, id: &ResourceId) -> Option<TextureLayer> {
        if let Some(layer) = self.by_id.get(id) {
            return Some(*layer);
        }
        let layer = TextureLayer(u16::try_from(self.textures.len()).ok()?);
        self.textures.push(id.clone());
        self.by_id.insert(id.clone(), layer);
        Some(layer)
    }
}

/// Rejects a `format` other than [`BLOCKS_FORMAT`].
fn check_format(file: &ResourceId, found: u32) -> Result<(), ContentError> {
    if found == BLOCKS_FORMAT {
        return Ok(());
    }
    Err(ContentError::UnsupportedFormat {
        file: file.clone(),
        found,
        supported: BLOCKS_FORMAT,
    })
}

/// Checks a local block name: the registry name `<namespace>:<name>` must be a valid resource
/// id (so lowercase `[a-z0-9_.-]`, no empty or reserved name, no trailing dot) and the name
/// itself must not contain `/`, which would make it a path.
fn check_name(file: &ResourceId, name: &str) -> Result<(), ContentError> {
    let valid = !name.contains(PATH_SEPARATOR)
        && ResourceId::parse(&format!("{}{NAMESPACE_SEPARATOR}{name}", file.namespace())).is_ok();
    if valid {
        return Ok(());
    }
    Err(ContentError::InvalidName {
        file: file.clone(),
        block: name.to_owned(),
    })
}

fn parse_key(
    file: &ResourceId,
    block: &str,
    key: &'static str,
    text: Option<&str>,
) -> Result<Option<ResourceId>, ContentError> {
    text.map(ResourceId::parse)
        .transpose()
        .map_err(|source| ContentError::InvalidTexture {
            file: file.clone(),
            block: block.to_owned(),
            key,
            source: Box::new(source),
        })
}

fn parse_keys(
    file: &ResourceId,
    block: &str,
    keys: &TextureKeys,
) -> Result<ParsedKeys, ContentError> {
    Ok(ParsedKeys {
        all: parse_key(file, block, "all", keys.all.as_deref())?,
        side: parse_key(file, block, "side", keys.side.as_deref())?,
        top: parse_key(file, block, "top", keys.top.as_deref())?,
        bottom: parse_key(file, block, "bottom", keys.bottom.as_deref())?,
    })
}

/// Reads [`BLOCKS_FILE`] from the highest pack that has it and registers its blocks.
///
/// Returns the registry and the texture id of each layer: index 0 is [`MISSING_TEXTURE`],
/// the rest follow in order of first use (blocks in file order, faces in [`Face::ALL`]
/// order), and a texture shared by several faces or blocks has one layer. The layers are
/// also stored in the registry's face textures.
///
/// # Errors
/// [`ContentError`] if the file is missing or unreadable, is not valid JSON of the expected
/// shape (unknown keys included), has a block name that is not a valid name, declares a `format` other than [`BLOCKS_FORMAT`], has a
/// texture id that is not a valid resource id, leaves a face without a texture, or repeats a
/// block name.
pub fn load_blocks(stack: &PackStack) -> Result<(BlockRegistry, Vec<ResourceId>), ContentError> {
    load_blocks_from(stack, &ResourceId::parse(BLOCKS_FILE)?)
}

/// Like [`load_blocks`] for the blocks file `file`. The namespace of `file` is the namespace
/// of the registered blocks, so a mod's file `mymod:blocks.json` registers `mymod:<name>`.
///
/// # Errors
/// As [`load_blocks`].
pub fn load_blocks_from(
    stack: &PackStack,
    file: &ResourceId,
) -> Result<(BlockRegistry, Vec<ResourceId>), ContentError> {
    let file = file.clone();
    let text = stack.read_to_string(&file)?;
    let parse_error = |source| ContentError::Parse {
        file: file.clone(),
        source,
    };
    let header: Header = serde_json::from_str(&text).map_err(parse_error)?;
    check_format(&file, header.format)?;
    let parsed: BlocksFile = serde_json::from_str(&text).map_err(parse_error)?;
    check_format(&file, parsed.format)?;

    let mut registry = BlockRegistry::new();
    let mut layers = Layers::new()?;
    for block in &parsed.blocks {
        check_name(&file, &block.name)?;
        let keys = parse_keys(&file, &block.name, &block.textures)?;
        let mut faces = [TextureLayer::MISSING; Face::COUNT];
        for face in Face::ALL {
            let missing_face = || ContentError::MissingFace {
                file: file.clone(),
                block: block.name.clone(),
                face,
            };
            let texture = keys.for_face(face).ok_or_else(missing_face)?;
            faces[usize::from(face.index())] = layers
                .layer_of(texture)
                .ok_or_else(|| ContentError::TooManyTextures { file: file.clone() })?;
        }
        let name = format!("{}:{}", file.namespace(), block.name);
        registry
            .register(&name, faces)
            .map_err(|source| ContentError::Register {
                file: file.clone(),
                block: block.name.clone(),
                source,
            })?;
    }
    Ok((registry, layers.textures))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};

    use vox_resources::DirPack;
    use vox_world::BlockId;

    use super::*;

    /// Makes the directory names of concurrently running tests different.
    static NEXT_DIR: AtomicU32 = AtomicU32::new(0);

    /// A pack directory in the system temp folder, removed on drop.
    struct TempPack(PathBuf);

    impl TempPack {
        /// A pack whose `base/blocks.json` holds `json`.
        fn with_blocks(json: &str) -> Self {
            Self::with_file("base/blocks.json", json)
        }

        /// A pack whose file `relative` (under the pack root) holds `contents`.
        fn with_file(relative: &str, contents: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "vox-content-test-{}-{}",
                std::process::id(),
                NEXT_DIR.fetch_add(1, Ordering::Relaxed)
            ));
            let path = dir.join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, contents).unwrap();
            Self(dir)
        }

        fn pack(&self) -> DirPack {
            DirPack::new(&self.0)
        }
    }

    impl Drop for TempPack {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn stack_of(packs: &[&TempPack]) -> PackStack {
        let mut stack = PackStack::new();
        for pack in packs {
            stack.push(pack.pack());
        }
        stack
    }

    fn load(json: &str) -> Result<(BlockRegistry, Vec<ResourceId>), ContentError> {
        let pack = TempPack::with_blocks(json);
        load_blocks(&stack_of(&[&pack]))
    }

    fn block(name: &str, textures: &str) -> String {
        format!(r#"{{ "name": "{name}", "textures": {{ {textures} }} }}"#)
    }

    fn file_of(blocks: &[String]) -> String {
        format!(r#"{{ "format": 1, "blocks": [ {} ] }}"#, blocks.join(","))
    }

    fn id(text: &str) -> ResourceId {
        ResourceId::parse(text).unwrap()
    }

    fn layers_of(registry: &BlockRegistry, name: &str) -> [TextureLayer; Face::COUNT] {
        let block = registry.id(name).unwrap();
        Face::ALL.map(|face| registry.face_textures().get(block, face))
    }

    #[test]
    fn all_gives_every_face_the_same_texture() {
        let (registry, textures) =
            load(&file_of(&[block("dirt", r#""all": "base:d.png""#)])).unwrap();
        assert_eq!(layers_of(&registry, "base:dirt"), [TextureLayer(1); 6]);
        assert_eq!(textures, [id(MISSING_TEXTURE), id("base:d.png")]);
    }

    #[test]
    fn top_bottom_and_side_go_to_the_right_faces() {
        let json = file_of(&[block(
            "grass",
            r#""top": "base:t.png", "bottom": "base:b.png", "side": "base:s.png""#,
        )]);
        let (registry, textures) = load(&json).unwrap();
        let layers = layers_of(&registry, "base:grass");
        // Layers follow first use in the order of Face::ALL, so the expected numbers are
        // derived by walking it: the texture each face must get, numbered from 1 as met.
        let mut met: Vec<&str> = Vec::new();
        for face in Face::ALL {
            let texture = match face {
                Face::PosY => "base:t.png",
                Face::NegY => "base:b.png",
                _ => "base:s.png",
            };
            if !met.contains(&texture) {
                met.push(texture);
            }
            let expected = 1 + met.iter().position(|met| *met == texture).unwrap();
            assert_eq!(
                layers[usize::from(face.index())],
                TextureLayer(u16::try_from(expected).unwrap()),
                "{face:?}"
            );
        }
        let expected_ids: Vec<ResourceId> = met.iter().map(|text| id(text)).collect();
        assert_eq!(textures[1..], expected_ids[..]);
    }

    #[test]
    fn specific_keys_override_all() {
        let json = file_of(&[block("log", r#""all": "base:a.png", "top": "base:t.png""#)]);
        let (registry, _) = load(&json).unwrap();
        let layers = layers_of(&registry, "base:log");
        // PosX comes first in Face::ALL and uses `all`, so `a.png` is layer 1; PosY is next
        // among the faces that differ and uses `top`, so `t.png` is layer 2.
        assert_eq!(layers[usize::from(Face::PosY.index())], TextureLayer(2));
        assert_eq!(layers[usize::from(Face::NegY.index())], TextureLayer(1));
        assert_eq!(layers[usize::from(Face::PosX.index())], TextureLayer(1));
    }

    #[test]
    fn side_alone_leaves_top_and_bottom_without_a_texture() {
        let json = file_of(&[block("pillar", r#""side": "base:s.png""#)]);
        let error = load(&json).unwrap_err();
        assert!(
            matches!(&error, ContentError::MissingFace { block, face, .. }
                if block == "pillar" && *face == Face::PosY),
            "{error}"
        );
        assert!(error.to_string().contains("pillar"));
        assert!(error.to_string().contains("base:blocks.json"));
    }

    #[test]
    fn a_block_without_textures_is_a_missing_face() {
        let json = file_of(&[block("ghost", "")]);
        assert!(matches!(load(&json), Err(ContentError::MissingFace { .. })));
    }

    #[test]
    fn shared_textures_get_one_layer_and_layers_follow_first_use() {
        let json = file_of(&[
            block(
                "grass",
                r#""top": "base:g.png", "bottom": "base:d.png", "side": "base:gs.png""#,
            ),
            block("dirt", r#""all": "base:d.png""#),
            block("stone", r#""all": "base:s.png""#),
        ]);
        let (registry, textures) = load(&json).unwrap();
        assert_eq!(
            textures,
            [
                id(MISSING_TEXTURE),
                id("base:gs.png"),
                id("base:g.png"),
                id("base:d.png"),
                id("base:s.png"),
            ]
        );
        assert_eq!(layers_of(&registry, "base:dirt"), [TextureLayer(3); 6]);
        assert_eq!(layers_of(&registry, "base:stone"), [TextureLayer(4); 6]);
    }

    #[test]
    fn ids_follow_the_order_of_the_file_after_air() {
        let json = file_of(&[
            block("a", r#""all": "base:x.png""#),
            block("b", r#""all": "base:x.png""#),
        ]);
        let (registry, _) = load(&json).unwrap();
        assert_eq!(registry.id("base:a"), Some(BlockId(1)));
        assert_eq!(registry.id("base:b"), Some(BlockId(2)));
        assert_eq!(registry.len(), 3);
    }

    #[test]
    fn the_registry_name_uses_the_namespace_of_the_file() {
        let json = file_of(&[block("a", r#""all": "other:x.png""#)]);
        let pack = TempPack::with_file("mymod/blocks.json", &json);
        let file = id("mymod:blocks.json");
        let (registry, _) = load_blocks_from(&stack_of(&[&pack]), &file).unwrap();
        assert!(registry.id("mymod:a").is_some());
        assert!(registry.id("base:a").is_none());
    }

    #[test]
    fn a_block_name_must_be_a_plain_lowercase_name() {
        for bad in ["", "Stone", "a:b", "a/b", "a b", "a.", "nul", "ü", ".."] {
            let json = file_of(&[
                block("ok", r#""all": "base:x.png""#),
                block(bad, r#""all": "base:x.png""#),
            ]);
            let error = load(&json).unwrap_err();
            assert!(
                matches!(&error, ContentError::InvalidName { block, .. } if block == bad),
                "`{bad}`: {error}"
            );
        }
    }

    #[test]
    fn plain_names_with_digits_underscores_dots_and_dashes_are_accepted() {
        let json = file_of(&[block("oak_log-2.b", r#""all": "base:x.png""#)]);
        assert!(load(&json).is_ok());
    }

    #[test]
    fn an_unknown_key_is_rejected_at_every_level() {
        let top = r#"{ "format": 1, "blocks": [], "extra": 1 }"#;
        let block_level = file_of(&[
            r#"{ "name": "a", "textures": { "all": "base:x.png" }, "hardness": 3 }"#.to_owned(),
        ]);
        let texture_level = file_of(&[block("a", r#""all": "base:x.png", "left": "base:y.png""#)]);
        for json in [top.to_owned(), block_level, texture_level] {
            let error = load(&json).unwrap_err();
            assert!(matches!(error, ContentError::Parse { .. }), "{json}");
            assert!(error.to_string().contains("unknown field"), "{error}");
        }
    }

    #[test]
    fn an_unknown_format_is_reported_before_unknown_keys() {
        let error = load(r#"{ "format": 2, "blocks": [], "new_thing": true }"#).unwrap_err();
        assert!(
            matches!(
                error,
                ContentError::UnsupportedFormat {
                    found: 2,
                    supported: BLOCKS_FORMAT,
                    ..
                }
            ),
            "{error}"
        );
    }

    #[test]
    fn a_missing_format_or_broken_json_is_a_parse_error_with_a_position() {
        for json in [r#"{ "blocks": [] }"#, "{ not json"] {
            let error = load(json).unwrap_err();
            assert!(matches!(error, ContentError::Parse { .. }));
            assert!(error.to_string().contains("line"), "{error}");
        }
    }

    #[test]
    fn a_duplicate_name_is_rejected_naming_the_block() {
        let json = file_of(&[
            block("a", r#""all": "base:x.png""#),
            block("a", r#""all": "base:y.png""#),
        ]);
        let error = load(&json).unwrap_err();
        assert!(
            matches!(&error, ContentError::Register { block, .. } if block == "a"),
            "{error}"
        );
    }

    #[test]
    fn an_invalid_texture_id_names_the_block_and_the_key() {
        for bad in ["stone.png", "base:../x.png", "Base:x.png", ""] {
            let json = file_of(&[block(
                "a",
                &format!(r#""all": "base:ok.png", "top": "{bad}""#),
            )]);
            let error = load(&json).unwrap_err();
            assert!(
                matches!(&error, ContentError::InvalidTexture { block, key: "top", .. }
                    if block == "a"),
                "`{bad}`: {error}"
            );
        }
    }

    #[test]
    fn an_invalid_id_is_rejected_even_when_another_key_overrides_it() {
        let json = file_of(&[block(
            "a",
            r#""all": "nope", "top": "base:t.png", "bottom": "base:t.png", "side": "base:t.png""#,
        )]);
        assert!(matches!(
            load(&json),
            Err(ContentError::InvalidTexture { key: "all", .. })
        ));
    }

    #[test]
    fn a_missing_blocks_file_names_the_id() {
        let error = load_blocks(&PackStack::new()).unwrap_err();
        assert!(matches!(error, ContentError::Read(_)));
        assert!(error.to_string().contains(BLOCKS_FILE), "{error}");
    }

    #[test]
    fn a_higher_pack_replaces_the_whole_file() {
        let low = TempPack::with_blocks(&file_of(&[block("a", r#""all": "base:x.png""#)]));
        let high = TempPack::with_blocks(&file_of(&[block("b", r#""all": "base:y.png""#)]));
        let (registry, _) = load_blocks(&stack_of(&[&low, &high])).unwrap();
        assert!(registry.id("base:b").is_some());
        assert!(registry.id("base:a").is_none());
    }

    #[test]
    fn an_empty_block_list_gives_only_air_and_the_missing_texture() {
        let (registry, textures) = load(&file_of(&[])).unwrap();
        assert_eq!(registry.len(), 1);
        assert_eq!(textures, [id(MISSING_TEXTURE)]);
    }

    #[test]
    fn the_missing_texture_id_is_a_valid_resource_id() {
        assert!(ResourceId::parse(MISSING_TEXTURE).is_ok());
    }
}
