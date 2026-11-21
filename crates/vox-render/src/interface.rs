//! What the chunk pipeline provides to its shaders: vertex attributes and push constants.
//!
//! This is the single description of that contract. The Vulkan backend builds its
//! vertex input state from it, and the shader loader checks every shader against it,
//! so a shader pack cannot ask for an input the pipeline does not feed.
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

/// Bytes from one vertex to the next.
pub(crate) const VERTEX_STRIDE: u32 = 6;

/// Size of the push constants, visible to the vertex stage only: one column-major
/// `mat4`, the camera's view-projection. Vulkan 1.0 guarantees at least 128 bytes.
pub(crate) const PUSH_CONSTANT_SIZE: u32 = 64;

/// How an attribute is stored in the vertex buffer. Every format is an unsigned
/// integer format, so the shader always sees 32-bit unsigned components.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AttributeFormat {
    /// Four `u8` values; the shader sees a `uvec4`.
    Uint8x4,
    /// One `u16` value; the shader sees a `uint`.
    Uint16,
}

impl AttributeFormat {
    /// Number of components the shader input has.
    pub(crate) const fn components(self) -> u32 {
        match self {
            Self::Uint8x4 => 4,
            Self::Uint16 => 1,
        }
    }

    /// The GLSL type a shader must declare for this attribute.
    pub(crate) const fn glsl_type(self) -> &'static str {
        match self {
            Self::Uint8x4 => "uvec4",
            Self::Uint16 => "uint",
        }
    }
}

/// One vertex attribute the pipeline feeds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct VertexAttribute {
    /// `layout(location = ...)` in the vertex shader.
    pub(crate) location: u32,
    /// Byte offset inside a vertex.
    pub(crate) offset: u32,
    /// Storage format in the vertex buffer.
    pub(crate) format: AttributeFormat,
}

/// Every vertex attribute, matching the layout table in the module docs.
pub(crate) const VERTEX_ATTRIBUTES: [VertexAttribute; 2] = [
    VertexAttribute {
        location: 0,
        offset: 0,
        format: AttributeFormat::Uint8x4,
    },
    VertexAttribute {
        location: 1,
        offset: 4,
        format: AttributeFormat::Uint16,
    },
];

/// The attribute at `location`, if the pipeline feeds one there.
pub(crate) fn vertex_attribute(location: u32) -> Option<VertexAttribute> {
    VERTEX_ATTRIBUTES
        .into_iter()
        .find(|attribute| attribute.location == location)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Bytes an attribute occupies in a vertex.
    fn size(format: AttributeFormat) -> u32 {
        match format {
            AttributeFormat::Uint8x4 => 4,
            AttributeFormat::Uint16 => 2,
        }
    }

    #[test]
    fn attributes_cover_the_whole_stride() {
        let end = VERTEX_ATTRIBUTES
            .iter()
            .map(|a| a.offset + size(a.format))
            .max();
        assert_eq!(end, Some(VERTEX_STRIDE));
    }

    #[test]
    fn attributes_do_not_overlap() {
        let [first, second] = VERTEX_ATTRIBUTES;
        assert!(first.offset + size(first.format) <= second.offset);
    }

    #[test]
    fn lookup_by_location() {
        assert_eq!(vertex_attribute(1), Some(VERTEX_ATTRIBUTES[1]));
        assert_eq!(vertex_attribute(2), None);
    }
}
