//! Per-direction culling (decision 0015, (c)): which parts of a chunk mesh can face the
//! camera.
//!
//! The mesher groups the quads of a mesh by face direction in [`Face::ALL`] order, so each
//! direction is one contiguous range of the index buffer ([`ChunkMesh::face_index_range`]). A
//! face looking along `+X` lies on a plane `x = p` inside the chunk's box, and the camera
//! sees its front only from `x > p`. When the whole box lies on the far side
//! (`camera.x < box.min.x`) none of the `+X` faces can be front-facing, the rasterizer would
//! discard every one of them as a back face, and the range need not be drawn. The test
//! is the same for every direction and costs a comparison each.
//!
//! The test is conservative so the picture can never change: a camera exactly on a face
//! plane counts as in front of it, and a box that is not a number (a broken camera) draws
//! everything. The box is built from the same rounded camera-relative origin the vertex
//! shader adds the chunk-local positions to, and rounding to nearest keeps order, so a plane
//! the test puts beyond the camera is beyond it on the GPU too. It assumes what the built-in
//! chunk shader does: vertices stay inside the
//! chunk's box and every quad faces the direction of its group. A shader pack that moves
//! vertices out of the box or turns faces would break that contract.
//!
//! Pure logic with no graphics API, so it is unit tested; the renderer turns the ranges
//! into indexed draws.

use std::ops::Range;

use vox_world::{ChunkMesh, Face, INDICES_PER_QUAD};

/// Number of axes, the number of opposite face pairs.
const AXIS_COUNT: usize = Face::COUNT / 2;

/// Most draws one mesh needs. In [`Face::ALL`] the two directions of an axis are
/// neighbours (pinned by a test), and the mesher lays the groups out in that order, so the
/// directions drawn for one axis (one, the other or both) are always one contiguous range,
/// and a mesh never needs more than one draw per axis.
pub(crate) const MAX_DRAWS_PER_MESH: usize = AXIS_COUNT;

/// Where the six direction groups of a mesh lie in its index buffer: group `i` (the face
/// `Face::ALL[i]`) holds the indices `bounds[i]..bounds[i + 1]`. Storing the boundaries
/// instead of six ranges makes the groups contiguous and in order by construction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct FaceGroups {
    bounds: [u32; Face::COUNT + 1],
}

impl FaceGroups {
    /// The groups of `mesh`, copied from [`ChunkMesh::face_index_range`], or `None` if the
    /// quads per direction (`face_quads`) do not add up to the indices: drawing ranges taken
    /// from such counts could read past the index buffer.
    ///
    /// Only the sum is checked, which is what keeps every draw inside the buffer. That the
    /// quads in each range really look in that direction is the mesh's contract (the greedy
    /// mesher keeps it); a mesh that breaks it may lose faces, but never draws out of range.
    pub(crate) fn from_mesh(mesh: &ChunkMesh) -> Option<Self> {
        let quads: u64 = mesh.face_quads.iter().map(|&count| u64::from(count)).sum();
        // Lossless: `usize` is at most 64 bits on every supported target, and a quad count
        // of at most 6 * u32::MAX times six indices fits a `u64`.
        let index_count = mesh.indices.len() as u64;
        if quads * INDICES_PER_QUAD as u64 != index_count {
            return None;
        }
        let end = u32::try_from(index_count).ok()?;
        // The sum fits `u32` (checked above), so `face_index_range` cannot overflow, and its
        // own consistency check holds.
        let mut bounds = [end; Face::COUNT + 1];
        for (bound, face) in bounds.iter_mut().zip(Face::ALL) {
            *bound = mesh.face_index_range(face).start;
        }
        Some(Self { bounds })
    }

    /// The indices of the group of the face at `face_index` in [`Face::ALL`].
    fn group(&self, face_index: usize) -> Range<u32> {
        self.bounds[face_index]..self.bounds[face_index + 1]
    }
}

