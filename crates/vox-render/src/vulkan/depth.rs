//! The depth buffer: choosing its format and its depth mapping ([`DepthMode`]), and owning
//! the image it lives in.

use ash::vk;

use super::context::Context;
use super::gpu::Gpu;
use super::memory;
use crate::{DepthMode, RenderError};

/// Depth formats in order of preference. Vulkan 1.0 guarantees that at least one of
/// `X8_D24_UNORM_PACK32` and `D32_SFLOAT` works as a depth attachment, and at least one
/// of `D24_UNORM_S8_UINT` and `D32_SFLOAT_S8_UINT`; `D32_SFLOAT` alone is not guaranteed.
/// `D32_SFLOAT` comes first because reversed Z needs a float buffer ([`mode_for`]).
const DEPTH_CANDIDATES: [vk::Format; 3] = [
    vk::Format::D32_SFLOAT,
    vk::Format::X8_D24_UNORM_PACK32,
    vk::Format::D24_UNORM_S8_UINT,
];

/// Chooses the depth format for this GPU and the depth mapping that goes with it. Called once
/// at startup.
pub fn depth_setup(context: &Context, gpu: &Gpu) -> Result<(vk::Format, DepthMode), RenderError> {
    let format = choose_depth_format(|format| {
        // SAFETY: the physical device was enumerated from this live instance.
        let properties = unsafe {
            context
                .instance()
                .get_physical_device_format_properties(gpu.physical(), format)
        };
        properties
            .optimal_tiling_features
            .contains(vk::FormatFeatureFlags::DEPTH_STENCIL_ATTACHMENT)
    })
    .ok_or(RenderError::Unsupported("no depth buffer format"))?;
    Ok((format, mode_for(format)))
}

/// The depth mapping for a depth buffer of `format`: reversed on `D32_SFLOAT`, where the
/// float's fine steps near 0 go to the far distance; standard on the 24-bit fixed-point
/// formats, whose steps are even, so reversing them gains nothing (decision 0015).
fn mode_for(format: vk::Format) -> DepthMode {
    if format == vk::Format::D32_SFLOAT {
        DepthMode::Reversed
    } else {
        DepthMode::Standard
    }
}

/// The first candidate format for which `is_supported` holds.
fn choose_depth_format(is_supported: impl Fn(vk::Format) -> bool) -> Option<vk::Format> {
    DEPTH_CANDIDATES
        .into_iter()
        .find(|&format| is_supported(format))
}

/// The image aspects a depth view of `format` must cover.
fn aspect_mask(format: vk::Format) -> vk::ImageAspectFlags {
    if format == vk::Format::D24_UNORM_S8_UINT {
        vk::ImageAspectFlags::DEPTH | vk::ImageAspectFlags::STENCIL
    } else {
        vk::ImageAspectFlags::DEPTH
    }
}

/// A depth image with its memory and view, sized like the swapchain images.
pub struct DepthImage {
    device: ash::Device,
    image: vk::Image,
    memory: vk::DeviceMemory,
    view: vk::ImageView,
}

impl DepthImage {
    /// Creates a depth image of `format` and `extent` in device-local memory.
    pub fn new(gpu: &Gpu, format: vk::Format, extent: vk::Extent2D) -> Result<Self, RenderError> {
        let device = gpu.device();
        // Start with null handles: if a later call fails, `Drop` destroys whatever
        // was created so far (destroying or freeing a null handle is a no-op).
        let mut depth = Self {
            device: device.clone(),
            image: vk::Image::null(),
            memory: vk::DeviceMemory::null(),
            view: vk::ImageView::null(),
        };
        let info = image_info(format, extent);
        // SAFETY: the device is alive, `info` outlives the call, and the format supports
        // depth attachments with optimal tiling (checked by `depth_setup`).
        depth.image = unsafe { device.create_image(&info, None)? };
        // SAFETY: the image was just created from this device.
        let requirements = unsafe { device.get_image_memory_requirements(depth.image) };
        depth.memory = memory::allocate(
            device,
            gpu.memory_properties(),
            requirements,
            vk::MemoryPropertyFlags::DEVICE_LOCAL,
        )?;
        // SAFETY: the memory was allocated for this image's requirements and is unbound.
        unsafe { device.bind_image_memory(depth.image, depth.memory, 0)? };
        let view_info = vk::ImageViewCreateInfo::default()
            .image(depth.image)
            .view_type(vk::ImageViewType::TYPE_2D)
            .format(format)
            .subresource_range(vk::ImageSubresourceRange {
                aspect_mask: aspect_mask(format),
                base_mip_level: 0,
                level_count: 1,
                base_array_layer: 0,
                layer_count: 1,
            });
        // SAFETY: the image is alive, bound to memory and was created with `format`.
        depth.view = unsafe { device.create_image_view(&view_info, None)? };
        Ok(depth)
    }

