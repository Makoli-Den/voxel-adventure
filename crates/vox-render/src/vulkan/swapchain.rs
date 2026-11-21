//! Swapchain: the queue of images that are presented to the window.

use ash::{khr, vk};

use super::context::{Context, Surface};
use super::gpu::Gpu;
use crate::RenderError;

/// Usage we need from swapchain images: drawing, and clearing through transfer commands.
fn image_usage() -> vk::ImageUsageFlags {
    vk::ImageUsageFlags::COLOR_ATTACHMENT | vk::ImageUsageFlags::TRANSFER_DST
}

/// A swapchain together with its images.
pub struct Swapchain {
    loader: khr::swapchain::Device,
    handle: vk::SwapchainKHR,
    images: Vec<vk::Image>,
}

impl Swapchain {
    /// Creates a swapchain for a window of `size` pixels.
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
        size: (u32, u32),
        old: Option<&Self>,
    ) -> Result<Option<Self>, RenderError> {
        let physical = gpu.physical();
        // SAFETY: `physical` and the surface are alive and belong to the same instance.
        let capabilities = unsafe {
            surface
                .loader()
                .get_physical_device_surface_capabilities(physical, surface.handle())?
        };
        // SAFETY: as above.
        let formats = unsafe {
            surface
                .loader()
                .get_physical_device_surface_formats(physical, surface.handle())?
        };

        let extent = choose_extent(&capabilities, size);
        if extent.width == 0 || extent.height == 0 {
            return Ok(None);
        }
        if !capabilities.supported_usage_flags.contains(image_usage()) {
            return Err(RenderError::Unsupported(
                "swapchain images cannot be cleared",
            ));
        }
        if !capabilities
            .supported_composite_alpha
            .contains(vk::CompositeAlphaFlagsKHR::OPAQUE)
        {
            return Err(RenderError::Unsupported("opaque window composition"));
        }

        let format = choose_format(&formats)?;
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
            loader,
            handle,
            images: Vec::new(),
        };
        // SAFETY: `handle` was just created by this loader.
        swapchain.images = unsafe { swapchain.loader.get_swapchain_images(swapchain.handle)? };
        Ok(Some(swapchain))
    }

    /// Number of images in the swapchain.
    pub fn image_count(&self) -> usize {
        self.images.len()
    }

    /// The image with the given index, as returned by [`Self::acquire_next_image`].
    pub fn image(&self, index: u32) -> vk::Image {
        // Indexing is safe: the driver only returns indices below the image count.
        self.images[index as usize]
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
}

impl Drop for Swapchain {
    fn drop(&mut self) {
        // SAFETY: the renderer waits for the device to go idle before dropping or replacing
        // a swapchain, so no GPU work still uses it.
        unsafe { self.loader.destroy_swapchain(self.handle, None) };
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
}
