//! The overlay on the GPU: one buffer with the vertices of every rectangle.
//!
//! The vertices are serialized by `crate::overlay`; this module only uploads them.

use ash::vk;

use super::buffer::Buffer;
use super::gpu::Gpu;
use crate::RenderError;
use crate::overlay::{self, OverlayRect};

/// A batch of overlay rectangles in one host-visible vertex buffer, drawn with one
/// non-indexed draw of [`Self::vertex_count`] vertices.
pub struct GpuOverlay {
    buffer: Buffer,
    vertex_count: u32,
}

impl GpuOverlay {
    /// Uploads `rects`. Returns `None` if none of them covers any area, which leaves
    /// nothing to draw.
    pub fn new(gpu: &Gpu, rects: &[OverlayRect]) -> Result<Option<Self>, RenderError> {
        let vertices = overlay::overlay_vertices(rects)?;
        if vertices.count == 0 {
            return Ok(None);
        }
        let buffer = Buffer::with_data(gpu, vk::BufferUsageFlags::VERTEX_BUFFER, &vertices.bytes)?;
        Ok(Some(Self {
            buffer,
            vertex_count: vertices.count,
        }))
    }

    /// The vertex buffer; bind it at offset 0. It holds exactly [`Self::vertex_count`]
    /// vertices.
    pub fn buffer(&self) -> vk::Buffer {
        self.buffer.handle()
    }

    /// How many vertices to draw: six per rectangle.
    pub fn vertex_count(&self) -> u32 {
        self.vertex_count
    }

    /// Gives up the overlay, keeping only its buffer, so it can be retired like a mesh.
    pub fn into_buffer(self) -> Buffer {
        self.buffer
    }
}
