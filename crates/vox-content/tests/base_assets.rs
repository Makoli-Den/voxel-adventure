//! The real `mods/base/assets/base/blocks.json` loads and keeps the ids the terrain
//! fingerprints depend on.

#![allow(clippy::unwrap_used)]

use std::path::Path;

use vox_content::load_blocks;
use vox_resources::{DirPack, PackStack};
use vox_world::{BlockId, Face, TextureLayer};

fn built_in() -> PackStack {
    let mut stack = PackStack::new();
    stack.push(DirPack::new(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../mods/base/assets"),
    ));
    stack
}

#[test]
fn grass_dirt_and_stone_keep_ids_one_two_three() {
    let (registry, _) = load_blocks(&built_in()).unwrap();
    assert_eq!(registry.id("base:grass"), Some(BlockId(1)));
    assert_eq!(registry.id("base:dirt"), Some(BlockId(2)));
    assert_eq!(registry.id("base:stone"), Some(BlockId(3)));
    // Air plus the three terrain blocks plus the blocks only the player places plus bedrock.
    assert_eq!(registry.len(), 1 + 3 + PLAYER_BLOCKS.len() + 1);
    for name in PLAYER_BLOCKS {
        assert!(registry.id(name).is_some(), "{name}");
    }
}

/// The blocks after grass, dirt and stone, in file order: terrain never places them.
const PLAYER_BLOCKS: [&str; 4] = ["base:sand", "base:cobblestone", "base:planks", "base:log"];

#[test]
fn bedrock_comes_last_with_its_own_texture() {
    let (registry, _) = load_blocks(&built_in()).unwrap();
    let bedrock = registry.id("base:bedrock").unwrap();
    // Appended after every other block, so no earlier id moved.
    assert_eq!(usize::from(bedrock.0), registry.len() - 1);
    let layer = registry.face_textures().get(bedrock, Face::PosY);
    assert_ne!(layer, TextureLayer::MISSING);
    assert_ne!(
        layer,
        registry
            .face_textures()
            .get(registry.id("base:stone").unwrap(), Face::PosY)
    );
}

#[test]
fn the_log_has_its_own_top_and_bottom_and_one_side_texture() {
    let (registry, _) = load_blocks(&built_in()).unwrap();
    let layer = |face| {
        registry
            .face_textures()
            .get(registry.id("base:log").unwrap(), face)
    };
    assert_eq!(layer(Face::PosY), layer(Face::NegY));
    assert_ne!(layer(Face::PosY), layer(Face::PosX));
    assert_eq!(layer(Face::PosX), layer(Face::NegZ));
    assert_ne!(layer(Face::PosX), TextureLayer::MISSING);
}

#[test]
fn grass_has_different_top_side_and_bottom_and_dirt_is_shared() {
    let (registry, textures) = load_blocks(&built_in()).unwrap();
    let layer = |name: &str, face| {
        registry
            .face_textures()
            .get(registry.id(name).unwrap(), face)
    };
    let top = layer("base:grass", Face::PosY);
    let side = layer("base:grass", Face::PosX);
    let bottom = layer("base:grass", Face::NegY);
    assert!(top != side && side != bottom && top != bottom);
    assert_eq!(bottom, layer("base:dirt", Face::PosX));
    assert_ne!(top, TextureLayer::MISSING);
    // Missing + grass_top + grass_side + dirt + stone + sand + cobblestone + planks + log_side
    // + log_top + bedrock.
    assert_eq!(textures.len(), 11);
}
