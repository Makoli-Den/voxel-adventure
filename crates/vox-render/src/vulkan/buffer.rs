//! GPU buffers filled once from the CPU.

use ash::vk;

use super::gpu::Gpu;
use super::memory;
use crate::RenderError;

/// A buffer with its own memory, filled with data at creation.
///
/// The memory is host-visible and host-coherent, so the CPU writes it directly and
/// no transfer commands are needed. On discrete GPUs the vertex fetch then reads over
/// the bus. A staging copy into device-local memory and sub-allocation come later,
/// once a benchmark shows the cost (see `docs/decisions/0004-chunk-rendering.md`).
pub struct Buffer {
    device: ash::Device,
    handle: vk::Buffer,
    memory: vk::DeviceMemory,
}

impl Buffer {
    /// Creates a buffer for `usage` holding a copy of `data`. `data` must not be empty.
    pub fn with_data(
        gpu: &Gpu,
        usage: vk::BufferUsageFlags,
        data: &[u8],
    ) -> Result<Self, RenderError> {
        let device = gpu.device();
        // Start with null handles: if a later call fails, `Drop` destroys whatever
        // was created so far (destroying or freeing a null handle is a no-op).
        let mut buffer = Self {
            device: device.clone(),
            handle: vk::Buffer::null(),
            memory: vk::DeviceMemory::null(),
        };
        let size = data.len() as vk::DeviceSize;
        let info = vk::BufferCreateInfo::default()
            .size(size)
            .usage(usage)
            .sharing_mode(vk::SharingMode::EXCLUSIVE);
        // SAFETY: the device is alive and `info` outlives the call. A zero size is
        // invalid; callers never pass empty data (see `GpuMesh::new`).
        buffer.handle = unsafe { device.create_buffer(&info, None)? };
        // SAFETY: the buffer was just created from this device.
        let requirements = unsafe { device.get_buffer_memory_requirements(buffer.handle) };
        let wanted = vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT;
        buffer.memory = memory::allocate(device, gpu.memory_properties(), requirements, wanted)?;
        // SAFETY: the memory was allocated for this buffer's requirements and is unbound.
        unsafe { device.bind_buffer_memory(buffer.handle, buffer.memory, 0)? };
        buffer.fill(data)?;
        Ok(buffer)
    }

    /// The raw buffer handle.
    pub fn handle(&self) -> vk::Buffer {
        self.handle
    }

    /// Copies `data` to the start of the buffer's memory.
    fn fill(&self, data: &[u8]) -> Result<(), RenderError> {
        // SAFETY: the memory is host-visible, not mapped yet, and the GPU does not use
        // it (the buffer was just created). The mapped range covers the whole allocation,
        // which is at least as large as the buffer, which is `data.len()` bytes.
        unsafe {
            let mapped = self.device.map_memory(
                self.memory,
                0,
                vk::WHOLE_SIZE,
                vk::MemoryMapFlags::empty(),
            )?;
            std::ptr::copy_nonoverlapping(data.as_ptr(), mapped.cast::<u8>(), data.len());
            // Host-coherent memory needs no flush; unmapping makes the copy final.
            self.device.unmap_memory(self.memory);
        }
        Ok(())
    }
}

impl Drop for Buffer {
    fn drop(&mut self) {
        // SAFETY: the renderer waits for the device to go idle before dropping a buffer,
        // so no command buffer still reads it. The buffer goes before its memory.
        // Null handles are ignored.
        unsafe {
            self.device.destroy_buffer(self.handle, None);
            self.device.free_memory(self.memory, None);
        }
    }
}
