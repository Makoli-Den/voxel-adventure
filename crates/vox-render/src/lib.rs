//! Rendering layer.
//!
//! This is the only crate that knows about the graphics API. The rest of the
//! game talks to it through the [`Renderer`] trait, so the backend can be
//! replaced (for example by a fallback for old devices) without touching other layers.

mod error;
mod frustum;
mod interface;
mod mipmap;
mod overlay;
mod shader;
mod stats;
mod texture;
mod vulkan;

use std::sync::Arc;

use raw_window_handle::{HasDisplayHandle, HasWindowHandle};

pub use error::RenderError;
pub use overlay::OverlayRect;
pub use shader::{ShaderSource, ShaderSources};
pub use stats::RenderStats;
pub use texture::TextureLayers;

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
    /// After an error the renderer must not be used again: drop it. A frame that failed
    /// halfway may leave a fence that is never signaled, so the next frame would wait for
    /// it forever, and the freeing of replaced meshes is only proven safe for frames that
    /// were fully submitted. Dropping is always safe: it waits for the GPU first.
    fn render_frame(&mut self) -> Result<(), RenderError>;

    /// Human-readable name of the GPU in use.
    fn device_name(&self) -> &str;

    /// Adds or replaces the mesh of the chunk at `pos`. An empty mesh (no indices) removes the entry.
    ///
    /// Every chunk that has a mesh is drawn each frame unless its bounding box lies entirely
    /// outside the camera's view (frustum culling). Mesh coordinates are chunk-local
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

    /// Replaces the overlay. The rectangles are drawn on top of everything else, in slice
    /// order (later ones over earlier ones), blended by their alpha. An empty slice removes
    /// the overlay.
    ///
    /// Coordinates are window pixels from the top-left corner of the drawing area, see
    /// [`OverlayRect`]. Every frame maps them with the current swapchain size, so after a
    /// resize the rectangles keep their size in pixels and nothing needs to be set again.
    /// Rectangles with a width or height of zero or less are skipped. The previous
    /// overlay's GPU memory is freed a few frames later, once no frame in flight can use
    /// it, so this never waits for the GPU; it is cheap enough to call several times a
    /// second. The overlay is not counted in [`RenderStats`].
    ///
    /// If the window's image format cannot blend (Vulkan guarantees blending for the
    /// common 8-bit formats the renderer prefers, so this is rare), the overlay is drawn
    /// opaque and alpha is ignored: even a rectangle with alpha 0 is drawn solid.
    ///
    /// # Errors
    /// Returns [`RenderError::InvalidOverlay`] if a coordinate or size is NaN or infinite,
    /// if there are too many rectangles (more than `u32::MAX / 6`, the vertex count a
    /// single draw takes, checked before anything is allocated), or if their vertices do
    /// not fit in memory; the whole batch is refused. Another error is returned if the GPU
    /// cannot allocate or fill the buffer. The previous overlay is kept in every case.
    fn set_overlay(&mut self, rects: &[OverlayRect]) -> Result<(), RenderError>;

    /// Replaces the block textures the chunk shaders sample: a 2D array, one layer per
    /// texture, repeated across each face and filtered with `NEAREST` (sharp texels up
    /// close). The caller passes the full-size pictures only; the renderer builds their mip
    /// chain (halved copies down to 1x1) and blends between levels, so distant faces show
    /// averaged colours instead of shimmering.
    ///
    /// Until this is called, a single-layer magenta and black checker is bound, so drawing
    /// is always valid. The pixels are copied to the GPU before this returns, so the caller
    /// may drop them afterwards.
    ///
    /// **Waits for the GPU:** the upload waits for its own copy to finish, then for the whole
    /// device to go idle before the new texture replaces the old one, because frames in
    /// flight may still sample the old one. Meant for startup and pack reloads, never for
    /// every frame.
    ///
    /// # Errors
    /// Returns [`RenderError::InvalidTextures`] if the size is not a power of two or above
    /// [`TextureLayers::MAX_SIZE`], the layer count is zero or above
    /// [`TextureLayers::MAX_LAYERS`] or the device's limit, or the pixel slice does not have
    /// exactly the expected length; nothing is allocated then. Also if the mip chain (a
    /// third more than the pixels) does not fit into memory. Another error is returned if
    /// the GPU cannot allocate or upload the image, or if waiting for the GPU to go idle
    /// before the swap fails (the new image is then freed and the old one stays bound). The
    /// previous textures are kept in every case.
    fn set_block_textures(&mut self, textures: &TextureLayers<'_>) -> Result<(), RenderError>;

    /// What the renderer drew and how long the GPU took. Cheap: returns a copy.
    ///
    /// The overlay ([`Self::set_overlay`]) is not part of the counts.
    ///
    /// The draw counts describe the most recently recorded frame: the last call to
    /// [`Self::render_frame`] that built GPU commands, whether or not presenting it
    /// succeeded. Calls that draw nothing because the window is minimized or its
    /// swapchain is being rebuilt do not change them.
    ///
    /// The GPU time belongs to the most recent finished frame: the newest frame that the
    /// renderer has proven finished (by waiting for it before reusing its per-frame
    /// resources) and whose timing it has read. That is usually a frame or two older than
    /// the one the counts describe. It is `None` until the first such frame, and forever
    /// on a device whose queue cannot write timestamps.
    fn stats(&self) -> RenderStats;
}

/// Creates the default renderer (Vulkan) that draws into `window`.
///
/// The renderer keeps the `Arc` alive for as long as it exists, which guarantees
/// the native window outlives the surface created from it.
///
/// `shaders` holds the GLSL text of every pipeline's shaders; the renderer reads no files,
/// so the caller decides where they come from (the game resolves them through its resource
/// packs). They are compiled and checked once, here.
///
/// # Errors
/// Returns an error if Vulkan is unavailable, no GPU can present to the window,
/// or a shader does not compile or does not fit its pipeline ([`RenderError::Shader`],
/// naming the shader by [`ShaderSource::name`]).
pub fn create_renderer<W>(
    window: Arc<W>,
    width: u32,
    height: u32,
    shaders: &ShaderSources,
) -> Result<Box<dyn Renderer>, RenderError>
where
    W: HasDisplayHandle + HasWindowHandle + Send + Sync + 'static,
{
    let renderer = vulkan::VulkanRenderer::new(window, width, height, shaders)?;
    Ok(Box::new(renderer))
}
