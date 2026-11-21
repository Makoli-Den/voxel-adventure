//! The Vulkan renderer: owns every Vulkan object and runs the frame loop.

use std::any::Any;
use std::sync::Arc;
use std::time::Instant;

use ash::vk;
use raw_window_handle::{HasDisplayHandle, HasWindowHandle};

use super::context::{Context, Surface};
use super::frames::{FrameSlot, Frames};
use super::gpu::Gpu;
use super::swapchain::Swapchain;
use crate::{RenderError, Renderer};

/// Draws into a window with Vulkan.
///
/// For now a frame is just the whole window cleared to a slowly changing color.
/// That is enough to prove the window, device, swapchain and synchronization work.
pub struct VulkanRenderer {
    // Rust drops fields in declaration order, so each object comes before the
    // ones it depends on. Do not reorder.
    frames: Frames,
    /// `None` while the window has no area to draw into (minimized).
    swapchain: Option<Swapchain>,
    gpu: Gpu,
    surface: Surface,
    context: Context,
    /// Keeps the native window alive for as long as the surface exists.
    _window: Arc<dyn Any + Send + Sync>,
    /// Window size in pixels, as last reported by [`Renderer::resize`].
    size: (u32, u32),
    /// Set when the swapchain no longer matches the window and must be rebuilt.
    needs_recreate: bool,
    /// Start of the demo animation: the clear color is a function of the time since then.
    started: Instant,
}

impl VulkanRenderer {
    /// Creates the renderer for `window`, whose current size is `width` by `height` pixels.
    pub fn new<W>(window: Arc<W>, width: u32, height: u32) -> Result<Self, RenderError>
    where
        W: HasDisplayHandle + HasWindowHandle + Send + Sync + 'static,
    {
        let display = window.display_handle()?.as_raw();
        let window_handle = window.window_handle()?.as_raw();

        let context = Context::new(display)?;
        // SAFETY: the handles come from `window`, which this renderer keeps alive
        // (`_window`) for longer than the surface (see the field order).
        let surface = unsafe { Surface::new(&context, display, window_handle)? };
        let gpu = Gpu::new(&context, &surface)?;
        let swapchain = Swapchain::new(&context, &surface, &gpu, (width, height), None)?;
        let mut frames = Frames::new(&gpu)?;
        if let Some(swapchain) = &swapchain {
            // SAFETY: nothing has been submitted yet, so the GPU is idle.
            unsafe { frames.recreate_render_finished(swapchain.image_count())? };
        }

        Ok(Self {
            frames,
            needs_recreate: swapchain.is_none(),
            swapchain,
            gpu,
            surface,
            context,
            _window: window,
            size: (width, height),
            started: Instant::now(),
        })
    }

    /// Rebuilds the swapchain for the current window size.
    ///
    /// Returns `false` if the surface has no area right now; the old swapchain is kept
    /// and the caller should try again later.
    fn recreate_swapchain(&mut self) -> Result<bool, RenderError> {
        let Some(new) = Swapchain::new(
            &self.context,
            &self.surface,
            &self.gpu,
            self.size,
            self.swapchain.as_ref(),
        )?
        else {
            return Ok(false);
        };
        let image_count = new.image_count();

        // SAFETY: the device is alive. Waiting for idle makes it safe to destroy the old
        // swapchain and the per-image semaphores below.
        unsafe { self.gpu.device().device_wait_idle()? };
        self.swapchain = Some(new); // the old swapchain is destroyed here
        // SAFETY: the GPU is idle, see above.
        unsafe { self.frames.recreate_render_finished(image_count)? };
        Ok(true)
    }

    /// Records the commands that clear `image` and prepare it for presentation.
    fn record_clear(
        &self,
        command_buffer: vk::CommandBuffer,
        image: vk::Image,
    ) -> Result<(), RenderError> {
        let device = self.gpu.device();
        let range = vk::ImageSubresourceRange {
            aspect_mask: vk::ImageAspectFlags::COLOR,
            base_mip_level: 0,
            level_count: 1,
            base_array_layer: 0,
            layer_count: 1,
        };
        let to_clear = image_barrier(
            image,
            range,
            (vk::ImageLayout::UNDEFINED, vk::AccessFlags::empty()),
            (
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                vk::AccessFlags::TRANSFER_WRITE,
            ),
        );
        let to_present = image_barrier(
            image,
            range,
            (
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                vk::AccessFlags::TRANSFER_WRITE,
            ),
            (vk::ImageLayout::PRESENT_SRC_KHR, vk::AccessFlags::empty()),
        );
        let clear = vk::ClearColorValue {
            float32: color_at(self.started.elapsed().as_secs_f32()),
        };
        let begin_info = vk::CommandBufferBeginInfo::default()
            .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);

