//! Choosing a GPU and creating the logical device.

use ash::{khr, vk};

use super::context::{Context, Surface};
use super::timestamps::TimestampClock;
use crate::RenderError;
use crate::texture::TextureLimits;

/// The GPU name shown when the driver reports one that is not valid text.
const UNKNOWN_GPU_NAME: &str = "unknown GPU";

/// A chosen GPU, its logical device and the single queue we use.
///
/// One queue handles both graphics and presentation, which every desktop GPU supports.
pub struct Gpu {
    physical: vk::PhysicalDevice,
    device: ash::Device,
    queue: vk::Queue,
    queue_family: u32,
    name: String,
    /// Memory types and heaps of the GPU, read once because they never change.
    memory_properties: vk::PhysicalDeviceMemoryProperties,
    /// How to read the queue's timestamps; `None` if the queue cannot write them.
    timestamp_clock: Option<TimestampClock>,
    /// How large the block texture array may be on this GPU.
    texture_limits: TextureLimits,
}

impl Gpu {
    /// Picks the best GPU that can draw and present to `surface`, preferring discrete ones.
    pub fn new(context: &Context, surface: &Surface) -> Result<Self, RenderError> {
        let instance = context.instance();
        // SAFETY: `instance` is alive for the whole call; it is borrowed from `context`.
        let candidates = unsafe { instance.enumerate_physical_devices()? };

        let mut best: Option<(u32, vk::PhysicalDevice, u32, vk::PhysicalDeviceProperties)> = None;
        for physical in candidates {
            if !supports_swapchain(instance, physical)? {
                continue;
            }
            let Some(family) = find_queue_family(instance, surface, physical)? else {
                continue;
            };
            // SAFETY: `physical` was just returned by `enumerate_physical_devices`
            // on this live instance.
            let properties = unsafe { instance.get_physical_device_properties(physical) };
            let score = score_device(&properties);
            if best.is_none_or(|(best_score, ..)| score > best_score) {
                best = Some((score, physical, family, properties));
            }
        }
        let (_, physical, queue_family, properties) = best.ok_or(RenderError::NoSuitableGpu)?;

        let priorities = [1.0_f32];
        let queue_infos = [vk::DeviceQueueCreateInfo::default()
            .queue_family_index(queue_family)
            .queue_priorities(&priorities)];
        let extensions = [khr::swapchain::NAME.as_ptr()];
        let device_info = vk::DeviceCreateInfo::default()
            .queue_create_infos(&queue_infos)
            .enabled_extension_names(&extensions);

        // SAFETY: `physical` came from this instance and `device_info` outlives the call.
        let device = unsafe { instance.create_device(physical, &device_info, None)? };
        // SAFETY: the queue family and index 0 were requested in `device_info`.
        let queue = unsafe { device.get_device_queue(queue_family, 0) };

        // SAFETY: `physical` came from this live instance.
        let memory_properties = unsafe { instance.get_physical_device_memory_properties(physical) };
        let timestamp_clock = TimestampClock::new(
            timestamp_valid_bits(instance, physical, queue_family),
            properties.limits.timestamp_period,
        );
        let texture_limits = TextureLimits::of_device(
            properties.limits.max_image_dimension2_d,
            properties.limits.max_image_array_layers,
        );
        let name = properties.device_name_as_c_str().map_or_else(
            |_| String::from(UNKNOWN_GPU_NAME),
            |n| n.to_string_lossy().into_owned(),
        );

        Ok(Self {
            physical,
            device,
            queue,
            queue_family,
            name,
            memory_properties,
            timestamp_clock,
            texture_limits,
        })
    }

    /// The physical device.
    pub fn physical(&self) -> vk::PhysicalDevice {
        self.physical
    }

    /// The logical device.
    pub fn device(&self) -> &ash::Device {
        &self.device
    }

    /// The graphics and present queue.
    pub fn queue(&self) -> vk::Queue {
        self.queue
    }

