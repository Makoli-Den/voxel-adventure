//! Greedy chunk mesher: turns a [`Chunk`] into vertices and indices.
//!
//! Only faces that can be seen are generated (see `docs/performance-notes.md`,
//! item 2): a face is visible when its block is solid and the neighbour on that
//! side is air. On the chunk border the neighbour lives in the next chunk, which the
//! caller passes as [`Neighbours`]; a missing chunk counts as air.
//!
//! Visible faces are then merged (item 8): neighbouring faces of the same block and
//! the same direction in one plane become one rectangle, so a flat top of 100 blocks
//! costs one quad, not 100. The GPU cost is proportional to the triangles, so fewer
//! quads is a direct gain. The output is plain data, so it has no dependency on any
//! graphics API.

use crate::block::BlockId;
use crate::chunk::{
    AXIS_STRIDES, AXIS_X, AXIS_Y, AXIS_Z, CHUNK_SIZE, CHUNK_VOLUME, Chunk, LocalPos,
};
use crate::world::Neighbours;

/// Corners of one quad: every face of a block is a square.
pub const VERTICES_PER_QUAD: usize = 4;

/// Indices of one quad: two triangles of three indices each.
pub const INDICES_PER_QUAD: usize = 6;

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
    /// Number of faces, the number of sides of a cube. The length of [`Face::ALL`] is
    /// tied to it, so the two cannot drift apart.
    pub const COUNT: usize = 6;

    /// All faces, ordered by [`Face::index`].
    pub const ALL: [Face; Face::COUNT] = [
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

    /// Axes of the plane of this face: `[normal, u, v]`, each 0 for X, 1 for Y, 2 for Z.
    ///
    /// `normal` is the axis of [`Face::offset`]; `u` and `v` are the two axes a
    /// merged rectangle grows along. `u` is the one that lies closer together in the
    /// chunk's block array (see [`AXIS_STRIDES`]), so the inner loop of the mask fill
    /// reads memory in short steps; the order does not change the winding of a quad.
    const fn axes(self) -> [usize; 3] {
        match self {
            Face::PosX | Face::NegX => [AXIS_X, AXIS_Z, AXIS_Y],
            Face::PosY | Face::NegY => [AXIS_Y, AXIS_X, AXIS_Z],
            Face::PosZ | Face::NegZ => [AXIS_Z, AXIS_X, AXIS_Y],
        }
    }

    /// Quad corners relative to the block origin, counter-clockwise seen from outside.
    ///
    /// Every face uses the same winding, so the cross product of the first two
    /// edges always points along [`Face::offset`]. A merged quad stretches these unit
    /// corners along its two tangent axes, which keeps the winding.
    const fn corners(self) -> [[u8; 3]; VERTICES_PER_QUAD] {
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
///
/// The vectors are public plain data; every quad is [`VERTICES_PER_QUAD`] vertices and
/// [`INDICES_PER_QUAD`] indices.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ChunkMesh {
    /// Quad corners, four per quad (merged rectangle of faces).
    pub vertices: Vec<ChunkVertex>,
    /// Triangle list, six indices per quad.
    pub indices: Vec<u32>,
}

impl ChunkMesh {
    /// Number of quads (merged rectangles of visible faces) in the mesh.
    pub fn quad_count(&self) -> usize {
        self.indices.len() / INDICES_PER_QUAD
    }
}

/// Builds the mesh of a chunk, skipping every face hidden by a solid neighbour and
/// merging neighbouring visible faces of the same block and direction into one quad.
///
/// Neighbours outside the chunk count as air, so faces on the chunk border are always
/// emitted. Use [`build_mesh_with_neighbours_into`] to look into neighbouring chunks.
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
    let mut mesh = ChunkMesh::default();
    build_mesh_into(chunk, &mut mesh);
    mesh
}

/// Builds the mesh of a chunk into an existing mesh, replacing its contents.
///
/// The result equals [`build_mesh`]. The vectors are cleared, not dropped, so their
/// capacity is kept between calls: a worker that meshes many chunks stops
/// allocating once the buffers have grown to the size of a typical mesh.
///
/// # Example
///
/// ```
/// use vox_world::{BlockId, Chunk, ChunkMesh, LocalPos, build_mesh_into};
///
/// let mut chunk = Chunk::new();
/// if let Some(pos) = LocalPos::new(1, 1, 1) {
///     chunk.set(pos, BlockId(1));
/// }
/// let mut mesh = ChunkMesh::default();
/// build_mesh_into(&chunk, &mut mesh);
/// build_mesh_into(&chunk, &mut mesh);
/// assert_eq!(mesh.indices.len(), 36);
/// ```
pub fn build_mesh_into(chunk: &Chunk, mesh: &mut ChunkMesh) {
    build_mesh_with_neighbours_into(chunk, &Neighbours::none(), mesh);
}