/// A part of a mesh's index buffer to draw: `count` indices from `first` on, exactly the
/// `firstIndex` and `indexCount` of an indexed draw.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct IndexRange {
    /// The first index to draw.
    pub(crate) first: u32,
    /// How many indices to draw, never zero in a [`DrawRanges`].
    pub(crate) count: u32,
}

impl IndexRange {
    /// The index after the last one drawn. Cannot overflow: ranges come from a buffer of at
    /// most `u32::MAX` indices.
    fn end(self) -> u32 {
        self.first + self.count
    }
}

/// The draws of one mesh, at most [`MAX_DRAWS_PER_MESH`], on the stack.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct DrawRanges {
    ranges: [IndexRange; MAX_DRAWS_PER_MESH],
    len: usize,
}

impl DrawRanges {
    /// The draws, in index buffer order, none empty, no two touching.
    pub(crate) fn as_slice(&self) -> &[IndexRange] {
        &self.ranges[..self.len]
    }

    /// Adds `range` (not empty), merged into the last draw if it starts where that one ends.
    fn push(&mut self, range: Range<u32>) {
        let count = range.end - range.start;
        if let Some(last) = self.ranges[..self.len].last_mut() {
            if last.end() == range.start {
                last.count += count;
                return;
            }
        }
        if self.len == MAX_DRAWS_PER_MESH {
            // Unreachable with groups in `Face::ALL` order (see `MAX_DRAWS_PER_MESH`). Should
            // it happen, the last draw is stretched over the gap: more faces are drawn, never
            // fewer, so the picture stays right.
            let last = &mut self.ranges[MAX_DRAWS_PER_MESH - 1];
            last.count = range.end - last.first;
            return;
        }
        self.ranges[self.len] = IndexRange {
            first: range.start,
            count,
        };
        self.len += 1;
    }
}

/// The parts of a mesh's index buffer that can face a camera at the origin, for the mesh's
/// box from `min` to `max` relative to the camera (camera-relative space, as the frustum
/// test uses). Empty groups are skipped and groups next to each other in the buffer are
/// merged into one draw.
// HOT PATH: runs for every chunk in view every frame. Six direction tests of one comparison
// each over fixed-size arrays on the stack; nothing is allocated.
pub(crate) fn visible_ranges(min: [f32; 3], max: [f32; 3], groups: &FaceGroups) -> DrawRanges {
    let mut draws = DrawRanges::default();
    for (face_index, face) in Face::ALL.into_iter().enumerate() {
        let group = groups.group(face_index);
        if !group.is_empty() && may_face_camera(face, min, max) {
            draws.push(group);
        }
    }
    draws
}

/// Whether a face looking along `face`, somewhere in the box from `min` to `max` relative to
/// the camera, can show its front to the camera. A face along `+X` lies on a plane `x = p`
/// with `p >= min.x`, and the camera at `x = 0` is in front of it only if `p <= 0`, which
/// some plane allows exactly when `min.x <= 0`; for `-X`, when `max.x >= 0`. Equality (the
/// camera on the plane) counts as in front, and NaN never culls.
fn may_face_camera(face: Face, min: [f32; 3], max: [f32; 3]) -> bool {
    face.offset()
        .into_iter()
        .enumerate()
        .all(|(axis, step)| match step {
            0 => true,
            step if step > 0 => min[axis] <= 0.0 || min[axis].is_nan(),
            _ => max[axis] >= 0.0 || max[axis].is_nan(),
        })
}

#[cfg(test)]
mod tests {
    use vox_world::{BlockId, CHUNK_SIZE, Chunk, FaceTextures, LocalPos, build_mesh};

    use super::*;

    /// The side of a chunk's box in blocks.
    const SIDE: f32 = CHUNK_SIZE as f32;

    /// Index of `face` in [`Face::ALL`].
    fn index(face: Face) -> usize {
        usize::from(face.index())
    }

    /// Groups with `quads[i]` quads of `Face::ALL[i]`, built from a mesh whose indices
    /// match, so they go through the same path as a real upload.
    fn groups(quads: [u32; Face::COUNT]) -> FaceGroups {
        let total: u32 = quads.iter().sum();
        let mesh = ChunkMesh {
            vertices: Vec::new(),
            indices: vec![0; (total as usize) * INDICES_PER_QUAD],
            face_quads: quads,
        };
        FaceGroups::from_mesh(&mesh).expect("the counts match the indices")
    }

