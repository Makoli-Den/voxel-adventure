//! Startup, before any window: finds the mods, puts them in load order and builds the pack
//! stack and the list of blocks files from them (decision 0014).
//!
//! [`assemble`] returns every problem at once as text instead of exiting, so a missing
//! dependency or a cycle can be tested without starting the game.

use std::path::PathBuf;

use vox_mods::{DiscoveredMod, MOD_API_VERSION, ModId, ModManifest, discover, resolve};
use vox_resources::{DirPack, PackStack, ResourceId};

use crate::packs;

/// The name of a mod's blocks file inside its own namespace: `<mod id>:blocks.json`.
const BLOCKS_FILE_NAME: &str = "blocks.json";

/// What the game starts from: the resources and the content files, ready to load.
#[derive(Debug)]
pub struct Startup {
    /// The resource packs, bottom to top: the engine's `assets/`, the `assets/` of every mod in
    /// load order, then every `--pack`.
    pub stack: PackStack,
    /// The blocks files in load order, one per mod that has one. Block ids follow this order.
    pub blocks_files: Vec<ResourceId>,
    /// The ids of the mods in load order.
    pub load_order: Vec<ModId>,
    /// The mods themselves in load order, for starting their scripts.
    pub mods: Vec<DiscoveredMod>,
    /// The `--pack` folders that are not directories, for a warning.
    pub missing_packs: Vec<PathBuf>,
}

impl Startup {
    /// The load order as one line, for the log: `Mods: base, ruby`.
    pub fn load_order_line(&self) -> String {
        let ids: Vec<&str> = self.load_order.iter().map(ModId::as_str).collect();
        format!("Mods: {}", ids.join(", "))
    }
}

/// The mods folders to look in: the `--mods` folders as given, or the built-in one when the
/// player gave none. `--mods` replaces the built-in folder, it does not add to it.
pub fn mod_dirs(requested: &[PathBuf]) -> Vec<PathBuf> {
    if requested.is_empty() {
        vec![packs::built_in_mods_dir()]
    } else {
        requested.to_vec()
    }
}

/// Discovers the mods in `mod_dirs`, resolves their dependencies and builds the pack stack.
///
/// A mod id found in two folders is an error. `mod_dirs` is the built-in mods folder when the
/// player gave no `--mods`.
///
/// # Errors
/// Every problem of the failing stage, as text and never empty. The stages run in order and the
/// first one with a problem stops the next: the built-in pack is missing; a folder cannot be
/// read, a link is found or a manifest is broken (all folders are checked); a duplicate id, a
/// missing or mismatching dependency or a cycle (resolution); a blocks file cannot be looked up.
/// Nothing is skipped silently.
pub fn assemble(
    mod_dirs: &[PathBuf],
    built_in: DirPack,
    packs: &[PathBuf],
) -> Result<Startup, Vec<String>> {
    if !built_in.exists() {
        return Err(vec![format!(
            "the built-in resource pack {} is not a directory",
            packs::describe_root(built_in.root())
        )]);
    }
    let found = discover_all(mod_dirs)?;
    let manifests: Vec<ModManifest> = found.iter().map(|m| m.manifest().clone()).collect();
    let order = resolve(&manifests, MOD_API_VERSION)
        .map_err(|errors| errors.iter().map(ToString::to_string).collect::<Vec<_>>())?;
    let ordered = in_load_order(&found, &order)?;
    let assets: Vec<PathBuf> = ordered.iter().map(|m| m.assets_dir()).collect();
    let (stack, missing_packs) = packs::pack_stack(built_in, &assets, packs);
    let blocks_files = blocks_files(&stack, &order)?;
    Ok(Startup {
        stack,
        blocks_files,
        load_order: order,
        mods: ordered.into_iter().cloned().collect(),
        missing_packs,
    })
}

