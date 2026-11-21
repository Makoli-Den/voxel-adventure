//! Device memory allocation shared by buffers and images.
//!
//! Every resource gets its own allocation for now. Drivers limit the number of
//! allocations (`maxMemoryAllocationCount`, at least 4096), so sub-allocating from
//! large blocks comes later, once the number of meshes makes it necessary.

use ash::vk;

use crate::RenderError;

/// Finds the first memory type allowed by `type_bits` that has all of the `wanted` flags.
///
/// `type_bits` comes from a resource's memory requirements: bit `i` set means memory
/// type `i` may back the resource.
pub fn find_memory_type(
    properties: &vk::PhysicalDeviceMemoryProperties,
    type_bits: u32,
    wanted: vk::MemoryPropertyFlags,
) -> Option<u32> {
    let count = properties
        .memory_type_count
        .min(vk::MAX_MEMORY_TYPES as u32);
    (0..count).find(|&index| {
        let allowed = type_bits & (1 << index) != 0;
        // Indexing is safe: `count` is capped at the array length above.
        allowed
            && properties.memory_types[index as usize]
                .property_flags
                .contains(wanted)
    })
}

/// Allocates memory for a resource with `requirements`, from a type with the `wanted` flags.
pub fn allocate(
    device: &ash::Device,
    properties: &vk::PhysicalDeviceMemoryProperties,
    requirements: vk::MemoryRequirements,
    wanted: vk::MemoryPropertyFlags,
) -> Result<vk::DeviceMemory, RenderError> {
    let type_index = find_memory_type(properties, requirements.memory_type_bits, wanted).ok_or(
        RenderError::Unsupported("no GPU memory type fits the resource"),
    )?;
    let info = vk::MemoryAllocateInfo::default()
        .allocation_size(requirements.size)
        .memory_type_index(type_index);
    // SAFETY: the device is alive, `info` outlives the call and the type index was
    // taken from this device's memory properties.
    Ok(unsafe { device.allocate_memory(&info, None)? })
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOST: vk::MemoryPropertyFlags = vk::MemoryPropertyFlags::from_raw(
        vk::MemoryPropertyFlags::HOST_VISIBLE.as_raw()
            | vk::MemoryPropertyFlags::HOST_COHERENT.as_raw(),
    );

    /// A device with three memory types: device-local, host-visible only, host-coherent.
    fn properties() -> vk::PhysicalDeviceMemoryProperties {
        let mut properties = vk::PhysicalDeviceMemoryProperties {
            memory_type_count: 3,
            ..Default::default()
        };
        properties.memory_types[0].property_flags = vk::MemoryPropertyFlags::DEVICE_LOCAL;
        properties.memory_types[1].property_flags = vk::MemoryPropertyFlags::HOST_VISIBLE;
        properties.memory_types[2].property_flags = HOST;
        properties
    }

    #[test]
    fn finds_type_with_all_wanted_flags() {
        assert_eq!(find_memory_type(&properties(), 0b111, HOST), Some(2));
    }

    #[test]
    fn takes_the_first_matching_type() {
        let wanted = vk::MemoryPropertyFlags::HOST_VISIBLE;
        assert_eq!(find_memory_type(&properties(), 0b111, wanted), Some(1));
    }

    #[test]
    fn respects_the_type_bits() {
        let wanted = vk::MemoryPropertyFlags::HOST_VISIBLE;
        assert_eq!(find_memory_type(&properties(), 0b100, wanted), Some(2));
        assert_eq!(find_memory_type(&properties(), 0b001, wanted), None);
    }

    #[test]
    fn ignores_types_past_the_count() {
        let mut properties = properties();
        properties.memory_type_count = 1;
        assert_eq!(find_memory_type(&properties, 0b111, HOST), None);
    }
}
