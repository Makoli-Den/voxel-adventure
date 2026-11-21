//! Loading GLSL shaders from files and compiling them to SPIR-V at runtime.
//!
//! Shaders are read from disk instead of being embedded in the binary, because shader
//! packs replace them with their own files. Compilation uses `naga`, which is pure Rust,
//! so neither players nor pack authors need a C++ toolchain or the Vulkan SDK.
//! See `docs/decisions/0003-runtime-shader-compilation.md`.
//!
//! Every shader is validated by naga and then checked against what the pipeline
//! provides (`check`), so a pack shader that asks for more fails with a readable
//! error instead of producing an invalid pipeline.

mod check;

use std::fs;
use std::path::Path;

use naga::back::spv;
use naga::front::glsl;
use naga::proc::{BoundsCheckPolicies, BoundsCheckPolicy};
use naga::valid::{Capabilities, ModuleInfo, ValidationFlags, Validator};
use naga::{Module, ShaderStage};

use crate::RenderError;

/// SPIR-V words of the vertex and fragment stage of one pipeline.
pub(crate) struct PipelineShaders {
    /// The vertex stage.
    pub(crate) vertex: Vec<u32>,
    /// The fragment stage.
    pub(crate) fragment: Vec<u32>,
}

/// Reads and compiles the vertex and fragment shader files of the chunk pipeline.
///
/// Both are validated, checked against the pipeline interface (`crate::interface`),
/// and checked against each other: every fragment input must be a vertex output.
pub(crate) fn load_pipeline_shaders(
    vertex_path: &Path,
    fragment_path: &Path,
) -> Result<PipelineShaders, RenderError> {
    let vertex = load(vertex_path, ShaderStage::Vertex)?;
    let fragment = load(fragment_path, ShaderStage::Fragment)?;
    link(&vertex, &fragment)?;
    Ok(PipelineShaders {
        vertex: vertex.to_spirv()?,
        fragment: fragment.to_spirv()?,
    })
}

/// A validated shader module that passed the interface check of its stage.
struct Compiled {
    module: Module,
    info: ModuleInfo,
    /// Labels the shader in error messages, normally its file path.
    name: String,
}

impl Compiled {
    /// Writes the module as SPIR-V 1.0 words.
    fn to_spirv(&self) -> Result<Vec<u32>, RenderError> {
        spv::write_vec(&self.module, &self.info, &spirv_options(), None)
            .map_err(|error| shader_error(&self.name, &error.to_string()))
    }
}

/// Reads the GLSL file at `path` and compiles it for `stage`.
fn load(path: &Path, stage: ShaderStage) -> Result<Compiled, RenderError> {
    let name = path.display().to_string();
    let source = fs::read_to_string(path)
        .map_err(|error| shader_error(&name, &format!("cannot read the file: {error}")))?;
    compile_glsl(&source, stage, &name)
}

/// Parses and validates GLSL 450 (Vulkan style), then checks it against the pipeline.
///
/// `name` labels the shader in error messages, normally its file path.
fn compile_glsl(source: &str, stage: ShaderStage, name: &str) -> Result<Compiled, RenderError> {
    let module = glsl::Frontend::default()
        .parse(&glsl::Options::from(stage), source)
        .map_err(|errors| shader_error(name, &errors.emit_to_string(source)))?;
    // Validation catches mistakes the parser lets through, so a broken shader pack
    // fails here with a readable message instead of crashing the driver.
    let info = Validator::new(ValidationFlags::all(), allowed_capabilities())
        .validate(&module)
        .map_err(|error| shader_error(name, &error.emit_to_string_with_path(source, name)))?;
    check::check_stage(&module, stage).map_err(|message| shader_error(name, &message))?;
    Ok(Compiled {
        module,
        info,
        name: name.to_owned(),
    })
}

