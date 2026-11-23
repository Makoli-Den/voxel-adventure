//! What each pipeline provides to its shaders: vertex attributes and push constants.
//!
//! Every pipeline has one [`PipelineInterface`], defined in its own module. It is the
//! single description of that contract: the Vulkan backend builds the pipeline's vertex
//! input state and push-constant range from it, and the shader loader checks every
//! shader of that pipeline against it, so a shader pack cannot ask for an input the
//! pipeline does not feed.

pub(crate) mod chunk;
pub(crate) mod overlay;

use naga::Scalar;

/// Push-constant bytes every Vulkan 1.0 device guarantees (`maxPushConstantsSize` is at
/// least this). A pipeline that stays within it needs no capability check.
pub(crate) const GUARANTEED_PUSH_CONSTANT_SIZE: u32 = 128;

/// Bytes of one 32-bit float, in vertex data and in push constants.
const F32_SIZE: u32 = size_of::<f32>() as u32;

/// How an attribute is stored in the vertex buffer, and therefore what the shader sees.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AttributeFormat {
    /// Four `u8` values; the shader sees a `uvec4`.
    Uint8x4,
    /// One `u16` value; the shader sees a `uint`.
    Uint16,
    /// Two `f32` values; the shader sees a `vec2`.
    Float32x2,
    /// Four `u8` values normalized to `0.0..=1.0` (`value / 255`); the shader sees a `vec4`.
    Unorm8x4,
}

impl AttributeFormat {
    /// Number of components the shader input has.
    pub(crate) const fn components(self) -> u32 {
        match self {
            Self::Uint8x4 | Self::Unorm8x4 => 4,
            Self::Float32x2 => 2,
            Self::Uint16 => 1,
        }
    }

    /// The scalar type of each component as the shader sees it: integer formats arrive
    /// as 32-bit unsigned integers, float and normalized formats as 32-bit floats.
    pub(crate) const fn shader_scalar(self) -> Scalar {
        match self {
            Self::Uint8x4 | Self::Uint16 => Scalar::U32,
            Self::Float32x2 | Self::Unorm8x4 => Scalar::F32,
        }
    }

    /// The GLSL type a shader must declare for this attribute.
    pub(crate) const fn glsl_type(self) -> &'static str {
        match self {
            Self::Uint8x4 => "uvec4",
            Self::Uint16 => "uint",
            Self::Float32x2 => "vec2",
            Self::Unorm8x4 => "vec4",
        }
    }

    /// Bytes the attribute occupies in a vertex: its components times the bytes each
    /// one is stored in.
    pub(crate) const fn size(self) -> u32 {
        self.components() * self.component_size()
    }

    /// Bytes one component is stored in, in the vertex buffer.
    const fn component_size(self) -> u32 {
        match self {
            Self::Uint8x4 | Self::Unorm8x4 => size_of::<u8>() as u32,
            Self::Uint16 => size_of::<u16>() as u32,
            Self::Float32x2 => F32_SIZE,
        }
    }
}

/// A shader stage the pipelines use. Only these two exist in a graphics pipeline built
/// by the renderer, so every match over it is exhaustive.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Stage {
    /// The vertex shader.
    Vertex,
    /// The fragment shader.
    Fragment,
}

impl Stage {
    /// The stage's name as error messages use it.
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Vertex => "vertex",
            Self::Fragment => "fragment",
        }
    }
}

/// One vertex attribute a pipeline feeds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct VertexAttribute {
    /// `layout(location = ...)` in the vertex shader.
    pub(crate) location: u32,
    /// Byte offset inside a vertex.
    pub(crate) offset: u32,
    /// Storage format in the vertex buffer.
    pub(crate) format: AttributeFormat,
}

/// The one push-constant range of a pipeline, at offset 0.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PushConstants {
    /// The only shader stage that may read them.
    pub(crate) stage: Stage,
    /// Size of the range in bytes.
    pub(crate) size: u32,
}

/// Everything a pipeline provides to its shaders.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PipelineInterface {
    /// Bytes from one vertex to the next, in a single buffer binding.
    pub(crate) vertex_stride: u32,
    /// Every vertex attribute, each at a distinct location.
    pub(crate) attributes: &'static [VertexAttribute],
    /// The push-constant range.
    pub(crate) push_constants: PushConstants,
}

impl PipelineInterface {
    /// The attribute at `location`, if the pipeline feeds one there.
    pub(crate) fn attribute(&self, location: u32) -> Option<VertexAttribute> {
        self.attributes
            .iter()
            .copied()
            .find(|attribute| attribute.location == location)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [&PipelineInterface; 2] = [&chunk::INTERFACE, &overlay::INTERFACE];

    fn end(attribute: &VertexAttribute) -> u32 {
        attribute.offset + attribute.format.size()
    }

    #[test]
    fn attributes_cover_the_whole_stride() {
        for interface in ALL {
            let last = interface.attributes.iter().map(end).max();
            assert_eq!(last, Some(interface.vertex_stride), "{interface:?}");
        }
    }

    #[test]
    fn attributes_do_not_overlap() {
        for interface in ALL {
            for (i, a) in interface.attributes.iter().enumerate() {
                for b in &interface.attributes[i + 1..] {
                    assert!(end(a) <= b.offset || end(b) <= a.offset, "{a:?} {b:?}");
                }
            }
        }
    }

    #[test]
    fn locations_are_distinct() {
        for interface in ALL {
            for (i, a) in interface.attributes.iter().enumerate() {
                for b in &interface.attributes[i + 1..] {
                    assert_ne!(a.location, b.location);
                }
            }
        }
    }

    #[test]
    fn push_constants_fit_every_device() {
        for interface in ALL {
            assert!(interface.push_constants.size <= GUARANTEED_PUSH_CONSTANT_SIZE);
            // Vulkan requires the size of a push-constant range to be a multiple of 4.
            assert_eq!(interface.push_constants.size % 4, 0);
        }
    }

    #[test]
    fn formats_match_their_glsl_types() {
        assert_eq!(AttributeFormat::Float32x2.size(), 8);
        assert_eq!(AttributeFormat::Unorm8x4.size(), 4);
        assert_eq!(AttributeFormat::Uint16.size(), 2);
        assert_eq!(AttributeFormat::Unorm8x4.shader_scalar(), Scalar::F32);
        assert_eq!(AttributeFormat::Uint8x4.shader_scalar(), Scalar::U32);
        assert_eq!(AttributeFormat::Unorm8x4.glsl_type(), "vec4");
    }

    #[test]
    fn lookup_by_location() {
        let chunk = chunk::INTERFACE;
        assert_eq!(chunk.attribute(1), Some(chunk.attributes[1]));
        assert_eq!(chunk.attribute(2), None);
    }
}
