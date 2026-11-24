//! Block textures on the GPU: the 2D array image and the descriptor set the chunk pipeline
//! samples it through (decision 0013, sections "Textures" and "Descriptors").
//!
//! [`TextureArray`] is one device-local `R8G8B8A8_UNORM` image with optimal tiling, filled
//! once through a host-visible staging buffer, every mip level at once (the chain is built
//! on the CPU by [`MipChain`]). [`BlockTextureSet`] owns the descriptor set layout, pool, set
//! and sampler; the set is pointed at a new array when the textures are replaced.

use ash::vk;

use super::buffer::Buffer;
use super::gpu::Gpu;
use super::interface;
use super::memory;
use super::one_time::OneTimeCommands;
use crate::RenderError;
use crate::TextureLayers;
use crate::interface::PipelineInterface;
use crate::interface::chunk::{BLOCK_SAMPLER_BINDING, BLOCK_TEXTURES_BINDING};
use crate::mipmap::{MipChain, MipLevel, mip_level_count};

/// The image format: 8-bit RGBA used as stored. Vulkan 1.0 requires every device to support
/// it for sampling with optimal tiling and linear filtering; the sampler's `LINEAR` mip mode
/// needs the latter (`SAMPLED_IMAGE_FILTER_LINEAR`), so no capability check is required.
const FORMAT: vk::Format = vk::Format::R8G8B8A8_UNORM;

/// The only descriptor set the pool hands out: the block textures'.
const MAX_SETS: u32 = 1;

/// The whole color image: every layer of every mip level.
fn color_range(layer_count: u32, mip_levels: u32) -> vk::ImageSubresourceRange {
    vk::ImageSubresourceRange {
        aspect_mask: vk::ImageAspectFlags::COLOR,
        base_mip_level: 0,
        level_count: mip_levels,
        base_array_layer: 0,
        layer_count,
    }
}

/// A 2D array image of block textures in device-local memory, with its view, ready to sample.
pub struct TextureArray {
    device: ash::Device,
    image: vk::Image,
    memory: vk::DeviceMemory,
    view: vk::ImageView,
}

impl TextureArray {
    /// Validates `textures` against this GPU's limits, builds the mip chain, creates the
    /// image and uploads every level. Returns once the GPU has finished the copy (it waits
    /// on a fence) and the image is in the shader-read layout.
    ///
    /// # Errors
    /// [`RenderError::InvalidTextures`] before anything is allocated on the GPU if the data
    /// does not pass [`TextureLayers::validate`] or the chain does not fit into memory;
    /// another error if the GPU cannot allocate or copy.
    pub fn upload(gpu: &Gpu, textures: &TextureLayers<'_>) -> Result<Self, RenderError> {
        textures.validate(gpu.texture_limits())?;
        let chain = MipChain::build(textures)?;
        let staging = Buffer::with_data(gpu, vk::BufferUsageFlags::TRANSFER_SRC, chain.pixels())?;
        let array = Self::new(gpu, textures.size, textures.layer_count)?;
        let commands = OneTimeCommands::begin(gpu)?;
        // SAFETY: the command buffer is recording. The staging buffer holds exactly the
        // chain's pixels, built for this size and layer count, and the image was just
        // created with that size, layer count and `mip_level_count(size)` levels, the
        // chain's level count. Both are handed to `submit_and_wait`, which gives them back
        // only once the GPU finished and leaks them if nothing proves that.
        unsafe {
            array.record_upload(
                commands.command_buffer(),
                &staging,
                chain.levels(),
                textures.layer_count,
            );
        }
        let (array, staging) = commands.submit_and_wait((array, staging))?;
        // The fence wait proved the copy finished, so the staging buffer can go.
        drop(staging);
        Ok(array)
    }

