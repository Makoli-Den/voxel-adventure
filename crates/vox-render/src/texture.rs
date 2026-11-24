//! Block textures as the caller hands them to the renderer, and their validation.
//!
//! [`TextureLayers`] is plain data: a square tile size, a layer count and RGBA8 pixels. The
//! renderer uploads it as one 2D array image, one layer per texture (decision 0013, section
//! "Textures"). This module checks the data against the limits of the format and the device
//! and builds the default "missing" picture. It is pure, so it is unit tested without a GPU.
//! Callers pass the base level only; the renderer builds the mip chain from it
//! (`crate::mipmap`).

use std::fmt;

use crate::RenderError;

/// A borrowed view of block texture pixels, passed to [`crate::Renderer::set_block_textures`].
///
/// All layers are squares of the same [`Self::size`]. The pixels are RGBA8 (red, green, blue,
/// alpha, one byte each, used as stored: no sRGB conversion), rows top to bottom, each row
/// left to right, layer after layer.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct TextureLayers<'a> {
    /// Side of every layer in pixels: a power of two, at most [`Self::MAX_SIZE`].
    pub size: u32,
    /// Number of layers: at least 1, at most [`Self::MAX_LAYERS`] and the device's limit.
    pub layer_count: u32,
    /// Exactly `layer_count * size * size * BYTES_PER_TEXEL` bytes: the full-size picture
    /// only. The renderer builds the smaller mip levels from it.
    pub pixels: &'a [u8],
}

/// Shows the pixel count instead of the pixels, which can be megabytes.
impl fmt::Debug for TextureLayers<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TextureLayers")
            .field("size", &self.size)
            .field("layer_count", &self.layer_count)
            .field("pixels_len", &self.pixels.len())
            .finish()
    }
}

impl TextureLayers<'_> {
    /// Bytes of one texel: red, green, blue, alpha.
    pub const BYTES_PER_TEXEL: usize = 4;

    /// The largest side accepted, in pixels. It bounds the memory of the whole array to
    /// `MAX_LAYERS * MAX_SIZE * MAX_SIZE * BYTES_PER_TEXEL` = 64 MiB and is far below the
    /// 4096 pixels (`maxImageDimension2D`) every Vulkan 1.0 device supports.
    pub const MAX_SIZE: u32 = 256;

    /// The most layers accepted: the number of array layers every Vulkan 1.0 device
    /// supports (`maxImageArrayLayers` is at least this), so the limit is the same on all
    /// devices. A device that reports less (it would break the specification) lowers it.
    pub const MAX_LAYERS: u32 = 256;

    /// Checks the data against `limits`.
    ///
    /// # Errors
    /// [`RenderError::InvalidTextures`] naming the problem: the size is not a power of two
    /// or exceeds the limit, the layer count is zero or exceeds the limit, or the pixel
    /// slice does not have exactly the expected length.
    pub(crate) fn validate(&self, limits: TextureLimits) -> Result<(), RenderError> {
        if !self.size.is_power_of_two() {
            return Err(invalid(format!(
                "the tile size {} is not a power of two",
                self.size
            )));
        }
        if self.size > limits.max_size {
            return Err(invalid(format!(
                "the tile size {} exceeds the limit of {}",
                self.size, limits.max_size
            )));
        }
        if self.layer_count == 0 {
            return Err(invalid(String::from("there are no layers")));
        }
        if self.layer_count > limits.max_layers {
            return Err(invalid(format!(
                "{} layers exceed the limit of {}",
                self.layer_count, limits.max_layers
            )));
        }
        let expected = self.byte_len();
        if self.pixels.len() != expected {
            return Err(invalid(format!(
                "the pixels have {} bytes, {expected} expected",
                self.pixels.len()
            )));
        }
        Ok(())
    }

    /// The exact byte length the pixels must have. Cannot overflow once size and layer
    /// count are within [`Self::MAX_SIZE`] and [`Self::MAX_LAYERS`] (64 MiB); saturates
    /// otherwise, which no slice can match.
    fn byte_len(&self) -> usize {
        level_byte_len(self.size, self.layer_count)
    }
}

