//! Rendering layer.
//!
//! This is the only crate that knows about the graphics API. The rest of the
//! game talks to it through the [`Renderer`] trait, so the backend can be
//! replaced (for example by a fallback for old devices) without touching other layers.

mod error;
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
}

/// Creates the default renderer (Vulkan) that draws into `window`.
///
/// The renderer keeps the `Arc` alive for as long as it exists, which guarantees
/// the native window outlives the surface created from it.
///
/// `shader_dir` is the folder with the GLSL shader files (`triangle.vert`,
/// `triangle.frag`). They are read and compiled once, here.
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
