//! The graphics pipelines (chunk meshes and the overlay), and the shader modules they
//! are built from.

use std::path::Path;

use ash::vk;

use super::context::Context;
use super::gpu::Gpu;
use super::interface;
use super::render_pass::RenderPass;
use crate::RenderError;
use crate::interface::{PipelineInterface, chunk, overlay};
use crate::shader;

/// Entry point name of every shader stage; GLSL always calls it `main`.
const ENTRY_POINT: &std::ffi::CStr = c"main";

/// A graphics pipeline and its layout (no descriptor sets, one push-constant range).
///
/// Viewport and scissor are dynamic, so a window resize never rebuilds the pipeline.
pub struct Pipeline {
    device: ash::Device,
    layout: vk::PipelineLayout,
    handle: vk::Pipeline,
    /// The stages of the layout's push-constant range.
    push_constant_stages: vk::ShaderStageFlags,
}

/// What differs between the pipelines the renderer builds.
struct Spec {
    /// File names of the two shader stages inside the shader folder.
    vertex_file: &'static str,
    fragment_file: &'static str,
    /// What the pipeline provides to its shaders; checked against them and turned into
    /// the vertex input state and the push-constant range.
    interface: &'static PipelineInterface,
    cull_mode: vk::CullModeFlags,
    depth: vk::PipelineDepthStencilStateCreateInfo<'static>,
    blend: vk::PipelineColorBlendAttachmentState,
}

impl Pipeline {
    /// Compiles `chunk.vert` and `chunk.frag` from `shader_dir` and builds the opaque,
    /// depth-tested chunk pipeline for `render_pass`.
    pub fn chunk(
        device: &ash::Device,
        render_pass: &RenderPass,
        shader_dir: &Path,
    ) -> Result<Self, RenderError> {
        let spec = Spec {
            vertex_file: "chunk.vert",
            fragment_file: "chunk.frag",
            interface: &chunk::INTERFACE,
            cull_mode: vk::CullModeFlags::BACK,
            depth: depth_tested(),
            blend: opaque(),
        };
        Self::new(device, render_pass, shader_dir, &spec)
    }

    /// Compiles `overlay.vert` and `overlay.frag` from `shader_dir` and builds the
    /// overlay pipeline for `render_pass`: no depth test or write, no culling, and alpha
    /// blending if `can_blend` (see [`supports_blending`]); otherwise opaque.
    pub fn overlay(
        device: &ash::Device,
        render_pass: &RenderPass,
        shader_dir: &Path,
        can_blend: bool,
    ) -> Result<Self, RenderError> {
        let spec = Spec {
            vertex_file: "overlay.vert",
            fragment_file: "overlay.frag",
            interface: &overlay::INTERFACE,
            cull_mode: vk::CullModeFlags::NONE,
            depth: depth_ignored(),
            blend: if can_blend { alpha_blended() } else { opaque() },
        };
        Self::new(device, render_pass, shader_dir, &spec)
    }

    /// Compiles the shaders of `spec` and builds its pipeline.
    fn new(
        device: &ash::Device,
        render_pass: &RenderPass,
        shader_dir: &Path,
        spec: &Spec,
    ) -> Result<Self, RenderError> {
        // Compile and check first: a broken or mismatched shader fails before any
        // Vulkan object exists.
        let code = shader::load_pipeline_shaders(
            &shader_dir.join(spec.vertex_file),
            &shader_dir.join(spec.fragment_file),
            spec.interface,
        )?;
        let vertex = ShaderModule::new(device, &code.vertex)?;
        let fragment = ShaderModule::new(device, &code.fragment)?;

        // Start with null handles: if a later call fails, `Drop` destroys whatever
        // was created so far (destroying a null handle is a no-op).
        let mut pipeline = Self {
            device: device.clone(),
            layout: vk::PipelineLayout::null(),
            handle: vk::Pipeline::null(),
            push_constant_stages: interface::push_constant_stages(spec.interface),
        };
        let push_ranges = interface::push_constant_ranges(spec.interface);
        let layout_info =
            vk::PipelineLayoutCreateInfo::default().push_constant_ranges(&push_ranges);
        // SAFETY: the device is alive and the create info with its array outlives the call.
        pipeline.layout = unsafe { device.create_pipeline_layout(&layout_info, None)? };
        pipeline.handle = pipeline.create_graphics(render_pass, spec, &vertex, &fragment)?;
        // The shader modules are dropped here; a built pipeline no longer needs them.
        Ok(pipeline)
    }