/// Bytes of `layer_count` square layers of `size` pixels: one mip level of an array, or
/// the whole array the caller passes. Saturates instead of overflowing; no real slice or
/// allocation is that long.
pub(crate) fn level_byte_len(size: u32, layer_count: u32) -> usize {
    // Lossless: `u32` fits `usize` on every target the game supports (32 bits and up).
    let side = size as usize;
    side.saturating_mul(side)
        .saturating_mul(layer_count as usize)
        .saturating_mul(TextureLayers::BYTES_PER_TEXEL)
}

/// How large a texture array may be on this device: the crate's own limits, lowered by the
/// device's where it reports less.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TextureLimits {
    /// The largest tile side in pixels.
    pub(crate) max_size: u32,
    /// The most layers.
    pub(crate) max_layers: u32,
}

impl TextureLimits {
    /// The limits for a device that reports `max_image_dimension_2d` and
    /// `max_image_array_layers` (Vulkan's `maxImageDimension2D` and `maxImageArrayLayers`).
    pub(crate) fn of_device(max_image_dimension_2d: u32, max_image_array_layers: u32) -> Self {
        Self {
            max_size: TextureLayers::MAX_SIZE.min(max_image_dimension_2d),
            max_layers: TextureLayers::MAX_LAYERS.min(max_image_array_layers),
        }
    }
}

/// The bright cell of the missing picture. Magenta, because it appears in no natural texture.
const MISSING_COLOR_A: [u8; TextureLayers::BYTES_PER_TEXEL] = [255, 0, 255, u8::MAX];

/// The dark cell of the missing picture, opaque black.
const MISSING_COLOR_B: [u8; TextureLayers::BYTES_PER_TEXEL] = [0, 0, 0, u8::MAX];

/// Side of the missing picture in pixels: a 2x2 checker, one pixel per cell. The sampler
/// magnifies with `NEAREST` and repeats, so each block face shows the checker sharply. Like
/// every texture it gets a mip chain; its 1x1 level is the checker's mean, a dark magenta.
pub(crate) const MISSING_SIZE: u32 = 2;

/// The pixels of the missing picture: one layer, a magenta and black checker with magenta
/// in the top-left corner. Bound until [`crate::Renderer::set_block_textures`] is called,
/// so the chunk pipeline always has a valid texture to sample.
pub(crate) fn missing_texture_pixels() -> Vec<u8> {
    (0..MISSING_SIZE)
        .flat_map(|y| (0..MISSING_SIZE).map(move |x| (x + y) % 2 == 0))
        .flat_map(|bright| {
            if bright {
                MISSING_COLOR_A
            } else {
                MISSING_COLOR_B
            }
        })
        .collect()
}

/// The missing picture as layers: one layer of [`MISSING_SIZE`].
pub(crate) fn missing_texture(pixels: &[u8]) -> TextureLayers<'_> {
    TextureLayers {
        size: MISSING_SIZE,
        layer_count: 1,
        pixels,
    }
}

