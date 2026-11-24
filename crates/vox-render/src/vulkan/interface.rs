//! Vulkan state built from a pipeline's interface description (`crate::interface`).
//!
//! The description is the single source of truth: the shader check reads it, and the
//! vertex input state, the push-constant range and the descriptor set layout bindings here
//! are derived from it, so the two cannot drift apart.

use ash::vk;

use crate::interface::{AttributeFormat, PipelineInterface, ResourceKind, Stage};

/// The vertex buffer binding: one tightly packed vertex after another.
pub fn vertex_bindings(interface: &PipelineInterface) -> [vk::VertexInputBindingDescription; 1] {
    [vk::VertexInputBindingDescription {
        binding: 0,
        stride: interface.vertex_stride,
        input_rate: vk::VertexInputRate::VERTEX,
    }]
}

/// The vertex attributes, all in binding 0.
pub fn vertex_attributes(
    interface: &PipelineInterface,
) -> Vec<vk::VertexInputAttributeDescription> {
    interface
        .attributes
        .iter()
        .map(|attribute| vk::VertexInputAttributeDescription {
            location: attribute.location,
            binding: 0,
            format: vk_format(attribute.format),
            offset: attribute.offset,
        })
        .collect()
}

/// The pipeline layout's only push-constant range, at offset 0.
pub fn push_constant_ranges(interface: &PipelineInterface) -> [vk::PushConstantRange; 1] {
    [vk::PushConstantRange {
        stage_flags: push_constant_stages(interface),
        offset: 0,
        size: interface.push_constants.size,
    }]
}

/// The stage flags a `vkCmdPushConstants` for this pipeline must name: exactly those of
/// the range.
pub fn push_constant_stages(interface: &PipelineInterface) -> vk::ShaderStageFlags {
    interface
        .push_constants
        .stages
        .iter()
        .fold(vk::ShaderStageFlags::empty(), |flags, &stage| {
            flags | stage_flags(stage)
        })
}

/// The bindings of the pipeline's descriptor set layout (set `RESOURCE_SET`), one
/// descriptor each, visible only to the stage that may use it. Empty if the pipeline has
/// no resources.
pub fn descriptor_set_bindings(
    interface: &PipelineInterface,
) -> Vec<vk::DescriptorSetLayoutBinding<'static>> {
    interface
        .resources
        .iter()
        .map(|resource| {
            vk::DescriptorSetLayoutBinding::default()
                .binding(resource.binding)
                .descriptor_type(descriptor_type(resource.kind))
                .descriptor_count(1)
                .stage_flags(stage_flags(resource.stage))
        })
        .collect()
}

fn stage_flags(stage: Stage) -> vk::ShaderStageFlags {
    match stage {
        Stage::Vertex => vk::ShaderStageFlags::VERTEX,
        Stage::Fragment => vk::ShaderStageFlags::FRAGMENT,
    }
}

/// The descriptor type of a resource kind. Separate image and sampler descriptors, as the
/// shader declares them (see `crate::interface::chunk`).
fn descriptor_type(kind: ResourceKind) -> vk::DescriptorType {
    match kind {
        ResourceKind::SampledImage2DArray => vk::DescriptorType::SAMPLED_IMAGE,
        ResourceKind::Sampler => vk::DescriptorType::SAMPLER,
    }
}

/// The Vulkan format of an attribute. All four are required vertex formats in Vulkan
/// 1.0; `R8G8B8_UINT` is not, which is why the chunk position is read as four bytes.
fn vk_format(format: AttributeFormat) -> vk::Format {
    match format {
        AttributeFormat::Uint8x4 => vk::Format::R8G8B8A8_UINT,
        AttributeFormat::Uint16 => vk::Format::R16_UINT,
        AttributeFormat::Float32x2 => vk::Format::R32G32_SFLOAT,
        AttributeFormat::Unorm8x4 => vk::Format::R8G8B8A8_UNORM,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interface::{chunk, overlay};

    #[test]
    fn chunk_attributes_follow_the_interface() {
        let attributes = vertex_attributes(&chunk::INTERFACE);
        assert_eq!(attributes[0].format, vk::Format::R8G8B8A8_UINT);
        assert_eq!(attributes[1].format, vk::Format::R16_UINT);
        assert_eq!((attributes[1].location, attributes[1].offset), (1, 4));
        assert_eq!(vertex_bindings(&chunk::INTERFACE)[0].stride, 6);
    }

    #[test]
    fn overlay_attributes_follow_the_interface() {
        let attributes = vertex_attributes(&overlay::INTERFACE);
        assert_eq!(attributes[0].format, vk::Format::R32G32_SFLOAT);
        assert_eq!((attributes[0].location, attributes[0].offset), (0, 0));
        assert_eq!(attributes[1].format, vk::Format::R8G8B8A8_UNORM);
        assert_eq!((attributes[1].location, attributes[1].offset), (1, 8));
        assert_eq!(vertex_bindings(&overlay::INTERFACE)[0].stride, 12);
    }

    #[test]
    fn push_constant_ranges_follow_the_interface() {
        let [chunk_range] = push_constant_ranges(&chunk::INTERFACE);
        assert_eq!(
            chunk_range.stage_flags,
            vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT
        );
        assert_eq!((chunk_range.offset, chunk_range.size), (0, 112));
        let [overlay_range] = push_constant_ranges(&overlay::INTERFACE);
        assert_eq!(overlay_range.stage_flags, vk::ShaderStageFlags::VERTEX);
        assert_eq!((overlay_range.offset, overlay_range.size), (0, 8));
    }

    #[test]
    fn descriptor_bindings_follow_the_interface() {
        let bindings = descriptor_set_bindings(&chunk::INTERFACE);
        assert_eq!(bindings.len(), 2);
        assert_eq!(bindings[0].binding, 0);
        assert_eq!(
            bindings[0].descriptor_type,
            vk::DescriptorType::SAMPLED_IMAGE
        );
        assert_eq!(bindings[1].binding, 1);
        assert_eq!(bindings[1].descriptor_type, vk::DescriptorType::SAMPLER);
        for binding in &bindings {
            assert_eq!(binding.descriptor_count, 1);
            assert_eq!(binding.stage_flags, vk::ShaderStageFlags::FRAGMENT);
        }
        assert!(descriptor_set_bindings(&overlay::INTERFACE).is_empty());
    }
}
