//! The box a tile mesh occupies, the one the frustum test and the per-direction culling use
//! (decision 0015, (c)).
//!
//! A mesh of a tile of level `k` is made of cells of [`cell_size`]`(k)` blocks: its vertex
//! positions count cells (`0..=32` per axis) and the vertex shader multiplies them by the cell
//! size pushed in the origin's `w`. On X and Z the box is the whole tile, `32 * cell size`
//! blocks ([`TilePos::extent`]). On Y it is the mesh's own height, taken once at upload from
//! its vertices ([`MeshHeight`]): terrain fills only a thin band of a 512-block tile, and a
//! tile box that tall would pass the frustum test (and draw its tops from below) far more
//! often than the terrain it holds. Level 0 (chunks) uses the same rule, so a chunk's box is
//! as tall as its faces too. Pure math with no graphics API, so it is unit tested.

use vox_world::{CHUNK_SIZE, ChunkVertex, TilePos, cell_size};

use crate::view::CameraView;

/// The lowest and highest vertex `y` of a mesh, in cells of its tile.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct MeshHeight {
    /// The lowest vertex `y`, in cells.
    low: u8,
    /// The highest vertex `y`, in cells.
    high: u8,
}

impl MeshHeight {
    /// The height of a mesh with `vertices`, or `None` without vertices (nothing to draw).
    pub(crate) fn of(vertices: &[ChunkVertex]) -> Option<Self> {
        // HOT PATH: runs for every mesh upload; one linear pass, cheap next to the copy.
        let mut heights = vertices.iter().map(|vertex| vertex.position[1]);
        let first = heights.next()?;
        Some(heights.fold(
            Self {
                low: first,
                high: first,
            },
            |range, y| Self {
                low: range.low.min(y),
                high: range.high.max(y),
            },
        ))
    }
}

/// The cell size of `tile` in blocks as the `f32` the vertex shader multiplies by. Exact:
/// cell sizes are small powers of two.
pub(crate) fn cell_size_f32(tile: TilePos) -> f32 {
    cell_size(tile.level()) as f32
}

/// The box of a mesh of `tile` with `height`, relative to the camera, as `(min, max)`:
/// `relative_origin` is the tile's origin relative to the camera, rounded once to `f32`, the
/// value the vertex shader adds the scaled positions to. Every bound is computed the way the
/// shader computes a vertex on it (origin plus cells times cell size), so rounding cannot move
/// a face out of the box.
// HOT PATH: runs for every mesh every frame; a few multiplications and additions.
pub(crate) fn tile_box(
    relative_origin: [f32; 3],
    tile: TilePos,
    height: MeshHeight,
) -> ([f32; 3], [f32; 3]) {
    let cell = cell_size_f32(tile);
    let side = CHUNK_SIZE as f32 * cell;
    let [x, y, z] = relative_origin;
    let low = f32::from(height.low) * cell;
    let high = f32::from(height.high) * cell;
    ([x, y + low, z], [x + side, y + high, z + side])
}

/// Where one mesh is drawn this frame, everything relative to the camera: what the renderer
/// pushes (`origin`, `cell_size`) and the box it tests (`min`, `max`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Placement {
    /// The lowest corner of the mesh's box.
    pub(crate) min: [f32; 3],
    /// The highest corner of the mesh's box.
    pub(crate) max: [f32; 3],
    /// The tile's origin, rounded once to `f32`: the `xyz` of the origin push.
    pub(crate) origin: [f32; 3],
    /// The tile's cell size in blocks: the `w` of the origin push.
    pub(crate) cell_size: f32,
}

/// The placement of the mesh of `tile` with `height` as seen from `camera`. The only place
/// the renderer's per-mesh code derives origin, cell size and box, so they always agree.
// HOT PATH: runs for every mesh every frame; three subtractions in f64 and the box math.
pub(crate) fn placement(camera: &CameraView, tile: TilePos, height: MeshHeight) -> Placement {
    let origin = camera.relative(tile.origin());
    let (min, max) = tile_box(origin, tile, height);
    Placement {
        min,
        max,
        origin,
        cell_size: cell_size_f32(tile),
    }
}

/// The height of a mesh that reaches from the bottom to the top of its tile: a whole chunk's
/// cube at level 0. For tests that need a box without a real mesh.
#[cfg(test)]
pub(crate) const FULL_HEIGHT: MeshHeight = MeshHeight {
    low: 0,
    high: CHUNK_SIZE as u8,
};

#[cfg(test)]
mod tests {
    use vox_world::{ChunkPos, MAX_LOD_LEVEL, TextureLayer};

    use super::*;