fn invalid(message: String) -> RenderError {
    RenderError::InvalidTextures(message)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The limits on a device that reports exactly what Vulkan 1.0 guarantees
    /// (`maxImageDimension2D` 4096, `maxImageArrayLayers` 256): the crate's own limits.
    fn device() -> TextureLimits {
        TextureLimits::of_device(4096, 256)
    }

    fn pixels(size: u32, layer_count: u32) -> Vec<u8> {
        vec![0; (size * size * layer_count * 4) as usize]
    }

    fn check(size: u32, layer_count: u32, pixels: &[u8]) -> Result<(), String> {
        let layers = TextureLayers {
            size,
            layer_count,
            pixels,
        };
        layers.validate(device()).map_err(|error| error.to_string())
    }

    #[test]
    fn valid_layers_pass() {
        assert_eq!(check(16, 4, &pixels(16, 4)), Ok(()));
        assert_eq!(check(1, 1, &pixels(1, 1)), Ok(()));
    }

    #[test]
    fn the_largest_array_passes() {
        assert_eq!(check(256, 256, &pixels(256, 256)), Ok(()));
    }

    #[test]
    fn a_size_that_is_not_a_power_of_two_is_rejected() {
        let error = check(24, 1, &pixels(24, 1)).unwrap_err();
        assert!(
            error.contains("24") && error.contains("power of two"),
            "{error}"
        );
        assert!(check(0, 1, &[]).is_err());
    }

    #[test]
    fn a_size_above_the_limit_is_rejected() {
        let error = check(512, 1, &pixels(512, 1)).unwrap_err();
        assert!(error.contains("512") && error.contains("256"), "{error}");
    }

    #[test]
    fn zero_layers_are_rejected() {
        let error = check(16, 0, &[]).unwrap_err();
        assert!(error.contains("no layers"), "{error}");
    }

    #[test]
    fn more_layers_than_the_limit_are_rejected() {
        let error = check(1, 257, &pixels(1, 257)).unwrap_err();
        assert!(error.contains("257") && error.contains("256"), "{error}");
    }

    #[test]
    fn the_device_limit_lowers_the_layer_count() {
        let limits = TextureLimits::of_device(16_384, 128);
        assert_eq!(limits.max_layers, 128);
        assert_eq!(limits.max_size, TextureLayers::MAX_SIZE);
        let data = pixels(1, 129);
        let layers = TextureLayers {
            size: 1,
            layer_count: 129,
            pixels: &data,
        };
        let error = layers.validate(limits).unwrap_err().to_string();
        assert!(error.contains("129") && error.contains("128"), "{error}");
    }

    #[test]
    fn a_device_above_the_crate_limits_keeps_them() {
        let limits = TextureLimits::of_device(16_384, 2048);
        assert_eq!(limits, device());
        assert_eq!(device().max_size, TextureLayers::MAX_SIZE);
        assert_eq!(device().max_layers, TextureLayers::MAX_LAYERS);
    }

    #[test]
    fn pixels_one_byte_short_or_long_are_rejected() {
        let mut data = pixels(16, 2);
        data.pop();
        let error = check(16, 2, &data).unwrap_err();
        assert!(error.contains("2047") && error.contains("2048"), "{error}");
        data.extend_from_slice(&[0, 0]);
        assert!(check(16, 2, &data).is_err());
    }

    #[test]
    fn huge_counts_do_not_overflow_the_length() {
        let layers = TextureLayers {
            size: 1 << 31,
            layer_count: u32::MAX,
            pixels: &[],
        };
        assert_eq!(layers.byte_len(), usize::MAX);
    }

    #[test]
    fn debug_shows_the_pixel_count_not_the_pixels() {
        let data = pixels(2, 1);
        let layers = TextureLayers {
            size: 2,
            layer_count: 1,
            pixels: &data,
        };
        let text = format!("{layers:?}");
        assert_eq!(
            text,
            "TextureLayers { size: 2, layer_count: 1, pixels_len: 16 }"
        );
    }

    #[test]
    fn the_missing_texture_is_a_valid_magenta_and_black_checker() {
        let data = missing_texture_pixels();
        assert_eq!(missing_texture(&data).validate(device()).ok(), Some(()));
        // Row 0: magenta, black. Row 1: black, magenta.
        assert_eq!(data[..4], [255, 0, 255, 255]);
        assert_eq!(data[4..8], [0, 0, 0, 255]);
        assert_eq!(data[8..12], [0, 0, 0, 255]);
        assert_eq!(data[12..], [255, 0, 255, 255]);
    }
}