    /// Creates an empty image of `layer_count` layers of `size` pixels with a full mip
    /// chain, its memory and view.
    fn new(gpu: &Gpu, size: u32, layer_count: u32) -> Result<Self, RenderError> {
        let device = gpu.device();
        // Start with null handles: if a later call fails, `Drop` destroys whatever
        // was created so far (destroying or freeing a null handle is a no-op).
        let mut array = Self {
            device: device.clone(),
            image: vk::Image::null(),
            memory: vk::DeviceMemory::null(),
            view: vk::ImageView::null(),
        };
        let info = image_info(size, layer_count);
        // SAFETY: the device is alive and `info` outlives the call. The format supports
        // sampling with optimal tiling on every device; size and layer count were validated
        // against the device's `maxImageDimension2D` and `maxImageArrayLayers`, and
        // `log2(size) + 1` levels is the full chain every image of that size may have.
        array.image = unsafe { device.create_image(&info, None)? };
        // SAFETY: the image was just created from this device.
        let requirements = unsafe { device.get_image_memory_requirements(array.image) };
        array.memory = memory::allocate(
            device,
            gpu.memory_properties(),
            requirements,
            vk::MemoryPropertyFlags::DEVICE_LOCAL,
        )?;
        // SAFETY: the memory was allocated for this image's requirements and is unbound.
        unsafe { device.bind_image_memory(array.image, array.memory, 0)? };
        let view_info = vk::ImageViewCreateInfo::default()
            .image(array.image)
            .view_type(vk::ImageViewType::TYPE_2D_ARRAY)
            .format(FORMAT)
            .subresource_range(color_range(layer_count, mip_level_count(size)));
        // SAFETY: the image is alive, bound to memory, created with `FORMAT`, `layer_count`
        // layers and `mip_level_count(size)` levels, which the view covers exactly.
        array.view = unsafe { device.create_image_view(&view_info, None)? };
        Ok(array)
    }

    /// The view the descriptor set points at.
    pub fn view(&self) -> vk::ImageView {
        self.view
    }

    /// Records the move to the transfer layout, the copy of every level and layer and the
    /// move to the shader-read layout.
    ///
    /// # Safety
    /// `command_buffer` must be recording, outside a render pass. `staging` must hold a mip
    /// chain laid out as `levels` describe (see [`MipChain`]) with `layer_count` layers;
    /// levels and layer count must be those this image was created with, and buffer and
    /// image must stay alive until the GPU finished the commands.
    unsafe fn record_upload(
        &self,
        command_buffer: vk::CommandBuffer,
        staging: &Buffer,
        levels: &[MipLevel],
        layer_count: u32,
    ) {
        // `levels` has at most `mip_level_count(MAX_SIZE)` entries, far below `u32::MAX`.
        let range = color_range(layer_count, levels.len() as u32);
        // The previous contents do not matter (UNDEFINED): the copy overwrites everything.
        let to_transfer = vk::ImageMemoryBarrier::default()
            .src_access_mask(vk::AccessFlags::empty())
            .dst_access_mask(vk::AccessFlags::TRANSFER_WRITE)
            .old_layout(vk::ImageLayout::UNDEFINED)
            .new_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
            .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .image(self.image)
            .subresource_range(range);
        // The copy must be visible to every later fragment shader read, also in the frames
        // submitted after this upload: a barrier covers all later commands on the queue.
        let to_shader = vk::ImageMemoryBarrier::default()
            .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
            .dst_access_mask(vk::AccessFlags::SHADER_READ)
            .old_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
            .new_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
            .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .image(self.image)
            .subresource_range(range);
        let copies = copy_regions(levels, layer_count);
        // SAFETY: the caller guarantees the recording state, the sizes and the lifetimes.
        // The image was created with TRANSFER_DST and SAMPLED usage, the buffer with
        // TRANSFER_SRC. Each region reads one level's `layer_count * side * side * 4` bytes
        // from that level's offset (tightly packed, layer after layer), so the regions
        // cover the buffer exactly and write disjoint subresources. Every offset is a
        // multiple of 4, the texel size, as the format requires. Both barriers cover every
        // level and layer. Host writes to the staging memory are made visible by the
        // submission itself.
        unsafe {
            self.device.cmd_pipeline_barrier(
                command_buffer,
                vk::PipelineStageFlags::TOP_OF_PIPE,
                vk::PipelineStageFlags::TRANSFER,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[to_transfer],
            );
            self.device.cmd_copy_buffer_to_image(
                command_buffer,
                staging.handle(),
                self.image,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                &copies,
            );
            self.device.cmd_pipeline_barrier(
                command_buffer,
                vk::PipelineStageFlags::TRANSFER,
                vk::PipelineStageFlags::FRAGMENT_SHADER,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[to_shader],
            );
        }
    }
}

