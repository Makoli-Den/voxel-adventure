//! Swapchain: the queue of images that are presented to the window,
//! with the image views and framebuffers that draw into them.

use ash::{khr, vk};

use super::context::{Context, Surface};
use super::gpu::Gpu;
use super::render_pass::RenderPass;
use crate::RenderError;

/// Usage we need from swapchain images: drawing into them as color attachments.
fn image_usage() -> vk::ImageUsageFlags {
    vk::ImageUsageFlags::COLOR_ATTACHMENT
}

/// Chooses the format for swapchain images on this surface.
///
/// Called once at startup: the render pass and pipeline are built for this format.
/// The list of formats a surface offers may change later (for example when the window
/// moves to another monitor), so [`Swapchain::new`] checks it again. Rebuilding the
/// render pass and pipeline for a new format is not implemented until it is seen in practice.
pub fn surface_format(surface: &Surface, gpu: &Gpu) -> Result<vk::SurfaceFormatKHR, RenderError> {
    choose_format(&query_formats(surface, gpu)?)
}

/// Asks the driver which image formats the surface offers right now.
fn query_formats(surface: &Surface, gpu: &Gpu) -> Result<Vec<vk::SurfaceFormatKHR>, RenderError> {
    // SAFETY: the physical device and the surface are alive and belong to the same instance.
    Ok(unsafe {
        surface
            .loader()
            .get_physical_device_surface_formats(gpu.physical(), surface.handle())?
    })
}

/// A swapchain together with its images and one image view and framebuffer per image.
///
/// The views and framebuffers depend on the image size, so they are rebuilt with the
/// swapchain on every resize.
pub struct Swapchain {
    device: ash::Device,
    loader: khr::swapchain::Device,
    handle: vk::SwapchainKHR,
    extent: vk::Extent2D,
    images: Vec<vk::Image>,
    views: Vec<vk::ImageView>,
    framebuffers: Vec<vk::Framebuffer>,
}

impl Swapchain {
    /// Creates a swapchain for a window of `size` pixels, with images in the format
    /// of `render_pass` and a framebuffer for that pass on each image.
    ///
    /// Returns `None` when the surface currently has no area (for example a minimized
    /// window on Windows), because Vulkan forbids zero-sized swapchains. The caller
    /// should try again later.
    ///
    /// Pass the previous swapchain as `old` when recreating after a resize,
    /// so the driver can reuse its resources.
    pub fn new(
        context: &Context,
        surface: &Surface,
        gpu: &Gpu,
        render_pass: &RenderPass,
        size: (u32, u32),
        old: Option<&Self>,
    ) -> Result<Option<Self>, RenderError> {
        // SAFETY: the physical device and the surface are alive and belong to the same instance.
        let capabilities = unsafe {
            surface
                .loader()
                .get_physical_device_surface_capabilities(gpu.physical(), surface.handle())?
        };
        let extent = choose_extent(&capabilities, size);
        if extent.width == 0 || extent.height == 0 {
            return Ok(None);
        }
        check_capabilities(&capabilities)?;

        let format = render_pass.format();
        if !is_listed(&query_formats(surface, gpu)?, format) {
            return Err(RenderError::Unsupported(
                "the surface no longer offers the swapchain image format",
            ));
        }
        let create_info = vk::SwapchainCreateInfoKHR::default()
            .surface(surface.handle())
            .min_image_count(choose_image_count(&capabilities))
            .image_format(format.format)
            .image_color_space(format.color_space)
            .image_extent(extent)
            .image_array_layers(1)
            .image_usage(image_usage())
            .image_sharing_mode(vk::SharingMode::EXCLUSIVE)
            .pre_transform(capabilities.current_transform)
            .composite_alpha(vk::CompositeAlphaFlagsKHR::OPAQUE)
            // FIFO is the only present mode every driver must support (vsync).
            .present_mode(vk::PresentModeKHR::FIFO)
            .clipped(true)
            .old_swapchain(old.map_or(vk::SwapchainKHR::null(), |s| s.handle));

        let loader = khr::swapchain::Device::new(context.instance(), gpu.device());
        // SAFETY: `create_info` is fully initialized and the old swapchain, if any, is still alive.
        let handle = unsafe { loader.create_swapchain(&create_info, None)? };

        // Build `Self` before the next fallible call so `Drop` cleans up on failure.
        let mut swapchain = Self {
            device: gpu.device().clone(),
            loader,
            handle,
            extent,
            images: Vec::new(),
            views: Vec::new(),
            framebuffers: Vec::new(),
        };
        // SAFETY: `handle` was just created by this loader.
        swapchain.images = unsafe { swapchain.loader.get_swapchain_images(swapchain.handle)? };
        swapchain.create_views(format.format)?;
        swapchain.create_framebuffers(render_pass)?;
        Ok(Some(swapchain))
    }

