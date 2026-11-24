//! Compiling GLSL shaders to SPIR-V at runtime.
//!
//! The GLSL text comes from the caller ([`ShaderSources`]), which reads it from resource
//! packs instead of the binary, because shader packs replace the built-in shaders.
//! Compilation uses `naga`, which is pure Rust, so neither players nor pack authors need a
//! C++ toolchain or the Vulkan SDK. See `docs/decisions/0003-runtime-shader-compilation.md`
//! and `0013-resources-and-textures.md`.
//!
//! Every shader is validated by naga and then checked against what the pipeline
//! provides (`check`), so a pack shader that asks for more fails with a readable
//! error instead of producing an invalid pipeline.

mod check;
mod sources;

use naga::back::spv;
use naga::front::glsl;
use naga::proc::{BoundsCheckPolicies, BoundsCheckPolicy};
use naga::valid::{Capabilities, ModuleInfo, ValidationFlags, Validator};
use naga::{Module, ShaderStage};

use crate::RenderError;
use crate::interface::PipelineInterface;

pub use sources::{ShaderSource, ShaderSources};

/// SPIR-V words of the vertex and fragment stage of one pipeline.
pub(crate) struct PipelineShaders {
    /// The vertex stage.
    pub(crate) vertex: Vec<u32>,
    /// The fragment stage.
    pub(crate) fragment: Vec<u32>,
}

/// Compiles the vertex and fragment shaders of one pipeline.
///
/// Both are validated, checked against `interface`, the description of what that
/// pipeline provides (see `crate::interface`), and checked against each other: every
/// fragment input must be a vertex output.
pub(crate) fn load_pipeline_shaders(
    vertex: &ShaderSource,
    fragment: &ShaderSource,
    interface: &PipelineInterface,
) -> Result<PipelineShaders, RenderError> {
    let vertex = compile_glsl(&vertex.text, ShaderStage::Vertex, interface, &vertex.name)?;
    let fragment = compile_glsl(
        &fragment.text,
        ShaderStage::Fragment,
        interface,
        &fragment.name,
    )?;
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
    /// Labels the shader in error messages, normally its resource id.
    name: String,
}

impl Compiled {
    /// Writes the module as SPIR-V 1.0 words.
    fn to_spirv(&self) -> Result<Vec<u32>, RenderError> {
        spv::write_vec(&self.module, &self.info, &spirv_options(), None)
            .map_err(|error| shader_error(&self.name, &error.to_string()))
    }
}

