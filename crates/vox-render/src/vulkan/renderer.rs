//! The Vulkan renderer: owns every Vulkan object and runs the frame loop.

use std::any::Any;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use ash::vk;
use raw_window_handle::{HasDisplayHandle, HasWindowHandle};
use vox_world::{ChunkMesh, ChunkPos};

use super::NO_TIMEOUT;
use super::buffer::Buffer;
use super::context::{Context, Surface};
use super::depth;
use super::frames::{FRAMES_IN_FLIGHT, FrameSlot, Frames};
use super::gpu::Gpu;
use super::mesh::GpuMesh;
use super::overlay::GpuOverlay;
use super::pipeline::{self, Pipeline};
use super::render_pass::RenderPass;
use super::retired::{self, Retired};
use super::swapchain::{self, Swapchain};
use super::texture::{BlockTextureSet, TextureArray};
use super::timestamps;
use crate::chunk_push::{frame_bytes, origin_bytes};
use crate::frustum::Frustum;
use crate::interface::RESOURCE_SET;
use crate::interface::chunk::{self, CHUNK_ORIGIN_OFFSET};
use crate::overlay::window_size_bytes;
use crate::texture::{missing_texture, missing_texture_pixels};
use crate::{
    CameraView, Fog, OverlayRect, RenderError, RenderStats, Renderer, ShaderSources, TextureLayers,
};

/// The sky: everything the meshes do not cover is cleared to this color, and it is the fog
/// color too, so a fully fogged face looks exactly like the sky behind it.
const SKY_COLOR: [f32; 4] = [0.53, 0.81, 0.92, 1.0];

/// The farthest depth: the depth buffer is cleared to it and the viewport's depth range
/// ends at it, so the chunk pipeline's LESS test passes for anything drawn.
const FAR_DEPTH: f32 = 1.0;

/// Draws into a window with Vulkan.
///
/// A frame is the chunk meshes in front of a plain sky color, then the overlay on top.
pub struct VulkanRenderer {
    // Rust drops fields in declaration order, so each object comes before the
    // ones it depends on: buffers, framebuffers (in the swapchain) and the pipelines
    // before the render pass, the pipelines before the texture set (their layout names
    // its set layout), the set before the texture array it points at, everything before
    // the device. Do not reorder.
    frames: Frames,
    /// The meshes that are candidates for drawing every frame (those outside the view are
    /// skipped), one per chunk that has something to draw.
    meshes: HashMap<ChunkPos, GpuMesh>,
    /// The overlay drawn over every frame, `None` while there is none.
    overlay: Option<GpuOverlay>,
    /// Buffers of replaced and removed meshes and overlays that frames in flight may
    /// still read.
    retired: Retired<Buffer>,
    /// `None` while the window has no area to draw into (minimized).
    swapchain: Option<Swapchain>,
    chunk_pipeline: Pipeline,
    overlay_pipeline: Pipeline,
    /// The descriptor set chunk draws sample the block textures through; always points
    /// at `block_textures`.
    block_texture_set: BlockTextureSet,
    /// The block textures: the missing picture until `set_block_textures` is called.
    block_textures: TextureArray,
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
    /// The camera, as given to [`Renderer::set_camera`].
    camera: CameraView,
    /// The fog, as given to [`Renderer::set_fog`].
    fog: Fog,
    /// Draw counts of the most recently recorded frame; its `gpu_time` stays `None`.
    drawn: RenderStats,
    /// GPU time of the most recent frame whose timestamps were read.
    last_gpu_time: Option<Duration>,
}