/// Builds the mesh of a chunk like [`build_mesh_into`], but looks into the neighbouring
/// chunks: a face on the chunk border is emitted only if the block it touches in the
/// neighbouring chunk is air. A missing neighbour counts as air, so the faces on the edge
/// of the loaded area are drawn.
///
/// A mesh built while a neighbour was missing is out of date once that neighbour
/// appears; the caller has to mesh the chunk again.
///
/// # Example
///
/// ```
/// use vox_world::{BlockId, Chunk, ChunkMesh, LocalPos, Neighbours};
/// use vox_world::build_mesh_with_neighbours_into;
///
/// let mut chunk = Chunk::new();
/// let mut wall = Chunk::new();
/// if let (Some(a), Some(b)) = (LocalPos::new(0, 5, 5), LocalPos::new(31, 5, 5)) {
///     chunk.set(a, BlockId(1));
///     wall.set(b, BlockId(1));
/// }
/// // `wall` lies on the -X side of `chunk`, the index of `Face::NegX` is 1.
/// let neighbours = Neighbours::new([None, Some(&wall), None, None, None, None]);
/// let mut mesh = ChunkMesh::default();
/// build_mesh_with_neighbours_into(&chunk, &neighbours, &mut mesh);
/// assert_eq!(mesh.indices.len(), 30); // five faces, the one towards the wall is hidden
/// ```
pub fn build_mesh_with_neighbours_into(
    chunk: &Chunk,
    neighbours: &Neighbours<'_>,
    mesh: &mut ChunkMesh,
) {
    // HOT PATH: runs for every chunk that is generated or edited. For each of the six
    // directions it sweeps the 32 slices across the chunk, and for each slice it first
    // writes the visible block ids into one flat 32x32 mask (a stack array reused for
    // every slice, so nothing is allocated), then cuts rectangles out of the mask.
    // Working on a flat mask keeps the merge a few comparisons per cell. The output
    // vectors keep their capacity between calls. A face whose neighbour is inside the
    // chunk (almost all of them) takes the cheap in-chunk test; only a border face goes
    // through `neighbours`. One cheap pass first records which slices hold any solid block:
    // an empty (sky) chunk, the most common kind, ends there, and a sparse chunk skips the
    // slices without blocks, because a face needs a solid block in its own slice.
    mesh.vertices.clear();
    mesh.indices.clear();
    let solid = solid_slices(chunk);
    if solid.iter().all(|&bits| bits == 0) {
        return;
    }
    let mut mask = [BlockId::AIR; MASK_CELLS];
    for face in Face::ALL {
        let solid_along_normal = solid[face.axes()[0]];
        for slice in 0..CHUNK_SIZE {
            if solid_along_normal & (1 << slice) == 0 {
                continue;
            }
            if fill_mask(chunk, neighbours, face, slice, &mut mask) {
                emit_rectangles(mesh, &mut mask, face, slice);
            }
        }
    }
}

/// Cells in the mask of one slice: one per block of a `CHUNK_SIZE` x `CHUNK_SIZE` plane.
const MASK_CELLS: usize = CHUNK_SIZE * CHUNK_SIZE;

// `solid_slices` keeps one bit per slice in a `u32`, and reads a row of x as contiguous.
const _: () = assert!(CHUNK_SIZE <= u32::BITS as usize);
const _: () = assert!(AXIS_STRIDES[AXIS_X] == 1);

/// Index in the mask of the cell at `u` and `v` on the tangent axes of a face.
const fn mask_index(u: usize, v: usize) -> usize {
    v * CHUNK_SIZE + u
}

/// For each axis, a bit set with bit `i` on when the slice at coordinate `i` along that
/// axis holds at least one solid block. Indexed like [`AXIS_STRIDES`]. All zero means the
/// chunk is empty.
fn solid_slices(chunk: &Chunk) -> [u32; 3] {
    // HOT PATH: one linear pass over the block array, a row of x at a time, so it is
    // cheap enough to run before every mesh and pay for itself on empty chunks.
    let blocks = chunk.blocks();
    let mut solid = [0u32; 3];
    for y in 0..CHUNK_SIZE {
        for z in 0..CHUNK_SIZE {
            let start = y * AXIS_STRIDES[AXIS_Y] + z * AXIS_STRIDES[AXIS_Z];
            let row = &blocks[start..start + CHUNK_SIZE];
            let x_bits = row.iter().enumerate().fold(0u32, |bits, (x, block)| {
                bits | (u32::from(!block.is_air()) << x)
            });
            if x_bits != 0 {
                solid[AXIS_X] |= x_bits;
                solid[AXIS_Y] |= 1 << y;
                solid[AXIS_Z] |= 1 << z;
            }
        }
    }
    solid
}

