//! The real `assets/base/blocks.json` loads and keeps the ids the shader palette and the
//! terrain fingerprints depend on.

#![allow(clippy::unwrap_used)]

use std::path::Path;

use vox_content::load_blocks;
use vox_resources::{DirPack, PackStack};
use vox_world::{BlockId, Face, TextureLayer};

fn built_in() -> PackStack {
    let mut stack = PackStack::new();
    stack.push(DirPack::new(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets"),
    ));
    stack
}

#[test]
fn grass_dirt_and_stone_keep_ids_one_two_three() {
    let (registry, _) = load_blocks(&built_in()).unwrap();
    assert_eq!(registry.id("base:grass"), Some(BlockId(1)));
    assert_eq!(registry.id("base:dirt"), Some(BlockId(2)));
    assert_eq!(registry.id("base:stone"), Some(BlockId(3)));
    assert_eq!(registry.len(), 4);
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
    // Missing + grass_top + grass_side + dirt + stone.
    assert_eq!(textures.len(), 5);
}
