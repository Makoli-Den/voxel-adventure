//! Chunk meshes on the GPU: the vertex input state and the buffers that hold one mesh.
//!
//! The byte layout of a vertex is defined once, in `crate::interface`; this module
//! serializes [`ChunkVertex`] into it and turns it into Vulkan vertex input state.
//! Indices are little-endian `u32`.

use ash::vk;
use vox_world::{ChunkMesh, ChunkVertex};

use super::buffer::Buffer;
use super::gpu::Gpu;
use crate::RenderError;
use crate::interface::{AttributeFormat, VERTEX_ATTRIBUTES, VERTEX_STRIDE};

/// The vertex buffer binding: one tightly packed vertex after another.
pub fn vertex_bindings() -> [vk::VertexInputBindingDescription; 1] {
    [vk::VertexInputBindingDescription {
        binding: 0,
        stride: VERTEX_STRIDE,
        input_rate: vk::VertexInputRate::VERTEX,
    }]
}

/// The vertex attributes, built from [`VERTEX_ATTRIBUTES`].
pub fn vertex_attributes() -> [vk::VertexInputAttributeDescription; VERTEX_ATTRIBUTES.len()] {
    VERTEX_ATTRIBUTES.map(|attribute| vk::VertexInputAttributeDescription {
        location: attribute.location,
        binding: 0,
        format: vk_format(attribute.format),
        offset: attribute.offset,
    })
}

/// The Vulkan format of an attribute. Both are required vertex formats in Vulkan 1.0;
/// `R8G8B8_UINT` is not, which is why the position is read as four bytes.
fn vk_format(format: AttributeFormat) -> vk::Format {
    match format {
        AttributeFormat::Uint8x4 => vk::Format::R8G8B8A8_UINT,
        AttributeFormat::Uint16 => vk::Format::R16_UINT,
    }
}

/// One chunk mesh in GPU buffers, ready to draw.
pub struct GpuMesh {
    vertices: Buffer,
    indices: Buffer,
    index_count: u32,
}

impl GpuMesh {
    /// Uploads `mesh`. Returns `None` for a mesh without indices, which has nothing to draw.
    pub fn new(gpu: &Gpu, mesh: &ChunkMesh) -> Result<Option<Self>, RenderError> {
        if mesh.indices.is_empty() {
            return Ok(None);
        }
        // The device has no `robustBufferAccess`, so an index past the last vertex would
        // make the GPU read outside the vertex buffer. `ChunkMesh` is plain data that any
        // caller can build, so this is checked here and not trusted.
        if !indices_in_range(&mesh.indices, mesh.vertices.len()) {
            return Err(RenderError::InvalidMesh(
                "an index points past the last vertex",
            ));
        }
        let index_count = u32::try_from(mesh.indices.len())
            .map_err(|_| RenderError::InvalidMesh("more than 2^32 indices"))?;
        let vertices = Buffer::with_data(
            gpu,
            vk::BufferUsageFlags::VERTEX_BUFFER,
            &vertex_bytes(&mesh.vertices),
        )?;
        let indices = Buffer::with_data(
            gpu,
            vk::BufferUsageFlags::INDEX_BUFFER,
            &index_bytes(&mesh.indices),
        )?;
        Ok(Some(Self {
            vertices,
            indices,
            index_count,
        }))
    }

    /// The vertex buffer, for binding 0.
    pub fn vertex_buffer(&self) -> vk::Buffer {
        self.vertices.handle()
    }

    /// The index buffer, `u32` indices.
    pub fn index_buffer(&self) -> vk::Buffer {
        self.indices.handle()
    }

    /// How many indices to draw.
    pub fn index_count(&self) -> u32 {
        self.index_count
    }
}

/// Whether every index refers to one of `vertex_count` vertices.
fn indices_in_range(indices: &[u32], vertex_count: usize) -> bool {
    // HOT PATH: runs for every mesh upload; one linear pass, cheap next to the copy.
    indices.iter().all(|&index| (index as usize) < vertex_count)
}

/// Serializes vertices into the layout described in `crate::interface`.
fn vertex_bytes(vertices: &[ChunkVertex]) -> Vec<u8> {
    // HOT PATH: runs for every mesh upload. One allocation of the exact size, then a
    // linear write; explicit bytes keep it free of `unsafe` and independent of how
    // the compiler lays out `ChunkVertex`.
    let mut bytes = Vec::with_capacity(vertices.len() * VERTEX_STRIDE as usize);
    for vertex in vertices {
        let [x, y, z] = vertex.position;
        let [block_lo, block_hi] = vertex.block.0.to_le_bytes();
        bytes.extend_from_slice(&[x, y, z, vertex.face, block_lo, block_hi]);
    }
    bytes
}

/// Serializes indices as little-endian `u32`, the order every supported GPU reads.
fn index_bytes(indices: &[u32]) -> Vec<u8> {
    // HOT PATH: runs for every mesh upload; one allocation, linear write.
    let mut bytes = Vec::with_capacity(indices.len() * 4);
    for index in indices {
        bytes.extend_from_slice(&index.to_le_bytes());
    }
    bytes
}

#[cfg(test)]
mod tests {
    use vox_world::BlockId;

    use super::*;

    fn sample() -> ChunkVertex {
        ChunkVertex {
            position: [1, 2, 32],
            face: 5,
            block: BlockId(0x0302),
        }
    }

    #[test]
    fn stride_matches_the_vertex_size() {
        let stride = VERTEX_STRIDE as usize;
        assert_eq!(stride, size_of::<ChunkVertex>());
        assert_eq!(vertex_bytes(&[sample(), sample()]).len(), 2 * stride);
    }

    #[test]
    fn vertex_bytes_are_position_face_block_little_endian() {
        assert_eq!(vertex_bytes(&[sample()]), [1, 2, 32, 5, 0x02, 0x03]);
    }

    #[test]
    fn vulkan_attributes_follow_the_interface() {
        let attributes = vertex_attributes();
        assert_eq!(attributes[0].format, vk::Format::R8G8B8A8_UINT);
        assert_eq!(attributes[1].format, vk::Format::R16_UINT);
        assert_eq!((attributes[1].location, attributes[1].offset), (1, 4));
    }

    #[test]
    fn indices_are_little_endian_u32() {
        assert_eq!(index_bytes(&[1, 0x0102_0304]), [1, 0, 0, 0, 4, 3, 2, 1]);
    }

    #[test]
    fn indices_must_stay_below_the_vertex_count() {
        assert!(indices_in_range(&[0, 1, 2], 3));
        assert!(!indices_in_range(&[0, 1, 3], 3));
        assert!(!indices_in_range(&[0], 0));
    }

    #[test]
    fn empty_input_gives_no_bytes() {
        assert!(vertex_bytes(&[]).is_empty());
        assert!(index_bytes(&[]).is_empty());
    }
}