    /// The raw pipeline handle.
    pub fn handle(&self) -> vk::Pipeline {
        self.handle
    }

    /// The pipeline layout, needed to push constants.
    pub fn layout(&self) -> vk::PipelineLayout {
        self.layout
    }

    /// The stage flags every push to this layout must name: those of its only range.
    pub fn push_constant_stages(&self) -> vk::ShaderStageFlags {
        self.push_constant_stages
    }

    /// Builds the pipeline object from the two shader stages.
    fn create_graphics(
        &self,
        render_pass: &RenderPass,
        spec: &Spec,
        vertex: &ShaderModule,
        fragment: &ShaderModule,
    ) -> Result<vk::Pipeline, RenderError> {
        let stages = [
            stage_info(vk::ShaderStageFlags::VERTEX, vertex),
            stage_info(vk::ShaderStageFlags::FRAGMENT, fragment),
        ];
        let bindings = interface::vertex_bindings(spec.interface);
        let attributes = interface::vertex_attributes(spec.interface);
        let vertex_input = vk::PipelineVertexInputStateCreateInfo::default()
            .vertex_binding_descriptions(&bindings)
            .vertex_attribute_descriptions(&attributes);
        let input_assembly = vk::PipelineInputAssemblyStateCreateInfo::default()
            .topology(vk::PrimitiveTopology::TRIANGLE_LIST);
        // Only the counts are fixed; the rectangles are set per frame (dynamic state).
        let viewport = vk::PipelineViewportStateCreateInfo::default()
            .viewport_count(1)
            .scissor_count(1);
        let dynamic_states = [vk::DynamicState::VIEWPORT, vk::DynamicState::SCISSOR];
        let dynamic = vk::PipelineDynamicStateCreateInfo::default().dynamic_states(&dynamic_states);
        let rasterization = rasterization_state(spec.cull_mode);
        let multisample = vk::PipelineMultisampleStateCreateInfo::default()
            .rasterization_samples(vk::SampleCountFlags::TYPE_1);
        let blend_attachments = [spec.blend];
        let blend =
            vk::PipelineColorBlendStateCreateInfo::default().attachments(&blend_attachments);

        let create_info = vk::GraphicsPipelineCreateInfo::default()
            .stages(&stages)
            .vertex_input_state(&vertex_input)
            .input_assembly_state(&input_assembly)
            .viewport_state(&viewport)
            .rasterization_state(&rasterization)
            .multisample_state(&multisample)
            .depth_stencil_state(&spec.depth)
            .color_blend_state(&blend)
            .dynamic_state(&dynamic)
            .layout(self.layout)
            .render_pass(render_pass.handle())
            .subpass(0);

        // SAFETY: every handle belongs to this device and is alive, and all state
        // structs and arrays referenced by `create_info` outlive the call. Blending is
        // enabled only if the caller checked that the color format supports it.
        let created = unsafe {
            self.device
                .create_graphics_pipelines(vk::PipelineCache::null(), &[create_info], None)
        };
        // On failure ash returns the partial list; with a single pipeline it holds only null.
        let pipelines = created.map_err(|(_, error)| RenderError::from(error))?;
        pipelines
            .first()
            .copied()
            .ok_or(RenderError::Unsupported("the driver returned no pipeline"))
    }
}