impl Drop for TextureArray {
    fn drop(&mut self) {
        // SAFETY: the renderer drops an array only when no GPU work can use it: after the
        // device went idle (on replacement and when the renderer is dropped), or when its
        // upload failed before the submit or with the work proven finished (otherwise
        // `OneTimeCommands::submit_and_wait` leaks it). The view goes before the image, the
        // image before its memory. Null handles are ignored.
        unsafe {
            self.device.destroy_image_view(self.view, None);
            self.device.destroy_image(self.image, None);
            self.device.free_memory(self.memory, None);
        }
    }
}

/// A single-sample 2D array image with a full mip chain to copy into and sample from.
fn image_info(size: u32, layer_count: u32) -> vk::ImageCreateInfo<'static> {
    vk::ImageCreateInfo::default()
        .image_type(vk::ImageType::TYPE_2D)
        .format(FORMAT)
        .extent(vk::Extent3D {
            width: size,
            height: size,
            depth: 1,
        })
        .mip_levels(mip_level_count(size))
        .array_layers(layer_count)
        .samples(vk::SampleCountFlags::TYPE_1)
        .tiling(vk::ImageTiling::OPTIMAL)
        .usage(vk::ImageUsageFlags::TRANSFER_DST | vk::ImageUsageFlags::SAMPLED)
        .sharing_mode(vk::SharingMode::EXCLUSIVE)
        .initial_layout(vk::ImageLayout::UNDEFINED)
}

/// One region per mip level, each copying every layer of that level from the tightly packed
/// chain: a zero row length and image height mean "as wide and high as the level", and the
/// layers follow each other. A region spans all layers, so no level needs one per layer.
fn copy_regions(levels: &[MipLevel], layer_count: u32) -> Vec<vk::BufferImageCopy> {
    (0..)
        .zip(levels)
        .map(|(mip_level, level)| vk::BufferImageCopy {
            // Lossless: `usize` fits the 64-bit `DeviceSize`.
            buffer_offset: level.offset as vk::DeviceSize,
            buffer_row_length: 0,
            buffer_image_height: 0,
            image_subresource: vk::ImageSubresourceLayers {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                mip_level,
                base_array_layer: 0,
                layer_count,
            },
            image_offset: vk::Offset3D { x: 0, y: 0, z: 0 },
            image_extent: vk::Extent3D {
                width: level.size,
                height: level.size,
                depth: 1,
            },
        })
        .collect()
}

/// The descriptor set the chunk pipeline reads its block textures through, with its layout,
/// its pool and the sampler bound in it.
pub struct BlockTextureSet {
    device: ash::Device,
    layout: vk::DescriptorSetLayout,
    pool: vk::DescriptorPool,
    set: vk::DescriptorSet,
    sampler: vk::Sampler,
}