/// Checks that the fragment stage only reads what the vertex stage writes.
fn link(vertex: &Compiled, fragment: &Compiled) -> Result<(), RenderError> {
    check::check_link(&vertex.module, &fragment.module).map_err(|message| {
        let message = format!("{message} (vertex shader: {})", vertex.name);
        shader_error(&fragment.name, &message)
    })
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

    /// The error text of compiling `source`, which must fail.
    fn compile_error(source: &str, stage: ShaderStage) -> String {
        match compile_glsl(source, stage, "pack.glsl") {
            Ok(_) => panic!("the shader was accepted"),
            Err(error) => error.to_string(),
        }
    }

    /// The error text of linking two shaders that each compile, which must fail.
    fn link_error(vertex: &str, fragment: &str) -> String {
        let vertex = compile_glsl(vertex, ShaderStage::Vertex, "pack.vert").unwrap();
        let fragment = compile_glsl(fragment, ShaderStage::Fragment, "pack.frag").unwrap();
        link(&vertex, &fragment).unwrap_err().to_string()
    }

    #[test]
    fn real_shaders_compile_and_link() {
        let dir = shader_dir();
        let shaders =
            load_pipeline_shaders(&dir.join("chunk.vert"), &dir.join("chunk.frag")).unwrap();
        assert_is_spirv(&shaders.vertex);
        assert_is_spirv(&shaders.fragment);
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
        let Err(error) = compile_glsl(source, ShaderStage::Fragment, "broken.frag") else {
            panic!("broken GLSL was accepted");
        };
        assert!(matches!(&error, RenderError::Shader(text) if text.contains("broken.frag")));
    }

    #[test]
    fn missing_file_is_a_shader_error() {
        let dir = shader_dir();
        let missing = dir.join("does_not_exist.vert");
        let Err(error) = load_pipeline_shaders(&missing, &dir.join("chunk.frag")) else {
            panic!("a missing file was accepted");
        };
        let text = error.to_string();
        assert!(matches!(error, RenderError::Shader(_)));
        assert!(text.contains("does_not_exist.vert"));
        assert!(text.contains("cannot read"));
    }

    #[test]
    fn push_constants_larger_than_the_range_are_rejected() {
        let source = "#version 450
            layout(push_constant) uniform Big { mat4 a; mat4 b; } big;
            void main() { gl_Position = big.a * big.b[0]; }";
        let text = compile_error(source, ShaderStage::Vertex);
        assert!(
            text.contains("pack.glsl") && text.contains("128 bytes"),
            "{text}"
        );
    }

    #[test]
    fn push_constants_in_the_fragment_stage_are_rejected() {
        let source = "#version 450
            layout(push_constant) uniform Tint { vec4 color; } tint;
            layout(location = 0) out vec4 out_color;
            void main() { out_color = tint.color; }";
        let text = compile_error(source, ShaderStage::Fragment);
        assert!(text.contains("vertex stage"), "{text}");
    }

    #[test]
    fn unknown_vertex_input_location_is_rejected() {
        let source = "#version 450
            layout(location = 2) in uint extra;
            void main() { gl_Position = vec4(float(extra)); }";
        let text = compile_error(source, ShaderStage::Vertex);
        assert!(text.contains("location 2"), "{text}");
    }

    #[test]
    fn wrong_vertex_input_type_is_rejected() {
        let source = "#version 450
            layout(location = 1) in vec4 block;
            void main() { gl_Position = block; }";
        let text = compile_error(source, ShaderStage::Vertex);
        assert!(
            text.contains("location 1") && text.contains("`uint`"),
            "{text}"
        );
    }

    #[test]
    fn descriptor_resources_are_rejected() {
        let source = "#version 450
            layout(set = 0, binding = 0) uniform Data { vec4 color; } data;
            layout(location = 0) out vec4 out_color;
            void main() { out_color = data.color; }";
        let text = compile_error(source, ShaderStage::Fragment);
        assert!(text.contains("descriptor"), "{text}");
    }

    #[test]
    fn fragment_input_without_vertex_output_is_rejected() {
        let vertex = "#version 450
            layout(location = 0) out vec3 v_color;
            void main() { v_color = vec3(1.0); gl_Position = vec4(0.0); }";
        let fragment = "#version 450
            layout(location = 1) in vec3 v_light;
            layout(location = 0) out vec4 out_color;
            void main() { out_color = vec4(v_light, 1.0); }";
        let text = link_error(vertex, fragment);
        assert!(
            text.contains("pack.frag") && text.contains("location 1"),
            "{text}"
        );
    }

    #[test]
    fn fragment_input_of_another_type_is_rejected() {
        let vertex = "#version 450
            layout(location = 0) out vec3 v_color;
            void main() { v_color = vec3(1.0); gl_Position = vec4(0.0); }";
        let fragment = "#version 450
            layout(location = 0) in vec4 v_color;
            layout(location = 0) out vec4 out_color;
            void main() { out_color = v_color; }";
        let text = link_error(vertex, fragment);
        assert!(
            text.contains("location 0") && text.contains("type"),
            "{text}"
        );
    }
}
