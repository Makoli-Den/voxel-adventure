//! What the chunk pipeline provides to its shaders: vertex attributes and push constants.
//!
//! # Vertex layout
//! Each vertex is 6 bytes, little-endian, in one buffer binding:
//!
//! | byte | content                 | attribute                              |
//! |------|-------------------------|----------------------------------------|
//! | 0..3 | position x, y, z (`u8`) | location 0, 4 x `u8`, `uvec4` as `xyz` |
//! | 3    | face index (`u8`)       | location 0, 4 x `u8`, `uvec4` as `w`   |
//! | 4..6 | block id (`u16`)        | location 1, 1 x `u16`, `uint`          |
//!
//! Location 0 reads four bytes at once because a three-byte vertex format is not
//! guaranteed in Vulkan 1.0, while a four-byte one is. The fourth byte happens to be
//! the face index, so nothing is wasted.
//!
//! # Shader contract
//! The renderer skips chunks whose box (the chunk's 32-block cube at its origin) lies outside
//! the view, so a vertex shader must place every vertex inside that box:
//! `chunk_origin + position` with no displacement. A shader pack that moves vertices
//! (waves, wind) must keep them within the box, or chunks will disappear at the screen edges.
//!
//! # Push constants
//! One range for the vertex stage only, 80 bytes at offset 0, pushed once per chunk
//! drawn. Floats are little-endian `f32`:
//!
//! | byte   | content                                             | GLSL   |
//! |--------|-----------------------------------------------------|--------|
//! | 0..64  | the camera's view-projection, column-major          | `mat4` |
//! | 64..80 | the chunk's origin in blocks in `xyz`, `w` is 0     | `vec4` |
//!
//! A `mat4` followed by a `vec4` has these offsets under the std430 rules push
//! constants use, so a shader can declare the block as written in the table.

use super::{
    AttributeFormat, F32_SIZE, GUARANTEED_PUSH_CONSTANT_SIZE, PipelineInterface, PushConstants,
    Stage, VertexAttribute,
};

/// Byte offset of the block id inside a vertex, right after the position and face bytes.
const BLOCK_OFFSET: u32 = AttributeFormat::Uint8x4.size();

/// Bytes from one vertex to the next.
pub(crate) const VERTEX_STRIDE: u32 = BLOCK_OFFSET + AttributeFormat::Uint16.size();

/// Floats in a GLSL `vec4`: the four homogeneous coordinates `x, y, z, w`.
const VEC4_FLOATS: u32 = 4;

/// Bytes of a `vec4` of `f32`, tightly packed as std430 lays it out.
const VEC4_SIZE: u32 = VEC4_FLOATS * F32_SIZE;

/// Bytes of a `mat4`: one `vec4` per column, one column per homogeneous coordinate.
const MAT4_SIZE: u32 = VEC4_FLOATS * VEC4_SIZE;

/// Byte offset of the chunk origin inside the push constants, right after the matrix.
pub(crate) const CHUNK_ORIGIN_OFFSET: u32 = MAT4_SIZE;

/// Size of the push constants, see the table in the module docs: the matrix, then the
/// origin `vec4` filling the rest of the block.
pub(crate) const PUSH_CONSTANT_SIZE: u32 = CHUNK_ORIGIN_OFFSET + VEC4_SIZE;

// Staying within the guaranteed size means no capability check is needed.
const _: () = assert!(PUSH_CONSTANT_SIZE <= GUARANTEED_PUSH_CONSTANT_SIZE);

/// Every vertex attribute, matching the layout table in the module docs.
const ATTRIBUTES: [VertexAttribute; 2] = [
    VertexAttribute {
        location: 0,
        offset: 0,
        format: AttributeFormat::Uint8x4,
    },
    VertexAttribute {
        location: 1,
        offset: BLOCK_OFFSET,
        format: AttributeFormat::Uint16,
    },
];

/// The chunk pipeline's interface, as the module docs describe it.
pub(crate) const INTERFACE: PipelineInterface = PipelineInterface {
    vertex_stride: VERTEX_STRIDE,
    attributes: &ATTRIBUTES,
    push_constants: PushConstants {
        stage: Stage::Vertex,
        size: PUSH_CONSTANT_SIZE,
    },
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vertices_are_six_bytes() {
        assert_eq!(VERTEX_STRIDE, 6);
        assert_eq!(BLOCK_OFFSET, 4);
    }

    #[test]
    fn push_constants_are_a_matrix_then_a_vec4() {
        assert_eq!(CHUNK_ORIGIN_OFFSET, 16 * 4);
        assert_eq!(PUSH_CONSTANT_SIZE, CHUNK_ORIGIN_OFFSET + 4 * 4);
    }

    #[test]
    fn attributes_are_position_face_then_block() {
        let [first, second] = ATTRIBUTES;
        assert_eq!((first.location, first.offset), (0, 0));
        assert_eq!((second.location, second.offset), (1, 4));
    }
}
