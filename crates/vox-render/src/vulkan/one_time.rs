//! One-time GPU commands: record a command buffer, submit it, wait until it finished.
//!
//! Used for uploads that happen at startup or on a pack reload (block textures), never per
//! frame. A pool, a command buffer and a fence of their own keep this independent of the
//! frame loop's per-frame resources.

use ash::vk;

use super::NO_TIMEOUT;
use super::gpu::Gpu;
use crate::RenderError;

/// A command buffer being recorded for one submission, with its own pool and fence.
///
/// Dropping it destroys everything, unless the work was submitted and neither its fence nor
/// a wait for the whole device proved it finished: then the pool and fence are leaked,
/// because the GPU may still use them (see [`Self::submit_and_wait`]).
pub struct OneTimeCommands {
    device: ash::Device,
    queue: vk::Queue,
    pool: vk::CommandPool,
    command_buffer: vk::CommandBuffer,
    fence: vk::Fence,
    /// Submitted, and not yet proven finished. `Drop` leaks everything while this is set.
    pending: bool,
}

impl OneTimeCommands {
    /// Creates the objects and begins recording.
    pub fn begin(gpu: &Gpu) -> Result<Self, RenderError> {
        // Null handles until created: destroying a null handle is a no-op, so `Drop`
        // cleans up whatever exists if a later step fails.
        let mut commands = Self {
            device: gpu.device().clone(),
            queue: gpu.queue(),
            pool: vk::CommandPool::null(),
            command_buffer: vk::CommandBuffer::null(),
            fence: vk::Fence::null(),
            pending: false,
        };
        let device = &commands.device;
        let pool_info = vk::CommandPoolCreateInfo::default()
            .queue_family_index(gpu.queue_family())
            .flags(vk::CommandPoolCreateFlags::TRANSIENT);
        // SAFETY: the device is alive and `pool_info` outlives the call.
        commands.pool = unsafe { device.create_command_pool(&pool_info, None)? };
        let allocate_info = vk::CommandBufferAllocateInfo::default()
            .command_pool(commands.pool)
            .level(vk::CommandBufferLevel::PRIMARY)
            .command_buffer_count(1);
        // SAFETY: the pool was just created from this device.
        let buffers = unsafe { device.allocate_command_buffers(&allocate_info)? };
        commands.command_buffer = buffers.first().copied().ok_or(RenderError::Unsupported(
            "the driver returned no command buffer",
        ))?;
        // SAFETY: the device is alive and the create info is a plain default (unsignaled).
        commands.fence = unsafe { device.create_fence(&vk::FenceCreateInfo::default(), None)? };
        let begin_info = vk::CommandBufferBeginInfo::default()
            .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);
        // SAFETY: the command buffer was just allocated and is in the initial state.
        unsafe { device.begin_command_buffer(commands.command_buffer, &begin_info)? };
        Ok(commands)
    }

    /// The command buffer to record into; it is in the recording state.
    pub fn command_buffer(&self) -> vk::CommandBuffer {
        self.command_buffer
    }

    /// Ends recording, submits to the queue and waits until the GPU finished the commands.
    ///
    /// `in_use` holds everything the commands read or write (buffers, images). It is given
    /// back once the fence proved the work finished. On an error after the submit, the
    /// device is waited for; if that fails too (the device is lost, or out of memory),
    /// nothing proves the GPU is done with them, so `in_use` and the command objects are
    /// leaked rather than destroyed while possibly in use. The renderer is unusable after
    /// such an error anyway.
    pub fn submit_and_wait<T>(mut self, in_use: T) -> Result<T, RenderError> {
        match self.finish() {
            Ok(()) => Ok(in_use),
            Err(error) => {
                if self.pending && !self.wait_for_device() {
                    std::mem::forget(in_use);
                }
                Err(error)
            }
        }
    }

    /// Ends recording, submits and waits on the fence. `pending` is set while the
    /// submitted work is not proven finished.
    fn finish(&mut self) -> Result<(), RenderError> {
        let device = &self.device;
        // SAFETY: the command buffer is recording (begun in `begin`, ended only here).
        unsafe { device.end_command_buffer(self.command_buffer)? };
        let command_buffers = [self.command_buffer];
        let submit = vk::SubmitInfo::default().command_buffers(&command_buffers);
        // SAFETY: the queue, command buffer and fence belong to this device; the fence is
        // unsignaled and unused. The renderer uses the queue from one thread only (`&mut`),
        // which is the external synchronization Vulkan requires.
        unsafe { device.queue_submit(self.queue, &[submit], self.fence)? };
        self.pending = true;
        // SAFETY: the fence belongs to this device and was just submitted with the work.
        unsafe { device.wait_for_fences(&[self.fence], true, NO_TIMEOUT)? };
        self.pending = false;
        Ok(())
    }

    /// After a failed fence wait: waits for the whole device. Returns whether that proved
    /// the submitted work finished, and clears `pending` if so.
    fn wait_for_device(&mut self) -> bool {
        // SAFETY: the device is alive; waiting for idle has no other precondition.
        let idle = unsafe { self.device.device_wait_idle() }.is_ok();
        if idle {
            self.pending = false;
        }
        idle
    }
}

impl Drop for OneTimeCommands {
    fn drop(&mut self) {
        if self.pending {
            // Submitted work that neither the fence nor a device wait proved finished may
            // still use the command buffer and signal the fence: leak both.
            return;
        }
        // SAFETY: the device is alive, and the work was never submitted or was proven
        // finished (by the fence or a device wait), so the GPU no longer uses the fence or
        // the command buffer. Destroying the pool frees its command buffer. Null handles
        // are ignored.
        unsafe {
            self.device.destroy_fence(self.fence, None);
            self.device.destroy_command_pool(self.pool, None);
        }
    }
}