/// Writes the block ids of the visible faces of one slice into `mask`, air where there is
/// no visible face, and returns whether any face is visible. A slice is the plane of
/// blocks at `slice` along the normal of `face`; the mask cell of the block with
/// coordinates `u` and `v` on the tangent axes is [`mask_index`]`(u, v)`.
fn fill_mask(
    chunk: &Chunk,
    neighbours: &Neighbours<'_>,
    face: Face,
    slice: usize,
    mask: &mut [BlockId; MASK_CELLS],
) -> bool {
    let [normal, u_axis, v_axis] = face.axes();
    let [u_stride, v_stride, normal_stride] =
        [u_axis, v_axis, normal].map(|axis| AXIS_STRIDES[axis]);
    let blocks = chunk.blocks();
    let base = slice * normal_stride;
    // Where the block a face looks at lies, relative to the block, in the flat array. Only
    // meaningful when that block is inside the chunk, that is, not on the border slice.
    // The cast cannot wrap: a stride is at most `CHUNK_SIZE * CHUNK_SIZE`.
    let towards = isize::from(face.offset()[normal]) * normal_stride as isize;
    let on_border = match face.offset()[normal] {
        step if step > 0 => slice == CHUNK_SIZE - 1,
        _ => slice == 0,
    };
    let mut any = false;
    for v in 0..CHUNK_SIZE {
        for u in 0..CHUNK_SIZE {
            let index = base + v * v_stride + u * u_stride;
            let block = blocks[index];
            let visible = if block.is_air() {
                false
            } else if on_border {
                let mut coords = [0; 3];
                coords[normal] = slice;
                coords[u_axis] = u;
                coords[v_axis] = v;
                LocalPos::new(coords[0], coords[1], coords[2])
                    .is_some_and(|pos| is_face_visible(chunk, neighbours, pos, face))
            } else {
                blocks[index.wrapping_add_signed(towards)].is_air()
            };
            let shown = if visible { block } else { BlockId::AIR };
            any |= visible;
            mask[mask_index(u, v)] = shown;
        }
    }
    any
}

/// Cuts the faces left in `mask` into rectangles and appends one quad per rectangle.
/// Each rectangle starts at the first free cell in row order, grows along `u` as far as
/// the block stays the same, then along `v` as long as the whole row segment matches.
/// The cells it covers are cleared, so the mask is all air afterwards.
fn emit_rectangles(
    mesh: &mut ChunkMesh,
    mask: &mut [BlockId; MASK_CELLS],
    face: Face,
    slice: usize,
) {
    let [normal, u_axis, v_axis] = face.axes();
    for v in 0..CHUNK_SIZE {
        let mut u = 0;
        while u < CHUNK_SIZE {
            let block = mask[mask_index(u, v)];
            if block.is_air() {
                u += 1;
                continue;
            }
            let mut width = 1;
            while u + width < CHUNK_SIZE && mask[mask_index(u + width, v)] == block {
                width += 1;
            }
            let mut height = 1;
            while v + height < CHUNK_SIZE
                && mask[mask_index(u, v + height)..][..width]
                    .iter()
                    .all(|&cell| cell == block)
            {
                height += 1;
            }
            for row in v..v + height {
                mask[mask_index(u, row)..][..width].fill(BlockId::AIR);
            }
            let mut origin = [0; 3];
            origin[normal] = slice;
            origin[u_axis] = u;
            origin[v_axis] = v;
            let mut extent = [1; 3];
            extent[u_axis] = width;
            extent[v_axis] = height;
            push_quad(mesh, origin, extent, face, block);
            u += width;
        }
    }
}

/// A face is visible when the block it looks at is air. Inside the chunk that block is
/// read directly; across the border it is read from the neighbouring chunk, and a missing
/// chunk counts as air.
fn is_face_visible(chunk: &Chunk, neighbours: &Neighbours<'_>, pos: LocalPos, face: Face) -> bool {
    match pos.offset(face.offset()) {
        Some(neighbour) => chunk.get(neighbour).is_air(),
        None => is_border_face_visible(neighbours, pos, face),
    }
}

/// The slower path of [`is_face_visible`]: the block touching `pos` lies in the next chunk.
fn is_border_face_visible(neighbours: &Neighbours<'_>, pos: LocalPos, face: Face) -> bool {
    match neighbours.get(face) {
        Some(other) => other.get(pos.wrapped(face.offset())).is_air(),
        None => true,
    }
}

/// Most vertices a chunk mesh can have: one quad per block face, none merged.
const MAX_VERTICES: usize = Face::COUNT * CHUNK_VOLUME * VERTICES_PER_QUAD;

// Vertex indices are `u32`, so the largest mesh must fit it.
const _: () = assert!(MAX_VERTICES <= u32::MAX as usize);

/// Appends the four vertices and six indices of one rectangle of faces.
///
/// `origin` is the lowest block of the rectangle and `extent` its size in blocks along
/// each axis (1 along the normal of `face`). Both are at most `CHUNK_SIZE` in total
/// reach, so every coordinate fits a `u8`.
fn push_quad(
    mesh: &mut ChunkMesh,
    origin: [usize; 3],
    extent: [usize; 3],
    face: Face,
    block: BlockId,
) {
    // The cast cannot truncate: see `MAX_VERTICES`.
    let base = mesh.vertices.len() as u32;
    for corner in face.corners() {
        // Fits u8: `origin + extent` is at most `CHUNK_SIZE` (32).
        let position =
            std::array::from_fn(|axis| origin[axis] as u8 + corner[axis] * extent[axis] as u8);
        mesh.vertices.push(ChunkVertex {
            position,
            face: face.index(),
            block,
        });
    }
    mesh.indices
        .extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
}

