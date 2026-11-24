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
//! Positions reach the shaders **relative to the camera** (decision 0015): the renderer
//! subtracts the camera position from each chunk origin on the CPU and pushes a matrix
//! without the camera's translation, so the numbers the GPU sees stay small and `f32`
//! keeps sub-block precision however far the camera is from the world origin.
//!
//! The renderer skips chunks whose box (the chunk's 32-block cube at its origin) lies outside
//! the view, so a vertex shader must place every vertex inside that box:
//! `chunk_origin + position` with no displacement. A shader pack that moves vertices
//! (waves, wind) must keep them within the box, or chunks will disappear at the screen edges.
//!
//! # Push constants
//! One range for the vertex and the fragment stage, 112 bytes at offset 0. Floats are
//! little-endian `f32`. Every push names both stages, so either shader may declare the
//! whole block (or a prefix of it):
//!
//! | byte    | content                                                         | GLSL   |
//! |---------|-----------------------------------------------------------------|--------|
//! | 0..64   | the camera's view-projection without its translation, column-major | `mat4` |
//! | 64..80  | the chunk's origin relative to the camera in `xyz`; `w` the cell size | `vec4` |
//! | 80..96  | fog: `x` start, `y` end (blocks from the camera), `z` `1 / (end - start)`, `w` 0 | `vec4` |
//! | 96..112 | fog colour, `rgb` the sky colour the frame is cleared to, `a` 1  | `vec4` |
//!
//! The matrix and the fog are pushed once per frame, the origin once per chunk drawn.
//!
//! - **Cell size:** the edge of one mesh cell in blocks; always 1.0 today (chunk meshes are
//!   made of blocks). The built-in shaders ignore it; coarser level-of-detail tiles will use
//!   it (decision 0015, S5).
//! - **Fog:** the fraction of a fragment's colour replaced by the fog colour is
//!   `smoothstep(start, end, distance)`, with `distance` the straight-line (radial) distance
//!   from the camera, so the fog does not change when the camera turns. `z` saves the
//!   division and is 0 while fog is off (`start` and `end` are 0 then): a shader that
//!   computes `clamp((distance - x) * z, 0, 1)` needs no special case. Do not call GLSL
//!   `smoothstep(x, y, ...)` directly, it is undefined while fog is off.
//! - **Fog colour:** equal to the clear colour, so a fully fogged face cannot be told from
//!   the sky and the end of the loaded terrain is invisible.
//!
//! A `mat4` followed by `vec4`s has these offsets under the std430 rules push constants
//! use, so a shader can declare the block as written in the table.
//!
//! # Descriptor resources
//! One descriptor set (set 0) for the fragment stage only, holding the block textures
//! (decision 0013): a 2D array image, one layer per texture, with a full mip chain down to
//! 1x1, and a separate sampler that repeats, filters with `NEAREST` within a level and
//! blends `LINEAR` between levels.
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

/// Byte offset of the fog distances inside the push constants, right after the origin.
pub(crate) const FOG_OFFSET: u32 = CHUNK_ORIGIN_OFFSET + VEC4_SIZE;

/// Byte offset of the fog colour inside the push constants, right after the distances.
pub(crate) const FOG_COLOR_OFFSET: u32 = FOG_OFFSET + VEC4_SIZE;

/// Size of the push constants, see the table in the module docs: the matrix, the origin,
/// the fog distances and the fog colour.
pub(crate) const PUSH_CONSTANT_SIZE: u32 = FOG_COLOR_OFFSET + VEC4_SIZE;

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
        // The vertex stage places the chunk, the fragment stage fogs it.
        stages: &[Stage::Vertex, Stage::Fragment],
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
    fn push_constants_are_a_matrix_then_three_vec4s() {
        assert_eq!(CHUNK_ORIGIN_OFFSET, 64);
        assert_eq!(FOG_OFFSET, 80);
        assert_eq!(FOG_COLOR_OFFSET, 96);
        assert_eq!(PUSH_CONSTANT_SIZE, 112);
    }

    #[test]
    fn both_stages_read_the_push_constants() {
        let push = INTERFACE.push_constants;
        assert!(push.visible_to(Stage::Vertex) && push.visible_to(Stage::Fragment));
        assert_eq!(push.stage_names(), "vertex and fragment");
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
