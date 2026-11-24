//! Mip chains for the block texture array, built on the CPU (decision 0013, section
//! "Textures").
//!
//! Distant faces cover less than a texel per pixel; sampling only the full-size picture then
//! picks a different texel every frame the camera moves, and the terrain shimmers. Smaller
//! pre-averaged copies (mip levels) let the GPU pick one whose texels match the pixel size.
//! [`MipChain::build`] halves the caller's base level with a 2x2 box filter down to 1x1.
//! It runs once per upload (startup, later a pack reload), so it is plain code, not a hot
//! path. The chain is a third larger than its base level: about 7 KB for the default pack
//! (five 16x16 layers), at most about 85 MiB for the largest array the renderer accepts
//! (256 layers of 256x256, a 64 MiB base level). It is pure and unit tested without a GPU; the Vulkan
//! side copies the result into the image level by level.

use crate::RenderError;
use crate::TextureLayers;
use crate::texture::level_byte_len;

/// Texels a box filter averages into one: a square of two by two.
const BOX_TEXELS: u32 = 4;

/// Added to the sum of a box before dividing by [`BOX_TEXELS`], so the average is rounded
/// to the nearest value with halves going up, instead of always down (which would darken
/// every level a little more than the one before).
const BOX_ROUNDING: u32 = BOX_TEXELS / 2;

/// The message when the chain does not fit into memory.
const OUT_OF_MEMORY: &str = "not enough memory for the mip chain";

/// The number of mip levels of a square power-of-two `size`: the base level, then each half
/// as wide down to 1x1, so `log2(size) + 1`. A `size` of 0 (never valid) counts as 1.
pub(crate) fn mip_level_count(size: u32) -> u32 {
    size.max(1).ilog2() + 1
}

/// Where one level of a [`MipChain`] lies in its pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct MipLevel {
    /// Side of every layer at this level, in pixels.
    pub(crate) size: u32,
    /// Byte offset of the level's first layer in [`MipChain::pixels`].
    pub(crate) offset: usize,
}

/// Every mip level of a texture array, in the layout the GPU copies from: level after
/// level, each level layer after layer, each layer tightly packed rows of RGBA8 texels.
///
/// Level 0 is the caller's pixels unchanged. Each further level halves the previous one,
/// layer by layer, so texels of different layers are never mixed.
///
/// Colours are averaged as stored, with no premultiplied alpha and no conversion to
/// linear light. Plain averaging is right for opaque textures, which all block textures are
/// today (the chunk shader writes alpha 1). Once textures with transparent texels are drawn,
/// the colour of invisible texels would bleed into their neighbours at smaller levels; that
/// change must premultiply by alpha before averaging. Averaging gamma-encoded values makes
/// distant high-contrast textures slightly darker than a linear-light average would; the
/// whole pipeline is gamma-space for now (decision 0013, "Format and filtering").
pub(crate) struct MipChain {
    pixels: Vec<u8>,
    levels: Vec<MipLevel>,
}

impl MipChain {
    /// Builds every level of `textures` down to 1x1.
    ///
    /// # Errors
    /// [`RenderError::InvalidTextures`] if the size is not a power of two, the pixels do
    /// not have exactly the length size and layer count demand, or the chain does not fit
    /// into memory.
    pub(crate) fn build(textures: &TextureLayers<'_>) -> Result<Self, RenderError> {
        if !textures.size.is_power_of_two() {
            return Err(invalid("the tile size is not a power of two"));
        }
        let (levels, total) = layout(textures.size, textures.layer_count)?;
        if textures.pixels.len() != level_byte_len(textures.size, textures.layer_count) {
            return Err(invalid("the pixels do not match size and layer count"));
        }
        let mut pixels = Vec::new();
        pixels
            .try_reserve_exact(total)
            .map_err(|_| invalid(OUT_OF_MEMORY))?;
        pixels.extend_from_slice(textures.pixels);
        // Within the reserved capacity, so this does not allocate.
        pixels.resize(total, 0);
        for pair in levels.windows(2) {
            let [source, target] = [pair[0], pair[1]];
            fill_level(&mut pixels, source, target, textures.layer_count);
        }
        Ok(Self { pixels, levels })
    }