    /// The view used as the framebuffer's depth attachment.
    pub fn view(&self) -> vk::ImageView {
        self.view
    }
}

impl Drop for DepthImage {
    fn drop(&mut self) {
        // SAFETY: the owning swapchain is dropped only after the device went idle, and
        // its framebuffers (which point at the view) are destroyed first. The view goes
        // before the image, the image before its memory. Null handles are ignored.
        unsafe {
            self.device.destroy_image_view(self.view, None);
            self.device.destroy_image(self.image, None);
            self.device.free_memory(self.memory, None);
        }
    }
}

/// A single-sample 2D depth attachment that is never read after the frame.
fn image_info(format: vk::Format, extent: vk::Extent2D) -> vk::ImageCreateInfo<'static> {
    vk::ImageCreateInfo::default()
        .image_type(vk::ImageType::TYPE_2D)
        .format(format)
        .extent(vk::Extent3D {
            width: extent.width,
            height: extent.height,
            depth: 1,
        })
        .mip_levels(1)
        .array_layers(1)
        .samples(vk::SampleCountFlags::TYPE_1)
        .tiling(vk::ImageTiling::OPTIMAL)
        .usage(vk::ImageUsageFlags::DEPTH_STENCIL_ATTACHMENT)
        .sharing_mode(vk::SharingMode::EXCLUSIVE)
        .initial_layout(vk::ImageLayout::UNDEFINED)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefers_d32_float() {
        assert_eq!(choose_depth_format(|_| true), Some(vk::Format::D32_SFLOAT));
    }

    #[test]
    fn falls_back_in_candidate_order() {
        let no_d32 = |format| format != vk::Format::D32_SFLOAT;
        assert_eq!(
            choose_depth_format(no_d32),
            Some(vk::Format::X8_D24_UNORM_PACK32)
        );
        let only_stencil = |format| format == vk::Format::D24_UNORM_S8_UINT;
        assert_eq!(
            choose_depth_format(only_stencil),
            Some(vk::Format::D24_UNORM_S8_UINT)
        );
    }

    #[test]
    fn no_supported_format_gives_none() {
        assert_eq!(choose_depth_format(|_| false), None);
    }

    #[test]
    fn only_the_float_buffer_runs_reversed() {
        assert_eq!(mode_for(vk::Format::D32_SFLOAT), DepthMode::Reversed);
        assert_eq!(
            mode_for(vk::Format::X8_D24_UNORM_PACK32),
            DepthMode::Standard
        );
        assert_eq!(mode_for(vk::Format::D24_UNORM_S8_UINT), DepthMode::Standard);
        // Every candidate but the first falls back to standard depth.
        assert!(
            DEPTH_CANDIDATES[1..]
                .iter()
                .all(|&format| mode_for(format) == DepthMode::Standard)
        );
    }

    #[test]
    fn stencil_aspect_only_for_stencil_formats() {
        assert_eq!(
            aspect_mask(vk::Format::D32_SFLOAT),
            vk::ImageAspectFlags::DEPTH
        );
        assert!(aspect_mask(vk::Format::D24_UNORM_S8_UINT).contains(vk::ImageAspectFlags::STENCIL));
    }
}