        // SAFETY: the command buffer belongs to this device and is not in use (the frame's
        // fence was waited on; beginning implicitly resets it because the pool allows resets),
        // and `image` was acquired from the swapchain.
        unsafe {
            device.begin_command_buffer(command_buffer, &begin_info)?;
            device.cmd_pipeline_barrier(
                command_buffer,
                vk::PipelineStageFlags::TRANSFER,
                vk::PipelineStageFlags::TRANSFER,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[to_clear],
            );
            device.cmd_clear_color_image(
                command_buffer,
                image,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                &clear,
                &[range],
            );
            device.cmd_pipeline_barrier(
                command_buffer,
                vk::PipelineStageFlags::TRANSFER,
                vk::PipelineStageFlags::BOTTOM_OF_PIPE,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[to_present],
            );
            device.end_command_buffer(command_buffer)?;
        }
        Ok(())
    }

    /// Submits the frame's recorded commands to the GPU.
    fn submit(&self, slot: FrameSlot, render_finished: vk::Semaphore) -> Result<(), RenderError> {
        let device = self.gpu.device();
        let wait_semaphores = [slot.image_available];
        let wait_stages = [vk::PipelineStageFlags::TRANSFER];
        let command_buffers = [slot.command_buffer];
        let signal_semaphores = [render_finished];
        let submit_info = vk::SubmitInfo::default()
            .wait_semaphores(&wait_semaphores)
            .wait_dst_stage_mask(&wait_stages)
            .command_buffers(&command_buffers)
            .signal_semaphores(&signal_semaphores);

        // SAFETY: the fence belongs to this device and its previous use finished (it was
        // waited on). It is reset only here, right before the submit that signals it again,
        // so a failure earlier in the frame can never leave it unsignaled.
        unsafe { device.reset_fences(&[slot.in_flight])? };
        // SAFETY: all handles belong to this device, the arrays outlive the call, and the
        // fence was just reset, so it is unsignaled.
        unsafe { device.queue_submit(self.gpu.queue(), &[submit_info], slot.in_flight)? };
        Ok(())
    }

    /// Queues the finished image for presentation.
    /// Returns `true` if the swapchain no longer fits the window and must be rebuilt.
    fn present(
        &self,
        swapchain: &Swapchain,
        render_finished: vk::Semaphore,
        image_index: u32,
    ) -> Result<bool, RenderError> {
        match swapchain.present(self.gpu.queue(), render_finished, image_index) {
            Ok(suboptimal) => Ok(suboptimal),
            Err(vk::Result::ERROR_OUT_OF_DATE_KHR) => Ok(true),
            Err(error) => Err(error.into()),
        }
    }
}

impl Renderer for VulkanRenderer {
    fn resize(&mut self, width: u32, height: u32) {
        if (width, height) != self.size {
            self.size = (width, height);
            self.needs_recreate = true;
        }
    }

    fn render_frame(&mut self) -> Result<(), RenderError> {
        // A minimized window has no area to draw into.
        if self.size.0 == 0 || self.size.1 == 0 {
            return Ok(());
        }
        if self.needs_recreate {
            if !self.recreate_swapchain()? {
                return Ok(());
            }
            self.needs_recreate = false;
        }
        let Some(swapchain) = self.swapchain.as_ref() else {
            return Ok(());
        };

        let slot = self.frames.current_slot();
        // SAFETY: the fence belongs to this device. Waiting guarantees the GPU is done
        // with this slot's command buffer and semaphores.
        unsafe {
            self.gpu
                .device()
                .wait_for_fences(&[slot.in_flight], true, u64::MAX)?;
        }

        // SAFETY: the slot's fence was just waited on, so the frame that last used
        // `image_available` has consumed its signal: the semaphore is unsignaled and idle.
        let acquired = unsafe { swapchain.acquire_next_image(slot.image_available) };
        let image_index = match acquired {
            Ok((index, _suboptimal)) => index,
            Err(vk::Result::ERROR_OUT_OF_DATE_KHR) => {
                // The window changed under us; rebuild and try again next frame.
                self.needs_recreate = true;
                return Ok(());
            }
            Err(error) => return Err(error.into()),
        };

        self.record_clear(slot.command_buffer, swapchain.image(image_index))?;
        let render_finished = self.frames.render_finished(image_index);
        self.submit(slot, render_finished)?;
        if self.present(swapchain, render_finished, image_index)? {
            self.needs_recreate = true;
        }

        self.frames.advance();
        Ok(())
    }

    fn device_name(&self) -> &str {
        self.gpu.name()
    }
}

impl Drop for VulkanRenderer {
    fn drop(&mut self) {
        // SAFETY: the device is alive. Waiting for idle means no GPU work still uses the
        // objects that the fields destroy next. A failure here cannot be handled in
        // `drop`, and the process is shutting down anyway.
        unsafe {
            let _ = self.gpu.device().device_wait_idle();
        }
    }
}

/// A color that drifts smoothly with time, so it is obvious the frames are live.
/// Every component stays within `0.0..=1.0`; alpha is always opaque.
fn color_at(seconds: f32) -> [f32; 4] {
    let t = seconds * 0.8;
    let wave = |phase: f32| 0.5 + 0.5 * (t + phase).sin();
    [wave(0.0), wave(2.1), wave(4.2), 1.0]
}

/// A barrier that moves a whole color image between layouts.
/// Each side is given as `(layout, access mask)`.
fn image_barrier(
    image: vk::Image,
    range: vk::ImageSubresourceRange,
    from: (vk::ImageLayout, vk::AccessFlags),
    to: (vk::ImageLayout, vk::AccessFlags),
) -> vk::ImageMemoryBarrier<'static> {
    vk::ImageMemoryBarrier::default()
        .old_layout(from.0)
        .src_access_mask(from.1)
        .new_layout(to.0)
        .dst_access_mask(to.1)
        .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .image(image)
        .subresource_range(range)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn color_stays_in_range_and_opaque() {
        for step in 0..1000_u16 {
            let color = color_at(f32::from(step) * 0.1);
            assert!(color[..3].iter().all(|c| (0.0..=1.0).contains(c)));
            assert_eq!(color[3], 1.0);
        }
    }

    #[test]
    fn color_changes_over_time() {
        assert_ne!(color_at(0.0), color_at(1.0));
    }
}