    /// All levels, ready to copy into a staging buffer.
    pub(crate) fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    /// The levels from the base (largest) to 1x1.
    pub(crate) fn levels(&self) -> &[MipLevel] {
        &self.levels
    }
}

/// The levels of a chain of `size` and `layer_count` with their offsets, and the total
/// byte length.
fn layout(size: u32, layer_count: u32) -> Result<(Vec<MipLevel>, usize), RenderError> {
    let mut levels = Vec::new();
    let mut offset: usize = 0;
    for level in 0..mip_level_count(size) {
        let level_size = size >> level;
        levels.push(MipLevel {
            size: level_size,
            offset,
        });
        offset = offset
            .checked_add(level_byte_len(level_size, layer_count))
            .ok_or_else(|| invalid(OUT_OF_MEMORY))?;
    }
    Ok((levels, offset))
}

/// Writes level `target` of `pixels` by halving level `source`, one layer at a time.
fn fill_level(pixels: &mut [u8], source: MipLevel, target: MipLevel, layer_count: u32) {
    let source_layer_len = level_byte_len(source.size, 1);
    let target_layer_len = level_byte_len(target.size, 1);
    // The source level lies wholly before the target level, so the split separates them.
    let (before, after) = pixels.split_at_mut(target.offset);
    let source_level = &before[source.offset..];
    for layer in 0..layer_count as usize {
        let from = &source_level[layer * source_layer_len..][..source_layer_len];
        let to = &mut after[layer * target_layer_len..][..target_layer_len];
        halve_layer(from, source.size as usize, to);
    }
}

/// Writes into `target` the picture `source` (a square of `source_size` texels) at half
/// the size: each target texel is the rounded average of a 2x2 box of source texels,
/// channel by channel. A box never crosses the picture's edge, as the size is even.
fn halve_layer(source: &[u8], source_size: usize, target: &mut [u8]) {
    const TEXEL: usize = TextureLayers::BYTES_PER_TEXEL;
    let target_size = source_size / 2;
    for y in 0..target_size {
        for x in 0..target_size {
            let top = (2 * y * source_size + 2 * x) * TEXEL;
            let bottom = top + source_size * TEXEL;
            let out = (y * target_size + x) * TEXEL;
            for channel in 0..TEXEL {
                let sum = [top, top + TEXEL, bottom, bottom + TEXEL]
                    .iter()
                    .map(|&texel| u32::from(source[texel + channel]))
                    .sum::<u32>();
                // At most `u8::MAX`: the rounded average of four bytes.
                target[out + channel] = ((sum + BOX_ROUNDING) / BOX_TEXELS) as u8;
            }
        }
    }
}