/// Parses and validates GLSL 450 (Vulkan style), then checks it against `interface`.
///
/// `name` labels the shader in error messages, normally its resource id.
fn compile_glsl(
    source: &str,
    stage: ShaderStage,
    interface: &PipelineInterface,
    name: &str,
) -> Result<Compiled, RenderError> {
    let module = glsl::Frontend::default()
        .parse(&glsl::Options::from(stage), source)
        .map_err(|errors| shader_error(name, &errors.emit_to_string(source)))?;
    // Validation catches mistakes the parser lets through, so a broken shader pack
    // fails here with a readable message instead of crashing the driver.
    let info = Validator::new(ValidationFlags::all(), allowed_capabilities())
        .validate(&module)
        .map_err(|error| shader_error(name, &error.emit_to_string_with_path(source, name)))?;
    check::check_stage(&module, stage, interface)
        .map_err(|message| shader_error(name, &message))?;
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
    // A shader from a pack that indexes past the end of an array, or loads a texel
    // (`texelFetch`) outside an image such as the block textures, would then read or
    // write outside its data on the GPU. `Restrict` clamps every index and coordinate
    // into range.
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
    use std::fs;
    use std::path::Path;

    use super::*;
    use crate::interface::chunk::INTERFACE as CHUNK;
    use crate::interface::overlay::INTERFACE as OVERLAY;

    /// First word of every SPIR-V module.
    const SPIRV_MAGIC: u32 = 0x0723_0203;

    /// Where the built-in shaders live in the source tree (the `core:shaders/` resources).
    const BUILT_IN_SHADERS: &str = "../../assets/core/shaders";

    /// The built-in shader file `file_name`, named the way the game names it.
    fn built_in(file_name: &str) -> ShaderSource {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join(BUILT_IN_SHADERS)
            .join(file_name);
        ShaderSource {
            name: format!("core:shaders/{file_name}"),
            text: fs::read_to_string(path).unwrap(),
        }
    }

    fn assert_is_spirv(words: &[u32]) {
        assert!(words.len() > 5, "too short for a SPIR-V module");
        assert_eq!(words[0], SPIRV_MAGIC);
    }

    /// The error text of compiling `source` for the chunk pipeline, which must fail.
    fn compile_error(source: &str, stage: ShaderStage) -> String {
        compile_error_for(source, stage, &CHUNK)
    }

    /// The error text of compiling `source` for the pipeline `interface` describes,
    /// which must fail.
    fn compile_error_for(
        source: &str,
        stage: ShaderStage,
        interface: &PipelineInterface,
    ) -> String {
        match compile_glsl(source, stage, interface, "pack.glsl") {
            Ok(_) => panic!("the shader was accepted"),
            Err(error) => error.to_string(),
        }
    }

    /// Whether `source` compiles for the pipeline `interface` describes.
    fn compiles_for(source: &str, stage: ShaderStage, interface: &PipelineInterface) -> bool {
        compile_glsl(source, stage, interface, "pack.glsl").is_ok()
    }

    /// The error text of linking two chunk shaders that each compile, which must fail.
    fn link_error(vertex: &str, fragment: &str) -> String {
        link_error_for(vertex, fragment, &CHUNK)
    }

    /// The error text of linking two shaders that each compile for `interface`, which must fail.
    fn link_error_for(vertex: &str, fragment: &str, interface: &PipelineInterface) -> String {
        let vertex = compile_glsl(vertex, ShaderStage::Vertex, interface, "pack.vert").unwrap();
        let fragment =
            compile_glsl(fragment, ShaderStage::Fragment, interface, "pack.frag").unwrap();
        link(&vertex, &fragment).unwrap_err().to_string()
    }

    /// Compiles and links the real shader files `name.vert` and `name.frag`.
    fn assert_real_shaders_compile(name: &str, interface: &PipelineInterface) {
        let vertex = built_in(&format!("{name}.vert"));
        let fragment = built_in(&format!("{name}.frag"));
        let shaders = load_pipeline_shaders(&vertex, &fragment, interface).unwrap();
        assert_is_spirv(&shaders.vertex);
        assert_is_spirv(&shaders.fragment);
    }

    #[test]
    fn real_shaders_compile_and_link() {
        assert_real_shaders_compile("chunk", &CHUNK);
        assert_real_shaders_compile("overlay", &OVERLAY);
    }

    #[test]
    fn chunk_shaders_do_not_fit_the_overlay_pipeline() {
        let result =
            load_pipeline_shaders(&built_in("chunk.vert"), &built_in("chunk.frag"), &OVERLAY);
        assert!(matches!(result, Err(RenderError::Shader(_))));
    }

    #[test]
    fn array_indices_buffers_and_image_loads_are_bounds_checked() {
        let policies = spirv_options().bounds_check_policies;
        assert_eq!(policies.index, BoundsCheckPolicy::Restrict);
        assert_eq!(policies.buffer, BoundsCheckPolicy::Restrict);
        assert_eq!(policies.image_load, BoundsCheckPolicy::Restrict);
    }

    #[test]
    fn broken_glsl_reports_the_shader_name() {
        let source = "#version 450\nvoid main() { this is not glsl }\n";
        let Err(error) = compile_glsl(source, ShaderStage::Fragment, &CHUNK, "broken.frag") else {
            panic!("broken GLSL was accepted");
        };
        assert!(matches!(&error, RenderError::Shader(text) if text.contains("broken.frag")));
    }

    #[test]
    fn a_broken_pipeline_shader_is_named_by_its_source_name() {
        let broken = ShaderSource {
            name: "my_pack:shaders/chunk.vert".to_owned(),
            text: "#version 450
void main() { broken }
"
            .to_owned(),
        };
        let Err(error) = load_pipeline_shaders(&broken, &built_in("chunk.frag"), &CHUNK) else {
            panic!("a broken shader was accepted");
        };
        let text = error.to_string();
        assert!(matches!(error, RenderError::Shader(_)));
        assert!(text.contains("my_pack:shaders/chunk.vert"), "{text}");
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
    fn push_constants_of_exactly_the_range_are_accepted() {
        // 80 bytes: the matrix and the chunk origin, like the built-in shader.
        let source = "#version 450
            layout(push_constant) uniform Draw { mat4 m; vec4 origin; } draw;
            void main() { gl_Position = draw.m * draw.origin; }";
        assert!(compiles_for(source, ShaderStage::Vertex, &CHUNK));
    }

    #[test]
    fn push_constants_one_vec4_past_the_range_are_rejected() {
        // 96 bytes: one `vec4` more than the pipeline provides.
        let source = "#version 450
            layout(push_constant) uniform Draw { mat4 m; vec4 origin; vec4 extra; } draw;
            void main() { gl_Position = draw.m * draw.origin + draw.extra; }";
        let text = compile_error(source, ShaderStage::Vertex);
        assert!(
            text.contains("96 bytes") && text.contains("80 bytes"),
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

    /// A chunk fragment shader sampling the block textures, with `declarations` replacing
    /// the two resource declarations of the documented interface.
    fn textured_fragment(declarations: &str) -> String {
        format!(
            "#version 450
            {declarations}
            layout(location = 0) out vec4 out_color;
            void main() {{
                out_color = texture(sampler2DArray(block_textures, block_sampler), vec3(0.5, 0.5, 1.0));
            }}"
        )
    }

    /// The declarations `crate::interface::chunk` documents.
    const BLOCK_TEXTURE_DECLARATIONS: &str = "
        layout(set = 0, binding = 0) uniform texture2DArray block_textures;
        layout(set = 0, binding = 1) uniform sampler block_sampler;";

    #[test]
    fn naga_accepts_a_separate_texture_array_and_sampler() {
        // Decision 0013 relies on this: naga parses, validates and writes SPIR-V for it.
        let source = textured_fragment(BLOCK_TEXTURE_DECLARATIONS);
        let compiled = compile_glsl(&source, ShaderStage::Fragment, &CHUNK, "pack.frag").unwrap();
        assert_is_spirv(&compiled.to_spirv().unwrap());
    }

    #[test]
    fn naga_rejects_a_combined_sampler2d_array_uniform() {
        // Pins the naga limitation behind the separate image and sampler (decision 0013).
        // If a naga upgrade starts accepting it, the interface may offer it too.
        let source = "#version 450
            layout(set = 0, binding = 0) uniform sampler2DArray block_textures;
            layout(location = 0) out vec4 out_color;
            void main() { out_color = texture(block_textures, vec3(0.5, 0.5, 1.0)); }";
        let options = glsl::Options::from(ShaderStage::Fragment);
        assert!(glsl::Frontend::default().parse(&options, source).is_err());
    }

    #[test]
    fn a_fragment_shader_may_declare_only_the_image() {
        let source = "#version 450
            layout(set = 0, binding = 0) uniform texture2DArray block_textures;
            layout(location = 0) out vec4 out_color;
            void main() { out_color = vec4(float(textureSize(block_textures, 0).z)); }";
        assert!(compiles_for(source, ShaderStage::Fragment, &CHUNK));
    }

    #[test]
    fn a_resource_at_an_unknown_binding_is_rejected() {
        let source = textured_fragment(
            "layout(set = 0, binding = 0) uniform texture2DArray block_textures;
             layout(set = 0, binding = 2) uniform sampler block_sampler;",
        );
        let text = compile_error(&source, ShaderStage::Fragment);
        assert!(
            text.contains("block_sampler") && text.contains("binding 2"),
            "{text}"
        );
        assert!(text.contains("set 0, binding 1 (`sampler`"), "{text}");
    }

    #[test]
    fn a_resource_in_another_set_is_rejected() {
        let source = textured_fragment(
            "layout(set = 1, binding = 0) uniform texture2DArray block_textures;
             layout(set = 0, binding = 1) uniform sampler block_sampler;",
        );
        let text = compile_error(&source, ShaderStage::Fragment);
        assert!(text.contains("set 1, binding 0"), "{text}");
    }

    #[test]
    fn swapped_image_and_sampler_bindings_are_rejected() {
        let source = textured_fragment(
            "layout(set = 0, binding = 1) uniform texture2DArray block_textures;
             layout(set = 0, binding = 0) uniform sampler block_sampler;",
        );
        let text = compile_error(&source, ShaderStage::Fragment);
        assert!(text.contains("must be declared as `"), "{text}");
    }

    #[test]
    fn a_texture_of_the_wrong_type_is_rejected() {
        // A plain 2D texture where the array is bound.
        let source = "#version 450
            layout(set = 0, binding = 0) uniform texture2D block_textures;
            layout(set = 0, binding = 1) uniform sampler block_sampler;
            layout(location = 0) out vec4 out_color;
            void main() { out_color = texture(sampler2D(block_textures, block_sampler), vec2(0.5)); }";
        let text = compile_error(source, ShaderStage::Fragment);
        assert!(text.contains("`texture2DArray`"), "{text}");
    }

    #[test]
    fn an_integer_texture_array_is_rejected() {
        // The image is UNORM: an integer view of it would be invalid.
        let source = "#version 450
            layout(set = 0, binding = 0) uniform utexture2DArray block_textures;
            layout(location = 0) out vec4 out_color;
            void main() { out_color = vec4(vec3(textureSize(block_textures, 0)), 1.0); }";
        let text = compile_error(source, ShaderStage::Fragment);
        assert!(text.contains("`texture2DArray`"), "{text}");
    }

    #[test]
    fn a_storage_buffer_is_rejected() {
        let source = "#version 450
            layout(set = 0, binding = 0) buffer Data { vec4 colors[]; } data;
            layout(location = 0) out vec4 out_color;
            void main() { out_color = data.colors[0]; }";
        let text = compile_error(source, ShaderStage::Fragment);
        assert!(
            text.contains("descriptor") && text.contains("`data`"),
            "{text}"
        );
    }

    #[test]
    fn a_uniform_buffer_is_rejected() {
        let source = "#version 450
            layout(set = 0, binding = 0) uniform Data { vec4 color; } data;
            layout(location = 0) out vec4 out_color;
            void main() { out_color = data.color; }";
        let text = compile_error(source, ShaderStage::Fragment);
        assert!(text.contains("descriptor"), "{text}");
    }

    #[test]
    fn textures_in_the_chunk_vertex_shader_are_rejected() {
        let source = "#version 450
            layout(set = 0, binding = 0) uniform texture2DArray block_textures;
            layout(set = 0, binding = 1) uniform sampler block_sampler;
            void main() {
                gl_Position = textureLod(sampler2DArray(block_textures, block_sampler), vec3(0.0), 0.0);
            }";
        let text = compile_error(source, ShaderStage::Vertex);
        assert!(
            text.contains("only available in the fragment stage"),
            "{text}"
        );
    }

    #[test]
    fn textures_in_the_overlay_shaders_are_rejected() {
        let fragment = textured_fragment(BLOCK_TEXTURE_DECLARATIONS);
        let text = compile_error_for(&fragment, ShaderStage::Fragment, &OVERLAY);
        assert!(text.contains("no descriptor resources"), "{text}");
        let vertex = "#version 450
            layout(set = 0, binding = 1) uniform sampler block_sampler;
            void main() { gl_Position = vec4(0.0); }";
        let text = compile_error_for(vertex, ShaderStage::Vertex, &OVERLAY);
        assert!(text.contains("no descriptor resources"), "{text}");
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

    #[test]
    fn overlay_vertex_shader_with_its_inputs_and_window_size_is_accepted() {
        let source = "#version 450
            layout(push_constant) uniform Screen { vec2 size; } screen;
            layout(location = 0) in vec2 position;
            layout(location = 1) in vec4 color;
            layout(location = 0) out vec4 v_color;
            void main() { v_color = color; gl_Position = vec4(position / screen.size, 0.0, 1.0); }";
        assert!(compiles_for(source, ShaderStage::Vertex, &OVERLAY));
    }

    #[test]
    fn overlay_unknown_vertex_input_location_is_rejected() {
        let source = "#version 450
            layout(location = 2) in vec2 extra;
            void main() { gl_Position = vec4(extra, 0.0, 1.0); }";
        let text = compile_error_for(source, ShaderStage::Vertex, &OVERLAY);
        assert!(
            text.contains("location 2") && text.contains("0, 1"),
            "{text}"
        );
    }

    #[test]
    fn overlay_color_as_integers_is_rejected() {
        // The color arrives normalized, as floats; the chunk pipeline's `uvec4` is wrong here.
        let source = "#version 450
            layout(location = 1) in uvec4 color;
            void main() { gl_Position = vec4(color); }";
        let text = compile_error_for(source, ShaderStage::Vertex, &OVERLAY);
        assert!(
            text.contains("location 1") && text.contains("`vec4`"),
            "{text}"
        );
    }

    #[test]
    fn overlay_position_as_vec3_is_rejected() {
        let source = "#version 450
            layout(location = 0) in vec3 position;
            void main() { gl_Position = vec4(position, 1.0); }";
        let text = compile_error_for(source, ShaderStage::Vertex, &OVERLAY);
        assert!(
            text.contains("location 0") && text.contains("`vec2`"),
            "{text}"
        );
    }

    #[test]
    fn overlay_push_constants_past_the_window_size_are_rejected() {
        // 16 bytes: the chunk pipeline would accept this, the overlay has only 8.
        let source = "#version 450
            layout(push_constant) uniform Screen { vec4 size; } screen;
            void main() { gl_Position = screen.size; }";
        assert!(compiles_for(source, ShaderStage::Vertex, &CHUNK));
        let text = compile_error_for(source, ShaderStage::Vertex, &OVERLAY);
        assert!(
            text.contains("16 bytes") && text.contains("8 bytes"),
            "{text}"
        );
    }

    #[test]
    fn overlay_push_constants_in_the_fragment_stage_are_rejected() {
        let source = "#version 450
            layout(push_constant) uniform Screen { vec2 size; } screen;
            layout(location = 0) out vec4 out_color;
            void main() { out_color = vec4(screen.size, 0.0, 1.0); }";
        let text = compile_error_for(source, ShaderStage::Fragment, &OVERLAY);
        assert!(text.contains("vertex stage"), "{text}");
    }

    #[test]
    fn overlay_fragment_input_without_vertex_output_is_rejected() {
        let vertex = "#version 450
            layout(location = 0) in vec2 position;
            layout(location = 1) in vec4 color;
            layout(location = 0) out vec4 v_color;
            void main() { v_color = color; gl_Position = vec4(position, 0.0, 1.0); }";
        let fragment = "#version 450
            layout(location = 1) in vec4 v_tint;
            layout(location = 0) out vec4 out_color;
            void main() { out_color = v_tint; }";
        let text = link_error_for(vertex, fragment, &OVERLAY);
        assert!(
            text.contains("pack.frag") && text.contains("location 1"),
            "{text}"
        );
    }

    /// The `vec3` entries of the GLSL array constant `name` in `source`, one per face, in
    /// the order of `Face::ALL`. Expects the plain layout of the built-in shader:
    /// `const vec3 NAME[6] = vec3[6](vec3(a, b, c), ...);`.
    fn vec3_table(source: &str, name: &str) -> Vec<[f32; 3]> {
        let start = source
            .find(&format!("const vec3 {name}["))
            .unwrap_or_else(|| panic!("{name} is missing"));
        let body = &source[start..];
        let body = &body[body.find('(').unwrap() + 1..body.find(");").unwrap()];
        body.split("vec3(")
            .skip(1)
            .map(|entry| {
                let numbers: Vec<f32> = entry
                    .split(')')
                    .next()
                    .unwrap()
                    .split(',')
                    .map(|n| n.trim().parse().unwrap())
                    .collect();
                [numbers[0], numbers[1], numbers[2]]
            })
            .collect()
    }

    /// The number of entries of the GLSL `float` array constant `name` in `source`, written
    /// as `const float NAME[6] = float[6](a, b, ...);`.
    fn float_count(source: &str, name: &str) -> usize {
        let start = source
            .find(&format!("const float {name}["))
            .unwrap_or_else(|| panic!("{name} is missing"));
        let body = &source[start..];
        let body = &body[body.find('(').unwrap() + 1..body.find(");").unwrap()];
        body.split(',').count()
    }

    fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
        [
            a[1] * b[2] - a[2] * b[1],
            a[2] * b[0] - a[0] * b[2],
            a[0] * b[1] - a[1] * b[0],
        ]
    }

    /// The texture axes of the built-in `chunk.vert` (decision 0013, section "Textures"):
    /// side textures stand upright, and no face shows its texture mirrored from outside.
    #[test]
    fn built_in_chunk_uvs_are_upright_and_not_mirrored() {
        use vox_world::Face;

        let source = built_in("chunk.vert").text;
        assert_eq!(float_count(&source, "FACE_SHADE"), Face::COUNT);
        let u_axes = vec3_table(&source, "FACE_U");
        let v_axes = vec3_table(&source, "FACE_V");
        assert_eq!((u_axes.len(), v_axes.len()), (Face::COUNT, Face::COUNT));
        let down = [0.0, -1.0, 0.0];
        for face in Face::ALL {
            let index = usize::from(face.index());
            let (u, v) = (u_axes[index], v_axes[index]);
            let normal = face.offset().map(f32::from);
            // Picture right x picture down points into the face, away from the viewer
            // outside: the same handedness as the screen, so nothing is mirrored.
            assert_eq!(cross(u, v), normal.map(|n| -n), "{face:?} is mirrored");
            if normal[1] == 0.0 {
                assert_eq!(v, down, "{face:?} is not upright");
            }
        }
    }
}