impl Drop for Pipeline {
    fn drop(&mut self) {
        // SAFETY: the renderer waits for the device to go idle before dropping this,
        // so no command buffer still uses the pipeline. Null handles are ignored.
        unsafe {
            self.device.destroy_pipeline(self.handle, None);
            self.device.destroy_pipeline_layout(self.layout, None);
        }
    }
}

/// Whether color attachments of `format` can be blended on this GPU.
///
/// Vulkan 1.0 requires it for the common 8-bit formats (`B8G8R8A8_UNORM` among them),
/// but a surface may offer another format first; enabling blending without this
/// feature would make the pipeline invalid. Checks the potential format features: those
/// of optimal and linear tiling together.
pub fn supports_blending(context: &Context, gpu: &Gpu, format: vk::Format) -> bool {
    // SAFETY: the physical device was enumerated from this live instance.
    let properties = unsafe {
        context
            .instance()
            .get_physical_device_format_properties(gpu.physical(), format)
    };
    (properties.optimal_tiling_features | properties.linear_tiling_features)
        .contains(vk::FormatFeatureFlags::COLOR_ATTACHMENT_BLEND)
}

/// Filled triangles culled by `cull_mode`, no depth bias.
fn rasterization_state(
    cull_mode: vk::CullModeFlags,
) -> vk::PipelineRasterizationStateCreateInfo<'static> {
    vk::PipelineRasterizationStateCreateInfo::default()
        .polygon_mode(vk::PolygonMode::FILL)
        .cull_mode(cull_mode)
        // The mesher winds faces counter-clockwise seen from outside (right-handed,
        // Y up). The camera matrix flips Y for Vulkan clip space, so the image is
        // upright, and the winding a viewer sees on screen stays counter-clockwise.
        // Vulkan defines the facing with framebuffer Y pointing down (the minus sign in
        // the spec's area formula), so on-screen counter-clockwise is COUNTER_CLOCKWISE.
        // Checked visually: with CLOCKWISE the far inside faces of the chunk show.
        // Without culling (the overlay) the facing does not matter.
        .front_face(vk::FrontFace::COUNTER_CLOCKWISE)
        .line_width(1.0)
}

/// Nearer fragments win: test with LESS against a buffer cleared to 1.0, and write.
fn depth_tested() -> vk::PipelineDepthStencilStateCreateInfo<'static> {
    vk::PipelineDepthStencilStateCreateInfo::default()
        .depth_test_enable(true)
        .depth_write_enable(true)
        .depth_compare_op(vk::CompareOp::LESS)
}

/// Drawn over everything: no depth test and no depth write. The render pass still has a
/// depth attachment; the pipeline simply leaves it alone.
fn depth_ignored() -> vk::PipelineDepthStencilStateCreateInfo<'static> {
    vk::PipelineDepthStencilStateCreateInfo::default()
        .depth_test_enable(false)
        .depth_write_enable(false)
}

/// The fragment color replaces the image color.
fn opaque() -> vk::PipelineColorBlendAttachmentState {
    vk::PipelineColorBlendAttachmentState::default().color_write_mask(vk::ColorComponentFlags::RGBA)
}

/// Non-premultiplied "over": `color = src * src_alpha + dst * (1 - src_alpha)` and
/// `alpha = src_alpha + dst_alpha * (1 - src_alpha)`.
///
/// The alpha equation is Porter-Duff "over" for coverage: the image starts opaque (the
/// sky is cleared with alpha 1), and `a + 1 * (1 - a)` keeps it exactly 1, which is what
/// the opaque window composition expects. Taking `src_alpha` for the destination alpha
/// instead would leave a translucent value in the image wherever the overlay is.
fn alpha_blended() -> vk::PipelineColorBlendAttachmentState {
    vk::PipelineColorBlendAttachmentState::default()
        .blend_enable(true)
        .src_color_blend_factor(vk::BlendFactor::SRC_ALPHA)
        .dst_color_blend_factor(vk::BlendFactor::ONE_MINUS_SRC_ALPHA)
        .color_blend_op(vk::BlendOp::ADD)
        .src_alpha_blend_factor(vk::BlendFactor::ONE)
        .dst_alpha_blend_factor(vk::BlendFactor::ONE_MINUS_SRC_ALPHA)
        .alpha_blend_op(vk::BlendOp::ADD)
        .color_write_mask(vk::ColorComponentFlags::RGBA)
}