    /// One quad in every direction: group `i` holds indices `6 * i..6 * (i + 1)`.
    fn one_quad_each() -> FaceGroups {
        groups([1; Face::COUNT])
    }

    /// The faces whose indices `draws` covers, in `Face::ALL` order, for groups of one quad.
    fn faces_drawn(draws: &DrawRanges) -> Vec<Face> {
        let quad = INDICES_PER_QUAD as u32;
        Face::ALL
            .into_iter()
            .filter(|&face| {
                let start = index(face) as u32 * quad;
                draws
                    .as_slice()
                    .iter()
                    .any(|r| r.first <= start && start + quad <= r.end())
            })
            .collect()
    }

    /// A chunk-sized box with its lowest corner at `min`, relative to the camera.
    fn chunk_box(min: [f32; 3]) -> ([f32; 3], [f32; 3]) {
        (min, min.map(|value| value + SIDE))
    }

    fn drawn_for(min: [f32; 3]) -> Vec<Face> {
        let (min, max) = chunk_box(min);
        faces_drawn(&visible_ranges(min, max, &one_quad_each()))
    }

    #[test]
    fn opposite_directions_are_neighbours_in_face_all() {
        // The bound of three draws per mesh rests on this order.
        for pair in Face::ALL.chunks(2) {
            let [a, b] = [pair[0], pair[1]];
            let sum: [i8; 3] = std::array::from_fn(|axis| a.offset()[axis] + b.offset()[axis]);
            assert_eq!(sum, [0; 3], "{a:?} and {b:?} must be opposite");
        }
        assert_eq!(Face::ALL.len(), 2 * MAX_DRAWS_PER_MESH);
    }

    #[test]
    fn groups_follow_face_index_range_of_a_real_mesh() {
        let mut chunk = Chunk::new();
        for (x, y, z) in [(1, 1, 1), (2, 1, 1), (5, 6, 7)] {
            if let Some(pos) = LocalPos::new(x, y, z) {
                chunk.set(pos, BlockId(1));
            }
        }
        let mesh = build_mesh(&chunk, &FaceTextures::default());
        let groups = FaceGroups::from_mesh(&mesh).expect("the mesher fills face_quads");
        for face in Face::ALL {
            assert_eq!(groups.group(index(face)), mesh.face_index_range(face));
        }
        assert_eq!(groups.bounds[Face::COUNT] as usize, mesh.indices.len());
    }

    #[test]
    fn camera_inside_the_box_draws_all_six_directions_in_one_draw() {
        let draws = visible_ranges([-1.0; 3], [31.0; 3], &one_quad_each());
        assert_eq!(
            draws.as_slice(),
            [IndexRange {
                first: 0,
                count: 36
            }]
        );
    }

    #[test]
    fn outside_along_one_axis_drops_exactly_the_faces_turned_away() {
        let all = Face::ALL.to_vec();
        let without = |face: Face| -> Vec<Face> {
            all.iter().copied().filter(|&other| other != face).collect()
        };
        // The box beyond the camera on +X: its +X faces look away.
        assert_eq!(drawn_for([10.0, -5.0, -5.0]), without(Face::PosX));
        assert_eq!(drawn_for([-50.0, -5.0, -5.0]), without(Face::NegX));
        assert_eq!(drawn_for([-5.0, 10.0, -5.0]), without(Face::PosY));
        assert_eq!(drawn_for([-5.0, -50.0, -5.0]), without(Face::NegY));
        assert_eq!(drawn_for([-5.0, -5.0, 10.0]), without(Face::PosZ));
        assert_eq!(drawn_for([-5.0, -5.0, -50.0]), without(Face::NegZ));
    }