    /// Index of the queue family the queue belongs to.
    pub fn queue_family(&self) -> u32 {
        self.queue_family
    }

    /// Human-readable GPU name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The GPU's memory types and heaps.
    pub fn memory_properties(&self) -> &vk::PhysicalDeviceMemoryProperties {
        &self.memory_properties
    }

    /// How to read the queue's timestamps, or `None` if it cannot write them.
    pub fn timestamp_clock(&self) -> Option<TimestampClock> {
        self.timestamp_clock
    }

    /// How large the block texture array may be on this GPU.
    pub fn texture_limits(&self) -> TextureLimits {
        self.texture_limits
    }
}

impl Drop for Gpu {
    fn drop(&mut self) {
        // SAFETY: the renderer waits for the device to go idle and drops every object
        // created from the device before this runs (field order of `VulkanRenderer`).
        unsafe { self.device.destroy_device(None) };
    }
}

/// Discrete GPUs score highest, then integrated ones, then everything else.
fn score_device(properties: &vk::PhysicalDeviceProperties) -> u32 {
    match properties.device_type {
        vk::PhysicalDeviceType::DISCRETE_GPU => 2,
        vk::PhysicalDeviceType::INTEGRATED_GPU => 1,
        _ => 0,
    }
}

/// Whether the device offers the swapchain extension.
fn supports_swapchain(
    instance: &ash::Instance,
    physical: vk::PhysicalDevice,
) -> Result<bool, RenderError> {
    // SAFETY: `physical` was returned by `enumerate_physical_devices` on this live instance.
    let extensions = unsafe { instance.enumerate_device_extension_properties(physical)? };
    Ok(extensions.iter().any(|e| {
        e.extension_name_as_c_str()
            .is_ok_and(|name| name == khr::swapchain::NAME)
    }))
}

/// How many bits of a timestamp the queue family `family` writes; 0 if it writes none.
fn timestamp_valid_bits(
    instance: &ash::Instance,
    physical: vk::PhysicalDevice,
    family: u32,
) -> u32 {
    // SAFETY: `physical` was returned by `enumerate_physical_devices` on this live instance.
    let families = unsafe { instance.get_physical_device_queue_family_properties(physical) };
    // Lossless: a family index is a small `u32`.
    families
        .get(family as usize)
        .map_or(0, |properties| properties.timestamp_valid_bits)
}

/// Finds a queue family that supports both graphics and presenting to `surface`.
fn find_queue_family(
    instance: &ash::Instance,
    surface: &Surface,
    physical: vk::PhysicalDevice,
) -> Result<Option<u32>, RenderError> {
    // SAFETY: `physical` was returned by `enumerate_physical_devices` on this live instance.
    let families = unsafe { instance.get_physical_device_queue_family_properties(physical) };
    for (index, family) in (0_u32..).zip(families.iter()) {
        if !family.queue_flags.contains(vk::QueueFlags::GRAPHICS) {
            continue;
        }
        // SAFETY: `index` is a valid family index of `physical`, and the surface is alive.
        let can_present = unsafe {
            surface.loader().get_physical_device_surface_support(
                physical,
                index,
                surface.handle(),
            )?
        };
        if can_present {
            return Ok(Some(index));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn properties_of(device_type: vk::PhysicalDeviceType) -> vk::PhysicalDeviceProperties {
        vk::PhysicalDeviceProperties {
            device_type,
            ..Default::default()
        }
    }

    #[test]
    fn discrete_beats_integrated_beats_other() {
        let discrete = score_device(&properties_of(vk::PhysicalDeviceType::DISCRETE_GPU));
        let integrated = score_device(&properties_of(vk::PhysicalDeviceType::INTEGRATED_GPU));
        let other = score_device(&properties_of(vk::PhysicalDeviceType::CPU));
        assert!(discrete > integrated);
        assert!(integrated > other);
    }
}