    fn vertex(y: u8) -> ChunkVertex {
        ChunkVertex {
            position: [3, y, 7],
            face: 2,
            texture: TextureLayer(0),
        }
    }

    #[test]
    fn the_height_is_the_lowest_and_highest_vertex_y() {
        let vertices = [vertex(12), vertex(9), vertex(25), vertex(10)];
        assert_eq!(
            MeshHeight::of(&vertices),
            Some(MeshHeight { low: 9, high: 25 })
        );
        assert_eq!(
            MeshHeight::of(&[vertex(4)]),
            Some(MeshHeight { low: 4, high: 4 })
        );
        assert_eq!(MeshHeight::of(&[]), None);
    }

    #[test]
    fn a_chunk_box_is_the_chunk_on_x_and_z_and_its_faces_on_y() {
        let tile = TilePos::chunk(ChunkPos::new(1, 0, -2));
        let height = MeshHeight { low: 8, high: 24 };
        let (min, max) = tile_box([-16.0, -60.0, 4.0], tile, height);
        assert_eq!(min, [-16.0, -52.0, 4.0]);
        assert_eq!(max, [16.0, -36.0, 36.0]);
    }

    #[test]
    fn every_level_spans_its_extent_in_blocks() {
        for level in 0..=MAX_LOD_LEVEL {
            let tile = TilePos::new(level, ChunkPos::new(-3, -1, 2));
            let (min, max) = tile_box([0.0; 3], tile, FULL_HEIGHT);
            let extent = tile.extent() as f32;
            assert_eq!(min, [0.0; 3], "level {level}");
            assert_eq!(max, [extent; 3], "level {level}");
            assert_eq!(cell_size_f32(tile), (1 << level) as f32);
        }
        // Hand-computed: level 4 is 32 cells of 16 blocks.
        let coarsest = TilePos::new(MAX_LOD_LEVEL, ChunkPos::ORIGIN);
        assert_eq!(tile_box([0.0; 3], coarsest, FULL_HEIGHT).1, [512.0; 3]);
    }

    #[test]
    fn a_level_2_tile_at_negative_coordinates_lies_where_its_cells_are() {
        // Level 2: cells of 4 blocks, tiles of 128. Tile (-1, -1, -2) starts at block
        // (-128, -128, -256); a mesh from cell row 30 to 32 spans blocks -8..0 in y.
        let tile = TilePos::new(2, ChunkPos::new(-1, -1, -2));
        assert_eq!(tile.origin(), [-128, -128, -256]);
        let camera = CameraView {
            position: [10.0, 20.0, -30.0],
            ..CameraView::IDENTITY
        };
        let origin = camera.relative(tile.origin());
        let (min, max) = tile_box(origin, tile, MeshHeight { low: 30, high: 32 });
        assert_eq!(min, [-138.0, -28.0, -226.0]);
        assert_eq!(max, [-10.0, -20.0, -98.0]);
    }

    #[test]
    fn a_level_2_placement_wires_origin_cell_size_height_and_box_together() {
        // Level 2: cells of 4 blocks, tiles of 128. Tile (-1, -1, -2) starts at block
        // (-128, -128, -256); the camera at (10.5, 20, -30) sees it at (-138.5, -148, -226).
        // Mesh rows 30 to 32 are blocks 120 to 128 up from the tile's origin.
        let tile = TilePos::new(2, ChunkPos::new(-1, -1, -2));
        let camera = CameraView {
            position: [10.5, 20.0, -30.0],
            ..CameraView::IDENTITY
        };
        let placed = placement(&camera, tile, MeshHeight { low: 30, high: 32 });
        assert_eq!(
            placed,
            Placement {
                min: [-138.5, -28.0, -226.0],
                max: [-10.5, -20.0, -98.0],
                origin: [-138.5, -148.0, -226.0],
                cell_size: 4.0,
            }
        );
    }

    #[test]
    fn the_box_holds_every_vertex_where_the_shader_puts_it() {
        // The shader computes origin + position * cell in f32; so does the box, for the
        // extreme positions, at an origin that does not round evenly.
        let origin = [-1234.567_f32, 89.123, 4567.891];
        for level in 0..=MAX_LOD_LEVEL {
            let tile = TilePos::new(level, ChunkPos::ORIGIN);
            let cell = cell_size_f32(tile);
            let height = MeshHeight { low: 3, high: 29 };
            let (min, max) = tile_box(origin, tile, height);
            let place = |axis: usize, cells: u8| origin[axis] + f32::from(cells) * cell;
            let side = CHUNK_SIZE as u8;
            assert_eq!([place(0, 0), place(1, 3), place(2, 0)], min);
            assert_eq!([place(0, side), place(1, 29), place(2, side)], max);
        }
    }
}