    #[test]
    fn diagonally_outside_draws_three_directions() {
        use Face::*;
        assert_eq!(drawn_for([10.0, 10.0, 10.0]), [NegX, NegY, NegZ]);
        assert_eq!(drawn_for([-50.0, -50.0, -50.0]), [PosX, PosY, PosZ]);
        assert_eq!(drawn_for([10.0, -50.0, 10.0]), [NegX, PosY, NegZ]);
        assert_eq!(drawn_for([-50.0, 10.0, -50.0]), [PosX, NegY, PosZ]);
    }

    #[test]
    fn every_corner_region_draws_one_direction_per_axis_in_at_most_three_draws() {
        let positions = [-100.0, -16.0, 50.0];
        for x in positions {
            for y in positions {
                for z in positions {
                    let (min, max) = chunk_box([x, y, z]);
                    let draws = visible_ranges(min, max, &one_quad_each());
                    let drawn = faces_drawn(&draws);
                    for face in Face::ALL {
                        let axis = face.offset().iter().position(|&s| s != 0).unwrap_or(0);
                        let positive = face.offset()[axis] > 0;
                        let expected = if positive {
                            min[axis] <= 0.0
                        } else {
                            max[axis] >= 0.0
                        };
                        assert_eq!(drawn.contains(&face), expected, "{face:?} at {min:?}");
                    }
                    assert!(draws.as_slice().len() <= MAX_DRAWS_PER_MESH);
                    let triangles: u32 = draws.as_slice().iter().map(|r| r.count).sum();
                    assert_eq!(triangles as usize, drawn.len() * INDICES_PER_QUAD);
                }
            }
        }
    }

    #[test]
    fn a_camera_on_a_face_plane_counts_as_in_front() {
        // min.x == 0: the camera lies in the plane of the nearest +X faces.
        assert!(drawn_for([0.0, -5.0, -5.0]).contains(&Face::PosX));
        assert!(drawn_for([-0.0, -5.0, -5.0]).contains(&Face::PosX));
        // max.x == 0: the camera lies in the plane of the farthest -X faces.
        assert!(drawn_for([-SIDE, -5.0, -5.0]).contains(&Face::NegX));
        // Just past the plane they go.
        assert!(!drawn_for([f32::MIN_POSITIVE, -5.0, -5.0]).contains(&Face::PosX));
        assert!(!drawn_for([-SIDE - 1.0 / 64.0, -5.0, -5.0]).contains(&Face::NegX));
    }

    #[test]
    fn negative_and_far_coordinates_work_the_same() {
        use Face::*;
        assert_eq!(
            drawn_for([-1_000_000.0, -1_000_000.0, -1_000_000.0]),
            [PosX, PosY, PosZ]
        );
        assert_eq!(
            drawn_for([1_000_000.0, 1_000_000.0, 1_000_000.0]),
            [NegX, NegY, NegZ]
        );
    }

    #[test]
    fn a_box_that_is_not_a_number_draws_everything() {
        let draws = visible_ranges([f32::NAN; 3], [f32::NAN; 3], &one_quad_each());
        assert_eq!(
            draws.as_slice(),
            [IndexRange {
                first: 0,
                count: 36
            }]
        );
    }

    #[test]
    fn empty_groups_are_skipped_and_do_not_split_a_draw() {
        // Only +X and -Y have quads; with the camera inside, both draw, as one range,
        // because the empty groups between them take no room in the buffer.
        let groups = groups([2, 0, 0, 3, 0, 0]);
        let draws = visible_ranges([-1.0; 3], [31.0; 3], &groups);
        assert_eq!(
            draws.as_slice(),
            [IndexRange {
                first: 0,
                count: 30
            }]
        );
        // A mesh with nothing facing the camera draws nothing.
        let (min, max) = chunk_box([10.0, 10.0, -5.0]);
        let only_up = self::groups([0, 0, 4, 0, 0, 0]);
        assert_eq!(visible_ranges(min, max, &only_up).as_slice(), []);
    }