/// The simple mesher the greedy one replaced: one unit quad per visible face, with the
/// same visibility rule. It stays as the reference the greedy mesher is checked against.
#[cfg(test)]
pub(crate) fn naive_mesh(chunk: &Chunk, neighbours: &Neighbours<'_>) -> ChunkMesh {
    let mut mesh = ChunkMesh::default();
    for pos in LocalPos::all() {
        let block = chunk.get(pos);
        if block.is_air() {
            continue;
        }
        for face in Face::ALL {
            if is_face_visible(chunk, neighbours, pos, face) {
                let origin = [pos.x(), pos.y(), pos.z()].map(usize::from);
                push_quad(&mut mesh, origin, [1; 3], face, block);
            }
        }
    }
    mesh
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::World;
    use crate::{ChunkPos, generate_chunk};

    const STONE: BlockId = BlockId(1);

    fn pos(x: usize, y: usize, z: usize) -> LocalPos {
        LocalPos::new(x, y, z).expect("test coordinates are known to be inside the chunk")
    }

    fn assert_counts(mesh: &ChunkMesh, faces: usize) {
        assert_eq!(mesh.vertices.len(), faces * VERTICES_PER_QUAD);
        assert_eq!(mesh.indices.len(), faces * INDICES_PER_QUAD);
        assert_eq!(mesh.quad_count(), faces);
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
    fn empty_chunk_takes_the_fast_path_and_clears_a_reused_mesh() {
        assert_eq!(solid_slices(&Chunk::new()), [0; 3]);
        let mut mesh = build_mesh(&single_block_chunk(1, 2, 3));
        assert_ne!(mesh, ChunkMesh::default());
        build_mesh_into(&Chunk::new(), &mut mesh);
        assert_eq!(mesh, ChunkMesh::default());
    }

    #[test]
    fn solid_slices_mark_exactly_the_slices_with_blocks() {
        let solid = solid_slices(&single_block_chunk(1, 2, 3));
        assert_eq!(solid[AXIS_X], 1 << 1);
        assert_eq!(solid[AXIS_Y], 1 << 2);
        assert_eq!(solid[AXIS_Z], 1 << 3);
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
        // Ten visible faces, and the four long sides of the pair merge in twos: 6 quads.
        assert_eq!(naive_mesh(&chunk, &Neighbours::none()).quad_count(), 10);
        assert_counts(&build_mesh(&chunk), 6);
    }

    #[test]
    fn solid_chunk_gives_only_outer_shell_in_six_quads() {
        let mesh = build_mesh(&solid_chunk());
        assert_counts(&mesh, Face::COUNT);
        // Each quad covers a whole side of the chunk.
        let side = CHUNK_SIZE as u8;
        assert!(
            mesh.vertices
                .iter()
                .all(|v| v.position.iter().all(|&c| c == 0 || c == side))
        );
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
    fn build_into_reused_mesh_equals_fresh_mesh() {
        let mut solid = Chunk::new();
        let mut checkerboard = Chunk::new();
        for p in LocalPos::all() {
            solid.set(p, STONE);
            if (p.x() + p.y() + p.z()) % 2 == 0 {
                checkerboard.set(p, STONE);
            }
        }
        let chunks = [
            Chunk::new(),
            single_block_chunk(10, 10, 10),
            solid,
            checkerboard,
            crate::generate_terrain(1),
            crate::generate_terrain(2),
        ];
        // Start from a mesh far bigger than any result but the checkerboard, so stale
        // contents would show if they leaked through.
        let mut mesh = build_mesh(&chunks[3]);
        for chunk in &chunks {
            build_mesh_into(chunk, &mut mesh);
            assert_eq!(mesh, build_mesh(chunk));
        }
    }

    #[test]
    fn build_into_keeps_capacity() {
        let chunk = single_block_chunk(10, 10, 10);
        let mut mesh = build_mesh(&crate::generate_terrain(1));
        let capacity = mesh.vertices.capacity();
        build_mesh_into(&chunk, &mut mesh);
        assert_eq!(mesh.vertices.len(), 24);
        assert_eq!(mesh.vertices.capacity(), capacity);
    }

    fn solid_chunk() -> Chunk {
        let mut chunk = Chunk::new();
        for p in LocalPos::all() {
            chunk.set(p, STONE);
        }
        chunk
    }

    fn mesh_with(chunk: &Chunk, neighbours: &Neighbours<'_>) -> ChunkMesh {
        let mut mesh = ChunkMesh::default();
        build_mesh_with_neighbours_into(chunk, neighbours, &mut mesh);
        mesh
    }

    /// Neighbours with the same chunk on the sides in `present` and nothing elsewhere.
    fn neighbours_on<'a>(chunk: &'a Chunk, present: &[Face]) -> Neighbours<'a> {
        Neighbours::new(Face::ALL.map(|face| present.contains(&face).then_some(chunk)))
    }

    fn checkerboard_chunk() -> Chunk {
        let mut chunk = Chunk::new();
        for p in LocalPos::all() {
            if (p.x() + p.y() + p.z()) % 2 == 0 {
                chunk.set(p, STONE);
            }
        }
        chunk
    }

    /// The old per-block mesher written once more, independently of `is_face_visible`: a
    /// face is hidden by a solid block inside the chunk and never by anything outside it.
    /// It guards the visibility rule itself, which `naive_mesh` shares with the mesher.
    fn count_visible_faces_without_neighbours(chunk: &Chunk) -> usize {
        let mut count = 0;
        for pos in LocalPos::all() {
            for face in Face::ALL {
                let hidden = pos
                    .offset(face.offset())
                    .is_some_and(|next| !chunk.get(next).is_air());
                if !chunk.get(pos).is_air() && !hidden {
                    count += 1;
                }
            }
        }
        count
    }

    #[test]
    fn naive_reference_matches_an_independent_face_count() {
        for chunk in [
            single_block_chunk(10, 10, 10),
            solid_chunk(),
            checkerboard_chunk(),
            crate::generate_terrain(1),
            crate::generate_terrain(2),
        ] {
            let naive = naive_mesh(&chunk, &Neighbours::none());
            assert_eq!(
                naive.quad_count(),
                count_visible_faces_without_neighbours(&chunk)
            );
        }
    }

    /// Every visible face as `(face index, block id, block position)`, one entry per
    /// unit square of every quad, sorted. A merged quad is expanded into its squares, so
    /// two meshes with the same list cover the same squares with the same blocks no
    /// matter how the squares are grouped into quads. The block position (not the
    /// position of the square) identifies a face: the square of a positive face lies one
    /// block further along its normal.
    fn unit_faces(mesh: &ChunkMesh) -> Vec<(u8, u16, [i32; 3])> {
        let mut faces = Vec::new();
        for quad in mesh.vertices.chunks(VERTICES_PER_QUAD) {
            let face = Face::ALL[usize::from(quad[0].face)];
            let low: [i32; 3] = std::array::from_fn(|axis| {
                quad.iter()
                    .map(|v| i32::from(v.position[axis]))
                    .min()
                    .unwrap_or(0)
            });
            let high: [i32; 3] = std::array::from_fn(|axis| {
                quad.iter()
                    .map(|v| i32::from(v.position[axis]))
                    .max()
                    .unwrap_or(0)
            });
            let offset = face.offset();
            let [normal, u, v] = face.axes();
            assert_eq!(high[normal] - low[normal], 0, "a quad is flat");
            for a in low[u]..high[u] {
                for b in low[v]..high[v] {
                    let mut block = low;
                    block[normal] -= i32::from(offset[normal].max(0));
                    block[u] = a;
                    block[v] = b;
                    faces.push((quad[0].face, quad[0].block.0, block));
                }
            }
        }
        faces.sort_unstable();
        faces
    }

    /// Asserts the geometry of `mesh` is sound and covers exactly the squares of the naive
    /// mesh, each once, with no more quads than the naive mesh. Returns the quad counts
    /// as `(greedy, naive)`.
    fn assert_matches_naive(chunk: &Chunk, neighbours: &Neighbours<'_>) -> (usize, usize) {
        let greedy = mesh_with(chunk, neighbours);
        let naive = naive_mesh(chunk, neighbours);
        assert_valid(&greedy);
        let faces = unit_faces(&greedy);
        assert_eq!(faces, unit_faces(&naive));
        let mut unique = faces.clone();
        unique.dedup();
        assert_eq!(unique.len(), faces.len(), "quads must not overlap");
        assert!(greedy.quad_count() <= naive.quad_count());
        (greedy.quad_count(), naive.quad_count())
    }

    /// Indices in range, six per four vertices, positions within the chunk, and every quad
    /// wound so that its normal points along its face, with the length of its area.
    fn assert_valid(mesh: &ChunkMesh) {
        assert_counts(mesh, mesh.vertices.len() / VERTICES_PER_QUAD);
        let size = CHUNK_SIZE as u8;
        assert!(
            mesh.vertices
                .iter()
                .all(|v| v.position.iter().all(|&c| c <= size))
        );
        for (quad, triangles) in mesh
            .vertices
            .chunks(VERTICES_PER_QUAD)
            .zip(mesh.indices.chunks(INDICES_PER_QUAD))
        {
            let face = Face::ALL[usize::from(quad[0].face)];
            let [a, b, c, d] = [0, 1, 2, 3].map(|i| quad[i].position.map(f32::from));
            // The four corners form a parallelogram, so the first two edges give the area.
            assert_eq!(sub(d, a), sub(c, b));
            let normal = cross(sub(b, a), sub(d, a));
            let area = (0..3).map(|i| normal[i].abs()).sum::<f32>();
            assert!(area >= 1.0);
            let expected = face.offset().map(|step| f32::from(step) * area);
            assert_eq!(normal, expected, "{face:?} winding");
            // The triangle pattern is the same as for a unit quad.
            let base = triangles[0];
            assert_eq!(
                triangles,
                [base, base + 1, base + 2, base, base + 2, base + 3]
            );
        }
    }

    /// A small deterministic generator (xorshift32), so the random tests need no crate and
    /// give the same chunks on every run.
    struct Rng(u32);

    impl Rng {
        fn next(&mut self) -> u32 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 17;
            self.0 ^= self.0 << 5;
            self.0
        }
    }

    /// A chunk of `kinds` block kinds in cubic clumps of `clump` blocks, solid with the
    /// given chance in percent. Clumps of one block give noise; larger ones give runs that merge.
    fn random_chunk(rng: &mut Rng, kinds: u32, clump: usize, solid_percent: u32) -> Chunk {
        let cells = CHUNK_SIZE.div_ceil(clump);
        let mut table = vec![BlockId::AIR; cells * cells * cells];
        for cell in &mut table {
            if rng.next() % 100 < solid_percent {
                *cell = BlockId((rng.next() % kinds + 1) as u16);
            }
        }
        let mut chunk = Chunk::new();
        for p in LocalPos::all() {
            let [x, y, z] = [p.x(), p.y(), p.z()].map(|c| usize::from(c) / clump);
            chunk.set(p, table[(y * cells + z) * cells + x]);
        }
        chunk
    }

    /// Seeds, kinds, clump sizes and densities of the random chunks.
    fn random_chunks() -> Vec<Chunk> {
        let mut rng = Rng(0x9E37_79B9);
        let mut chunks = Vec::new();
        for (kinds, clump, percent) in [(1, 1, 50), (3, 1, 70), (2, 3, 60), (3, 5, 50), (4, 8, 80)]
        {
            for _ in 0..4 {
                chunks.push(random_chunk(&mut rng, kinds, clump, percent));
            }
        }
        chunks
    }

    #[test]
    fn greedy_covers_the_same_squares_as_the_naive_mesher_on_random_chunks() {
        let mut merged_somewhere = false;
        for chunk in &random_chunks() {
            let (greedy, naive) = assert_matches_naive(chunk, &Neighbours::none());
            merged_somewhere |= greedy < naive;
        }
        assert!(
            merged_somewhere,
            "the clumped chunks must give something to merge"
        );
    }

    #[test]
    fn greedy_covers_the_same_squares_with_random_neighbours() {
        let mut rng = Rng(0x1234_5678);
        let chunks = random_chunks();
        let around: Vec<Chunk> = (0..Face::COUNT)
            .map(|_| random_chunk(&mut rng, 2, 4, 60))
            .collect();
        for (i, chunk) in chunks.iter().enumerate() {
            // Leave a different side out each time so missing neighbours are covered too.
            let missing = i % (Face::COUNT + 1);
            let sides = std::array::from_fn(|side| (side != missing).then(|| &around[side]));
            assert_matches_naive(chunk, &Neighbours::new(sides));
        }
    }

    #[test]
    fn greedy_covers_the_same_squares_on_terrain() {
        for seed in 1..=3 {
            let (greedy, naive) =
                assert_matches_naive(&crate::generate_terrain(seed), &Neighbours::none());
            assert!(greedy < naive);
        }
    }

    #[test]
    fn greedy_covers_the_same_squares_on_terrain_with_neighbours() {
        let world = terrain_world();
        for (chunk_pos, chunk) in world.iter() {
            assert_matches_naive(chunk, &world.neighbours(chunk_pos));
        }
    }

    #[test]
    fn flat_slab_merges_to_six_quads() {
        let mut slab = Chunk::new();
        for p in LocalPos::all().filter(|p| p.y() < 8) {
            slab.set(p, STONE);
        }
        let (greedy, naive) = assert_matches_naive(&slab, &Neighbours::none());
        assert_eq!(greedy, Face::COUNT);
        assert!(naive > greedy);
    }

    #[test]
    fn checkerboard_cannot_merge() {
        let (greedy, naive) = assert_matches_naive(&checkerboard_chunk(), &Neighbours::none());
        assert_eq!(greedy, naive);
    }

    #[test]
    fn different_blocks_do_not_merge() {
        let dirt = BlockId(2);
        let mut chunk = Chunk::new();
        for x in 0..4 {
            chunk.set(pos(x, 0, 0), if x < 2 { STONE } else { dirt });
        }
        let mesh = build_mesh(&chunk);
        // Top, bottom, front and back each split in two by the block change; the two
        // ends are one square each, and the shared faces are hidden.
        assert_eq!(mesh.quad_count(), 4 * 2 + 2);
        assert_valid(&mesh);
    }

    #[test]
    fn an_l_shape_is_cut_into_two_rectangles() {
        let mut chunk = Chunk::new();
        for (x, z) in [(0, 0), (1, 0), (2, 0), (0, 1), (0, 2)] {
            chunk.set(pos(x, 0, z), STONE);
        }
        let mesh = build_mesh(&chunk);
        let top = mesh
            .vertices
            .chunks(VERTICES_PER_QUAD)
            .filter(|q| q[0].face == Face::PosY.index());
        assert_eq!(top.count(), 2);
    }

    #[test]
    fn neighbours_split_a_quad_where_they_hide_part_of_a_face() {
        // A slab with a solid block in the neighbour across the middle of its +X side.
        let mut slab = Chunk::new();
        for p in LocalPos::all().filter(|p| p.y() < 4) {
            slab.set(p, STONE);
        }
        let mut wall = Chunk::new();
        wall.set(pos(0, 1, 7), STONE);
        let mesh = mesh_with(&slab, &neighbours_on(&wall, &[Face::PosX]));
        let x_faces = mesh
            .vertices
            .chunks(VERTICES_PER_QUAD)
            .filter(|q| q[0].face == Face::PosX.index());
        // The hole is in the second row of a 32x4 face (rows run along y). Verified by running
        // the algorithm: the full row below the hole is one quad; the row of the hole leaves a
        // left and a right block, each growing up over the two rows above; and the cell
        // above the hole is a fourth quad, two rows tall.
        assert_eq!(x_faces.count(), 4);
        assert_matches_naive(&slab, &neighbours_on(&wall, &[Face::PosX]));
    }

    #[test]
    fn quad_count_counts_faces() {
        assert_eq!(ChunkMesh::default().quad_count(), 0);
        let mesh = build_mesh(&single_block_chunk(3, 3, 3));
        assert_eq!(mesh.quad_count(), Face::COUNT);
    }

    /// A block of a chunk on the border `face` looks at, away from the corners.
    fn border_block(face: Face) -> [usize; 3] {
        let mut block = [5, 7, 9];
        for (axis, step) in face.offset().into_iter().enumerate() {
            match step {
                1 => block[axis] = CHUNK_SIZE - 1,
                -1 => block[axis] = 0,
                _ => {}
            }
        }
        block
    }

    /// The block across the border from `block`, in the chunk on the side of `face`.
    fn block_across(block: [usize; 3], face: Face) -> [usize; 3] {
        let mut across = block;
        for (axis, step) in face.offset().into_iter().enumerate() {
            match step {
                1 => across[axis] = 0,
                -1 => across[axis] = CHUNK_SIZE - 1,
                _ => {}
            }
        }
        across
    }

    fn chunk_with(block: [usize; 3]) -> Chunk {
        let mut chunk = Chunk::new();
        chunk.set(pos(block[0], block[1], block[2]), STONE);
        chunk
    }

    #[test]
    fn a_marker_block_across_each_border_hides_exactly_the_face_that_touches_it() {
        for face in Face::ALL {
            let block = border_block(face);
            let chunk = chunk_with(block);
            let marker = chunk_with(block_across(block, face));
            let mesh = mesh_with(&chunk, &neighbours_on(&marker, &[face]));
            assert_counts(&mesh, Face::COUNT - 1);
            assert!(
                mesh.vertices.iter().all(|v| v.face != face.index()),
                "{face:?}"
            );
        }
    }

    #[test]
    fn a_marker_block_beside_the_touching_one_hides_nothing() {
        for face in Face::ALL {
            let block = border_block(face);
            let chunk = chunk_with(block);
            // Shift the marker by one block along each of the two axes parallel to the border.
            for axis in (0..3).filter(|&axis| face.offset()[axis] == 0) {
                let mut beside = block_across(block, face);
                beside[axis] += 1;
                let marker = chunk_with(beside);
                let mesh = mesh_with(&chunk, &neighbours_on(&marker, &[face]));
                assert_counts(&mesh, Face::COUNT);
            }
        }
    }

    #[test]
    fn touching_solid_chunks_hide_their_shared_faces() {
        let (left, right) = (solid_chunk(), solid_chunk());
        // The five other sides remain, one merged quad each.
        let expected = Face::COUNT - 1;
        // `right` lies on the +X side of `left`, and `left` on the -X side of `right`.
        let left_mesh = mesh_with(&left, &neighbours_on(&right, &[Face::PosX]));
        let right_mesh = mesh_with(&right, &neighbours_on(&left, &[Face::NegX]));
        assert_eq!(left_mesh.quad_count(), expected);
        assert_eq!(right_mesh.quad_count(), expected);
        assert!(
            left_mesh
                .vertices
                .iter()
                .all(|v| v.face != Face::PosX.index())
        );
        assert!(
            right_mesh
                .vertices
                .iter()
                .all(|v| v.face != Face::NegX.index())
        );
    }

    #[test]
    fn solid_chunk_inside_solid_chunks_has_no_mesh() {
        let solid = solid_chunk();
        let mesh = mesh_with(&solid, &neighbours_on(&solid, &Face::ALL));
        assert!(mesh.vertices.is_empty());
        assert!(mesh.indices.is_empty());
    }

    #[test]
    fn one_missing_neighbour_leaves_exactly_that_face() {
        let solid = solid_chunk();
        for missing in Face::ALL {
            let present: Vec<Face> = Face::ALL.into_iter().filter(|&f| f != missing).collect();
            let mesh = mesh_with(&solid, &neighbours_on(&solid, &present));
            assert_eq!(mesh.quad_count(), 1, "missing {missing:?}");
            assert!(mesh.vertices.iter().all(|v| v.face == missing.index()));
        }
    }

    #[test]
    fn air_neighbours_keep_the_border_faces() {
        let air = Chunk::new();
        let solid = solid_chunk();
        let mesh = mesh_with(&solid, &neighbours_on(&air, &Face::ALL));
        assert_eq!(mesh, build_mesh(&solid));
        assert_counts(&mesh, Face::COUNT);
    }

    #[test]
    fn corner_block_is_hidden_only_by_the_block_that_touches_it() {
        let last = CHUNK_SIZE - 1;
        let chunk = single_block_chunk(0, 0, 0);
        // One solid block in the neighbour, directly across the border from the corner.
        let across = [
            (Face::NegX, pos(last, 0, 0), pos(last, 1, 0)),
            (Face::NegY, pos(0, last, 0), pos(1, last, 0)),
            (Face::NegZ, pos(0, 0, last), pos(1, 0, last)),
        ];
        for (face, touching, beside) in across {
            let mut other = Chunk::new();
            other.set(touching, STONE);
            let mut sides = [None; Face::COUNT];
            sides[usize::from(face.index())] = Some(&other);
            let mesh = mesh_with(&chunk, &Neighbours::new(sides));
            assert_counts(&mesh, 5);
            assert!(mesh.vertices.iter().all(|v| v.face != face.index()));

            // A solid block next to the touching one does not hide anything.
            let mut near = Chunk::new();
            near.set(beside, STONE);
            sides[usize::from(face.index())] = Some(&near);
            assert_counts(&mesh_with(&chunk, &Neighbours::new(sides)), 6);
        }
    }

    /// 3x3x3 chunks of terrain (seed 1) around the origin chunk. The layer `y = 1` is all
    /// air and the layer `y = -1` all stone, so the vertical neighbours see real data.
    fn terrain_world() -> World {
        let mut world = World::new();
        for y in -1..=1 {
            for z in -1..=1 {
                for x in -1..=1 {
                    let pos = ChunkPos::new(x, y, z);
                    world.insert(pos, generate_chunk(1, pos));
                }
            }
        }
        world
    }

    /// Number of face pairs hidden between two chunks, counted block by block: both
    /// facing blocks are solid. `first_edge` and `second_edge` map a position along the
    /// shared border to the facing block of each chunk.
    fn hidden_pairs(
        first: &Chunk,
        second: &Chunk,
        edge: impl Fn(usize, usize) -> ([usize; 3], [usize; 3]),
    ) -> usize {
        let mut count = 0;
        for i in 0..CHUNK_SIZE {
            for j in 0..CHUNK_SIZE {
                let ([ax, ay, az], [bx, by, bz]) = edge(i, j);
                let solid = |chunk: &Chunk, x, y, z| !chunk.get(pos(x, y, z)).is_air();
                if solid(first, ax, ay, az) && solid(second, bx, by, bz) {
                    count += 1;
                }
            }
        }
        count
    }

    #[test]
    fn world_of_terrain_hides_exactly_the_touching_border_faces() {
        let world = terrain_world();
        let last = CHUNK_SIZE - 1;
        let mut isolated = 0;
        let mut meshed = 0;
        let mut hidden = 0;
        let mut mesh = ChunkMesh::default();
        for (chunk_pos, chunk) in world.iter() {
            // Quads are merged, so count the unit squares they cover.
            isolated += unit_faces(&build_mesh(chunk)).len();
            build_mesh_with_neighbours_into(chunk, &world.neighbours(chunk_pos), &mut mesh);
            meshed += unit_faces(&mesh).len();
            assert_no_face_towards_solid(&world, chunk_pos, &mesh);
            // Each pair of chunks is visited once, from its lower chunk.
            if let Some(next) = world.get(chunk_pos.neighbour(Face::PosX)) {
                hidden += hidden_pairs(chunk, next, |y, z| ([last, y, z], [0, y, z]));
            }
            if let Some(next) = world.get(chunk_pos.neighbour(Face::PosZ)) {
                hidden += hidden_pairs(chunk, next, |x, y| ([x, y, last], [x, y, 0]));
            }
            if let Some(next) = world.get(chunk_pos.neighbour(Face::PosY)) {
                hidden += hidden_pairs(chunk, next, |x, z| ([x, last, z], [x, 0, z]));
            }
        }
        assert!(
            hidden > 0,
            "the test needs border faces that are really hidden"
        );
        assert_eq!(meshed, isolated - 2 * hidden);
    }

    /// Brute force: no square of `mesh` may look at a solid block of a neighbouring chunk.
    fn assert_no_face_towards_solid(world: &World, chunk_pos: ChunkPos, mesh: &ChunkMesh) {
        let size = CHUNK_SIZE as i32;
        for (face_index, _, block) in unit_faces(mesh) {
            let face = Face::ALL[usize::from(face_index)];
            let offset = face.offset();
            let target: [i32; 3] =
                std::array::from_fn(|axis| block[axis] + i32::from(offset[axis]));
            if target.iter().all(|&c| (0..size).contains(&c)) {
                continue;
            }
            let Some(other) = world.get(chunk_pos.neighbour(face)) else {
                continue;
            };
            let wrapped = target.map(|c| c.rem_euclid(size) as usize);
            assert!(
                other.get(pos(wrapped[0], wrapped[1], wrapped[2])).is_air(),
                "{chunk_pos:?}: quad of block {block:?} faces a solid block"
            );
        }
    }

    #[test]
    fn face_axes_follow_the_normal_and_the_memory_order() {
        for face in Face::ALL {
            let [normal, u, v] = face.axes();
            assert_ne!(face.offset()[normal], 0);
            assert_eq!(face.offset().iter().filter(|&&step| step != 0).count(), 1);
            let mut all = [normal, u, v];
            all.sort_unstable();
            assert_eq!(all, [AXIS_X, AXIS_Y, AXIS_Z]);
            assert!(AXIS_STRIDES[u] < AXIS_STRIDES[v]);
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
