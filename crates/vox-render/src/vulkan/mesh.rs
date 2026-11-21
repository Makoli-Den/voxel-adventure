//! Chunk meshes on the GPU: the vertex layout and the buffers that hold one mesh.
//!
//! # Vertex layout
//! This is the one place that defines how a [`ChunkVertex`] looks to the GPU.
//! Each vertex is 6 bytes, little-endian, in binding 0:
//!
//! | byte | content                 | read by attribute                 |
//! |------|-------------------------|-----------------------------------|
//! | 0..3 | position x, y, z (`u8`) | 0, `R8G8B8A8_UINT`, as `xyz`      |
//! | 3    | face index (`u8`)       | 0, `R8G8B8A8_UINT`, as `w`        |
//! | 4..6 | block id (`u16`)        | 1, `R16_UINT`                     |
//!
//! Attribute 0 reads four bytes at once because `R8G8B8_UINT` is not a guaranteed
//! vertex format in Vulkan 1.0, while `R8G8B8A8_UINT` is. The fourth byte happens to
//! be the face index, so nothing is wasted. Indices are `u32`.

use ash::vk;
use vox_world::{ChunkMesh, ChunkVertex};

use super::buffer::Buffer;
use super::gpu::Gpu;
use crate::RenderError;

/// Bytes per vertex in the vertex buffer. Equal to `size_of::<ChunkVertex>()`.
const VERTEX_STRIDE: usize = 6;

/// The vertex buffer binding: one tightly packed vertex after another.
pub fn vertex_bindings() -> [vk::VertexInputBindingDescription; 1] {
    [vk::VertexInputBindingDescription {
        binding: 0,
        stride: VERTEX_STRIDE as u32,
        input_rate: vk::VertexInputRate::VERTEX,
    }]
}

/// The vertex attributes, matching the layout table in the module docs.
pub fn vertex_attributes() -> [vk::VertexInputAttributeDescription; 2] {
    [
        vk::VertexInputAttributeDescription {
            location: 0,
            binding: 0,
            format: vk::Format::R8G8B8A8_UINT,
            offset: 0,
        },
        vk::VertexInputAttributeDescription {
            location: 1,
            binding: 0,
            format: vk::Format::R16_UINT,
            offset: 4,
        },
    ]
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
            return Err(RenderError::Unsupported(
                "a mesh index points past the last vertex",
            ));
        }
        let index_count = u32::try_from(mesh.indices.len())
            .map_err(|_| RenderError::Unsupported("a mesh with more than 2^32 indices"))?;
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

/// Serializes vertices into the layout described in the module docs.
fn vertex_bytes(vertices: &[ChunkVertex]) -> Vec<u8> {
    // HOT PATH: runs for every mesh upload. One allocation of the exact size, then a
    // linear write; explicit bytes keep it free of `unsafe` and independent of how
    // the compiler lays out `ChunkVertex`.
    let mut bytes = Vec::with_capacity(vertices.len() * VERTEX_STRIDE);
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
        assert_eq!(VERTEX_STRIDE, size_of::<ChunkVertex>());
        assert_eq!(vertex_bytes(&[sample(), sample()]).len(), 2 * VERTEX_STRIDE);
    }

    #[test]
    fn vertex_bytes_are_position_face_block_little_endian() {
        assert_eq!(vertex_bytes(&[sample()]), [1, 2, 32, 5, 0x02, 0x03]);
    }

    #[test]
    fn attributes_cover_the_whole_stride() {
        let [position_face, block] = vertex_attributes();
        assert_eq!(position_face.offset, 0);
        // R8G8B8A8 reads four bytes, R16 two: together exactly one vertex.
        assert_eq!(block.offset, 4);
        assert_eq!(
            vertex_bindings()[0].stride as usize,
            block.offset as usize + 2
        );
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