    #[test]
    fn neighbouring_groups_merge_and_separated_ones_do_not() {
        use Face::*;
        // -X and +Y are neighbours in Face::ALL: one draw. +Z is apart: a second.
        let (min, max) = chunk_box([10.0, -50.0, -50.0]);
        let draws = visible_ranges(min, max, &one_quad_each());
        assert_eq!(
            faces_drawn(&draws),
            [NegX, PosY, PosZ],
            "the box test itself"
        );
        let quad = INDICES_PER_QUAD as u32;
        assert_eq!(
            draws.as_slice(),
            [
                IndexRange {
                    first: quad * index(NegX) as u32,
                    count: 2 * quad
                },
                IndexRange {
                    first: quad * index(PosZ) as u32,
                    count: quad
                },
            ]
        );
        // +X, +Y and +Z have a face between each: three draws, the most a mesh needs.
        let (min, max) = chunk_box([-50.0; 3]);
        assert_eq!(
            visible_ranges(min, max, &one_quad_each()).as_slice().len(),
            MAX_DRAWS_PER_MESH
        );
    }

    #[test]
    fn counts_that_do_not_add_up_to_the_indices_are_refused() {
        let mesh = |indices: usize, face_quads| ChunkMesh {
            vertices: Vec::new(),
            indices: vec![0; indices],
            face_quads,
        };
        let six = INDICES_PER_QUAD;
        assert!(FaceGroups::from_mesh(&mesh(six, [1, 0, 0, 0, 0, 0])).is_some());
        assert!(FaceGroups::from_mesh(&mesh(six, [0; Face::COUNT])).is_none());
        assert!(FaceGroups::from_mesh(&mesh(six, [1, 1, 0, 0, 0, 0])).is_none());
        assert!(FaceGroups::from_mesh(&mesh(3, [0; Face::COUNT])).is_none());
        // Counts whose sum overflows u32 must not wrap around to a match.
        let huge = [u32::MAX, 1, 0, 0, 0, 0];
        assert!(FaceGroups::from_mesh(&mesh(0, huge)).is_none());
        assert!(FaceGroups::from_mesh(&mesh(0, [0; Face::COUNT])).is_some());
    }

    #[test]
    fn a_fourth_draw_stretches_the_last_instead_of_dropping_faces() {
        let mut draws = DrawRanges::default();
        for start in [0, 10, 20, 30] {
            draws.push(start..start + 5);
        }
        assert_eq!(draws.as_slice().len(), MAX_DRAWS_PER_MESH);
        assert_eq!(
            draws.as_slice()[MAX_DRAWS_PER_MESH - 1],
            IndexRange {
                first: 20,
                count: 15
            }
        );
    }
}

/// The CPU cost of per-direction culling on the renderer's per-chunk path, measured against
/// the path before it (one frustum test per mesh). A timing, not a check: ignored by default.
/// Run with
/// `cargo test -p vox-render --release -- --ignored per_chunk_cpu_cost --nocapture`.
#[cfg(test)]
mod timing {
    use std::hint::black_box;
    use std::time::{Duration, Instant};

    use glam::Vec3;
    use vox_world::ChunkPos;

    use super::*;
    use crate::frustum::{Frustum, chunk_box_max};
    use crate::view::CameraView;

    /// View radius in chunks of the measured set: about 200 columns, the default view of
    /// decision 0015 at radius 8.
    const RADIUS: i32 = 8;
    /// The camera: the game's start height, looking along -Z slightly down.
    const EYE: Vec3 = Vec3::new(16.5, 60.0, 16.5);
    const PITCH: f32 = -0.3;
    /// Field of view, aspect and planes like the game's camera at radius 8.
    const FIELD_OF_VIEW_DEGREES: f32 = 70.0;
    const ASPECT: f32 = 16.0 / 9.0;
    const NEAR: f32 = 0.1;
    const FAR: f32 = 210.0;
    /// Frames timed per round and rounds per path; the fastest round counts.
    const FRAMES: usize = 20_000;
    const ROUNDS: usize = 7;
    /// Nanoseconds in a second, to print the time per mesh.
    const NANOS_PER_SECOND: f64 = 1e9;

