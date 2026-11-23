//! What the overlay pipeline provides to its shaders: vertex attributes and push constants.
//!
//! # Vertex layout
//! Each vertex is 12 bytes, little-endian, in one buffer binding. A rectangle is six
//! vertices (two triangles, no index buffer).
//!
//! | byte  | content                                 | attribute                         |
//! |-------|-----------------------------------------|-----------------------------------|
//! | 0..8  | position x, y in window pixels (`f32`)  | location 0, 2 x `f32`, `vec2`     |
//! | 8..12 | color r, g, b, a (`u8`, 255 is 1.0)     | location 1, 4 x unorm `u8`, `vec4` |
//!
//! Pixels are measured from the top-left corner of the window's drawing area, x to the
//! right and y down. Both formats are required vertex formats in Vulkan 1.0.
//!
//! # Push constants
//! One range for the vertex stage only, 8 bytes at offset 0, pushed once per frame:
//!
//! | byte | content                                                 | GLSL   |
//! |------|---------------------------------------------------------|--------|
//! | 0..8 | width and height of the swapchain image in pixels (`f32`) | `vec2` |
//!
//! The vertex shader maps a pixel to clip space as `pixel / size * 2 - 1` on both axes.
//! Vulkan clip space has y pointing down, like the pixels, so nothing is flipped.

use super::{
    AttributeFormat, F32_SIZE, GUARANTEED_PUSH_CONSTANT_SIZE, PipelineInterface, PushConstants,
    Stage, VertexAttribute,
};

/// Byte offset of the color inside a vertex, right after the position.
pub(crate) const COLOR_OFFSET: u32 = AttributeFormat::Float32x2.size();

/// Bytes from one vertex to the next.
pub(crate) const VERTEX_STRIDE: u32 = COLOR_OFFSET + AttributeFormat::Unorm8x4.size();

/// Size of the push constants: the window width and height as two `f32`.
pub(crate) const PUSH_CONSTANT_SIZE: u32 = 2 * F32_SIZE;

// Staying within the guaranteed size means no capability check is needed.
const _: () = assert!(PUSH_CONSTANT_SIZE <= GUARANTEED_PUSH_CONSTANT_SIZE);

/// Every vertex attribute, matching the layout table in the module docs.
const ATTRIBUTES: [VertexAttribute; 2] = [
    VertexAttribute {
        location: 0,
        offset: 0,
        format: AttributeFormat::Float32x2,
    },
    VertexAttribute {
        location: 1,
        offset: COLOR_OFFSET,
        format: AttributeFormat::Unorm8x4,
    },
];

/// The overlay pipeline's interface, as the module docs describe it.
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
    fn vertices_are_a_vec2_then_four_color_bytes() {
        assert_eq!(COLOR_OFFSET, 8);
        assert_eq!(VERTEX_STRIDE, 12);
    }

    #[test]
    fn push_constants_are_one_vec2() {
        assert_eq!(PUSH_CONSTANT_SIZE, 8);
        assert_eq!(INTERFACE.push_constants.stage, Stage::Vertex);
    }
}
