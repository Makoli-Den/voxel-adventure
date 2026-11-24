//! Where the game's resources come from: the stack of resource packs, and the shaders read
//! through it.
//!
//! The built-in pack is the `assets/` folder at the bottom (the engine's own resources, the
//! shaders); the `assets/` of every mod goes above it in load order (decision 0014) and every
//! `--pack` above those in command-line order (decision 0013). A shader pack is just a pack
//! with `core/shaders/...`.

use std::path::{Path, PathBuf};

use vox_render::{ShaderSource, ShaderSources};
use vox_resources::{DirPack, PackStack, ResourceError, ResourceId};

/// The root of the source tree, relative to this crate's directory. Temporary, like the shader
/// folder before it (decision 0003): the path is baked in at build time, so a copied executable
/// does not find the built-in pack or mods.
// TODO: look next to the executable once the game is packaged (a later packaging step).
const WORKSPACE_ROOT: &str = "../..";

/// The built-in pack, the engine's own `assets/` folder, inside [`WORKSPACE_ROOT`].
const BUILT_IN_PACK: &str = "assets";

/// The built-in mods folder, the `mods/` folder inside [`WORKSPACE_ROOT`], which holds the base
/// game.
const BUILT_IN_MODS: &str = "mods";

/// Resource id of the terrain vertex shader ([`ShaderSources::chunk_vertex`]).
const CHUNK_VERTEX_SHADER: &str = "core:shaders/chunk.vert";
/// Resource id of the terrain fragment shader ([`ShaderSources::chunk_fragment`]).
const CHUNK_FRAGMENT_SHADER: &str = "core:shaders/chunk.frag";
/// Resource id of the overlay vertex shader ([`ShaderSources::overlay_vertex`]).
const OVERLAY_VERTEX_SHADER: &str = "core:shaders/overlay.vert";
/// Resource id of the overlay fragment shader ([`ShaderSources::overlay_fragment`]).
const OVERLAY_FRAGMENT_SHADER: &str = "core:shaders/overlay.frag";

/// The built-in pack from the source tree.
pub fn built_in_pack() -> DirPack {
    DirPack::new(workspace_path(BUILT_IN_PACK))
}

/// The built-in mods folder from the source tree, used when no `--mods` is given.
pub fn built_in_mods_dir() -> PathBuf {
    workspace_path(BUILT_IN_MODS)
}

/// `relative` inside the source tree.
fn workspace_path(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(WORKSPACE_ROOT)
        .join(relative)
}

/// `root` for messages: as given, plus the absolute path when that differs, so a relative
/// `--pack` path shows where it was looked for.
pub fn describe_root(root: &Path) -> String {
    match std::path::absolute(root) {
        Ok(absolute) if absolute != root => format!("{} ({})", root.display(), absolute.display()),
        _ => root.display().to_string(),
    }
}

/// Stacks `built_in` at the bottom, then the `mod_assets` folders (one per mod, in load order),
/// then the `packs`, each over the previous one.
///
/// A mod's assets folder may be missing (a mod without resources), so only `packs` are checked.
///
/// Also returns the `packs` whose root is not a directory, so the caller can warn: such a
/// pack reads as empty, which is almost always a mistyped path.
pub fn pack_stack(
    built_in: DirPack,
    mod_assets: &[PathBuf],
    packs: &[PathBuf],
) -> (PackStack, Vec<PathBuf>) {
    let mut stack = PackStack::new();
    stack.push(built_in);
    for root in mod_assets {
        stack.push(DirPack::new(root));
    }
    let mut missing = Vec::new();
    for root in packs {
        let pack = DirPack::new(root);
        if !pack.exists() {
            missing.push(root.clone());
        }
        stack.push(pack);
    }
    (stack, missing)
}

/// Reads the four shaders from `stack`, each named by its resource id.
///
/// # Errors
/// [`ResourceError::NotFound`] naming the id of a shader no pack has, or
/// [`ResourceError::Io`] when one cannot be read or is not UTF-8.
pub fn load_shaders(stack: &PackStack) -> Result<ShaderSources, ResourceError> {
    Ok(ShaderSources {
        chunk_vertex: load_shader(stack, CHUNK_VERTEX_SHADER)?,
        chunk_fragment: load_shader(stack, CHUNK_FRAGMENT_SHADER)?,
        overlay_vertex: load_shader(stack, OVERLAY_VERTEX_SHADER)?,
        overlay_fragment: load_shader(stack, OVERLAY_FRAGMENT_SHADER)?,
    })
}

