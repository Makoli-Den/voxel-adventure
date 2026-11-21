//! The Vulkan renderer: owns every Vulkan object and runs the frame loop.

use std::any::Any;
use std::path::Path;
use std::sync::Arc;

use ash::vk;
use raw_window_handle::{HasDisplayHandle, HasWindowHandle};
use vox_world::ChunkMesh;

use super::context::{Context, Surface};
use super::depth;
use super::frames::{FrameSlot, Frames};
use super::gpu::Gpu;
use super::mesh::GpuMesh;
use super::pipeline::{CAMERA_PUSH_SIZE, Pipeline};
use super::render_pass::RenderPass;
use super::swapchain::{self, Swapchain};
use crate::{RenderError, Renderer};

/// The sky: everything the mesh does not cover is cleared to this color.
const SKY_COLOR: [f32; 4] = [0.53, 0.81, 0.92, 1.0];

/// Draws into a window with Vulkan.
///
/// For now a frame is one chunk mesh in front of a plain sky color.
pub struct VulkanRenderer {
    // Rust drops fields in declaration order, so each object comes before the
    // ones it depends on: buffers, framebuffers (in the swapchain) and the pipeline
    // before the render pass, everything before the device. Do not reorder.
    frames: Frames,
    /// The mesh drawn every frame; `None` while there is nothing to draw (no mesh yet,
    /// or an empty one).
    mesh: Option<GpuMesh>,
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
    /// The view-projection matrix as push-constant bytes, see [`camera_bytes`].
    camera: [u8; CAMERA_PUSH_SIZE as usize],
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
        let depth_format = depth::depth_format(&context, &gpu)?;
        let render_pass = RenderPass::new(gpu.device(), format, depth_format)?;
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
            mesh: None,
            needs_recreate: swapchain.is_none(),
            swapchain,
            pipeline,
            render_pass,
            gpu,
            surface,
            context,
            _window: window,
            size: (width, height),
            camera: camera_bytes(&IDENTITY),
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
    /// image `image_index` to the sky color and the depth buffer to the far plane, then
    /// the mesh is drawn. The render pass also moves the image to the present layout,
    /// so no barriers are needed.
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
        let clear_values = [
            vk::ClearValue {
                color: vk::ClearColorValue { float32: SKY_COLOR },
            },
            vk::ClearValue {
                depth_stencil: vk::ClearDepthStencilValue {
                    depth: 1.0,
                    stencil: 0,
                },
            },
        ];
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
        // which the pipeline was built for too. Viewport and scissor are set before the
        // draw. The mesh lives in `self` and is only replaced after `device_wait_idle`.
        unsafe {
            device.begin_command_buffer(command_buffer, &begin_info)?;
            device.cmd_begin_render_pass(command_buffer, &pass_info, vk::SubpassContents::INLINE);
            device.cmd_set_viewport(command_buffer, 0, &[full_viewport(extent)]);
            device.cmd_set_scissor(command_buffer, 0, &[area]);
            if let Some(mesh) = &self.mesh {
                self.record_mesh(command_buffer, mesh);
            }
            device.cmd_end_render_pass(command_buffer);
            device.end_command_buffer(command_buffer)?;
        }
        Ok(())
    }

    /// Records the draw of `mesh` with the current camera.
    ///
    /// # Safety
    /// `command_buffer` must be recording inside the render pass the pipeline was built
    /// for, with viewport and scissor set. `mesh` must stay alive until the GPU has
    /// finished the command buffer.
    // HOT PATH: runs every frame; only binds, one push and one indexed draw.
    unsafe fn record_mesh(&self, command_buffer: vk::CommandBuffer, mesh: &GpuMesh) {
        let device = self.gpu.device();
        // SAFETY: the caller guarantees the recording state and the mesh lifetime. The
        // pipeline, its layout and the buffers belong to this device, and the pushed
        // range (64 bytes at offset 0, vertex stage) matches the layout's range.
        unsafe {
            device.cmd_bind_pipeline(
                command_buffer,
                vk::PipelineBindPoint::GRAPHICS,
                self.pipeline.handle(),
            );
            device.cmd_push_constants(
                command_buffer,
                self.pipeline.layout(),
                vk::ShaderStageFlags::VERTEX,
                0,
                &self.camera,
            );
            device.cmd_bind_vertex_buffers(command_buffer, 0, &[mesh.vertex_buffer()], &[0]);
            device.cmd_bind_index_buffer(
                command_buffer,
                mesh.index_buffer(),
                0,
                vk::IndexType::UINT32,
            );
            device.cmd_draw_indexed(command_buffer, mesh.index_count(), 1, 0, 0, 0);
        }
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

    fn set_mesh(&mut self, mesh: &ChunkMesh) -> Result<(), RenderError> {
        // Upload first: if it fails, the old mesh stays and keeps being drawn.
        let new = GpuMesh::new(&self.gpu, mesh)?;
        // SAFETY: the device is alive. Waiting for idle means no frame in flight still
        // reads the old buffers, which are freed by the assignment below.
        unsafe { self.gpu.device().device_wait_idle()? };
        self.mesh = new;
        Ok(())
    }

    fn set_camera(&mut self, view_projection: [[f32; 4]; 4]) {
        self.camera = camera_bytes(&view_projection);
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

/// The camera before [`Renderer::set_camera`] is called.
const IDENTITY: [[f32; 4]; 4] = [
    [1.0, 0.0, 0.0, 0.0],
    [0.0, 1.0, 0.0, 0.0],
    [0.0, 0.0, 1.0, 0.0],
    [0.0, 0.0, 0.0, 1.0],
];

/// Packs a column-major matrix into the bytes of a GLSL `mat4` push constant:
/// columns one after another, each float little-endian like the GPU reads it.
fn camera_bytes(matrix: &[[f32; 4]; 4]) -> [u8; CAMERA_PUSH_SIZE as usize] {
    let mut bytes = [0; CAMERA_PUSH_SIZE as usize];
    let floats = matrix.iter().flatten();
    for (chunk, value) in bytes.chunks_exact_mut(4).zip(floats) {
        chunk.copy_from_slice(&value.to_le_bytes());
    }
    bytes
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
    fn camera_bytes_are_columns_in_order() {
        let mut matrix = IDENTITY;
        matrix[3][0] = 2.0; // translation x, first value of the last column
        let bytes = camera_bytes(&matrix);
        assert_eq!(bytes[..4], 1.0_f32.to_le_bytes());
        assert_eq!(bytes[48..52], 2.0_f32.to_le_bytes());
        assert_eq!(bytes[60..], 1.0_f32.to_le_bytes());
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