    /// Number of images in the swapchain.
    pub fn image_count(&self) -> usize {
        self.images.len()
    }

    /// Size of the swapchain images in pixels.
    pub fn extent(&self) -> vk::Extent2D {
        self.extent
    }

    /// The framebuffer of the image with the given index, as returned by
    /// [`Self::acquire_next_image`].
    pub fn framebuffer(&self, index: u32) -> vk::Framebuffer {
        // Indexing is safe: there is one framebuffer per image, and the driver only
        // returns indices below the image count.
        self.framebuffers[index as usize]
    }

    /// Acquires the next image to draw into. Returns its index and whether the
    /// swapchain is suboptimal for the surface.
    ///
    /// `semaphore` is signaled once the image is ready.
    ///
    /// # Safety
    /// `semaphore` must be unsignaled and have no pending signal or wait operation.
    pub unsafe fn acquire_next_image(
        &self,
        semaphore: vk::Semaphore,
    ) -> Result<(u32, bool), vk::Result> {
        // SAFETY: the swapchain is alive; the semaphore's state is the caller's contract.
        unsafe {
            self.loader
                .acquire_next_image(self.handle, u64::MAX, semaphore, vk::Fence::null())
        }
    }

    /// Queues the image for presentation once `wait` is signaled.
    /// Returns whether the swapchain is suboptimal for the surface.
    pub fn present(
        &self,
        queue: vk::Queue,
        wait: vk::Semaphore,
        image_index: u32,
    ) -> Result<bool, vk::Result> {
        let wait_semaphores = [wait];
        let swapchains = [self.handle];
        let image_indices = [image_index];
        let info = vk::PresentInfoKHR::default()
            .wait_semaphores(&wait_semaphores)
            .swapchains(&swapchains)
            .image_indices(&image_indices);
        // SAFETY: the queue supports presentation, the image was acquired and its
        // rendering will signal `wait`.
        unsafe { self.loader.queue_present(queue, &info) }
    }

    /// Creates one color view per swapchain image. Views are pushed one by one,
    /// so `Drop` destroys those already made if a later one fails.
    fn create_views(&mut self, format: vk::Format) -> Result<(), RenderError> {
        for &image in &self.images {
            let create_info = vk::ImageViewCreateInfo::default()
                .image(image)
                .view_type(vk::ImageViewType::TYPE_2D)
                .format(format)
                .subresource_range(color_range());
            // SAFETY: `image` belongs to this live swapchain, which was created with `format`.
            let view = unsafe { self.device.create_image_view(&create_info, None)? };
            self.views.push(view);
        }
        Ok(())
    }

    /// Creates one framebuffer per image view. Pushed one by one, like the views.
    fn create_framebuffers(&mut self, render_pass: &RenderPass) -> Result<(), RenderError> {
        for &view in &self.views {
            let attachments = [view];
            let create_info = vk::FramebufferCreateInfo::default()
                .render_pass(render_pass.handle())
                .attachments(&attachments)
                .width(self.extent.width)
                .height(self.extent.height)
                .layers(1);
            // SAFETY: the render pass and the view are alive, and the view's format matches
            // the pass attachment (both come from `render_pass.format()`).
            let framebuffer = unsafe { self.device.create_framebuffer(&create_info, None)? };
            self.framebuffers.push(framebuffer);
        }
        Ok(())
    }
}

impl Drop for Swapchain {
    fn drop(&mut self) {
        // SAFETY: the renderer waits for the device to go idle before dropping or replacing
        // a swapchain, so no GPU work still uses it, its views or its framebuffers.
        // Framebuffers go first, then the views they point to, then the swapchain that
        // owns the images.
        unsafe {
            for framebuffer in self.framebuffers.drain(..) {
                self.device.destroy_framebuffer(framebuffer, None);
            }
            for view in self.views.drain(..) {
                self.device.destroy_image_view(view, None);
            }
            self.loader.destroy_swapchain(self.handle, None);
        }
    }
}

