//! Naive chunk mesher: turns a [`Chunk`] into vertices and indices.
//!
//! Only faces that can be seen are generated (see `docs/performance-notes.md`,
//! item 2): a face is emitted when its block is solid and the neighbour on that
//! side is air. The output is plain data, so it has no dependency on any graphics API.

use crate::block::BlockId;
use crate::chunk::{Chunk, LocalPos};

/// One of the six directions a block face can look in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Face {
    /// Towards +X.
    PosX,
    /// Towards -X.
    NegX,
    /// Towards +Y.
    PosY,
    /// Towards -Y.
    NegY,
    /// Towards +Z.
    PosZ,
    /// Towards -Z.
    NegZ,
}

impl Face {
    /// All faces, ordered by [`Face::index`].
    pub const ALL: [Face; 6] = [
        Face::PosX,
        Face::NegX,
        Face::PosY,
        Face::NegY,
        Face::PosZ,
        Face::NegZ,
    ];

    /// Stable index in `0..6`, stored in vertices so shaders can pick a normal or shade.
    pub const fn index(self) -> u8 {
        self as u8
    }

    /// Unit offset to the neighbouring block this face looks at.
    pub const fn offset(self) -> [i8; 3] {
        match self {
            Face::PosX => [1, 0, 0],
            Face::NegX => [-1, 0, 0],
            Face::PosY => [0, 1, 0],
            Face::NegY => [0, -1, 0],
            Face::PosZ => [0, 0, 1],
            Face::NegZ => [0, 0, -1],
        }
    }

    /// Quad corners relative to the block origin, counter-clockwise seen from outside.
    ///
    /// Every face uses the same winding, so the cross product of the first two
    /// edges always points along [`Face::offset`].
    const fn corners(self) -> [[u8; 3]; 4] {
        match self {
            Face::PosX => [[1, 0, 0], [1, 1, 0], [1, 1, 1], [1, 0, 1]],
            Face::NegX => [[0, 0, 0], [0, 0, 1], [0, 1, 1], [0, 1, 0]],
            Face::PosY => [[0, 1, 0], [0, 1, 1], [1, 1, 1], [1, 1, 0]],
            Face::NegY => [[0, 0, 0], [1, 0, 0], [1, 0, 1], [0, 0, 1]],
            Face::PosZ => [[0, 0, 1], [1, 0, 1], [1, 1, 1], [0, 1, 1]],
            Face::NegZ => [[0, 0, 0], [0, 1, 0], [1, 1, 0], [1, 0, 0]],
        }
    }
}

/// A compact mesh vertex.
///
/// Layout is `#[repr(C)]`: `position` (3 bytes), `face` (1 byte), `block` (2 bytes),
/// 6 bytes in total with alignment 2 and no padding. Positions are in blocks inside
/// the chunk; the value 32 occurs on the far edge and still fits a `u8`. Small
/// vertices cut memory and bandwidth, and the GPU can unpack them cheaply.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChunkVertex {
    /// Corner position inside the chunk, each coordinate in `0..=32`.
    pub position: [u8; 3],
    /// Face direction, see [`Face::index`].
    pub face: u8,
    /// Block the face belongs to, used to pick a texture.
    pub block: BlockId,
}

/// Mesh of one chunk: vertices and `u32` triangle indices into them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ChunkMesh {
    /// Quad corners, four per visible face.
    pub vertices: Vec<ChunkVertex>,
    /// Triangle list, six indices per visible face.
    pub indices: Vec<u32>,
}

/// Builds the mesh of a chunk, skipping every face hidden by a solid neighbour.
///
/// Neighbours outside the chunk count as air for now, so faces on the chunk border
/// are always emitted. Later slices will look into neighbouring chunks.
///
/// # Example
///
/// ```
/// use vox_world::{BlockId, Chunk, LocalPos, build_mesh};
///
/// let mut chunk = Chunk::new();
/// if let Some(pos) = LocalPos::new(1, 1, 1) {
///     chunk.set(pos, BlockId(1));
/// }
/// assert_eq!(build_mesh(&chunk).indices.len(), 36);
/// ```
pub fn build_mesh(chunk: &Chunk) -> ChunkMesh {
    // HOT PATH: runs for every chunk that is generated or edited. It walks the flat
    // block array once in storage order, so memory is read sequentially, and writes
    // 6-byte vertices, so little data has to travel to the GPU. The only allocation
    // cost is the growth of the output vectors, which is known and not measured yet;
    // a capacity estimate waits for a benchmark.
    let mut mesh = ChunkMesh::default();
    for pos in LocalPos::all() {
        let block = chunk.get(pos);
        if block.is_air() {
            continue;
        }
        for face in Face::ALL {
            if is_face_visible(chunk, pos, face) {
                push_quad(&mut mesh, pos, face, block);
            }
        }
    }
    mesh
}

