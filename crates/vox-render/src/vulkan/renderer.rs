//! The Vulkan renderer: owns every Vulkan object and runs the frame loop.

use std::any::Any;
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use ash::vk;
use raw_window_handle::{HasDisplayHandle, HasWindowHandle};

use super::context::{Context, Surface};
use super::frames::{FrameSlot, Frames};
use super::gpu::Gpu;
use super::pipeline::Pipeline;
use super::render_pass::RenderPass;
use super::swapchain::{self, Swapchain};
use crate::{RenderError, Renderer};

/// Draws into a window with Vulkan.
///
/// For now a frame is a colored triangle on a slowly changing background color.
/// That is enough to prove the render pass, the pipeline and the shaders loaded
/// from files work.
pub struct VulkanRenderer {
    // Rust drops fields in declaration order, so each object comes before the
    // ones it depends on: framebuffers (in the swapchain) and the pipeline before
    // the render pass, everything before the device. Do not reorder.
    frames: Frames,
    /// `None` while the window has no area to draw into (minimized).
    swapchain: Option<Swapchain>,
    pipeline: Pipeline,
    render_pass: RenderPass,
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
    /// Creates the renderer for `window`, whose current size is `width` by `height` pixels,
    /// with the shaders from `shader_dir`.
    ///
    /// If a step fails, the locals created so far are dropped in reverse order of
    /// creation, which is the same safe order as the struct fields. Nothing has been
    /// submitted to the GPU yet, so they can be destroyed right away.
    pub fn new<W>(
        window: Arc<W>,
        width: u32,
        height: u32,
        shader_dir: &Path,
    ) -> Result<Self, RenderError>
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
        let format = swapchain::surface_format(&surface, &gpu)?;
        let render_pass = RenderPass::new(gpu.device(), format)?;
        let pipeline = Pipeline::new(gpu.device(), &render_pass, shader_dir)?;
        let swapchain = Swapchain::new(
            &context,
            &surface,
            &gpu,
            &render_pass,
            (width, height),
            None,
        )?;
        let mut frames = Frames::new(&gpu)?;
        if let Some(swapchain) = &swapchain {
            // SAFETY: nothing has been submitted yet, so the GPU is idle.
            unsafe { frames.recreate_render_finished(swapchain.image_count())? };
        }

        Ok(Self {
            frames,
            needs_recreate: swapchain.is_none(),
            swapchain,
            pipeline,
            render_pass,
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
            &self.render_pass,
            self.size,
            self.swapchain.as_ref(),
        )?
        else {
            return Ok(false);
        };
        let image_count = new.image_count();

        // SAFETY: the device is alive. Waiting for idle makes it safe to destroy the old
        // swapchain with its framebuffers and views, and the per-image semaphores below.
        unsafe { self.gpu.device().device_wait_idle()? };
        self.swapchain = Some(new); // the old swapchain is destroyed here
        // SAFETY: the GPU is idle, see above.
        unsafe { self.frames.recreate_render_finished(image_count)? };
        Ok(true)
    }

    /// Records one frame into `command_buffer`: the render pass clears the swapchain
    /// image `image_index` to the background color and the pipeline draws the triangle.
    /// The render pass also moves the image to the present layout, so no barriers are needed.
    // HOT PATH: runs every frame. Everything lives on the stack, nothing is allocated.
    fn record_frame(
        &self,
        command_buffer: vk::CommandBuffer,
        swapchain: &Swapchain,
        image_index: u32,
    ) -> Result<(), RenderError> {
        let device = self.gpu.device();
        let extent = swapchain.extent();
        let area = vk::Rect2D {
            offset: vk::Offset2D { x: 0, y: 0 },
            extent,
        };
        let clear_values = [vk::ClearValue {
            color: vk::ClearColorValue {
                float32: color_at(self.started.elapsed().as_secs_f32()),
            },
        }];
        let pass_info = vk::RenderPassBeginInfo::default()
            .render_pass(self.render_pass.handle())
            .framebuffer(swapchain.framebuffer(image_index))
            .render_area(area)
            .clear_values(&clear_values);
        let begin_info = vk::CommandBufferBeginInfo::default()
            .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);

        // SAFETY: the command buffer belongs to this device and is not in use (the frame's
        // fence was waited on; beginning implicitly resets it because the pool allows resets).
        // The framebuffer belongs to the acquired image and was built for this render pass,
        // which the pipeline was built for too. The pipeline's viewport and scissor are
        // dynamic and both are set before the draw.
        unsafe {
            device.begin_command_buffer(command_buffer, &begin_info)?;
            device.cmd_begin_render_pass(command_buffer, &pass_info, vk::SubpassContents::INLINE);
            device.cmd_bind_pipeline(
                command_buffer,
                vk::PipelineBindPoint::GRAPHICS,
                self.pipeline.handle(),
            );
            device.cmd_set_viewport(command_buffer, 0, &[full_viewport(extent)]);
            device.cmd_set_scissor(command_buffer, 0, &[area]);
            // Three vertices, one instance: the triangle from the vertex shader.
            device.cmd_draw(command_buffer, 3, 1, 0, 0);
            device.cmd_end_render_pass(command_buffer);
            device.end_command_buffer(command_buffer)?;
        }
        Ok(())
    }

    /// Submits the frame's recorded commands to the GPU.
    fn submit(&self, slot: FrameSlot, render_finished: vk::Semaphore) -> Result<(), RenderError> {
        let device = self.gpu.device();
        let wait_semaphores = [slot.image_available];
        // Only the color output must wait for the image; the vertex work can start earlier.
        // The render pass's external dependency orders its layout change after this wait.
        let wait_stages = [vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT];
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

        self.record_frame(slot.command_buffer, swapchain, image_index)?;
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

/// A viewport covering the whole image, with the standard `0..1` depth range.
fn full_viewport(extent: vk::Extent2D) -> vk::Viewport {
    vk::Viewport {
        x: 0.0,
        y: 0.0,
        // Exact: image sides are far below 2^24, where f32 starts to round integers.
        width: extent.width as f32,
        height: extent.height as f32,
        min_depth: 0.0,
        max_depth: 1.0,
    }
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

    #[test]
    fn viewport_covers_the_whole_image() {
        let viewport = full_viewport(vk::Extent2D {
            width: 1280,
            height: 720,
        });
        assert_eq!((viewport.x, viewport.y), (0.0, 0.0));
        assert_eq!((viewport.width, viewport.height), (1280.0, 720.0));
        assert_eq!((viewport.min_depth, viewport.max_depth), (0.0, 1.0));
    }
}
