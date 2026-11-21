//! Rendering layer.
//!
//! This is the only crate that knows about the graphics API. The rest of the
//! game talks to it through the [`Renderer`] trait, so the backend can be
//! replaced (for example by a fallback for old devices) without touching other layers.

mod error;
mod interface;
mod shader;
mod vulkan;

use std::path::Path;
use std::sync::Arc;

use raw_window_handle::{HasDisplayHandle, HasWindowHandle};

pub use error::RenderError;

/// What the game needs from a renderer.
pub trait Renderer {
    /// Tells the renderer that the window changed size, in physical pixels.
    ///
    /// Cheap to call: the swapchain is rebuilt lazily on the next frame.
    /// A size of zero (minimized window) pauses drawing.
    fn resize(&mut self, width: u32, height: u32);

    /// Draws and presents one frame.
    ///
    /// # Errors
    /// Returns an error if the GPU or the window surface fails irrecoverably.
    /// After an error the renderer must not be used again: drop it.
    fn render_frame(&mut self) -> Result<(), RenderError>;

    /// Human-readable name of the GPU in use.
    fn device_name(&self) -> &str;

    /// Adds or replaces the mesh of the chunk at `pos`. An empty mesh (no indices) removes the entry.
    ///
    /// Every chunk that has a mesh is drawn each frame. Mesh coordinates are chunk-local
    /// block units; the renderer places the chunk at `pos.origin()`. A replaced mesh's GPU
    /// memory is freed a few frames later, once no frame in flight can use it, so this
    /// never waits for the GPU.
    ///
    /// # Errors
    /// Returns [`RenderError::InvalidMesh`] if the data is malformed (an index points
    /// past the last vertex), or another error if the GPU cannot allocate or fill the
    /// buffer. The chunk's previous mesh is then kept.
    fn set_chunk_mesh(
        &mut self,
        pos: vox_world::ChunkPos,
        mesh: &vox_world::ChunkMesh,
    ) -> Result<(), RenderError>;

    /// Removes the mesh of the chunk at `pos`, if there is one. Does nothing otherwise.
    fn remove_chunk_mesh(&mut self, pos: vox_world::ChunkPos);

    /// Sets the combined view-projection matrix, column-major, exactly `glam::Mat4::to_cols_array_2d()`.
    ///
    /// The matrix must already produce Vulkan clip space (Y down, depth 0..1); the
    /// renderer applies it unchanged.
    fn set_camera(&mut self, view_projection: [[f32; 4]; 4]);
}

/// Creates the default renderer (Vulkan) that draws into `window`.
///
/// The renderer keeps the `Arc` alive for as long as it exists, which guarantees
/// the native window outlives the surface created from it.
///
/// `shader_dir` is the folder with the GLSL shader files (`chunk.vert`,
/// `chunk.frag`). They are read and compiled once, here.
///
/// # Errors
/// Returns an error if Vulkan is unavailable, no GPU can present to the window,
/// or a shader file is missing or does not compile ([`RenderError::Shader`]).
pub fn create_renderer<W>(
    window: Arc<W>,
    width: u32,
    height: u32,
    shader_dir: &Path,
) -> Result<Box<dyn Renderer>, RenderError>
where
    W: HasDisplayHandle + HasWindowHandle + Send + Sync + 'static,
{
    let renderer = vulkan::VulkanRenderer::new(window, width, height, shader_dir)?;
    Ok(Box::new(renderer))
}