impl VulkanRenderer {
    /// Creates the renderer for `window`, whose current size is `width` by `height` pixels,
    /// with the GLSL text in `shaders`.
    ///
    /// If a step fails, the locals created so far are dropped in reverse order of
    /// creation, which is the same safe order as the struct fields. Nothing has been
    /// submitted to the GPU yet, so they can be destroyed right away.
    pub fn new<W>(
        window: Arc<W>,
        width: u32,
        height: u32,
        shaders: &ShaderSources,
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
        let block_texture_set = BlockTextureSet::new(gpu.device(), &chunk::INTERFACE)?;
        let chunk_pipeline = Pipeline::chunk(
            gpu.device(),
            &render_pass,
            shaders,
            block_texture_set.layout(),
        )?;
        let blending_supported = pipeline::supports_blending(&context, &gpu, format.format);
        let overlay_pipeline =
            Pipeline::overlay(gpu.device(), &render_pass, shaders, blending_supported)?;
        let swapchain = Swapchain::new(
            &context,
            &surface,
            &gpu,
            &render_pass,
            (width, height),
            None,
        )?;
        let missing_pixels = missing_texture_pixels();
        let block_textures = TextureArray::upload(&gpu, &missing_texture(&missing_pixels))?;
        // SAFETY: the set was never bound. The upload finished (fence) and left the view in
        // the shader-read layout; the array is stored next to the set, which the field
        // order drops first.
        unsafe { block_texture_set.point_at(block_textures.view()) };
        let mut frames = Frames::new(&gpu)?;
        if let Some(swapchain) = &swapchain {
            // SAFETY: nothing has been submitted yet, so the GPU is idle.
            unsafe { frames.recreate_render_finished(swapchain.image_count())? };
        }

        Ok(Self {
            frames,
            meshes: HashMap::new(),
            overlay: None,
            retired: Retired::new(),
            needs_recreate: swapchain.is_none(),
            swapchain,
            chunk_pipeline,
            overlay_pipeline,
            block_texture_set,
            block_textures,
            render_pass,
            gpu,
            surface,
            context,
            _window: window,
            size: (width, height),
            camera: CameraView::IDENTITY,
            fog: Fog::OFF,
            drawn: RenderStats::default(),
            last_gpu_time: None,
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
        // Idle means every submitted frame finished, so no retired buffer is in use.
        self.retired.release(self.frames.submitted());
        self.swapchain = Some(new); // the old swapchain is destroyed here
        // SAFETY: the GPU is idle, see above.
        unsafe { self.frames.recreate_render_finished(image_count)? };
        Ok(true)
    }

    /// Records one frame into the command buffer of `slot`: the render pass clears the
    /// swapchain image `image_index` to the sky color and the depth buffer to the far
    /// plane, then the chunk meshes are drawn, then the overlay over them. The render pass
    /// also moves the image to the present layout, so no barriers are needed. If the slot
    /// has a query pool, a timestamp is written before the first and after the last command.
    ///
    /// Returns the draw counts of the recorded frame (chunk meshes only).
    // HOT PATH: runs every frame. Everything lives on the stack, nothing is allocated; the
    // texture set is bound and the matrix and fog pushed once per frame, not per chunk; per
    // chunk only its offset from the camera is computed (three subtractions).
    fn record_frame(
        &self,
        slot: FrameSlot,
        swapchain: &Swapchain,
        image_index: u32,
    ) -> Result<RenderStats, RenderError> {
        let device = self.gpu.device();
        let command_buffer = slot.command_buffer;
        let mut drawn = RenderStats::default();
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
                    depth: FAR_DEPTH,
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
        let view_projection = self.camera.view_projection;
        let frame_push = frame_bytes(&view_projection, self.fog.push_values(), SKY_COLOR);

        // SAFETY: the command buffer belongs to this device and is not in use (the frame's
        // fence was waited on; beginning implicitly resets it because the pool allows resets).
        // The framebuffer belongs to the acquired image and was built for this render pass,
        // which both pipelines were built for too. Viewport and scissor are set before the
        // draws; both pipelines declare them dynamic, so binding the second keeps them. The
        // texture set was allocated with the set layout the chunk layout has at
        // `RESOURCE_SET`, points at a live array in the shader-read layout, and is only
        // updated while the device is idle (`set_block_textures`). The
        // frame push (bytes 0..112, the stages of the range) is exactly the chunk layout's
        // only range, and it stays valid for every mesh draw because they all use this
        // pipeline's layout. The meshes live in `self`; a replaced mesh moves to `retired`
        // and is freed only after the frames that may use it have finished. The overlay
        // draw follows all mesh draws, see `record_overlay`. The query pool
        // is a timestamp pool with the frame's queries; it exists only if the queue writes
        // timestamps, and the fence wait proved the previous frame in this slot, its last
        // user, finished. Its reset and both writes lie outside the render pass.
        unsafe {
            device.begin_command_buffer(command_buffer, &begin_info)?;
            if let Some(pool) = slot.timestamps {
                timestamps::record_frame_start(device, command_buffer, pool);
            }
            device.cmd_begin_render_pass(command_buffer, &pass_info, vk::SubpassContents::INLINE);
            device.cmd_set_viewport(command_buffer, 0, &[full_viewport(extent)]);
            device.cmd_set_scissor(command_buffer, 0, &[area]);
            if !self.meshes.is_empty() {
                device.cmd_bind_pipeline(
                    command_buffer,
                    vk::PipelineBindPoint::GRAPHICS,
                    self.chunk_pipeline.handle(),
                );
                device.cmd_bind_descriptor_sets(
                    command_buffer,
                    vk::PipelineBindPoint::GRAPHICS,
                    self.chunk_pipeline.layout(),
                    RESOURCE_SET,
                    &[self.block_texture_set.set()],
                    &[],
                );
                device.cmd_push_constants(
                    command_buffer,
                    self.chunk_pipeline.layout(),
                    self.chunk_pipeline.push_constant_stages(),
                    0,
                    &frame_push,
                );
                // Planes and boxes are both camera-relative, so they agree.
                let frustum = Frustum::from_view_projection(&view_projection);
                for (&pos, mesh) in &self.meshes {
                    let origin = self.camera.relative(pos.origin());
                    if !frustum.may_see_chunk(origin) {
                        continue;
                    }
                    self.record_mesh(command_buffer, origin, mesh);
                    drawn.count_chunk_draw(mesh.index_count());
                }
            }
            if let Some(overlay) = &self.overlay {
                self.record_overlay(command_buffer, overlay, extent);
            }
            device.cmd_end_render_pass(command_buffer);
            if let Some(pool) = slot.timestamps {
                timestamps::record_frame_end(device, command_buffer, pool);
            }
            device.end_command_buffer(command_buffer)?;
        }
        Ok(drawn)
    }

    /// Records the draw of a chunk whose origin lies at `relative_origin` from the camera.
    ///
    /// # Safety
    /// `command_buffer` must be recording inside the render pass the pipeline was built
    /// for, with the pipeline bound, the frame's push constants pushed, and viewport and
    /// scissor set. `mesh` must stay alive until the GPU has finished the command buffer.
    // HOT PATH: runs for every chunk every frame; only two binds, one 16-byte push and
    // one indexed draw. The matrix and the fog are pushed once per frame, not per chunk.
    unsafe fn record_mesh(
        &self,
        command_buffer: vk::CommandBuffer,
        relative_origin: [f32; 3],
        mesh: &GpuMesh,
    ) {
        let device = self.gpu.device();
        // SAFETY: the caller guarantees the recording state and the mesh lifetime. The
        // layout and the buffer belong to this device, and the pushed bytes (64..80, the
        // stages of the range, offset and size multiples of 4) lie inside the layout's only
        // range. The buffer was created with VERTEX and INDEX usage; the index offset is
        // a multiple of 4 as UINT32 indices require, and every index was checked against
        // the vertex count.
        unsafe {
            device.cmd_push_constants(
                command_buffer,
                self.chunk_pipeline.layout(),
                self.chunk_pipeline.push_constant_stages(),
                CHUNK_ORIGIN_OFFSET,
                &origin_bytes(relative_origin),
            );
            device.cmd_bind_vertex_buffers(command_buffer, 0, &[mesh.buffer()], &[0]);
            device.cmd_bind_index_buffer(
                command_buffer,
                mesh.buffer(),
                mesh.index_offset(),
                vk::IndexType::UINT32,
            );
            device.cmd_draw_indexed(command_buffer, mesh.index_count(), 1, 0, 0, 0);
        }
    }

    /// Records the overlay: binds its pipeline, pushes the image size and draws every
    /// rectangle in one call. Not counted in the frame's [`RenderStats`].
    ///
    /// # Safety
    /// `command_buffer` must be recording inside the render pass both pipelines were
    /// built for, after every mesh draw, with viewport and scissor set. `extent` must be
    /// the size of the framebuffer being drawn. `overlay` must stay alive until the GPU
    /// has finished the command buffer.
    // HOT PATH: runs every frame while an overlay is shown; one bind of each kind, one
    // 8-byte push and one draw, whatever the number of rectangles.
    unsafe fn record_overlay(
        &self,
        command_buffer: vk::CommandBuffer,
        overlay: &GpuOverlay,
        extent: vk::Extent2D,
    ) {
        let device = self.gpu.device();
        let pipeline = &self.overlay_pipeline;
        // SAFETY: the caller guarantees the recording state, the extent and the overlay
        // lifetime. The pipeline, its layout and the buffer belong to this device. The
        // push (bytes 0..8, the stages of the layout's only range) is made after binding
        // this pipeline, so it does not depend on what the chunk layout left behind. The
        // buffer was created with VERTEX usage and holds exactly `vertex_count` vertices
        // of the stride the pipeline's binding declares, so the draw reads only inside it.
        unsafe {
            device.cmd_bind_pipeline(
                command_buffer,
                vk::PipelineBindPoint::GRAPHICS,
                pipeline.handle(),
            );
            device.cmd_push_constants(
                command_buffer,
                pipeline.layout(),
                pipeline.push_constant_stages(),
                0,
                &window_size_bytes(extent.width, extent.height),
            );
            device.cmd_bind_vertex_buffers(command_buffer, 0, &[overlay.buffer()], &[0]);
            device.cmd_draw(command_buffer, overlay.vertex_count(), 1, 0, 0);
        }
    }

    /// Parks the buffer of a replaced or removed mesh or overlay until no frame in flight
    /// can read it.
    fn retire(&mut self, buffer: Buffer) {
        self.retired.retire(buffer, self.frames.submitted());
        self.release_if_gpu_idle();
    }

    /// Frees every retired buffer if a poll of the fences, without waiting, shows that all
    /// submitted frames finished. Otherwise leaves them for a later frame or poll.
    ///
    /// The frame loop frees retired buffers after its fence wait, but while no frames are
    /// drawn (the game does not even call `render_frame` while the window is minimized)
    /// that never happens. Polling here, where buffers are retired, keeps the list
    /// bounded by what changed since the GPU last went idle.
    // HOT PATH: runs for every mesh or overlay change; two non-blocking fence queries.
    fn release_if_gpu_idle(&mut self) {
        // An error (a lost device) proves nothing; `render_frame` reports it next time.
        let all_signaled = self.frames.all_fences_signaled().unwrap_or(false);
        if let Some(completed) =
            retired::completed_when_polled(self.frames.submitted(), all_signaled)
        {
            self.retired.release(completed);
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
                .wait_for_fences(&[slot.in_flight], true, NO_TIMEOUT)?;
        }
        // The wait proved the slot's previous frame finished, so its timing can be read.
        if let Some(gpu_time) = self.frames.take_gpu_time()? {
            self.last_gpu_time = Some(gpu_time);
        }
        // The wait proved which frames are finished; meshes only they used can go now.
        self.retired.release(retired::completed_after_wait(
            self.frames.submitted(),
            FRAMES_IN_FLIGHT,
        ));
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

        self.drawn = self.record_frame(slot, swapchain, image_index)?;
        let render_finished = self.frames.render_finished(image_index);
        self.submit(slot, render_finished)?;
        // Counted right after the submit: from here on this frame may read the meshes, and
        // its timestamps (written whenever the slot has a pool) wait to be read.
        self.frames.advance();
        if self.present(swapchain, render_finished, image_index)? {
            self.needs_recreate = true;
        }
        Ok(())
    }

    fn device_name(&self) -> &str {
        self.gpu.name()
    }

    fn set_chunk_mesh(&mut self, pos: ChunkPos, mesh: &ChunkMesh) -> Result<(), RenderError> {
        // Upload first: if it fails, the old mesh stays and keeps being drawn.
        let old = match GpuMesh::new(&self.gpu, mesh)? {
            Some(new) => self.meshes.insert(pos, new),
            None => self.meshes.remove(&pos),
        };
        if let Some(old) = old {
            self.retire(old.into_buffer());
        }
        Ok(())
    }

    fn remove_chunk_mesh(&mut self, pos: ChunkPos) {
        if let Some(old) = self.meshes.remove(&pos) {
            self.retire(old.into_buffer());
        }
    }

    fn set_overlay(&mut self, rects: &[OverlayRect]) -> Result<(), RenderError> {
        // Upload first: if it fails, the old overlay stays and keeps being drawn.
        let new = GpuOverlay::new(&self.gpu, rects)?;
        if let Some(old) = std::mem::replace(&mut self.overlay, new) {
            self.retire(old.into_buffer());
        }
        Ok(())
    }

    fn set_block_textures(&mut self, textures: &TextureLayers<'_>) -> Result<(), RenderError> {
        // Validate and upload first: if either fails, the old textures stay bound and keep
        // being drawn.
        let new = TextureArray::upload(&self.gpu, textures)?;
        // SAFETY: the device is alive. Frames in flight may still sample the old array
        // through the set, which may not be updated while in use; idle ends that. If the
        // wait fails, the set is left alone and `new` is dropped: its upload was proven
        // finished (fence) and nothing else refers to it.
        unsafe { self.gpu.device().device_wait_idle()? };
        // Idle means every submitted frame finished, so no retired buffer is in use.
        self.retired.release(self.frames.submitted());
        // SAFETY: the device is idle and every command buffer is recorded and submitted in
        // the same `render_frame` call, so no command buffer uses the set. The new array
        // finished its upload in the shader-read layout and is stored right below.
        unsafe { self.block_texture_set.point_at(new.view()) };
        // The old array is dropped here; the set no longer points at it and the GPU is idle.
        self.block_textures = new;
        Ok(())
    }

    fn set_camera(&mut self, camera: &CameraView) {
        self.camera = *camera;
    }

    fn set_fog(&mut self, fog: Fog) {
        self.fog = fog;
    }

    fn stats(&self) -> RenderStats {
        RenderStats {
            gpu_time: self.last_gpu_time,
            ..self.drawn
        }
    }
}

impl Drop for VulkanRenderer {
    fn drop(&mut self) {
        // SAFETY: the device is alive. Waiting for idle means no GPU work still uses the
        // objects that the fields destroy next, retired buffers included. A failure here
        // cannot be handled in `drop`, and the process is shutting down anyway.
        unsafe {
            let _ = self.gpu.device().device_wait_idle();
        }
    }
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
        max_depth: FAR_DEPTH,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