/// The discovered mods arranged as `order`. `resolve` returns exactly the ids it was given, so
/// an unknown id is a bug there; it is reported instead of skipped, so no mod silently drops out.
fn in_load_order<'a>(
    found: &'a [DiscoveredMod],
    order: &[ModId],
) -> Result<Vec<&'a DiscoveredMod>, Vec<String>> {
    let mut ordered = Vec::with_capacity(order.len());
    let mut errors = Vec::new();
    for id in order {
        match found.iter().find(|m| m.manifest().id() == id) {
            Some(found_mod) => ordered.push(found_mod),
            None => errors.push(format!(
                "internal error: the load order has the mod `{id}`, which was not discovered"
            )),
        }
    }
    if errors.is_empty() {
        Ok(ordered)
    } else {
        Err(errors)
    }
}

/// The mods of every folder in order, all errors collected over all folders.
fn discover_all(mod_dirs: &[PathBuf]) -> Result<Vec<DiscoveredMod>, Vec<String>> {
    let mut found = Vec::new();
    let mut errors = Vec::new();
    for dir in mod_dirs {
        match discover(dir) {
            Ok(mods) => found.extend(mods),
            Err(problems) => errors.extend(problems.iter().map(ToString::to_string)),
        }
    }
    if errors.is_empty() {
        Ok(found)
    } else {
        Err(errors)
    }
}