fn stage_info(
    stage: vk::ShaderStageFlags,
    module: &ShaderModule,
) -> vk::PipelineShaderStageCreateInfo<'static> {
    vk::PipelineShaderStageCreateInfo::default()
        .stage(stage)
        .module(module.handle)
        .name(ENTRY_POINT)
}

/// A compiled shader stage. Lives only while pipelines are being built.
struct ShaderModule {
    device: ash::Device,
    handle: vk::ShaderModule,
}

impl ShaderModule {
    /// Creates a shader module from SPIR-V words.
    fn new(device: &ash::Device, code: &[u32]) -> Result<Self, RenderError> {
        let create_info = vk::ShaderModuleCreateInfo::default().code(code);
        // SAFETY: the device is alive, and `code` is SPIR-V produced and validated by naga.
        // Vulkan also requires that the module uses only capabilities the device has
        // enabled, and the pipeline built from it requires that its interface matches
        // the pipeline layout and vertex input. Both hold because `shader::load_pipeline_shaders`
        // admits only core capabilities (push constants are the one optional naga
        // capability, and they are core Vulkan 1.0) AND checks the interface against the
        // pipeline's own description in `crate::interface`, the same one its layout's
        // push-constant range and its vertex input state are built from: no descriptor
        // resources, push constants only in the described stage and within the range,
        // only the vertex attributes the pipeline feeds with their exact types, and
        // fragment inputs that the vertex stage writes. Do not widen either without
        // changing the device features or the pipeline.
        let handle = unsafe { device.create_shader_module(&create_info, None)? };
        Ok(Self {
            device: device.clone(),
            handle,
        })
    }
}

impl Drop for ShaderModule {
    fn drop(&mut self) {
        // SAFETY: a shader module is only read while a pipeline is created from it, and
        // that call has returned by the time the module is dropped. Pipelines built from
        // it stay valid, so no GPU work depends on the module.
        unsafe { self.device.destroy_shader_module(self.handle, None) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlay_color_is_blended_by_source_alpha() {
        let blend = alpha_blended();
        assert_eq!(blend.blend_enable, vk::TRUE);
        assert_eq!(blend.src_color_blend_factor, vk::BlendFactor::SRC_ALPHA);
        assert_eq!(
            blend.dst_color_blend_factor,
            vk::BlendFactor::ONE_MINUS_SRC_ALPHA
        );
        assert_eq!(blend.color_blend_op, vk::BlendOp::ADD);
    }

    #[test]
    fn overlay_keeps_the_image_alpha_opaque() {
        let blend = alpha_blended();
        assert_eq!(blend.src_alpha_blend_factor, vk::BlendFactor::ONE);
        assert_eq!(
            blend.dst_alpha_blend_factor,
            vk::BlendFactor::ONE_MINUS_SRC_ALPHA
        );
    }

    #[test]
    fn opaque_writes_all_channels_without_blending() {
        let blend = opaque();
        assert_eq!(blend.blend_enable, vk::FALSE);
        assert_eq!(blend.color_write_mask, vk::ColorComponentFlags::RGBA);
        assert_eq!(
            alpha_blended().color_write_mask,
            vk::ColorComponentFlags::RGBA
        );
    }

    #[test]
    fn the_overlay_does_not_touch_the_depth_buffer() {
        let depth = depth_ignored();
        assert_eq!(depth.depth_test_enable, vk::FALSE);
        assert_eq!(depth.depth_write_enable, vk::FALSE);
        assert_eq!(depth.stencil_test_enable, vk::FALSE);
        assert_eq!(depth_tested().depth_test_enable, vk::TRUE);
    }
}
