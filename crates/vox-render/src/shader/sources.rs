//! The GLSL text of the shaders, as the caller hands it to the renderer.
//!
//! The renderer does not read files: the game resolves the shaders through its resource
//! packs (decision 0013), so a shader pack replaces them without the renderer knowing.

/// The GLSL text of one shader stage and the name that labels it in error messages.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShaderSource {
    /// Names the shader in error messages, normally its resource id
    /// (`core:shaders/chunk.vert`), so a broken pack shader can be found.
    pub name: String,
    /// The GLSL 450 source, Vulkan style.
    pub text: String,
}

/// The shaders of every pipeline the renderer builds, passed to [`crate::create_renderer`].
///
/// Each is compiled and checked against what its pipeline provides once, at startup.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShaderSources {
    /// Vertex stage of the terrain (chunk mesh) pipeline.
    pub chunk_vertex: ShaderSource,
    /// Fragment stage of the terrain (chunk mesh) pipeline.
    pub chunk_fragment: ShaderSource,
    /// Vertex stage of the overlay (screen rectangles) pipeline.
    pub overlay_vertex: ShaderSource,
    /// Fragment stage of the overlay (screen rectangles) pipeline.
    pub overlay_fragment: ShaderSource,
}
