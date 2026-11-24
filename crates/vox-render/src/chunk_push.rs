//! The chunk pipeline's push constants as bytes, laid out as `crate::interface::chunk`
//! describes: the whole block once per frame, then the origin and cell size once per mesh.
//!
//! Pure byte packing with no graphics API, so the layout is unit tested.

use crate::interface::chunk::{
    CHUNK_ORIGIN_OFFSET, FOG_COLOR_OFFSET, FOG_OFFSET, PUSH_CONSTANT_SIZE,
};

/// Bytes of the whole block, pushed once per frame.
pub(crate) const FRAME_BYTES: usize = PUSH_CONSTANT_SIZE as usize;

/// Bytes of the origin `vec4`, pushed once per mesh at [`CHUNK_ORIGIN_OFFSET`].
pub(crate) const ORIGIN_BYTES: usize = (FOG_OFFSET - CHUNK_ORIGIN_OFFSET) as usize;

/// The cell size in the frame push's origin `w`: one block, that of a chunk. Every mesh
/// pushes its own origin and cell size before it is drawn, so this value is never used by a
/// draw; it only keeps the block well defined.
const BLOCK_CELL_SIZE: f32 = 1.0;

/// The whole block for one frame: the camera-relative view-projection, an origin at the
/// camera (replaced by every mesh's own push), the fog `vec4` (`crate::Fog::push_values`)
/// and the fog colour. Each float little-endian, matrix columns one after another.
// HOT PATH: runs once per frame; 28 floats written into a stack array.
pub(crate) fn frame_bytes(
    view_projection: &[[f32; 4]; 4],
    fog: [f32; 4],
    fog_color: [f32; 4],
) -> [u8; FRAME_BYTES] {
    let mut bytes = [0; FRAME_BYTES];
    write_floats(&mut bytes, view_projection.iter().flatten().copied());
    bytes[CHUNK_ORIGIN_OFFSET as usize..FOG_OFFSET as usize]
        .copy_from_slice(&origin_bytes([0.0; 3], BLOCK_CELL_SIZE));
    write_floats(&mut bytes[FOG_OFFSET as usize..], fog);
    write_floats(&mut bytes[FOG_COLOR_OFFSET as usize..], fog_color);
    bytes
}

/// The origin `vec4` of one mesh: its tile's corner relative to the camera in blocks, and
/// in `w` the tile's cell size in blocks (1 for a chunk, `2^level` for a tile), each float
/// little-endian.
// HOT PATH: runs for every mesh drawn every frame; 4 floats written into a stack array.
pub(crate) fn origin_bytes(relative_origin: [f32; 3], cell_size: f32) -> [u8; ORIGIN_BYTES] {
    let [x, y, z] = relative_origin;
    let mut bytes = [0; ORIGIN_BYTES];
    write_floats(&mut bytes, [x, y, z, cell_size]);
    bytes
}

/// Writes `floats` little-endian into `bytes`, four bytes each, as far as both reach.
fn write_floats(bytes: &mut [u8], floats: impl IntoIterator<Item = f32>) {
    for (chunk, value) in bytes.chunks_exact_mut(size_of::<f32>()).zip(floats) {
        chunk.copy_from_slice(&value.to_le_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const IDENTITY: [[f32; 4]; 4] = [
        [1.0, 0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ];

    /// The four floats at byte `offset`.
    fn vec4_at(bytes: &[u8], offset: usize) -> [f32; 4] {
        std::array::from_fn(|i| {
            let start = offset + i * 4;
            f32::from_le_bytes(bytes[start..start + 4].try_into().unwrap())
        })
    }

    #[test]
    fn the_frame_push_fills_the_whole_block() {
        assert_eq!(FRAME_BYTES, 112);
        assert_eq!(ORIGIN_BYTES, 16);
    }

    #[test]
    fn the_frame_push_is_the_matrix_columns_then_origin_fog_and_colour() {
        let mut matrix = IDENTITY;
        matrix[3][0] = 2.0; // translation x, first value of the last column
        let fog = [100.0, 300.0, 0.005, 0.0];
        let color = [0.25, 0.5, 0.75, 1.0];
        let bytes = frame_bytes(&matrix, fog, color);
        assert_eq!(vec4_at(&bytes, 0), [1.0, 0.0, 0.0, 0.0]);
        assert_eq!(vec4_at(&bytes, 48), [2.0, 0.0, 0.0, 1.0]);
        assert_eq!(vec4_at(&bytes, 64), [0.0, 0.0, 0.0, BLOCK_CELL_SIZE]);
        assert_eq!(vec4_at(&bytes, 80), fog);
        assert_eq!(vec4_at(&bytes, 96), color);
    }

    #[test]
    fn the_origin_push_is_the_relative_corner_and_the_cell_size() {
        let bytes = origin_bytes([-32.5, 64.0, 96.25], 1.0);
        assert_eq!(vec4_at(&bytes, 0), [-32.5, 64.0, 96.25, 1.0]);
    }

    #[test]
    fn every_level_pushes_its_cell_size_in_w() {
        use vox_world::{ChunkPos, MAX_LOD_LEVEL, TilePos};

        use crate::tile_box::cell_size_f32;
        let expected = [1.0, 2.0, 4.0, 8.0, 16.0];
        for level in 0..=MAX_LOD_LEVEL {
            let tile = TilePos::new(level, ChunkPos::new(-1, 0, 3));
            let bytes = origin_bytes([1.0, 2.0, 3.0], cell_size_f32(tile));
            let pushed = vec4_at(&bytes, 0);
            assert_eq!(pushed, [1.0, 2.0, 3.0, expected[usize::from(level)]]);
        }
    }

    #[test]
    fn pushes_are_little_endian() {
        let bytes = frame_bytes(&IDENTITY, [0.0; 4], [0.0; 4]);
        // 1.0 is 0x3F80_0000; little-endian puts the low byte first.
        assert_eq!(bytes[..4], [0x00, 0x00, 0x80, 0x3F]);
    }
}