/// Fails if the surface cannot be used the way this renderer needs.
fn check_capabilities(capabilities: &vk::SurfaceCapabilitiesKHR) -> Result<(), RenderError> {
    if !capabilities.supported_usage_flags.contains(image_usage()) {
        return Err(RenderError::Unsupported(
            "swapchain images cannot be rendered to",
        ));
    }
    if !capabilities
        .supported_composite_alpha
        .contains(vk::CompositeAlphaFlagsKHR::OPAQUE)
    {
        return Err(RenderError::Unsupported("opaque window composition"));
    }
    Ok(())
}

/// The single mip level and array layer of a color image.
fn color_range() -> vk::ImageSubresourceRange {
    vk::ImageSubresourceRange {
        aspect_mask: vk::ImageAspectFlags::COLOR,
        base_mip_level: 0,
        level_count: 1,
        base_array_layer: 0,
        layer_count: 1,
    }
}

/// Prefers plain BGRA8; otherwise takes whatever the surface lists first.
fn choose_format(formats: &[vk::SurfaceFormatKHR]) -> Result<vk::SurfaceFormatKHR, RenderError> {
    // Some old drivers report a single UNDEFINED entry, which means "any format works".
    if let [only] = formats {
        if only.format == vk::Format::UNDEFINED {
            return Ok(vk::SurfaceFormatKHR {
                format: vk::Format::B8G8R8A8_UNORM,
                color_space: only.color_space,
            });
        }
    }
    formats
        .iter()
        .find(|f| f.format == vk::Format::B8G8R8A8_UNORM)
        .or_else(|| formats.first())
        .copied()
        .ok_or(RenderError::Unsupported("surface lists no image formats"))
}

/// Whether a swapchain may use `wanted` according to the formats the surface offers.
fn is_listed(formats: &[vk::SurfaceFormatKHR], wanted: vk::SurfaceFormatKHR) -> bool {
    // A single UNDEFINED entry means "any format works", see `choose_format`.
    if let [only] = formats {
        if only.format == vk::Format::UNDEFINED {
            return true;
        }
    }
    formats
        .iter()
        .any(|f| f.format == wanted.format && f.color_space == wanted.color_space)
}

/// Uses the size the surface dictates, or the window size clamped to the allowed range.
fn choose_extent(capabilities: &vk::SurfaceCapabilitiesKHR, size: (u32, u32)) -> vk::Extent2D {
    if capabilities.current_extent.width != u32::MAX {
        return capabilities.current_extent;
    }
    vk::Extent2D {
        width: size.0.clamp(
            capabilities.min_image_extent.width,
            capabilities.max_image_extent.width,
        ),
        height: size.1.clamp(
            capabilities.min_image_extent.height,
            capabilities.max_image_extent.height,
        ),
    }
}

