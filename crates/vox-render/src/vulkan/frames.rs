//! Per-frame resources: command buffers, synchronization objects and timestamp queries.

use std::time::Duration;

use ash::vk;

use super::gpu::Gpu;
use super::timestamps::{FRAME_START_QUERY, TIMESTAMPS_PER_FRAME, TimestampClock};
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
    /// The frame's start and end timestamps; `None` if the queue cannot write them.
    pub timestamps: Option<vk::QueryPool>,
}

/// All per-frame resources, plus one "rendering finished" semaphore per swapchain image.
pub struct Frames {
    device: ash::Device,
    pool: vk::CommandPool,
    slots: Vec<FrameSlot>,
    render_finished: Vec<vk::Semaphore>,
    /// How many frames have been submitted; the number of the next frame. Frame `k`
    /// always uses slot [`slot_index`]`(k)`; the slot is derived, never stored, so the
    /// two cannot drift apart.
    submitted: u64,
    /// How to read the timestamps; `Some` exactly when every slot has a query pool.
    clock: Option<TimestampClock>,
    /// Per slot: whether the last frame submitted in it wrote timestamps that have not
    /// been read yet. Set only after a successful submit, so a slot that was never used
    /// or whose frame failed before the submit has nothing to read.
    measured: [bool; FRAMES_IN_FLIGHT],
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
            submitted: 0,
            clock: gpu.timestamp_clock(),
            measured: [false; FRAMES_IN_FLIGHT],
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
        // Indexing is safe: `slot_index` is always below `FRAMES_IN_FLIGHT`,
        // and `new` creates exactly that many slots.
        self.slots[slot_index(self.submitted)]
    }

    /// Moves on to the next frame in flight. Call once per submitted frame, right after
    /// the submit. If the slot has a query pool, the submitted frame must have written
    /// its timestamps: the slot is flagged as having a measurement to read.
    pub fn advance(&mut self) {
        let index = slot_index(self.submitted);
        self.measured[index] = self.slots[index].timestamps.is_some();
        self.submitted += 1;
    }

    /// The GPU time of the last frame submitted in the current slot, if that frame was
    /// measured and its time has not been taken yet. Each measurement is returned once.
    ///
    /// Call right after waiting on the current slot's fence: that proves the frame
    /// finished, so its results are available and reading them never waits. Called
    /// earlier, the results may not be ready and the measurement is dropped.
    pub fn take_gpu_time(&mut self) -> Result<Option<Duration>, RenderError> {
        let index = slot_index(self.submitted);
        let (Some(clock), Some(pool)) = (self.clock, self.slots[index].timestamps) else {
            return Ok(None);
        };
        if !std::mem::take(&mut self.measured[index]) {
            return Ok(None);
        }
        let mut ticks = [0_u64; TIMESTAMPS_PER_FRAME as usize];
        // SAFETY: the pool belongs to this device and has `TIMESTAMPS_PER_FRAME` queries,
        // all read here, and the frame that wrote them was submitted. TYPE_64 matches the
        // 8-byte stride and the size of `ticks`. No WAIT flag: the caller's fence wait
        // made the results available, and without it the call returns NOT_READY instead
        // of blocking. PARTIAL is not set (not allowed for timestamps).
        let read = unsafe {
            self.device.get_query_pool_results(
                pool,
                FRAME_START_QUERY,
                &mut ticks,
                vk::QueryResultFlags::TYPE_64,
            )
        };
        match read {
            Ok(()) => {
                let [start, end] = ticks;
                Ok(clock.elapsed(start, end))
            }
            Err(vk::Result::NOT_READY) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    /// Whether the fence of every slot is signaled right now. Does not wait.
    ///
    /// Each fence is signaled by the last frame submitted in its slot (or was created
    /// signaled), so `true` means each of the last [`FRAMES_IN_FLIGHT`] frames finished.
    pub fn all_fences_signaled(&self) -> Result<bool, RenderError> {
        for slot in &self.slots {
            // SAFETY: the fence belongs to this device and stays alive as long as `self`.
            let signaled = unsafe { self.device.get_fence_status(slot.in_flight)? };
            if !signaled {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// How many frames have been submitted so far, which is also the number of the
    /// frame being prepared now (frames are numbered from 0).
    pub fn submitted(&self) -> u64 {
        self.submitted
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
        // Null handles until created: destroying a null handle is a no-op, so a failure
        // can destroy the partial slot as a whole.
        let mut slot = FrameSlot {
            command_buffer,
            image_available: vk::Semaphore::null(),
            in_flight: vk::Fence::null(),
            timestamps: None,
        };
        match self.fill_slot(&mut slot) {
            Ok(()) => Ok(slot),
            Err(error) => {
                // SAFETY: the slot's objects were created just now and never used.
                unsafe { self.destroy_slot(slot) };
                Err(error)
            }
        }
    }

    /// Creates the synchronization objects of `slot` and, if the queue can write
    /// timestamps, its query pool. Stops at the first failure, leaving the rest null.
    fn fill_slot(&self, slot: &mut FrameSlot) -> Result<(), RenderError> {
        slot.image_available = self.create_semaphore()?;
        // Created signaled, so the first wait on this frame returns immediately.
        let fence_info = vk::FenceCreateInfo::default().flags(vk::FenceCreateFlags::SIGNALED);
        // SAFETY: the device is alive and `fence_info` outlives the call.
        slot.in_flight = unsafe { self.device.create_fence(&fence_info, None)? };
        if self.clock.is_some() {
            let pool_info = vk::QueryPoolCreateInfo::default()
                .query_type(vk::QueryType::TIMESTAMP)
                .query_count(TIMESTAMPS_PER_FRAME);
            // SAFETY: the device is alive and `pool_info` outlives the call. Timestamp
            // pools are core Vulkan 1.0 and need no device feature.
            slot.timestamps = Some(unsafe { self.device.create_query_pool(&pool_info, None)? });
        }
        Ok(())
    }

    /// Destroys the objects of `slot`, except its command buffer, which the pool owns.
    ///
    /// # Safety
    /// No pending GPU work may use the slot's objects.
    unsafe fn destroy_slot(&self, slot: FrameSlot) {
        // SAFETY: the caller guarantees nothing uses the objects; null handles are no-ops.
        unsafe {
            self.device.destroy_semaphore(slot.image_available, None);
            self.device.destroy_fence(slot.in_flight, None);
            if let Some(pool) = slot.timestamps {
                self.device.destroy_query_pool(pool, None);
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

/// The slot frame number `frame` uses: frames take turns through the slots.
pub fn slot_index(frame: u64) -> usize {
    // Lossless: the remainder is below `FRAMES_IN_FLIGHT`, a small `usize`.
    (frame % FRAMES_IN_FLIGHT as u64) as usize
}

impl Drop for Frames {
    fn drop(&mut self) {
        // SAFETY: the renderer waits for the device to go idle before dropping this.
        unsafe {
            self.destroy_render_finished();
            for slot in std::mem::take(&mut self.slots) {
                self.destroy_slot(slot);
            }
            // Destroying the pool also frees its command buffers.
            self.device.destroy_command_pool(self.pool, None);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_take_turns_through_the_slots() {
        let slots: Vec<usize> = (0..5).map(slot_index).collect();
        assert_eq!(slots, [0, 1, 0, 1, 0]);
    }

    #[test]
    fn the_last_frames_in_flight_use_distinct_slots() {
        // The fence-status shortcut in `retired` relies on this: the last
        // FRAMES_IN_FLIGHT frames each own a different slot.
        for submitted in FRAMES_IN_FLIGHT as u64..20 {
            let mut used: Vec<usize> = (submitted - FRAMES_IN_FLIGHT as u64..submitted)
                .map(slot_index)
                .collect();
            used.sort_unstable();
            used.dedup();
            assert_eq!(used.len(), FRAMES_IN_FLIGHT);
        }
    }

    #[test]
    fn slot_index_stays_in_range_for_huge_frame_numbers() {
        assert!(slot_index(u64::MAX) < FRAMES_IN_FLIGHT);
    }
}
