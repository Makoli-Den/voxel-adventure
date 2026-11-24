//! Chunk meshes on the GPU: the buffer that holds one mesh.
//!
//! The byte layout of a vertex is defined once, in `crate::interface::chunk`; this
//! module serializes [`ChunkVertex`] into it. Indices are little-endian `u32` and follow
//! the vertices in the same buffer.

use ash::vk;
use vox_world::{ChunkMesh, ChunkVertex};

use super::buffer::Buffer;
use super::gpu::Gpu;
use crate::RenderError;
use crate::interface::chunk::VERTEX_STRIDE;

/// One chunk mesh in a single GPU buffer, ready to draw: the vertices first, then the
/// `u32` indices from [`Self::index_offset`] on.
///
/// One buffer instead of two halves the device memory allocations per chunk; Vulkan
/// guarantees only 4096 of them in total (`maxMemoryAllocationCount`).
pub struct GpuMesh {
    buffer: Buffer,
    index_offset: vk::DeviceSize,
    index_count: u32,
}

impl GpuMesh {
    /// Uploads `mesh`. Returns `None` for a mesh without indices, which has nothing to draw.
    pub fn new(gpu: &Gpu, mesh: &ChunkMesh) -> Result<Option<Self>, RenderError> {
        if mesh.indices.is_empty() {
            return Ok(None);
        }
        // The device has no `robustBufferAccess`, so an index past the last vertex would
        // make the GPU read outside the vertex data. `ChunkMesh` is plain data that any
        // caller can build, so this is checked here and not trusted.
        if !indices_in_range(&mesh.indices, mesh.vertices.len()) {
            return Err(RenderError::InvalidMesh(
                "an index points past the last vertex",
            ));
        }
        let index_count = u32::try_from(mesh.indices.len())
            .map_err(|_| RenderError::InvalidMesh("more than 2^32 indices"))?;
        let layout = MeshLayout::new(mesh.vertices.len(), mesh.indices.len())
            .ok_or(RenderError::InvalidMesh("the mesh does not fit in memory"))?;
        let usage = vk::BufferUsageFlags::VERTEX_BUFFER | vk::BufferUsageFlags::INDEX_BUFFER;
        let buffer = Buffer::with_data(gpu, usage, &mesh_bytes(mesh, layout))?;
        Ok(Some(Self {
            buffer,
            // Lossless: `usize` is at most 64 bits on every supported target.
            index_offset: layout.index_offset as vk::DeviceSize,
            index_count,
        }))
    }

    /// The buffer holding both parts: bind it at offset 0 as the vertex buffer and at
    /// [`Self::index_offset`] as the index buffer.
    pub fn buffer(&self) -> vk::Buffer {
        self.buffer.handle()
    }

    /// Where the `u32` indices start in the buffer; a multiple of 4, as Vulkan requires.
    pub fn index_offset(&self) -> vk::DeviceSize {
        self.index_offset
    }

    /// How many indices to draw.
    pub fn index_count(&self) -> u32 {
        self.index_count
    }

    /// Gives up the mesh, keeping only its buffer, so it can be retired with the others.
    pub fn into_buffer(self) -> Buffer {
        self.buffer
    }
}

/// Where the parts of a mesh lie in its buffer, in bytes. The vertices start at 0.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct MeshLayout {
    /// Start of the indices: the end of the vertices rounded up to a multiple of 4,
    /// because `vkCmdBindIndexBuffer` needs an offset aligned to the index size.
    index_offset: usize,
    /// Size of the whole buffer.
    size: usize,
}

impl MeshLayout {
    /// The layout for `vertex_count` vertices and `index_count` `u32` indices, or `None`
    /// if the size overflows `usize`.
    fn new(vertex_count: usize, index_count: usize) -> Option<Self> {
        let vertex_size = vertex_count.checked_mul(VERTEX_STRIDE as usize)?;
        let index_offset = vertex_size.checked_next_multiple_of(INDEX_SIZE)?;
        let size = index_offset.checked_add(index_count.checked_mul(INDEX_SIZE)?)?;
        Some(Self { index_offset, size })
    }
}

/// Bytes per index (`u32`).
const INDEX_SIZE: usize = size_of::<u32>();