/// `<id>:blocks.json` for every mod in `order` for which that file exists.
///
/// The question is asked of the whole `stack`, not of the mod's own assets: a `--pack` or a later
/// mod may supply `<id>:blocks.json` for a mod that has none, and it is loaded in that mod's
/// place in the order.
fn blocks_files(stack: &PackStack, order: &[ModId]) -> Result<Vec<ResourceId>, Vec<String>> {
    let mut files = Vec::new();
    let mut errors = Vec::new();
    for id in order {
        let file = match ResourceId::parse(&format!("{id}:{BLOCKS_FILE_NAME}")) {
            Ok(file) => file,
            Err(error) => {
                errors.push(format!("mod `{id}`: {error}"));
                continue;
            }
        };
        match stack.contains(&file) {
            Ok(true) => files.push(file),
            Ok(false) => {}
            Err(error) => errors.push(format!("mod `{id}`: {error}")),
        }
    }
    if errors.is_empty() {
        Ok(files)
    } else {
        Err(errors)
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use vox_content::load_content_from;

    use super::*;
    use crate::content::{Content, ContentError};

    /// A folder under the system temp folder, removed again on drop.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(label: &str) -> Self {
            let root = std::env::temp_dir()
                .join(format!("vox-game-startup-{label}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(&root).unwrap();
            Self(root)
        }

        fn write(&self, relative: &str, contents: impl AsRef<[u8]>) {
            let path = self.0.join(relative);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, contents).unwrap();
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn fixtures(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join(name)
    }

    fn built_in_mods() -> PathBuf {
        packs::built_in_mods_dir()
    }

    fn ids(startup: &Startup) -> Vec<&str> {
        startup.load_order.iter().map(ModId::as_str).collect()
    }

    fn file_names(startup: &Startup) -> Vec<String> {
        startup
            .blocks_files
            .iter()
            .map(ToString::to_string)
            .collect()
    }

    #[test]
    fn the_base_mod_alone_loads_first_with_its_blocks_file() {
        let startup = assemble(&[built_in_mods()], packs::built_in_pack(), &[]).unwrap();
        assert_eq!(ids(&startup), ["base"]);
        assert_eq!(file_names(&startup), ["base:blocks.json"]);
        assert_eq!(startup.load_order_line(), "Mods: base");
        // The engine's assets and the base mod's assets.
        assert_eq!(startup.stack.len(), 2);
    }

    #[test]
    fn a_data_only_mod_layers_after_base_and_adds_its_block() {
        let dirs = [built_in_mods(), fixtures("mods")];
        let startup = assemble(&dirs, packs::built_in_pack(), &[]).unwrap();
        assert_eq!(startup.load_order_line(), "Mods: base, ruby");
        assert_eq!(
            file_names(&startup),
            ["base:blocks.json", "ruby:blocks.json"]
        );
        assert_eq!(startup.stack.len(), 3);

        let (registry, textures, warnings) =
            load_content_from(&startup.stack, &startup.blocks_files).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        // Base is first, so its ids do not move; the mod's block comes after all of them.
        assert_eq!(registry.id("base:grass").map(|id| id.0), Some(1));
        assert_eq!(registry.id("base:dirt").map(|id| id.0), Some(2));
        assert_eq!(registry.id("base:stone").map(|id| id.0), Some(3));
        let ruby = registry.id("ruby:ruby_block").unwrap();
        assert_eq!(usize::from(ruby.0), registry.len() - 1);
        assert_eq!(registry.name(ruby), Some("ruby:ruby_block"));
        // The mod's own texture got a layer of its own, after the base layers.
        let base_only = assemble(&[built_in_mods()], packs::built_in_pack(), &[]).unwrap();
        let (_, base_textures, _) =
            load_content_from(&base_only.stack, &base_only.blocks_files).unwrap();
        assert_eq!(textures.layer_count, base_textures.layer_count + 1);
    }

    #[test]
    fn the_order_of_the_folders_does_not_change_the_load_order() {
        let dirs = [fixtures("mods"), built_in_mods()];
        let startup = assemble(&dirs, packs::built_in_pack(), &[]).unwrap();
        assert_eq!(startup.load_order_line(), "Mods: base, ruby");
    }

    #[test]
    fn a_pack_is_stacked_above_the_mods() {
        let dirs = [built_in_mods(), fixtures("mods")];
        let pack = fixtures("mods");
        let startup = assemble(&dirs, packs::built_in_pack(), &[pack]).unwrap();
        assert_eq!(startup.stack.len(), 4);
        assert!(startup.missing_packs.is_empty());
    }

    #[test]
    fn a_missing_dependency_stops_startup_and_is_named() {
        let dirs = [built_in_mods(), fixtures("missing")];
        let errors = assemble(&dirs, packs::built_in_pack(), &[]).unwrap_err();
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert!(errors[0].contains("lonely"), "{errors:?}");
        assert!(errors[0].contains("gem"), "{errors:?}");
    }

    #[test]
    fn a_cycle_stops_startup_and_names_both_mods() {
        let dirs = [built_in_mods(), fixtures("cycle")];
        let errors = assemble(&dirs, packs::built_in_pack(), &[]).unwrap_err();
        let text = errors.join("\n");
        assert!(text.contains("ping") && text.contains("pong"), "{text}");
    }

    #[test]
    fn an_unreadable_folder_is_listed_alone_before_resolution() {
        let nowhere = std::env::temp_dir().join("vox-game-startup-no-such-mods-folder");
        let dirs = [built_in_mods(), fixtures("missing"), nowhere];
        let errors = assemble(&dirs, packs::built_in_pack(), &[]).unwrap_err();
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert!(
            errors[0].contains("vox-game-startup-no-such-mods-folder"),
            "{errors:?}"
        );
    }

    #[test]
    fn a_missing_dependency_and_a_cycle_are_listed_together() {
        let dirs = [built_in_mods(), fixtures("missing"), fixtures("cycle")];
        let errors = assemble(&dirs, packs::built_in_pack(), &[]).unwrap_err();
        // One missing dependency and one cycle; no other mod is blamed.
        assert_eq!(errors.len(), 2, "{errors:?}");
        let with = |words: &[&str]| {
            errors
                .iter()
                .filter(|e| words.iter().all(|w| e.contains(w)))
                .count()
        };
        assert_eq!(with(&["lonely", "gem"]), 1, "{errors:?}");
        assert_eq!(with(&["ping", "pong"]), 1, "{errors:?}");
    }

    #[test]
    fn without_the_base_mod_startup_names_it() {
        let errors = assemble(&[fixtures("mods")], packs::built_in_pack(), &[]).unwrap_err();
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert!(
            errors[0].contains("ruby") && errors[0].contains("base"),
            "{errors:?}"
        );
    }

    #[test]
    fn the_game_needs_the_base_blocks() {
        // Only the mod's blocks file is listed, as when `base` is absent.
        let dirs = [built_in_mods(), fixtures("mods")];
        let startup = assemble(&dirs, packs::built_in_pack(), &[]).unwrap();
        let ruby_only: Vec<_> = startup.blocks_files[1..].to_vec();
        let result = Content::new(&startup.stack, &ruby_only);
        assert!(matches!(result, Err(ContentError::MissingBlock(_))));
    }

    #[test]
    fn a_pack_overrides_a_mods_blocks_file() {
        let pack = TempDir::new("pack-over-mod");
        pack.write(
            "ruby/blocks.json",
            r#"{ "format": 1, "blocks": [
                { "name": "from_pack", "textures": { "all": "ruby:textures/block/ruby.png" } } ] }"#,
        );
        let dirs = [built_in_mods(), fixtures("mods")];
        let startup =
            assemble(&dirs, packs::built_in_pack(), std::slice::from_ref(&pack.0)).unwrap();
        let (registry, _, _) = load_content_from(&startup.stack, &startup.blocks_files).unwrap();
        assert!(registry.id("ruby:from_pack").is_some());
        assert!(registry.id("ruby:ruby_block").is_none());
    }

    #[test]
    fn a_later_mod_overrides_an_earlier_one_through_its_assets() {
        let mods = TempDir::new("later-mod");
        mods.write(
            "zzz/mod.json",
            r#"{ "format": 1, "id": "zzz", "version": "1.0.0", "api": "1.0",
                "dependencies": { "ruby": "^1.0" } }"#,
        );
        mods.write(
            "zzz/assets/ruby/blocks.json",
            r#"{ "format": 1, "blocks": [
                { "name": "from_zzz", "textures": { "all": "ruby:textures/block/ruby.png" } } ] }"#,
        );
        let dirs = [built_in_mods(), fixtures("mods"), mods.0.clone()];
        let startup = assemble(&dirs, packs::built_in_pack(), &[]).unwrap();
        assert_eq!(startup.load_order_line(), "Mods: base, ruby, zzz");
        // zzz has no blocks file of its own; ruby's file is read from the highest pack.
        assert_eq!(
            file_names(&startup),
            ["base:blocks.json", "ruby:blocks.json"]
        );
        let (registry, _, _) = load_content_from(&startup.stack, &startup.blocks_files).unwrap();
        assert!(registry.id("ruby:from_zzz").is_some());
        assert!(registry.id("ruby:ruby_block").is_none());
    }

    #[test]
    fn the_blocks_file_is_looked_up_in_the_whole_stack() {
        // A pack supplies `zzz:blocks.json` for a mod that has no such file of its own.
        let mods = TempDir::new("stack-lookup-mods");
        mods.write(
            "zzz/mod.json",
            r#"{ "format": 1, "id": "zzz", "version": "1.0.0", "api": "1.0" }"#,
        );
        let pack = TempDir::new("stack-lookup-pack");
        pack.write(
            "zzz/blocks.json",
            r#"{ "format": 1, "blocks": [
                { "name": "gift", "textures": { "all": "zzz:textures/block/none.png" } } ] }"#,
        );
        let dirs = [mods.0.clone()];
        let startup =
            assemble(&dirs, packs::built_in_pack(), std::slice::from_ref(&pack.0)).unwrap();
        assert_eq!(file_names(&startup), ["zzz:blocks.json"]);
    }

    #[test]
    fn mods_given_on_the_command_line_replace_the_built_in_folder() {
        assert_eq!(mod_dirs(&[]), vec![packs::built_in_mods_dir()]);
        let given = vec![PathBuf::from("a"), PathBuf::from("b")];
        assert_eq!(mod_dirs(&given), given);
    }

    #[test]
    fn a_mod_id_in_two_folders_is_an_error() {
        let dirs = [built_in_mods(), built_in_mods()];
        let errors = assemble(&dirs, packs::built_in_pack(), &[]).unwrap_err();
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert!(errors[0].contains("base"), "{errors:?}");
    }

    #[test]
    fn a_missing_built_in_pack_is_an_error() {
        let nowhere = std::env::temp_dir().join("vox-game-startup-no-such-assets");
        let errors = assemble(&[built_in_mods()], DirPack::new(nowhere), &[]).unwrap_err();
        assert!(errors[0].contains("built-in resource pack"), "{errors:?}");
    }
}