/// One image more than the minimum, so we never wait on the driver; capped at the maximum.
fn choose_image_count(capabilities: &vk::SurfaceCapabilitiesKHR) -> u32 {
    let wanted = capabilities.min_image_count + 1;
    match capabilities.max_image_count {
        0 => wanted, // zero means "no maximum"
        max => wanted.min(max),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn format(format: vk::Format) -> vk::SurfaceFormatKHR {
        vk::SurfaceFormatKHR {
            format,
            color_space: vk::ColorSpaceKHR::SRGB_NONLINEAR,
        }
    }

    #[test]
    fn format_prefers_bgra8() {
        let formats = [
            format(vk::Format::R8G8B8A8_UNORM),
            format(vk::Format::B8G8R8A8_UNORM),
        ];
        let chosen = choose_format(&formats).unwrap();
        assert_eq!(chosen.format, vk::Format::B8G8R8A8_UNORM);
    }

    #[test]
    fn format_falls_back_to_first_listed() {
        let formats = [
            format(vk::Format::R8G8B8A8_UNORM),
            format(vk::Format::R5G6B5_UNORM_PACK16),
        ];
        let chosen = choose_format(&formats).unwrap();
        assert_eq!(chosen.format, vk::Format::R8G8B8A8_UNORM);
    }

    #[test]
    fn single_undefined_format_means_any() {
        let chosen = choose_format(&[format(vk::Format::UNDEFINED)]).unwrap();
        assert_eq!(chosen.format, vk::Format::B8G8R8A8_UNORM);
    }

    #[test]
    fn empty_format_list_is_an_error() {
        assert!(choose_format(&[]).is_err());
    }

    #[test]
    fn listed_format_is_accepted() {
        let formats = [
            format(vk::Format::R8G8B8A8_UNORM),
            format(vk::Format::B8G8R8A8_UNORM),
        ];
        assert!(is_listed(&formats, format(vk::Format::B8G8R8A8_UNORM)));
    }

    #[test]
    fn missing_format_is_rejected() {
        let formats = [format(vk::Format::R8G8B8A8_UNORM)];
        assert!(!is_listed(&formats, format(vk::Format::B8G8R8A8_UNORM)));
    }

    #[test]
    fn same_format_in_another_color_space_is_rejected() {
        let formats = [format(vk::Format::B8G8R8A8_UNORM)];
        let hdr = vk::SurfaceFormatKHR {
            format: vk::Format::B8G8R8A8_UNORM,
            color_space: vk::ColorSpaceKHR::HDR10_ST2084_EXT,
        };
        assert!(!is_listed(&formats, hdr));
    }

    #[test]
    fn single_undefined_entry_accepts_any_format() {
        let formats = [format(vk::Format::UNDEFINED)];
        assert!(is_listed(&formats, format(vk::Format::B8G8R8A8_UNORM)));
    }

    #[test]
    fn extent_follows_surface_when_it_dictates_one() {
        let capabilities = vk::SurfaceCapabilitiesKHR {
            current_extent: vk::Extent2D {
                width: 800,
                height: 600,
            },
            ..Default::default()
        };
        let extent = choose_extent(&capabilities, (1920, 1080));
        assert_eq!((extent.width, extent.height), (800, 600));
    }

    #[test]
    fn extent_is_clamped_when_surface_leaves_it_open() {
        let capabilities = vk::SurfaceCapabilitiesKHR {
            current_extent: vk::Extent2D {
                width: u32::MAX,
                height: u32::MAX,
            },
            min_image_extent: vk::Extent2D {
                width: 100,
                height: 100,
            },
            max_image_extent: vk::Extent2D {
                width: 1000,
                height: 1000,
            },
            ..Default::default()
        };
        let extent = choose_extent(&capabilities, (5000, 10));
        assert_eq!((extent.width, extent.height), (1000, 100));
    }

    #[test]
    fn zero_sized_surface_gives_zero_extent() {
        let capabilities = vk::SurfaceCapabilitiesKHR {
            current_extent: vk::Extent2D {
                width: 0,
                height: 0,
            },
            ..Default::default()
        };
        let extent = choose_extent(&capabilities, (800, 600));
        assert_eq!((extent.width, extent.height), (0, 0));
    }

    #[test]
    fn image_count_is_one_above_minimum() {
        let capabilities = vk::SurfaceCapabilitiesKHR {
            min_image_count: 2,
            max_image_count: 0,
            ..Default::default()
        };
        assert_eq!(choose_image_count(&capabilities), 3);
    }

    #[test]
    fn image_count_respects_the_maximum() {
        let capabilities = vk::SurfaceCapabilitiesKHR {
            min_image_count: 2,
            max_image_count: 2,
            ..Default::default()
        };
        assert_eq!(choose_image_count(&capabilities), 2);
    }

    #[test]
    fn surface_without_color_attachment_usage_is_rejected() {
        let capabilities = vk::SurfaceCapabilitiesKHR {
            supported_usage_flags: vk::ImageUsageFlags::TRANSFER_DST,
            supported_composite_alpha: vk::CompositeAlphaFlagsKHR::OPAQUE,
            ..Default::default()
        };
        assert!(check_capabilities(&capabilities).is_err());
    }

    #[test]
    fn usable_surface_passes_the_check() {
        let capabilities = vk::SurfaceCapabilitiesKHR {
            supported_usage_flags: vk::ImageUsageFlags::COLOR_ATTACHMENT,
            supported_composite_alpha: vk::CompositeAlphaFlagsKHR::OPAQUE,
            ..Default::default()
        };
        assert!(check_capabilities(&capabilities).is_ok());
    }
}
