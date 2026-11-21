//! The render pass that draws a frame straight into a swapchain image, with a depth buffer.

use ash::vk;

use crate::RenderError;

/// One subpass with two attachments: the swapchain image (color) and a depth image.
///
/// Both are cleared at the start. The color image ends up ready for presentation, so
/// the frame needs no explicit layout barriers; the depth contents are thrown away.
pub struct RenderPass {
    device: ash::Device,
    handle: vk::RenderPass,
    /// The surface format the pass was built for; swapchain images must use it.
    format: vk::SurfaceFormatKHR,
    /// The depth format the pass was built for; depth images must use it.
    depth_format: vk::Format,
}

impl RenderPass {
    /// Creates the render pass for swapchain images of `format` and depth images of `depth_format`.
    pub fn new(
        device: &ash::Device,
        format: vk::SurfaceFormatKHR,
        depth_format: vk::Format,
    ) -> Result<Self, RenderError> {
        let attachments = [
            color_attachment(format.format),
            depth_attachment(depth_format),
        ];
        let color_refs = [vk::AttachmentReference {
            attachment: 0,
            layout: vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
        }];
        let depth_ref = vk::AttachmentReference {
            attachment: 1,
            layout: vk::ImageLayout::DEPTH_STENCIL_ATTACHMENT_OPTIMAL,
        };
        let subpasses = [vk::SubpassDescription::default()
            .pipeline_bind_point(vk::PipelineBindPoint::GRAPHICS)
            .color_attachments(&color_refs)
            .depth_stencil_attachment(&depth_ref)];
        let dependencies = [frame_start_dependency()];
        let create_info = vk::RenderPassCreateInfo::default()
            .attachments(&attachments)
            .subpasses(&subpasses)
            .dependencies(&dependencies);

        // SAFETY: the device is alive and `create_info` with all arrays it points to
        // outlives the call.
        let handle = unsafe { device.create_render_pass(&create_info, None)? };
        Ok(Self {
            device: device.clone(),
            handle,
            format,
            depth_format,
        })
    }

    /// The raw render pass handle.
    pub fn handle(&self) -> vk::RenderPass {
        self.handle
    }

    /// The surface format swapchain images must have to be used with this pass.
    pub fn format(&self) -> vk::SurfaceFormatKHR {
        self.format
    }

    /// The format depth images must have to be used with this pass.
    pub fn depth_format(&self) -> vk::Format {
        self.depth_format
    }
}

impl Drop for RenderPass {
    fn drop(&mut self) {
        // SAFETY: the renderer waits for the device to go idle and destroys the
        // framebuffers and pipeline built on this pass first (field order of `VulkanRenderer`).
        unsafe { self.device.destroy_render_pass(self.handle, None) };
    }
}

/// The swapchain image: cleared on load, kept on store, handed to presentation at the end.
fn color_attachment(format: vk::Format) -> vk::AttachmentDescription {
    vk::AttachmentDescription::default()
        .format(format)
        .samples(vk::SampleCountFlags::TYPE_1)
        .load_op(vk::AttachmentLoadOp::CLEAR)
        .store_op(vk::AttachmentStoreOp::STORE)
        .stencil_load_op(vk::AttachmentLoadOp::DONT_CARE)
        .stencil_store_op(vk::AttachmentStoreOp::DONT_CARE)
        // The previous contents are cleared anyway, so the old layout does not matter.
        .initial_layout(vk::ImageLayout::UNDEFINED)
        .final_layout(vk::ImageLayout::PRESENT_SRC_KHR)
}

/// The depth image: cleared on load and not needed after the pass, so it is never stored.
fn depth_attachment(format: vk::Format) -> vk::AttachmentDescription {
    vk::AttachmentDescription::default()
        .format(format)
        .samples(vk::SampleCountFlags::TYPE_1)
        .load_op(vk::AttachmentLoadOp::CLEAR)
        .store_op(vk::AttachmentStoreOp::DONT_CARE)
        .stencil_load_op(vk::AttachmentLoadOp::DONT_CARE)
        .stencil_store_op(vk::AttachmentStoreOp::DONT_CARE)
        .initial_layout(vk::ImageLayout::UNDEFINED)
        .final_layout(vk::ImageLayout::DEPTH_STENCIL_ATTACHMENT_OPTIMAL)
}

/// Orders the start of the pass after the work it must not overlap with.
///
/// Color: the submit waits on the "image available" semaphore at the color output stage.
/// Without this dependency the implicit layout transition could run at the top of the
/// pipe, before the presentation engine has released the image.
///
/// Depth: one depth image is shared by all frames in flight. Its clear (a write in the
/// early fragment tests) must wait until the previous frame's depth tests finished
/// writing it (late fragment tests at the latest).
fn frame_start_dependency() -> vk::SubpassDependency {
    let stages = vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT
        | vk::PipelineStageFlags::EARLY_FRAGMENT_TESTS
        | vk::PipelineStageFlags::LATE_FRAGMENT_TESTS;
    vk::SubpassDependency {
        src_subpass: vk::SUBPASS_EXTERNAL,
        dst_subpass: 0,
        src_stage_mask: stages,
        dst_stage_mask: stages,
        src_access_mask: vk::AccessFlags::DEPTH_STENCIL_ATTACHMENT_WRITE,
        dst_access_mask: vk::AccessFlags::COLOR_ATTACHMENT_WRITE
            | vk::AccessFlags::DEPTH_STENCIL_ATTACHMENT_READ
            | vk::AccessFlags::DEPTH_STENCIL_ATTACHMENT_WRITE,
        dependency_flags: vk::DependencyFlags::empty(),
    }
}