/// A face is visible when the neighbour it looks at is air or outside the chunk.
fn is_face_visible(chunk: &Chunk, pos: LocalPos, face: Face) -> bool {
    match pos.offset(face.offset()) {
        Some(neighbour) => chunk.get(neighbour).is_air(),
        None => true,
    }
}

/// Appends the four vertices and six indices of one face.
fn push_quad(mesh: &mut ChunkMesh, pos: LocalPos, face: Face, block: BlockId) {
    // Fits u32: a chunk has at most 6 * 32^3 faces, so under 800,000 vertices.
    let base = mesh.vertices.len() as u32;
    for corner in face.corners() {
        mesh.vertices.push(ChunkVertex {
            position: [
                pos.x() + corner[0],
                pos.y() + corner[1],
                pos.z() + corner[2],
            ],
            face: face.index(),
            block,
        });
    }
    mesh.indices
        .extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunk::CHUNK_SIZE;

    const STONE: BlockId = BlockId(1);

    fn pos(x: usize, y: usize, z: usize) -> LocalPos {
        LocalPos::new(x, y, z).expect("test coordinates are known to be inside the chunk")
    }

    fn assert_counts(mesh: &ChunkMesh, faces: usize) {
        assert_eq!(mesh.vertices.len(), faces * 4);
        assert_eq!(mesh.indices.len(), faces * 6);
        let count = mesh.vertices.len();
        assert!(mesh.indices.iter().all(|&i| (i as usize) < count));
    }

    fn single_block_chunk(x: usize, y: usize, z: usize) -> Chunk {
        let mut chunk = Chunk::new();
        chunk.set(pos(x, y, z), STONE);
        chunk
    }

    #[test]
    fn empty_chunk_gives_empty_mesh() {
        let mesh = build_mesh(&Chunk::new());
        assert!(mesh.vertices.is_empty());
        assert!(mesh.indices.is_empty());
    }

    #[test]
    fn single_block_has_six_faces() {
        let mesh = build_mesh(&single_block_chunk(10, 10, 10));
        assert_eq!(mesh.vertices.len(), 24);
        assert_eq!(mesh.indices.len(), 36);
        assert_counts(&mesh, 6);
    }

    #[test]
    fn adjacent_blocks_hide_touching_faces() {
        let mut chunk = single_block_chunk(10, 10, 10);
        chunk.set(pos(11, 10, 10), STONE);
        assert_counts(&build_mesh(&chunk), 10);
    }

    #[test]
    fn solid_chunk_gives_only_outer_shell() {
        let mut chunk = Chunk::new();
        for p in LocalPos::all() {
            chunk.set(p, STONE);
        }
        assert_counts(&build_mesh(&chunk), 6 * CHUNK_SIZE * CHUNK_SIZE);
    }

    #[test]
    fn corner_block_keeps_all_faces() {
        assert_counts(&build_mesh(&single_block_chunk(0, 0, 0)), 6);
        let last = CHUNK_SIZE - 1;
        assert_counts(&build_mesh(&single_block_chunk(last, last, last)), 6);
    }

    #[test]
    fn far_edge_vertices_reach_chunk_size() {
        let last = CHUNK_SIZE - 1;
        let mesh = build_mesh(&single_block_chunk(last, last, last));
        let max = mesh.vertices.iter().flat_map(|v| v.position).max();
        assert_eq!(max, Some(CHUNK_SIZE as u8));
    }

    fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
        [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
    }

    fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
        [
            a[1] * b[2] - a[2] * b[1],
            a[2] * b[0] - a[0] * b[2],
            a[0] * b[1] - a[1] * b[0],
        ]
    }

    fn position(mesh: &ChunkMesh, index: u32) -> [f32; 3] {
        mesh.vertices[index as usize].position.map(f32::from)
    }

    #[test]
    fn triangles_face_away_from_the_block() {
        let mesh = build_mesh(&single_block_chunk(10, 10, 10));
        for triangle in mesh.indices.chunks(3) {
            let [a, b, c] = [0, 1, 2].map(|i| position(&mesh, triangle[i]));
            let normal = cross(sub(b, a), sub(c, a));
            let face = Face::ALL[mesh.vertices[triangle[0] as usize].face as usize];
            // Unit quads: the cross product is exactly the face offset, which points
            // out of the block. A face wound the wrong way would give the opposite vector.
            assert_eq!(normal, face.offset().map(f32::from));
        }
    }

    #[test]
    fn face_indices_are_stable() {
        for (i, face) in Face::ALL.iter().enumerate() {
            assert_eq!(face.index() as usize, i);
        }
    }

    #[test]
    fn vertex_size_is_pinned() {
        assert_eq!(std::mem::size_of::<ChunkVertex>(), 6);
    }
}