fn invalid(message: &str) -> RenderError {
    RenderError::InvalidTextures(String::from(message))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build(size: u32, layer_count: u32, pixels: &[u8]) -> MipChain {
        let textures = TextureLayers {
            size,
            layer_count,
            pixels,
        };
        MipChain::build(&textures).expect("valid test textures")
    }

    /// The texels of `layer` at `level` of `chain`.
    fn layer(chain: &MipChain, level: usize, layer: usize) -> &[u8] {
        let level = chain.levels()[level];
        let len = level_byte_len(level.size, 1);
        &chain.pixels()[level.offset + layer * len..][..len]
    }

    #[test]
    fn the_level_count_is_log2_plus_one() {
        assert_eq!(mip_level_count(1), 1);
        assert_eq!(mip_level_count(2), 2);
        assert_eq!(mip_level_count(16), 5);
        assert_eq!(mip_level_count(TextureLayers::MAX_SIZE), 9);
    }

    #[test]
    fn every_level_halves_down_to_one_pixel_and_follows_the_last() {
        let chain = build(16, 3, &vec![0; 16 * 16 * 3 * 4]);
        let sizes: Vec<u32> = chain.levels().iter().map(|level| level.size).collect();
        assert_eq!(sizes, [16, 8, 4, 2, 1]);
        let offsets: Vec<usize> = chain.levels().iter().map(|level| level.offset).collect();
        // Each level holds three layers of size * size * 4 bytes.
        assert_eq!(offsets, [0, 3072, 3840, 4032, 4080]);
        assert_eq!(chain.pixels().len(), 4080 + 3 * 4);
    }

    #[test]
    fn a_one_pixel_texture_has_only_its_base_level() {
        let chain = build(1, 2, &[1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(chain.levels(), [MipLevel { size: 1, offset: 0 }]);
        assert_eq!(chain.pixels(), [1, 2, 3, 4, 5, 6, 7, 8]);
    }

    #[test]
    fn the_base_level_is_the_input_unchanged() {
        let input: Vec<u8> = (0..4 * 4 * 4).map(|i| i as u8).collect();
        let chain = build(4, 1, &input);
        assert_eq!(&chain.pixels()[..input.len()], input);
    }

    #[test]
    fn a_uniform_texture_stays_uniform_at_every_level() {
        let texel = [12, 34, 56, 255];
        let input = texel.repeat(32 * 32);
        let chain = build(32, 1, &input);
        assert!(chain.pixels().chunks(4).all(|t| t == texel));
    }

    #[test]
    fn a_checker_averages_to_its_mean_rounded_half_up() {
        // Magenta and black, as the missing texture: (255 + 0 + 0 + 255) / 4 = 127.5.
        let input = [
            [255, 0, 255, 255],
            [0, 0, 0, 255],
            [0, 0, 0, 255],
            [255, 0, 255, 255],
        ];
        let chain = build(2, 1, input.as_flattened());
        assert_eq!(layer(&chain, 1, 0), [128, 0, 128, 255]);
    }

    #[test]
    fn each_texel_is_the_rounded_average_of_its_own_box() {
        // A 4x4 picture whose red channel is the texel index, other channels fixed.
        let input: Vec<u8> = (0..16u8).flat_map(|i| [i, 0, 200, 255]).collect();
        let chain = build(4, 1, &input);
        // Boxes: {0,1,4,5} = 2.5 -> 3, {2,3,6,7} = 4.5 -> 5,
        // {8,9,12,13} = 10.5 -> 11, {10,11,14,15} = 12.5 -> 13.
        let red: Vec<u8> = layer(&chain, 1, 0).chunks(4).map(|t| t[0]).collect();
        assert_eq!(red, [3, 5, 11, 13]);
        // Level 2 averages level 1: (3 + 5 + 11 + 13) / 4 = 8.
        assert_eq!(layer(&chain, 2, 0), [8, 0, 200, 255]);
    }

    #[test]
    fn layers_are_never_mixed() {
        let white = [u8::MAX; 4].repeat(8 * 8);
        let black = [0, 0, 0, u8::MAX].repeat(8 * 8);
        let chain = build(8, 2, &[white.clone(), black].concat());
        for level in 1..chain.levels().len() {
            assert!(layer(&chain, level, 0).iter().all(|&byte| byte == u8::MAX));
            assert!(
                layer(&chain, level, 1)
                    .chunks(4)
                    .all(|t| t == [0, 0, 0, 255])
            );
        }
    }

    #[test]
    fn wrong_input_is_an_error_not_a_panic() {
        let not_power_of_two = TextureLayers {
            size: 3,
            layer_count: 1,
            pixels: &[0; 36],
        };
        assert!(MipChain::build(&not_power_of_two).is_err());
        let short = TextureLayers {
            size: 2,
            layer_count: 1,
            pixels: &[0; 15],
        };
        assert!(MipChain::build(&short).is_err());
    }
}
