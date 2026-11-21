//! The render pass that draws a frame straight into a swapchain image.

use ash::vk;

use crate::RenderError;

/// One subpass with a single color attachment: the swapchain image.
///
/// The attachment is cleared at the start and ends up ready for presentation,
/// so the frame needs no explicit layout barriers.
pub struct RenderPass {
    device: ash::Device,
    handle: vk::RenderPass,
    /// The surface format the pass was built for; swapchain images must use it.
    format: vk::SurfaceFormatKHR,
}

impl RenderPass {
    /// Creates the render pass for swapchain images of `format`.
    pub fn new(device: &ash::Device, format: vk::SurfaceFormatKHR) -> Result<Self, RenderError> {
        let attachments = [color_attachment(format.format)];
        let color_refs = [vk::AttachmentReference {
            attachment: 0,
            layout: vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
        }];
        let subpasses = [vk::SubpassDescription::default()
            .pipeline_bind_point(vk::PipelineBindPoint::GRAPHICS)
            .color_attachments(&color_refs)];
        let dependencies = [acquire_dependency()];
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

/// Makes the layout transition at the start of the pass wait for the image to be acquired.
///
/// The submit waits on the "image available" semaphore at the color output stage.
/// Without this dependency the implicit transition could run at the top of the pipe,
/// before the presentation engine has released the image.
fn acquire_dependency() -> vk::SubpassDependency {
    vk::SubpassDependency {
        src_subpass: vk::SUBPASS_EXTERNAL,
        dst_subpass: 0,
        src_stage_mask: vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT,
        dst_stage_mask: vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT,
        src_access_mask: vk::AccessFlags::empty(),
        dst_access_mask: vk::AccessFlags::COLOR_ATTACHMENT_WRITE,
        dependency_flags: vk::DependencyFlags::empty(),
    }
}