/// Reads the shader `id` from `stack`.
fn load_shader(stack: &PackStack, id: &str) -> Result<ShaderSource, ResourceError> {
    let id = ResourceId::parse(id)?;
    let text = stack.read_to_string(&id)?;
    Ok(ShaderSource {
        name: id.to_string(),
        text,
    })
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    /// A pack directory under the system temp folder, removed again on drop.
    struct TempPack(PathBuf);

    impl TempPack {
        fn new(label: &str) -> Self {
            let root =
                std::env::temp_dir().join(format!("vox-game-packs-{label}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&root);
            fs::create_dir_all(&root).unwrap();
            Self(root)
        }

        fn write(&self, relative: &str, contents: impl AsRef<[u8]>) {
            let path = self.0.join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, contents).unwrap();
        }
    }

    impl Drop for TempPack {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn the_built_in_pack_has_every_shader_named_by_its_id() {
        let (stack, missing) = pack_stack(built_in_pack(), &[], &[]);
        assert!(missing.is_empty());
        let shaders = load_shaders(&stack).unwrap();
        assert_eq!(shaders.chunk_vertex.name, CHUNK_VERTEX_SHADER);
        assert_eq!(shaders.chunk_fragment.name, CHUNK_FRAGMENT_SHADER);
        assert_eq!(shaders.overlay_vertex.name, OVERLAY_VERTEX_SHADER);
        assert_eq!(shaders.overlay_fragment.name, OVERLAY_FRAGMENT_SHADER);
        assert!(shaders.chunk_vertex.text.contains("void main"));
    }

    #[test]
    fn a_missing_shader_names_its_resource_id() {
        let empty = TempPack::new("empty");
        let (stack, _) = pack_stack(DirPack::new(&empty.0), &[], &[]);
        let error = load_shaders(&stack).unwrap_err().to_string();
        assert!(error.contains(CHUNK_VERTEX_SHADER), "{error}");
    }

    #[test]
    fn a_later_pack_overrides_one_shader_and_leaves_the_rest() {
        let first = TempPack::new("first");
        first.write("core/shaders/chunk.frag", "first");
        first.write("core/shaders/overlay.frag", "first overlay");
        let second = TempPack::new("second");
        second.write("core/shaders/chunk.frag", "second");
        let packs = [first.0.clone(), second.0.clone()];
        let (stack, missing) = pack_stack(built_in_pack(), &[], &packs);
        assert!(missing.is_empty());
        let shaders = load_shaders(&stack).unwrap();
        assert_eq!(shaders.chunk_fragment.text, "second");
        assert_eq!(shaders.overlay_fragment.text, "first overlay");
        assert!(shaders.chunk_vertex.text.contains("void main"));
    }

    #[test]
    fn a_shader_that_is_not_utf8_is_an_io_error() {
        let pack = TempPack::new("not-utf8");
        // 0xFF never occurs in UTF-8.
        pack.write("core/shaders/chunk.frag", [0xFF, 0xFE, 0xFD]);
        let (stack, _) = pack_stack(built_in_pack(), &[], std::slice::from_ref(&pack.0));
        let error = load_shaders(&stack).unwrap_err();
        assert!(
            matches!(&error, ResourceError::Io { id, .. } if id.to_string() == CHUNK_FRAGMENT_SHADER),
            "{error}"
        );
    }

    #[test]
    fn a_relative_root_is_described_with_its_absolute_path() {
        let text = describe_root(Path::new("some_pack"));
        let absolute = std::path::absolute("some_pack").unwrap();
        assert!(text.starts_with("some_pack"), "{text}");
        assert!(text.contains(&absolute.display().to_string()), "{text}");
        let absolute_text = describe_root(&absolute);
        assert_eq!(absolute_text, absolute.display().to_string());
    }

    #[test]
    fn a_pack_that_is_not_a_directory_is_reported_and_reads_as_empty() {
        let missing_root = std::env::temp_dir().join("vox-game-packs-no-such-directory");
        let (stack, missing) =
            pack_stack(built_in_pack(), &[], std::slice::from_ref(&missing_root));
        assert_eq!(missing, vec![missing_root]);
        assert!(load_shaders(&stack).is_ok());
    }

    #[test]
    fn the_stack_is_core_then_mods_then_packs() {
        let module = TempPack::new("order-mod");
        module.write("core/shaders/chunk.frag", "mod");
        module.write("core/shaders/overlay.frag", "mod overlay");
        let pack = TempPack::new("order-pack");
        pack.write("core/shaders/overlay.frag", "pack overlay");
        let (stack, missing) = pack_stack(
            built_in_pack(),
            std::slice::from_ref(&module.0),
            std::slice::from_ref(&pack.0),
        );
        assert!(missing.is_empty());
        assert_eq!(stack.len(), 3);
        let shaders = load_shaders(&stack).unwrap();
        // The mod overrides the engine, the pack overrides the mod.
        assert_eq!(shaders.chunk_fragment.text, "mod");
        assert_eq!(shaders.overlay_fragment.text, "pack overlay");
    }

    #[test]
    fn a_mod_without_an_assets_folder_is_not_reported() {
        let nowhere = std::env::temp_dir().join("vox-game-packs-mod-without-assets");
        let (stack, missing) = pack_stack(built_in_pack(), &[nowhere], &[]);
        assert!(missing.is_empty());
        assert_eq!(stack.len(), 2);
    }

    #[test]
    fn the_built_in_mods_folder_holds_the_base_mod() {
        assert!(built_in_mods_dir().join("base").join("mod.json").is_file());
    }
}