/// Serializes `mesh` into the buffer layout: vertices, zero padding, indices.
fn mesh_bytes(mesh: &ChunkMesh, layout: MeshLayout) -> Vec<u8> {
    // HOT PATH: runs for every mesh upload. One allocation of the exact size, then a
    // linear write.
    let mut bytes = Vec::with_capacity(layout.size);
    write_vertices(&mut bytes, &mesh.vertices);
    bytes.resize(layout.index_offset, 0);
    write_indices(&mut bytes, &mesh.indices);
    bytes
}

/// Whether every index refers to one of `vertex_count` vertices.
fn indices_in_range(indices: &[u32], vertex_count: usize) -> bool {
    // HOT PATH: runs for every mesh upload; one linear pass, cheap next to the copy.
    indices.iter().all(|&index| (index as usize) < vertex_count)
}

/// Appends vertices in the layout described in `crate::interface`.
fn write_vertices(bytes: &mut Vec<u8>, vertices: &[ChunkVertex]) {
    // HOT PATH: runs for every mesh upload; a linear write into reserved space.
    // Explicit bytes keep it free of `unsafe` and independent of how the compiler
    // lays out `ChunkVertex`.
    for vertex in vertices {
        let [x, y, z] = vertex.position;
        let [layer_lo, layer_hi] = vertex.texture.0.to_le_bytes();
        bytes.extend_from_slice(&[x, y, z, vertex.face, layer_lo, layer_hi]);
    }
}

/// Appends indices as little-endian `u32`, the order every supported GPU reads.
fn write_indices(bytes: &mut Vec<u8>, indices: &[u32]) {
    // HOT PATH: runs for every mesh upload; a linear write into reserved space.
    for index in indices {
        bytes.extend_from_slice(&index.to_le_bytes());
    }
}

#[cfg(test)]
mod tests {
    use vox_world::TextureLayer;

    use super::*;

    fn sample() -> ChunkVertex {
        ChunkVertex {
            position: [1, 2, 32],
            face: 5,
            texture: TextureLayer(0x0302),
        }
    }

    fn vertex_bytes(vertices: &[ChunkVertex]) -> Vec<u8> {
        let mut bytes = Vec::new();
        write_vertices(&mut bytes, vertices);
        bytes
    }

    fn index_bytes(indices: &[u32]) -> Vec<u8> {
        let mut bytes = Vec::new();
        write_indices(&mut bytes, indices);
        bytes
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

    #[test]
    fn indices_follow_vertices_that_end_on_a_multiple_of_four() {
        // 2 vertices are 12 bytes: no padding needed.
        let layout = MeshLayout::new(2, 3);
        assert_eq!(
            layout,
            Some(MeshLayout {
                index_offset: 12,
                size: 24
            })
        );
    }

    #[test]
    fn indices_start_at_the_next_multiple_of_four() {
        // 3 vertices are 18 bytes: the indices start at 20, after 2 bytes of padding.
        let layout = MeshLayout::new(3, 3);
        assert_eq!(
            layout,
            Some(MeshLayout {
                index_offset: 20,
                size: 32
            })
        );
        // 1 vertex is 6 bytes: the indices start at 8.
        assert_eq!(MeshLayout::new(1, 1).map(|l| l.index_offset), Some(8));
    }

    #[test]
    fn layout_without_vertices_starts_indices_at_zero() {
        let layout = MeshLayout::new(0, 2);
        assert_eq!(
            layout,
            Some(MeshLayout {
                index_offset: 0,
                size: 8
            })
        );
    }

    #[test]
    fn layout_overflow_is_none() {
        assert_eq!(MeshLayout::new(usize::MAX, 1), None);
        assert_eq!(MeshLayout::new(1, usize::MAX), None);
    }

    #[test]
    fn mesh_bytes_are_vertices_padding_then_indices() {
        let mesh = ChunkMesh {
            vertices: vec![sample(); 3],
            indices: vec![0, 1, 2],
        };
        let layout = MeshLayout::new(3, 3).unwrap();
        let bytes = mesh_bytes(&mesh, layout);
        assert_eq!(bytes.len(), layout.size);
        assert_eq!(bytes[..18], vertex_bytes(&mesh.vertices)[..]);
        assert_eq!(bytes[18..20], [0, 0]);
        assert_eq!(bytes[20..], index_bytes(&mesh.indices)[..]);
    }
}
