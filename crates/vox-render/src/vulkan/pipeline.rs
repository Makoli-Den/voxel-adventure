//! The graphics pipeline that draws the demo triangle, and the shader modules it is built from.

use std::path::Path;

use ash::vk;

use super::render_pass::RenderPass;
use crate::RenderError;
use crate::shader::{self, ShaderStage};

/// Entry point name of every shader stage; GLSL always calls it `main`.
const ENTRY_POINT: &std::ffi::CStr = c"main";

/// The triangle pipeline and its (empty) layout.
///
/// Viewport and scissor are dynamic, so a window resize never rebuilds the pipeline.
pub struct Pipeline {
    device: ash::Device,
    layout: vk::PipelineLayout,
    handle: vk::Pipeline,
}

impl Pipeline {
    /// Compiles `triangle.vert` and `triangle.frag` from `shader_dir` and builds the
    /// pipeline for `render_pass`.
    pub fn new(
        device: &ash::Device,
        render_pass: &RenderPass,
        shader_dir: &Path,
    ) -> Result<Self, RenderError> {
        // Compile first: a broken shader fails before any Vulkan object exists.
        let vertex_code =
            shader::load_spirv(&shader_dir.join("triangle.vert"), ShaderStage::Vertex)?;
        let fragment_code =
            shader::load_spirv(&shader_dir.join("triangle.frag"), ShaderStage::Fragment)?;
        let vertex = ShaderModule::new(device, &vertex_code)?;
        let fragment = ShaderModule::new(device, &fragment_code)?;

        // Start with null handles: if a later call fails, `Drop` destroys whatever
        // was created so far (destroying a null handle is a no-op).
        let mut pipeline = Self {
            device: device.clone(),
            layout: vk::PipelineLayout::null(),
            handle: vk::Pipeline::null(),
        };
        let layout_info = vk::PipelineLayoutCreateInfo::default();
        // SAFETY: the device is alive and the empty create info outlives the call.
        pipeline.layout = unsafe { device.create_pipeline_layout(&layout_info, None)? };
        pipeline.handle = pipeline.create_graphics(render_pass, &vertex, &fragment)?;
        // The shader modules are dropped here; a built pipeline no longer needs them.
        Ok(pipeline)
    }

    /// The raw pipeline handle.
    pub fn handle(&self) -> vk::Pipeline {
        self.handle
    }

    /// Builds the pipeline object from the two shader stages.
    fn create_graphics(
        &self,
        render_pass: &RenderPass,
        vertex: &ShaderModule,
        fragment: &ShaderModule,
    ) -> Result<vk::Pipeline, RenderError> {
        let stages = [
            stage_info(vk::ShaderStageFlags::VERTEX, vertex),
            stage_info(vk::ShaderStageFlags::FRAGMENT, fragment),
        ];
        // No vertex buffers: the vertex shader picks positions by gl_VertexIndex.
        let vertex_input = vk::PipelineVertexInputStateCreateInfo::default();
        let input_assembly = vk::PipelineInputAssemblyStateCreateInfo::default()
            .topology(vk::PrimitiveTopology::TRIANGLE_LIST);
        // Only the counts are fixed; the rectangles are set per frame (dynamic state).
        let viewport = vk::PipelineViewportStateCreateInfo::default()
            .viewport_count(1)
            .scissor_count(1);
        let dynamic_states = [vk::DynamicState::VIEWPORT, vk::DynamicState::SCISSOR];
        let dynamic = vk::PipelineDynamicStateCreateInfo::default().dynamic_states(&dynamic_states);
        let rasterization = rasterization_state();
        let multisample = vk::PipelineMultisampleStateCreateInfo::default()
            .rasterization_samples(vk::SampleCountFlags::TYPE_1);
        let blend_attachments = [vk::PipelineColorBlendAttachmentState::default()
            .color_write_mask(vk::ColorComponentFlags::RGBA)];
        let blend =
            vk::PipelineColorBlendStateCreateInfo::default().attachments(&blend_attachments);

        let create_info = vk::GraphicsPipelineCreateInfo::default()
            .stages(&stages)
            .vertex_input_state(&vertex_input)
            .input_assembly_state(&input_assembly)
            .viewport_state(&viewport)
            .rasterization_state(&rasterization)
            .multisample_state(&multisample)
            .color_blend_state(&blend)
            .dynamic_state(&dynamic)
            .layout(self.layout)
            .render_pass(render_pass.handle())
            .subpass(0);

        // SAFETY: every handle belongs to this device and is alive, and all state
        // structs and arrays referenced by `create_info` outlive the call.
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

/// Filled triangles, no culling yet (the winding order is not settled), no depth bias.
fn rasterization_state() -> vk::PipelineRasterizationStateCreateInfo<'static> {
    vk::PipelineRasterizationStateCreateInfo::default()
        .polygon_mode(vk::PolygonMode::FILL)
        .cull_mode(vk::CullModeFlags::NONE)
        .front_face(vk::FrontFace::CLOCKWISE)
        .line_width(1.0)
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
        // enabled. That holds because the validator in `shader.rs` runs with no optional
        // capabilities, so only core SPIR-V features can appear. Do not widen that set
        // without enabling the matching device features.
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