impl BlockTextureSet {
    /// Creates the layout from `interface` (the chunk pipeline's), one set and the sampler,
    /// and writes the sampler into the set. The image binding stays empty until
    /// [`Self::point_at`]; the set must not be bound before that.
    pub fn new(device: &ash::Device, interface: &PipelineInterface) -> Result<Self, RenderError> {
        // Null handles until created: `Drop` destroys whatever exists if a step fails.
        let mut set = Self {
            device: device.clone(),
            layout: vk::DescriptorSetLayout::null(),
            pool: vk::DescriptorPool::null(),
            set: vk::DescriptorSet::null(),
            sampler: vk::Sampler::null(),
        };
        let bindings = interface::descriptor_set_bindings(interface);
        let layout_info = vk::DescriptorSetLayoutCreateInfo::default().bindings(&bindings);
        // SAFETY: the device is alive and the create info with its array outlives the call.
        // The bindings are distinct (pinned by a test of the interface).
        set.layout = unsafe { device.create_descriptor_set_layout(&layout_info, None)? };
        // One descriptor of each binding's type: exactly what the one set needs.
        let pool_sizes: Vec<vk::DescriptorPoolSize> = bindings
            .iter()
            .map(|binding| vk::DescriptorPoolSize {
                ty: binding.descriptor_type,
                descriptor_count: binding.descriptor_count,
            })
            .collect();
        let pool_info = vk::DescriptorPoolCreateInfo::default()
            .max_sets(MAX_SETS)
            .pool_sizes(&pool_sizes);
        // SAFETY: the device is alive and the create info with its array outlives the call.
        set.pool = unsafe { device.create_descriptor_pool(&pool_info, None)? };
        let layouts = [set.layout];
        let allocate_info = vk::DescriptorSetAllocateInfo::default()
            .descriptor_pool(set.pool)
            .set_layouts(&layouts);
        // SAFETY: pool and layout belong to this device; the pool has room for this set.
        let sets = unsafe { device.allocate_descriptor_sets(&allocate_info)? };
        set.set = sets.first().copied().ok_or(RenderError::Unsupported(
            "the driver returned no descriptor set",
        ))?;
        // SAFETY: the device is alive and the create info outlives the call. No optional
        // feature is used (no anisotropy, no comparison).
        set.sampler = unsafe { device.create_sampler(&sampler_info(), None)? };
        set.write_sampler();
        Ok(set)
    }

    /// The layout of the set, for the chunk pipeline's layout.
    pub fn layout(&self) -> vk::DescriptorSetLayout {
        self.layout
    }

    /// The set to bind for chunk draws.
    pub fn set(&self) -> vk::DescriptorSet {
        self.set
    }

    /// Points the image binding at `view`.
    ///
    /// # Safety
    /// No pending or recorded-but-unsubmitted command buffer may use the set (the device is
    /// idle, or the set was never bound). `view` must be a 2D array view in the
    /// shader-read-only layout and stay alive for as long as the set is bound in any
    /// submitted frame.
    pub unsafe fn point_at(&self, view: vk::ImageView) {
        let image_info = [vk::DescriptorImageInfo {
            sampler: vk::Sampler::null(),
            image_view: view,
            image_layout: vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
        }];
        let write = vk::WriteDescriptorSet::default()
            .dst_set(self.set)
            .dst_binding(BLOCK_TEXTURES_BINDING)
            .descriptor_type(vk::DescriptorType::SAMPLED_IMAGE)
            .image_info(&image_info);
        // SAFETY: the caller guarantees the set is unused and the view valid. The binding
        // was declared SAMPLED_IMAGE by the layout this set was allocated with.
        unsafe { self.device.update_descriptor_sets(&[write], &[]) };
    }

    /// Writes the sampler into its binding. Done once: the sampler never changes.
    fn write_sampler(&self) {
        let sampler_info = [vk::DescriptorImageInfo {
            sampler: self.sampler,
            image_view: vk::ImageView::null(),
            image_layout: vk::ImageLayout::UNDEFINED,
        }];
        let write = vk::WriteDescriptorSet::default()
            .dst_set(self.set)
            .dst_binding(BLOCK_SAMPLER_BINDING)
            .descriptor_type(vk::DescriptorType::SAMPLER)
            .image_info(&sampler_info);
        // SAFETY: the set was just allocated and never bound; the sampler is alive and the
        // binding was declared SAMPLER by the set's layout.
        unsafe { self.device.update_descriptor_sets(&[write], &[]) };
    }
}

impl Drop for BlockTextureSet {
    fn drop(&mut self) {
        // SAFETY: the renderer drops this after the device went idle and after the
        // pipelines whose layout names the set layout. Destroying the pool frees the set.
        // Null handles are ignored.
        unsafe {
            self.device.destroy_sampler(self.sampler, None);
            self.device.destroy_descriptor_pool(self.pool, None);
            self.device.destroy_descriptor_set_layout(self.layout, None);
        }
    }
}

