//! What the chunk pipeline provides to its shaders: vertex attributes, push constants and
//! the block textures.
//!
//! # Vertex layout
//! Each vertex is 6 bytes, little-endian, in one buffer binding:
//!
//! | byte | content                 | attribute                              |
//! |------|-------------------------|----------------------------------------|
//! | 0..3 | position x, y, z (`u8`) | location 0, 4 x `u8`, `uvec4` as `xyz` |
//! | 3    | face index (`u8`)       | location 0, 4 x `u8`, `uvec4` as `w`   |
//! | 4..6 | texture layer (`u16`)   | location 1, 1 x `u16`, `uint`          |
//!
//! Location 0 reads four bytes at once because a three-byte vertex format is not
//! guaranteed in Vulkan 1.0, while a four-byte one is. The fourth byte happens to be
//! the face index, so nothing is wasted.
//!
//! The texture layer is the layer of the block texture array the face is drawn with; the
//! mesher looks it up per quad from the block and the face, so the block id itself does not
//! reach the shaders. No texture coordinates are stored: corners are whole blocks, so the
//! built-in vertex shader derives them from the position (the table next to `FACE_SHADE` in
//! `chunk.vert`).
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
//!
//! # Descriptor resources
//! One descriptor set (set 0) for the fragment stage only, holding the block textures
//! (decision 0013): a 2D array image, one layer per texture, and a separate sampler that
//! repeats and filters with `NEAREST`.
//!
//! | binding | content                 | GLSL                                       |
//! |---------|-------------------------|--------------------------------------------|
//! | 0       | the block texture array | `uniform texture2DArray block_textures;`   |
//! | 1       | its sampler             | `uniform sampler block_sampler;`           |
//!
//! Sample with `texture(sampler2DArray(block_textures, block_sampler), vec3(uv, layer))`.
//! The image and sampler are separate because naga's GLSL front end rejects a combined
//! `sampler2DArray` uniform. A shader may declare neither, one or both.

use super::{
    AttributeFormat, F32_SIZE, GUARANTEED_PUSH_CONSTANT_SIZE, PipelineInterface, PushConstants,
    RESOURCE_SET, ResourceBinding, ResourceKind, Stage, VertexAttribute,
};

/// Byte offset of the texture layer inside a vertex, right after the position and face bytes.
const LAYER_OFFSET: u32 = AttributeFormat::Uint8x4.size();

/// Bytes from one vertex to the next.
pub(crate) const VERTEX_STRIDE: u32 = LAYER_OFFSET + AttributeFormat::Uint16.size();

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
        offset: LAYER_OFFSET,
        format: AttributeFormat::Uint16,
    },
];

/// Binding of the block texture array in set [`RESOURCE_SET`].
pub(crate) const BLOCK_TEXTURES_BINDING: u32 = 0;

/// Binding of the block textures' sampler in set [`RESOURCE_SET`].
pub(crate) const BLOCK_SAMPLER_BINDING: u32 = 1;

/// Every descriptor resource, matching the table in the module docs.
const RESOURCES: [ResourceBinding; 2] = [
    ResourceBinding {
        set: RESOURCE_SET,
        binding: BLOCK_TEXTURES_BINDING,
        kind: ResourceKind::SampledImage2DArray,
        stage: Stage::Fragment,
    },
    ResourceBinding {
        set: RESOURCE_SET,
        binding: BLOCK_SAMPLER_BINDING,
        kind: ResourceKind::Sampler,
        stage: Stage::Fragment,
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
    resources: &RESOURCES,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vertices_are_six_bytes() {
        assert_eq!(VERTEX_STRIDE, 6);
        assert_eq!(LAYER_OFFSET, 4);
    }

    #[test]
    fn push_constants_are_a_matrix_then_a_vec4() {
        assert_eq!(CHUNK_ORIGIN_OFFSET, 16 * 4);
        assert_eq!(PUSH_CONSTANT_SIZE, CHUNK_ORIGIN_OFFSET + 4 * 4);
    }

    #[test]
    fn textures_then_sampler_for_the_fragment_stage() {
        let [image, sampler] = RESOURCES;
        assert_eq!((image.set, image.binding), (0, 0));
        assert_eq!(image.kind, ResourceKind::SampledImage2DArray);
        assert_eq!((sampler.set, sampler.binding), (0, 1));
        assert_eq!(sampler.kind, ResourceKind::Sampler);
        assert!(RESOURCES.iter().all(|r| r.stage == Stage::Fragment));
    }

    #[test]
    fn attributes_are_position_face_then_texture_layer() {
        let [first, second] = ATTRIBUTES;
        assert_eq!((first.location, first.offset), (0, 0));
        assert_eq!((second.location, second.offset), (1, 4));
    }
}