    fn camera() -> CameraView {
        let forward = Vec3::new(0.0, PITCH.sin(), -PITCH.cos());
        let projection = glam::camera::rh::proj::vulkan::perspective(
            FIELD_OF_VIEW_DEGREES.to_radians(),
            ASPECT,
            NEAR,
            FAR,
        );
        let view = glam::camera::rh::view::look_at_mat4(Vec3::ZERO, forward, Vec3::Y);
        CameraView {
            view_projection: (projection * view).to_cols_array_2d(),
            position: EYE.to_array(),
        }
    }

    /// One surface chunk per column within the radius, with face groups shaped like terrain
    /// meshes: many tops, sides in every direction, almost no bottoms.
    fn meshes() -> Vec<(ChunkPos, FaceGroups)> {
        let mut meshes = Vec::new();
        for z in -RADIUS..=RADIUS {
            for x in -RADIUS..=RADIUS {
                if x * x + z * z > RADIUS * RADIUS {
                    continue;
                }
                let n = (x + 2 * z).unsigned_abs() % 7;
                let quads = [20 + n, 18 + n, 90 + n, n % 2, 19 + n, 21 + n];
                let total: u32 = quads.iter().sum();
                let mesh = ChunkMesh {
                    vertices: Vec::new(),
                    indices: vec![0; total as usize * INDICES_PER_QUAD],
                    face_quads: quads,
                };
                let groups = FaceGroups::from_mesh(&mesh).expect("counts match");
                meshes.push((ChunkPos::new(x, 1, z), groups));
            }
        }
        meshes
    }

    /// The fastest of [`ROUNDS`] rounds of [`FRAMES`] frames of `frame`, in nanoseconds per
    /// mesh.
    fn time_per_mesh(mesh_count: usize, mut frame: impl FnMut() -> u64) -> f64 {
        let mut best = Duration::MAX;
        for _ in 0..ROUNDS {
            let start = Instant::now();
            for _ in 0..FRAMES {
                black_box(frame());
            }
            best = best.min(start.elapsed());
        }
        best.as_secs_f64() * NANOS_PER_SECOND / (FRAMES * mesh_count) as f64
    }

    #[test]
    #[ignore = "a timing; run in release with --ignored --nocapture"]
    fn per_chunk_cpu_cost() {
        let camera = camera();
        let frustum = Frustum::from_view_projection(&camera.view_projection);
        let meshes = meshes();
        let origin = |pos: ChunkPos| camera.relative(pos.origin());
        // Before S2b: one frustum test per mesh, one draw of the whole mesh.
        let old = || {
            let mut drawn = 0;
            for (pos, _) in black_box(&meshes) {
                let min = origin(*pos);
                if frustum.may_see_box(min, chunk_box_max(min)) {
                    drawn += 1;
                }
            }
            drawn
        };
        // S2b: the same test, then the ranges of the directions facing the camera, summed
        // the way the renderer feeds its statistics.
        let new = || {
            let mut indices = 0u64;
            for (pos, groups) in black_box(&meshes) {
                let min = origin(*pos);
                let max = chunk_box_max(min);
                if !frustum.may_see_box(min, max) {
                    continue;
                }
                for range in visible_ranges(min, max, groups).as_slice() {
                    indices += u64::from(range.count);
                }
            }
            indices
        };
        let in_view = old();
        let mut draws = 0;
        for (pos, groups) in &meshes {
            let min = origin(*pos);
            let max = chunk_box_max(min);
            if frustum.may_see_box(min, max) {
                draws += visible_ranges(min, max, groups).as_slice().len();
            }
        }
        // Alternate the two paths so drift in clock speed hits both.
        let (mut old_best, mut new_best) = (f64::MAX, f64::MAX);
        for _ in 0..2 {
            old_best = old_best.min(time_per_mesh(meshes.len(), old));
            new_best = new_best.min(time_per_mesh(meshes.len(), new));
        }
        println!(
            "{} meshes, {in_view} in view, {draws} draws; per mesh: old {old_best:.2} ns, \
             new {new_best:.2} ns",
            meshes.len()
        );
    }
}
