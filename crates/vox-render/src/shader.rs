//! Loading GLSL shaders from files and compiling them to SPIR-V at runtime.
//!
//! Shaders are read from disk instead of being embedded in the binary, because shader
//! packs replace them with their own files. Compilation uses `naga`, which is pure Rust,
//! so neither players nor pack authors need a C++ toolchain or the Vulkan SDK.
//! See `docs/decisions/0003-runtime-shader-compilation.md`.

use std::fs;
use std::path::Path;

use naga::back::spv;
use naga::front::glsl;
use naga::proc::{BoundsCheckPolicies, BoundsCheckPolicy};
use naga::valid::{Capabilities, ValidationFlags, Validator};

pub(crate) use naga::ShaderStage;

use crate::RenderError;

/// Reads the GLSL file at `path` and compiles it to SPIR-V words for `stage`.
pub(crate) fn load_spirv(path: &Path, stage: ShaderStage) -> Result<Vec<u32>, RenderError> {
    let source = read_source(path)?;
    compile_glsl(&source, stage, &path.display().to_string())
}

/// Reads a shader source file.
fn read_source(path: &Path) -> Result<String, RenderError> {
    fs::read_to_string(path).map_err(|error| {
        shader_error(
            &path.display().to_string(),
            &format!("cannot read the file: {error}"),
        )
    })
}

/// Compiles GLSL 450 (Vulkan style) to SPIR-V 1.0 words.
///
/// `name` labels the shader in error messages, normally its file path.
fn compile_glsl(source: &str, stage: ShaderStage, name: &str) -> Result<Vec<u32>, RenderError> {
    let module = glsl::Frontend::default()
        .parse(&glsl::Options::from(stage), source)
        .map_err(|errors| shader_error(name, &errors.emit_to_string(source)))?;
    // Validation catches mistakes the parser lets through, so a broken shader pack
    // fails here with a readable message instead of crashing the driver.
    let info = Validator::new(ValidationFlags::all(), allowed_capabilities())
        .validate(&module)
        .map_err(|error| shader_error(name, &error.emit_to_string_with_path(source, name)))?;
    spv::write_vec(&module, &info, &spirv_options(), None)
        .map_err(|error| shader_error(name, &error.to_string()))
}

/// The optional features a shader may use: only those core Vulkan 1.0 always has.
///
/// Push constants (naga calls them "immediates") are core Vulkan 1.0 and need no
/// device feature. Anything added here must be enabled on the device first.
fn allowed_capabilities() -> Capabilities {
    Capabilities::IMMEDIATES
}

/// SPIR-V writer settings for Vulkan 1.0.
fn spirv_options() -> spv::Options<'static> {
    let mut options = spv::Options::default();
    // The default flips Y, because naga's own clip space points Y up. Our GLSL is
    // written for Vulkan clip space already, so it must reach the GPU unchanged.
    options
        .flags
        .remove(spv::WriterFlags::ADJUST_COORDINATE_SPACE);
    // The default does no bounds checking, and the device has no `robustBufferAccess`.
    // A shader from a pack that indexes past the end of an array would then read or
    // write outside its data on the GPU. `Restrict` clamps every index into range.
    options.bounds_check_policies = BoundsCheckPolicies {
        index: BoundsCheckPolicy::Restrict,
        buffer: BoundsCheckPolicy::Restrict,
        image_load: BoundsCheckPolicy::Restrict,
        binding_array: BoundsCheckPolicy::Restrict,
    };
    options
}

fn shader_error(name: &str, message: &str) -> RenderError {
    RenderError::Shader(format!("{name}: {}", message.trim_end()))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    /// First word of every SPIR-V module.
    const SPIRV_MAGIC: u32 = 0x0723_0203;

    fn shader_dir() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/shaders")
    }

    fn assert_is_spirv(words: &[u32]) {
        assert!(words.len() > 5, "too short for a SPIR-V module");
        assert_eq!(words[0], SPIRV_MAGIC);
    }

    #[test]
    fn real_vertex_shader_compiles() {
        let words = load_spirv(&shader_dir().join("chunk.vert"), ShaderStage::Vertex).unwrap();
        assert_is_spirv(&words);
    }

    #[test]
    fn real_fragment_shader_compiles() {
        let words = load_spirv(&shader_dir().join("chunk.frag"), ShaderStage::Fragment).unwrap();
        assert_is_spirv(&words);
    }

    #[test]
    fn array_indices_and_buffers_are_bounds_checked() {
        let policies = spirv_options().bounds_check_policies;
        assert_eq!(policies.index, BoundsCheckPolicy::Restrict);
        assert_eq!(policies.buffer, BoundsCheckPolicy::Restrict);
    }

    #[test]
    fn broken_glsl_reports_the_file_name() {
        let source = "#version 450\nvoid main() { this is not glsl }\n";
        let error = compile_glsl(source, ShaderStage::Fragment, "broken.frag").unwrap_err();
        assert!(matches!(&error, RenderError::Shader(text) if text.contains("broken.frag")));
    }

    #[test]
    fn missing_file_is_a_shader_error() {
        let path = shader_dir().join("does_not_exist.vert");
        let error = load_spirv(&path, ShaderStage::Vertex).unwrap_err();
        let text = error.to_string();
        assert!(matches!(error, RenderError::Shader(_)));
        assert!(text.contains("does_not_exist.vert"));
        assert!(text.contains("cannot read"));
    }
}