/// Pixel-art sampling with mipmaps, the tile repeated across a greedy quad spanning several
/// blocks. Within a level the filter is `NEAREST` both ways, so near faces keep sharp
/// texels. Between the two levels nearest the pixel size the mode is `LINEAR`: the GPU
/// blends them, so no visible line marks where one level hands over to the next. The LOD is
/// not clamped (`LOD_CLAMP_NONE`): the view's level count limits it, so the one sampler
/// fits arrays of every tile size. No anisotropy (an optional device feature).
fn sampler_info() -> vk::SamplerCreateInfo<'static> {
    vk::SamplerCreateInfo::default()
        .mag_filter(vk::Filter::NEAREST)
        .min_filter(vk::Filter::NEAREST)
        .mipmap_mode(vk::SamplerMipmapMode::LINEAR)
        .address_mode_u(vk::SamplerAddressMode::REPEAT)
        .address_mode_v(vk::SamplerAddressMode::REPEAT)
        .address_mode_w(vk::SamplerAddressMode::REPEAT)
        .anisotropy_enable(false)
        .compare_enable(false)
        .min_lod(0.0)
        .max_lod(vk::LOD_CLAMP_NONE)
        .unnormalized_coordinates(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sampler_repeats_and_keeps_pixels_sharp() {
        let info = sampler_info();
        assert_eq!(info.mag_filter, vk::Filter::NEAREST);
        assert_eq!(info.min_filter, vk::Filter::NEAREST);
        assert_eq!(info.mipmap_mode, vk::SamplerMipmapMode::LINEAR);
        assert_eq!(info.address_mode_u, vk::SamplerAddressMode::REPEAT);
        assert_eq!(info.address_mode_v, vk::SamplerAddressMode::REPEAT);
        assert_eq!(info.anisotropy_enable, vk::FALSE);
    }

    #[test]
    fn the_sampler_reaches_the_smallest_level_of_the_largest_tile() {
        let info = sampler_info();
        assert_eq!(info.min_lod, 0.0);
        let last_level = mip_level_count(TextureLayers::MAX_SIZE) - 1;
        assert!(info.max_lod >= last_level as f32);
    }

    #[test]
    fn the_image_is_a_sampled_rgba8_array() {
        let info = image_info(16, 3);
        assert_eq!(info.format, vk::Format::R8G8B8A8_UNORM);
        assert_eq!((info.extent.width, info.extent.height), (16, 16));
        assert_eq!((info.array_layers, info.mip_levels), (3, 5));
        assert!(info.usage.contains(vk::ImageUsageFlags::SAMPLED));
        assert_eq!(info.tiling, vk::ImageTiling::OPTIMAL);
    }

    #[test]
    fn one_copy_per_level_covers_every_layer_from_its_offset() {
        let pixels = vec![0; 4 * 4 * 5 * TextureLayers::BYTES_PER_TEXEL];
        let textures = TextureLayers {
            size: 4,
            layer_count: 5,
            pixels: &pixels,
        };
        let chain = MipChain::build(&textures).expect("valid test textures");
        let copies = copy_regions(chain.levels(), 5);
        let seen: Vec<(u32, u64, u32)> = copies
            .iter()
            .map(|copy| {
                assert_eq!(copy.image_subresource.layer_count, 5);
                assert_eq!(copy.image_extent.width, copy.image_extent.height);
                assert_eq!((copy.buffer_row_length, copy.buffer_image_height), (0, 0));
                let level = copy.image_subresource.mip_level;
                (level, copy.buffer_offset, copy.image_extent.width)
            })
            .collect();
        // Level offsets: 0, then 4 * 4 * 5 * 4 = 320, then 320 + 2 * 2 * 5 * 4 = 400.
        assert_eq!(seen, [(0, 0, 4), (1, 320, 2), (2, 400, 1)]);
    }
}
