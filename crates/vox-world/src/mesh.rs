//! Naive chunk mesher: turns a [`Chunk`] into vertices and indices.
//!
//! Only faces that can be seen are generated (see `docs/performance-notes.md`,
//! item 2): a face is emitted when its block is solid and the neighbour on that
//! side is air. On the chunk border the neighbour lives in the next chunk, which the
//! caller passes as [`Neighbours`]; a missing chunk counts as air. The output is plain data, so it has no dependency on any graphics API.

use crate::block::BlockId;
use crate::chunk::{Chunk, LocalPos};
use crate::world::Neighbours;

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
    // HOT PATH: runs for every chunk that is generated or edited. It walks the flat
    // block array once in storage order, so memory is read sequentially, and writes
    // 6-byte vertices, so little data has to travel to the GPU. The output vectors
    // keep their capacity between calls; a capacity estimate for a first call waits
    // for a benchmark. A face whose neighbour is inside the chunk (almost all of them)
    // takes the same cheap test as before; only a border face goes through `neighbours`.
    mesh.vertices.clear();
    mesh.indices.clear();
    for pos in LocalPos::all() {
        let block = chunk.get(pos);
        if block.is_air() {
            continue;
        }
        for face in Face::ALL {
            if is_face_visible(chunk, neighbours, pos, face) {
                push_quad(mesh, pos, face, block);
            }
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
    use crate::world::World;
    use crate::{ChunkPos, generate_chunk};

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

    fn quads(mesh: &ChunkMesh) -> usize {
        mesh.indices.len() / 6
    }

    /// Neighbours with the same chunk on the sides in `present` and nothing elsewhere.
    fn neighbours_on<'a>(chunk: &'a Chunk, present: &[Face]) -> Neighbours<'a> {
        Neighbours::new(Face::ALL.map(|face| present.contains(&face).then_some(chunk)))
    }

    #[test]
    fn no_neighbours_equals_build_mesh() {
        let mut checkerboard = Chunk::new();
        for p in LocalPos::all() {
            if (p.x() + p.y() + p.z()) % 2 == 0 {
                checkerboard.set(p, STONE);
            }
        }
        let chunks = [
            Chunk::new(),
            single_block_chunk(10, 10, 10),
            solid_chunk(),
            checkerboard,
            crate::generate_terrain(1),
            crate::generate_terrain(2),
        ];
        for chunk in &chunks {
            assert_eq!(mesh_with(chunk, &Neighbours::none()), build_mesh(chunk));
        }
    }

    #[test]
    fn touching_solid_chunks_hide_their_shared_faces() {
        let (left, right) = (solid_chunk(), solid_chunk());
        let expected = 6 * CHUNK_SIZE * CHUNK_SIZE - CHUNK_SIZE * CHUNK_SIZE;
        // `right` lies on the +X side of `left`, and `left` on the -X side of `right`.
        let left_mesh = mesh_with(&left, &neighbours_on(&right, &[Face::PosX]));
        let right_mesh = mesh_with(&right, &neighbours_on(&left, &[Face::NegX]));
        assert_eq!(quads(&left_mesh), expected);
        assert_eq!(quads(&right_mesh), expected);
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
            assert_eq!(quads(&mesh), CHUNK_SIZE * CHUNK_SIZE, "missing {missing:?}");
            assert!(mesh.vertices.iter().all(|v| v.face == missing.index()));
        }
    }

    #[test]
    fn air_neighbours_keep_the_border_faces() {
        let air = Chunk::new();
        let solid = solid_chunk();
        let mesh = mesh_with(&solid, &neighbours_on(&air, &Face::ALL));
        assert_eq!(mesh, build_mesh(&solid));
        assert_counts(&mesh, 6 * CHUNK_SIZE * CHUNK_SIZE);
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
            let mut sides = [None; 6];
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

    /// 3x1x3 chunks of terrain (seed 1) around the origin chunk.
    fn terrain_world() -> World {
        let mut world = World::new();
        for z in -1..=1 {
            for x in -1..=1 {
                let pos = ChunkPos::new(x, 0, z);
                world.insert(pos, generate_chunk(1, pos));
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
            isolated += quads(&build_mesh(chunk));
            build_mesh_with_neighbours_into(chunk, &world.neighbours(chunk_pos), &mut mesh);
            meshed += quads(&mesh);
            assert_no_face_towards_solid(&world, chunk_pos, &mesh);
            // Each pair of chunks is visited once, from its lower chunk.
            if let Some(next) = world.get(chunk_pos.neighbour(Face::PosX)) {
                hidden += hidden_pairs(chunk, next, |y, z| ([last, y, z], [0, y, z]));
            }
            if let Some(next) = world.get(chunk_pos.neighbour(Face::PosZ)) {
                hidden += hidden_pairs(chunk, next, |x, y| ([x, y, last], [x, y, 0]));
            }
        }
        assert!(
            hidden > 0,
            "the test needs border faces that are really hidden"
        );
        assert_eq!(meshed, isolated - 2 * hidden);
    }

    /// Brute force: no quad of `mesh` may look at a solid block of a neighbouring chunk.
    fn assert_no_face_towards_solid(world: &World, chunk_pos: ChunkPos, mesh: &ChunkMesh) {
        let size = CHUNK_SIZE as i32;
        for quad in mesh.vertices.chunks(4) {
            let face = Face::ALL[usize::from(quad[0].face)];
            let offset = face.offset();
            // The block is the lowest corner of the quad, moved back by one along the
            // axis of a face that points to a positive direction.
            let block: [i32; 3] = std::array::from_fn(|axis| {
                let low = quad
                    .iter()
                    .map(|v| i32::from(v.position[axis]))
                    .min()
                    .unwrap_or(0);
                low - i32::from(offset[axis].max(0))
            });
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
