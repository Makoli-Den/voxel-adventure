//! Per-frame resources: command buffers and synchronization objects.

use ash::vk;

use super::gpu::Gpu;
use crate::RenderError;

/// How many frames the CPU may prepare while the GPU is still drawing earlier ones.
/// Two keeps both busy without adding much input latency.
pub const FRAMES_IN_FLIGHT: usize = 2;

/// Resources used by one frame in flight.
#[derive(Clone, Copy)]
pub struct FrameSlot {
    /// Commands recorded for this frame.
    pub command_buffer: vk::CommandBuffer,
    /// Signaled when the swapchain image is ready to be drawn into.
    pub image_available: vk::Semaphore,
    /// Signaled when the GPU finished this frame; lets the CPU reuse the slot.
    pub in_flight: vk::Fence,
}

/// All per-frame resources, plus one "rendering finished" semaphore per swapchain image.
pub struct Frames {
    device: ash::Device,
    pool: vk::CommandPool,
    slots: Vec<FrameSlot>,
    render_finished: Vec<vk::Semaphore>,
    current: usize,
}

impl Frames {
    /// Creates the resources for [`FRAMES_IN_FLIGHT`] frames.
    ///
    /// The per-image semaphores are created later by [`Self::recreate_render_finished`],
    /// once a swapchain exists and its image count is known.
    pub fn new(gpu: &Gpu) -> Result<Self, RenderError> {
        // Start with a null pool and no objects: if a later call fails, `Drop` still
        // destroys whatever was created so far (destroying a null handle is a no-op).
        let mut frames = Self {
            device: gpu.device().clone(),
            pool: vk::CommandPool::null(),
            slots: Vec::with_capacity(FRAMES_IN_FLIGHT),
            render_finished: Vec::new(),
            current: 0,
        };

        let pool_info = vk::CommandPoolCreateInfo::default()
            .queue_family_index(gpu.queue_family())
            .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER);
        // SAFETY: the device is alive and `pool_info` outlives the call.
        frames.pool = unsafe { frames.device.create_command_pool(&pool_info, None)? };

        let allocate_info = vk::CommandBufferAllocateInfo::default()
            .command_pool(frames.pool)
            .level(vk::CommandBufferLevel::PRIMARY)
            .command_buffer_count(FRAMES_IN_FLIGHT as u32);
        // SAFETY: the pool was just created from this device.
        let command_buffers = unsafe { frames.device.allocate_command_buffers(&allocate_info)? };

        for command_buffer in command_buffers {
            let slot = frames.create_slot(command_buffer)?;
            frames.slots.push(slot);
        }
        Ok(frames)
    }

    /// The resources of the frame being prepared now.
    pub fn current_slot(&self) -> FrameSlot {
        // Indexing is safe: `current` is always kept below `FRAMES_IN_FLIGHT`,
        // and `new` creates exactly that many slots.
        self.slots[self.current]
    }

    /// Moves on to the next frame in flight.
    pub fn advance(&mut self) {
        self.current = (self.current + 1) % FRAMES_IN_FLIGHT;
    }

    /// The semaphore signaled when rendering to the swapchain image `image_index` is done.
    pub fn render_finished(&self, image_index: u32) -> vk::Semaphore {
        // Indexing is safe: there is one semaphore per swapchain image and the
        // driver only returns indices below the image count.
        self.render_finished[image_index as usize]
    }

    /// Replaces the per-image semaphores, one for each of `image_count` swapchain images.
    ///
    /// # Safety
    /// The GPU must be idle (for example after `device_wait_idle`), so that none of the
    /// old semaphores is still waited on or signaled by pending work.
    pub unsafe fn recreate_render_finished(
        &mut self,
        image_count: usize,
    ) -> Result<(), RenderError> {
        // SAFETY: forwarded from this function's own contract: the GPU is idle.
        unsafe { self.destroy_render_finished() };
        for _ in 0..image_count {
            let semaphore = self.create_semaphore()?;
            self.render_finished.push(semaphore);
        }
        Ok(())
    }

    /// Creates one frame slot, cleaning up after itself if a step fails.
    fn create_slot(&self, command_buffer: vk::CommandBuffer) -> Result<FrameSlot, RenderError> {
        let image_available = self.create_semaphore()?;
        // Created signaled, so the first wait on this frame returns immediately.
        let fence_info = vk::FenceCreateInfo::default().flags(vk::FenceCreateFlags::SIGNALED);
        // SAFETY: the device is alive and `fence_info` outlives the call.
        match unsafe { self.device.create_fence(&fence_info, None) } {
            Ok(in_flight) => Ok(FrameSlot {
                command_buffer,
                image_available,
                in_flight,
            }),
            Err(error) => {
                // SAFETY: the semaphore was created just above and was never used.
                unsafe { self.device.destroy_semaphore(image_available, None) };
                Err(error.into())
            }
        }
    }

    fn create_semaphore(&self) -> Result<vk::Semaphore, RenderError> {
        // SAFETY: the device is alive and the create info is a plain default.
        Ok(unsafe {
            self.device
                .create_semaphore(&vk::SemaphoreCreateInfo::default(), None)?
        })
    }

    /// # Safety
    /// The GPU must be idle, so no destroyed semaphore is in use.
    unsafe fn destroy_render_finished(&mut self) {
        for semaphore in self.render_finished.drain(..) {
            // SAFETY: the caller guarantees the GPU is idle.
            unsafe { self.device.destroy_semaphore(semaphore, None) };
        }
    }
}

impl Drop for Frames {
    fn drop(&mut self) {
        // SAFETY: the renderer waits for the device to go idle before dropping this.
        unsafe {
            self.destroy_render_finished();
            for slot in self.slots.drain(..) {
                self.device.destroy_semaphore(slot.image_available, None);
                self.device.destroy_fence(slot.in_flight, None);
            }
            // Destroying the pool also frees its command buffers.
            self.device.destroy_command_pool(self.pool, None);
        }
    }
}
