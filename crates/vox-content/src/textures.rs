//! Block textures on the CPU (decision 0013, section "Textures").
//!
//! [`build_textures`] reads the PNG of every texture layer through the pack stack and packs
//! them into one [`BlockTextures`]: RGBA8 pixels, layer after layer, all layers the same
//! square size. Layer 0 is the "missing" picture, a magenta and black checker generated here.
//! A texture that cannot be used gets that picture on its own layer and a [`Warning`]; the
//! game still starts. Warnings are returned as data, this crate never prints.
//!
//! Each texture is resolved on its own through the pack stack and the highest pack that has
//! the file wins as a whole. A corrupt texture in a high pack is therefore not replaced by the
//! good one of a lower pack: it shows the missing picture and a warning, so the pack author
//! sees the mistake instead of a silent fallback.
//!
//! The tile size is the largest size among the textures that loaded; smaller power-of-two
//! textures are scaled up by an integer factor with nearest-neighbour sampling, so a 32x pack
//! that misses a texture still shows the 16x base texture without blur.

use std::fmt;
use std::io::Cursor;

use png::{ColorType, Transformations};
use vox_resources::{PackStack, ResourceError, ResourceId};
use vox_world::{BlockRegistry, TextureLayer};

use crate::blocks;
use crate::error::ContentError;

/// The largest tile size accepted, in pixels per side. A larger file is rejected so one
/// careless texture cannot make every layer of the array enormous.
///
/// Together with [`MAX_LAYERS`] this bounds the memory of [`BlockTextures`]:
/// `MAX_LAYERS * MAX_TILE_SIZE * MAX_TILE_SIZE * BYTES_PER_PIXEL` = 256 * 256 * 256 * 4 =
/// 64 MiB, and the same again on the GPU. Real packs are far smaller (16x16 is 1 KiB a layer).
pub const MAX_TILE_SIZE: u32 = 256;

/// The most layers (distinct textures, the missing picture included) the array may have. It
/// is the number of array layers Vulkan guarantees (`maxImageArrayLayers`), so the renderer
/// never receives more than every device accepts. `load_blocks` rejects a blocks file that
/// needs more.
pub const MAX_LAYERS: usize = 256;

/// The largest PNG file read, in bytes. A 256x256 RGBA picture of pure noise is about 260 KiB
/// compressed, so 1 MiB is generous; a bigger file is refused before it is decoded.
pub const MAX_TEXTURE_FILE_BYTES: usize = 1 << 20;

/// The memory the PNG decoder may reserve for one texture, in bytes. The largest accepted
/// picture is 256x256, 16-bit RGBA, interlaced: about 0.5 MiB of pixels, so 1 MiB covers it
/// with room for the decoder's own buffers while a crafted file cannot ask for more.
const DECODE_LIMIT_BYTES: usize = 1 << 20;

/// The tile size when no texture loaded at all (every layer is then the missing picture).
pub const DEFAULT_TILE_SIZE: u32 = 16;

/// Bytes per pixel of the output: red, green, blue, alpha.
pub const BYTES_PER_PIXEL: usize = 4;

/// Alpha of a pixel that has none in the file.
const OPAQUE: u8 = u8::MAX;

/// The bright cell of the missing picture. Magenta, because it appears in no natural texture.
const MISSING_COLOR_A: [u8; BYTES_PER_PIXEL] = [255, 0, 255, OPAQUE];

/// The dark cell of the missing picture.
const MISSING_COLOR_B: [u8; BYTES_PER_PIXEL] = [0, 0, 0, OPAQUE];

/// Cells per side of the missing picture's checker: a 2x2 board, the classic look.
const MISSING_CELLS_PER_SIDE: u32 = 2;

/// Why a texture could not be used.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TextureProblem {
    /// No pack has the file.
    Missing,
    /// A pack has the file but reading it failed.
    Unreadable(String),
    /// The file is not a PNG this loader can decode.
    Undecodable(String),
    /// Width and height differ.
    NotSquare {
        /// Width in pixels.
        width: u32,
        /// Height in pixels.
        height: u32,
    },
    /// The side is not a power of two.
    NotPowerOfTwo {
        /// Side in pixels.
        size: u32,
    },
    /// The side is larger than [`MAX_TILE_SIZE`].
    TooLarge {
        /// Side in pixels.
        size: u32,
    },
    /// The file is larger than [`MAX_TEXTURE_FILE_BYTES`].
    FileTooLarge {
        /// Size of the file in bytes.
        bytes: usize,
    },
    /// The list had more than [`MAX_LAYERS`] textures; this one got no layer of its own.
    NoLayerLeft,
}

impl fmt::Display for TextureProblem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing => write!(f, "file not found"),
            Self::Unreadable(why) => write!(f, "cannot be read: {why}"),
            Self::Undecodable(why) => write!(f, "not a usable PNG: {why}"),
            Self::NotSquare { width, height } => {
                write!(f, "must be square, but is {width}x{height}")
            }
            Self::NotPowerOfTwo { size } => {
                write!(f, "the side must be a power of two, but is {size}")
            }
            Self::TooLarge { size } => {
                write!(f, "the side {size} exceeds the maximum {MAX_TILE_SIZE}")
            }
            Self::FileTooLarge { bytes } => {
                write!(
                    f,
                    "the file has {bytes} bytes, more than {MAX_TEXTURE_FILE_BYTES}"
                )
            }
            Self::NoLayerLeft => write!(f, "more than {MAX_LAYERS} textures"),
        }
    }
}

/// A texture that was replaced by the missing picture, and why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Warning {
    /// The texture that failed.
    pub texture: ResourceId,
    /// What was wrong with it.
    pub problem: TextureProblem,
}

impl fmt::Display for Warning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "texture `{}`: {}; showing the missing texture",
            self.texture, self.problem
        )
    }
}

/// The pixels of all block textures, ready to upload as a 2D array image.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlockTextures {
    /// Side of every layer in pixels: a power of two, at most [`MAX_TILE_SIZE`].
    pub size: u32,
    /// Number of layers; layer 0 is the missing picture.
    pub layer_count: u32,
    /// RGBA8, rows top to bottom, layer after layer; `layer_count * size * size * 4` bytes.
    pub pixels: Vec<u8>,
}

impl BlockTextures {
    /// The bytes of one layer, or `None` past the last layer.
    pub fn layer(&self, layer: TextureLayer) -> Option<&[u8]> {
        let bytes = self.layer_bytes();
        let start = usize::from(layer.0).checked_mul(bytes)?;
        self.pixels.get(start..start.checked_add(bytes)?)
    }

    /// Bytes in one layer.
    fn layer_bytes(&self) -> usize {
        side(self.size) * side(self.size) * BYTES_PER_PIXEL
    }
}

/// A pixel side as an index. Sides are at most [`MAX_TILE_SIZE`], and `u32` fits `usize` on
/// every target the game supports (32 bits and up), so the conversion is lossless.
fn side(size: u32) -> usize {
    size as usize
}

/// A decoded square picture, RGBA8.
struct Image {
    size: u32,
    pixels: Vec<u8>,
}

/// Decodes a PNG into RGBA8 and checks that it is a usable tile.
///
/// Every 8-bit colour type and the palette and low-bit-depth forms are expanded to RGBA8
/// (grayscale becomes equal red, green and blue; a missing alpha becomes opaque; a `tRNS`
/// chunk becomes alpha). 16-bit images are converted to 8 bits by keeping the high byte,
/// which is what the `png` crate's `STRIP_16` does; the loss is invisible at this size.
fn decode_png(bytes: &[u8]) -> Result<Image, TextureProblem> {
    let undecodable = |error: png::DecodingError| TextureProblem::Undecodable(error.to_string());
    let limits = png::Limits {
        bytes: DECODE_LIMIT_BYTES,
    };
    let mut decoder = png::Decoder::new_with_limits(Cursor::new(bytes), limits);
    decoder.set_transformations(Transformations::EXPAND | Transformations::STRIP_16);
    let mut reader = decoder.read_info().map_err(undecodable)?;
    // Checked from the header, before any pixel buffer is allocated.
    let (width, height) = {
        let info = reader.info();
        (info.width, info.height)
    };
    check_tile(width, height)?;
    // `None` means the size overflows `usize`; the tile check above makes that impossible, but
    // the decoder's own contract is still honoured.
    let buffer_size = reader
        .output_buffer_size()
        .ok_or_else(|| TextureProblem::Undecodable("image too large".to_owned()))?;
    let mut buffer = vec![0; buffer_size];
    let frame = reader.next_frame(&mut buffer).map_err(undecodable)?;
    buffer.truncate(frame.buffer_size());
    let pixels = match frame.color_type {
        // Already RGBA8: the decoder's buffer becomes the pixels, no copy.
        ColorType::Rgba => buffer,
        ColorType::Rgb => expand(&buffer, ColorType::Rgb.samples(), |p| {
            [p[0], p[1], p[2], OPAQUE]
        }),
        ColorType::Grayscale => expand(&buffer, ColorType::Grayscale.samples(), |p| {
            [p[0], p[0], p[0], OPAQUE]
        }),
        ColorType::GrayscaleAlpha => expand(&buffer, ColorType::GrayscaleAlpha.samples(), |p| {
            [p[0], p[0], p[0], p[1]]
        }),
        // EXPAND turns palette images into RGB or RGBA; seeing one here means the decoder
        // changed its contract.
        ColorType::Indexed => {
            return Err(TextureProblem::Undecodable(
                "palette image was not expanded".to_owned(),
            ));
        }
    };
    debug_assert_eq!(pixels.len(), side(width) * side(height) * BYTES_PER_PIXEL);
    Ok(Image {
        size: width,
        pixels,
    })
}

/// Converts `data`, `samples` bytes per pixel, to RGBA8 with `convert`.
fn expand(
    data: &[u8],
    samples: usize,
    convert: impl Fn(&[u8]) -> [u8; BYTES_PER_PIXEL],
) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() / samples * BYTES_PER_PIXEL);
    for pixel in data.chunks_exact(samples) {
        out.extend_from_slice(&convert(pixel));
    }
    out
}

/// The rules for a tile: square, a power of two, not larger than [`MAX_TILE_SIZE`].
fn check_tile(width: u32, height: u32) -> Result<(), TextureProblem> {
    if width != height {
        return Err(TextureProblem::NotSquare { width, height });
    }
    if !width.is_power_of_two() {
        return Err(TextureProblem::NotPowerOfTwo { size: width });
    }
    if width > MAX_TILE_SIZE {
        return Err(TextureProblem::TooLarge { size: width });
    }
    Ok(())
}

/// Reads and decodes one texture from the pack stack.
fn load_texture(stack: &PackStack, id: &ResourceId) -> Result<Image, TextureProblem> {
    let bytes = stack.read(id).map_err(|error| match error {
        ResourceError::NotFound { .. } => TextureProblem::Missing,
        other => TextureProblem::Unreadable(other.to_string()),
    })?;
    if bytes.len() > MAX_TEXTURE_FILE_BYTES {
        return Err(TextureProblem::FileTooLarge { bytes: bytes.len() });
    }
    decode_png(&bytes)
}

/// The missing picture at `size`: a checker of [`MISSING_CELLS_PER_SIDE`] cells per side, the
/// top-left cell magenta.
fn missing_pixels(size: u32) -> Vec<u8> {
    let cell = (size / MISSING_CELLS_PER_SIDE).max(1);
    let mut pixels = Vec::with_capacity(side(size) * side(size) * BYTES_PER_PIXEL);
    for y in 0..size {
        for x in 0..size {
            let odd = (x / cell + y / cell) % 2 == 1;
            pixels.extend_from_slice(if odd {
                &MISSING_COLOR_B
            } else {
                &MISSING_COLOR_A
            });
        }
    }
    pixels
}

/// Appends `image` scaled up to `size` by nearest neighbour; `size` is a multiple of the
/// image's size because both are powers of two and `size` is the larger.
fn push_scaled(out: &mut Vec<u8>, image: &Image, size: u32) {
    let factor = side(size / image.size);
    let row_bytes = side(image.size) * BYTES_PER_PIXEL;
    for row in image.pixels.chunks_exact(row_bytes) {
        let start = out.len();
        for pixel in row.chunks_exact(BYTES_PER_PIXEL) {
            for _ in 0..factor {
                out.extend_from_slice(pixel);
            }
        }
        for _ in 1..factor {
            out.extend_from_within(start..start + row_bytes * factor);
        }
    }
}

/// Builds the texture array for `textures`, the texture id of each layer as returned by
/// [`crate::load_blocks`].
///
/// Entry 0 is the missing texture: whatever id it holds is ignored and layer 0 is always the
/// generated picture, so an empty list gives an array of just that one layer. Entries past
/// [`MAX_LAYERS`] get no layer (`load_blocks` never produces such a list) and one
/// [`TextureProblem::NoLayerLeft`] warning each.
///
/// Never fails: a texture that is missing, unreadable, too big a file, not a PNG, not square,
/// not a power of two or larger than [`MAX_TILE_SIZE`] gets the missing picture and one
/// [`Warning`], in layer order. A corrupt texture in a high pack is not replaced by the one of
/// a lower pack (see the module docs).
pub fn build_textures(stack: &PackStack, textures: &[ResourceId]) -> (BlockTextures, Vec<Warning>) {
    let mut warnings = Vec::new();
    let kept = textures.len().min(MAX_LAYERS);
    for id in textures.iter().skip(kept) {
        warnings.push(Warning {
            texture: id.clone(),
            problem: TextureProblem::NoLayerLeft,
        });
    }
    let mut loaded: Vec<Option<Image>> = Vec::with_capacity(kept);
    for id in textures.iter().take(kept).skip(1) {
        match load_texture(stack, id) {
            Ok(image) => loaded.push(Some(image)),
            Err(problem) => {
                warnings.push(Warning {
                    texture: id.clone(),
                    problem,
                });
                loaded.push(None);
            }
        }
    }
    let size = loaded
        .iter()
        .flatten()
        .map(|image| image.size)
        .max()
        .unwrap_or(DEFAULT_TILE_SIZE);
    let missing = missing_pixels(size);
    // At least the missing layer, at most MAX_LAYERS: the sizes below cannot overflow.
    let layer_count = loaded.len() + 1;
    let mut pixels = Vec::with_capacity(layer_count * missing.len());
    pixels.extend_from_slice(&missing);
    for slot in &loaded {
        match slot {
            Some(image) => push_scaled(&mut pixels, image, size),
            None => pixels.extend_from_slice(&missing),
        }
    }
    debug_assert_eq!(pixels.len(), layer_count * missing.len());
    (
        BlockTextures {
            size,
            // At most MAX_LAYERS (256), which fits u32.
            layer_count: layer_count as u32,
            pixels,
        },
        warnings,
    )
}

/// Loads everything the world needs from `stack`: the block registry and the block textures.
///
/// A broken blocks file is an error; a broken texture is only a [`Warning`].
///
/// # Errors
/// As [`crate::load_blocks`], which also rejects a file that needs more than [`MAX_LAYERS`]
/// textures.
pub fn load_content(
    stack: &PackStack,
) -> Result<(BlockRegistry, BlockTextures, Vec<Warning>), ContentError> {
    load_content_from(stack, &[blocks::base_blocks_file()?])
}

/// Like [`load_content`] for several blocks files (one per mod, in load order): one registry
/// and one texture array for all of them. Ids and layers follow [`crate::load_blocks_many`].
/// The caller passes only files that exist; a missing file is an error.
///
/// # Errors
/// As [`crate::load_blocks_many`].
pub fn load_content_from(
    stack: &PackStack,
    files: &[ResourceId],
) -> Result<(BlockRegistry, BlockTextures, Vec<Warning>), ContentError> {
    let (registry, textures) = blocks::load_blocks_many(stack, files)?;
    let (block_textures, warnings) = build_textures(stack, &textures);
    Ok((registry, block_textures, warnings))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};

    use png::BitDepth;
    use vox_resources::DirPack;

    use crate::blocks::MISSING_TEXTURE;

    use super::*;

    /// Makes the directory names of concurrently running tests different.
    static NEXT_DIR: AtomicU32 = AtomicU32::new(0);

    /// The texture directory inside a pack, as a resource path.
    const BLOCK_DIR: &str = "base/textures/block";

    /// A pack directory in the system temp folder, removed on drop.
    struct TempPack(PathBuf);

    impl TempPack {
        fn new() -> Self {
            let id = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
            let dir =
                std::env::temp_dir().join(format!("vox-content-tex-{}-{id}", std::process::id()));
            fs::create_dir_all(dir.join(BLOCK_DIR)).unwrap();
            Self(dir)
        }

        fn write(&self, name: &str, bytes: &[u8]) {
            fs::write(self.0.join(BLOCK_DIR).join(name), bytes).unwrap();
        }

        fn stack(&self) -> PackStack {
            let mut stack = PackStack::new();
            stack.push(DirPack::new(&self.0));
            stack
        }
    }

    impl Drop for TempPack {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn id(name: &str) -> ResourceId {
        ResourceId::parse(&format!("base:textures/block/{name}")).unwrap()
    }

    /// The layer list `load_blocks` would give: the missing texture, then `names`.
    fn layer_ids(names: &[&str]) -> Vec<ResourceId> {
        let mut ids = vec![ResourceId::parse(MISSING_TEXTURE).unwrap()];
        ids.extend(names.iter().map(|name| id(name)));
        ids
    }

    /// Encodes a PNG of `width` x `height`.
    fn encode(
        width: u32,
        height: u32,
        color: ColorType,
        depth: BitDepth,
        palette: Option<Vec<u8>>,
        trns: Option<Vec<u8>>,
        data: &[u8],
    ) -> Vec<u8> {
        let mut out = Vec::new();
        let mut encoder = png::Encoder::new(&mut out, width, height);
        encoder.set_color(color);
        encoder.set_depth(depth);
        if let Some(colors) = palette {
            encoder.set_palette(colors);
        }
        if let Some(alpha) = trns {
            encoder.set_trns(alpha);
        }
        let mut writer = encoder.write_header().unwrap();
        writer.write_image_data(data).unwrap();
        writer.finish().unwrap();
        out
    }

    fn encode_8bit(size: u32, color: ColorType, data: &[u8]) -> Vec<u8> {
        encode(size, size, color, BitDepth::Eight, None, None, data)
    }

    /// `count` pixels of `pixel`.
    fn repeat(pixel: &[u8], count: u32) -> Vec<u8> {
        pixel.repeat(count as usize)
    }

    fn build_one(name: &str, png: &[u8]) -> (BlockTextures, Vec<Warning>) {
        let pack = TempPack::new();
        pack.write(name, png);
        build_textures(&pack.stack(), &layer_ids(&[name]))
    }

    fn layer1(textures: &BlockTextures) -> &[u8] {
        textures.layer(TextureLayer(1)).unwrap()
    }

    #[test]
    fn rgba_is_kept() {
        let png = encode_8bit(2, ColorType::Rgba, &repeat(&[1, 2, 3, 4], 4));
        let (textures, warnings) = build_one("a.png", &png);
        assert!(warnings.is_empty());
        assert_eq!(textures.size, 2);
        assert_eq!(layer1(&textures), repeat(&[1, 2, 3, 4], 4));
    }

    #[test]
    fn rgb_gets_an_opaque_alpha() {
        let png = encode_8bit(2, ColorType::Rgb, &repeat(&[10, 20, 30], 4));
        let (textures, warnings) = build_one("a.png", &png);
        assert!(warnings.is_empty());
        assert_eq!(layer1(&textures), repeat(&[10, 20, 30, 255], 4));
    }

    #[test]
    fn grayscale_fills_all_channels() {
        let png = encode_8bit(2, ColorType::Grayscale, &[7, 7, 7, 7]);
        let (textures, _) = build_one("a.png", &png);
        assert_eq!(layer1(&textures), repeat(&[7, 7, 7, 255], 4));
    }

    #[test]
    fn grayscale_alpha_keeps_its_alpha() {
        let png = encode_8bit(2, ColorType::GrayscaleAlpha, &repeat(&[9, 100], 4));
        let (textures, _) = build_one("a.png", &png);
        assert_eq!(layer1(&textures), repeat(&[9, 9, 9, 100], 4));
    }

    #[test]
    fn palette_is_expanded_with_its_transparency() {
        let palette = vec![255, 0, 0, 0, 255, 0];
        let png = encode(
            2,
            2,
            ColorType::Indexed,
            BitDepth::Eight,
            Some(palette),
            Some(vec![255, 128]),
            &[0, 1, 1, 0],
        );
        let (textures, warnings) = build_one("a.png", &png);
        assert!(warnings.is_empty());
        let red = [255, 0, 0, 255];
        let green = [0, 255, 0, 128];
        assert_eq!(
            layer1(&textures),
            [red, green, green, red].concat().as_slice()
        );
    }

    #[test]
    fn low_bit_depth_grayscale_is_scaled_to_eight_bits() {
        // Four pixels of 1 bit per row of 2: row byte holds the bits in the high end.
        let png = encode(
            2,
            2,
            ColorType::Grayscale,
            BitDepth::One,
            None,
            None,
            &[0b1000_0000, 0b0100_0000],
        );
        let (textures, warnings) = build_one("a.png", &png);
        assert!(warnings.is_empty());
        let white = [255, 255, 255, 255];
        let black = [0, 0, 0, 255];
        assert_eq!(
            layer1(&textures),
            [white, black, black, white].concat().as_slice()
        );
    }

    #[test]
    fn sixteen_bit_keeps_the_high_byte() {
        // One 16-bit RGB pixel per texel, big endian: 0xAB12, 0xCD34, 0xEF56.
        let png = encode(
            2,
            2,
            ColorType::Rgb,
            BitDepth::Sixteen,
            None,
            None,
            &repeat(&[0xAB, 0x12, 0xCD, 0x34, 0xEF, 0x56], 4),
        );
        let (textures, warnings) = build_one("a.png", &png);
        assert!(warnings.is_empty());
        assert_eq!(layer1(&textures), repeat(&[0xAB, 0xCD, 0xEF, 255], 4));
    }

    #[test]
    fn a_smaller_texture_is_scaled_up_to_the_largest_by_nearest_neighbour() {
        let pack = TempPack::new();
        // 2x2 with four different pixels, next to a 4x4 texture that sets the tile size.
        let small: Vec<u8> = vec![1, 1, 1, 255, 2, 2, 2, 255, 3, 3, 3, 255, 4, 4, 4, 255];
        pack.write("small.png", &encode_8bit(2, ColorType::Rgba, &small));
        pack.write(
            "big.png",
            &encode_8bit(4, ColorType::Rgba, &repeat(&[9, 9, 9, 255], 16)),
        );
        let ids = layer_ids(&["small.png", "big.png"]);
        let (textures, warnings) = build_textures(&pack.stack(), &ids);
        assert!(warnings.is_empty());
        assert_eq!(textures.size, 4);
        assert_eq!(textures.layer_count, 3);
        let px = |v: u8| [v, v, v, 255];
        let row = |a: u8, b: u8| [px(a), px(a), px(b), px(b)].concat();
        let expected = [row(1, 2), row(1, 2), row(3, 4), row(3, 4)].concat();
        assert_eq!(textures.layer(TextureLayer(1)).unwrap(), expected);
        assert_eq!(
            textures.layer(TextureLayer(2)).unwrap(),
            repeat(&[9, 9, 9, 255], 16)
        );
    }

    #[test]
    fn sixteen_scales_to_thirty_two() {
        let pack = TempPack::new();
        let mut small = Vec::new();
        for y in 0..16u8 {
            for x in 0..16u8 {
                small.extend_from_slice(&[x, y, 0, 255]);
            }
        }
        pack.write("small.png", &encode_8bit(16, ColorType::Rgba, &small));
        pack.write(
            "big.png",
            &encode_8bit(32, ColorType::Rgba, &repeat(&[0, 0, 0, 255], 32 * 32)),
        );
        let ids = layer_ids(&["small.png", "big.png"]);
        let (textures, warnings) = build_textures(&pack.stack(), &ids);
        assert!(warnings.is_empty());
        assert_eq!(textures.size, 32);
        let layer = textures.layer(TextureLayer(1)).unwrap();
        let at = |x: usize, y: usize| &layer[(y * 32 + x) * 4..(y * 32 + x) * 4 + 4];
        for (x, y) in [(0, 0), (1, 1), (31, 0), (30, 31), (5, 20), (17, 9)] {
            let (sx, sy) = (u8::try_from(x / 2).unwrap(), u8::try_from(y / 2).unwrap());
            assert_eq!(at(x, y), [sx, sy, 0, 255], "pixel {x},{y}");
        }
    }

    fn assert_rejected(png: &[u8], expected: &TextureProblem) {
        let (textures, warnings) = build_one("bad.png", png);
        assert_eq!(
            warnings,
            vec![Warning {
                texture: id("bad.png"),
                problem: expected.clone()
            }]
        );
        assert_eq!(textures.size, DEFAULT_TILE_SIZE);
        assert_eq!(textures.layer_count, 2);
        assert_eq!(layer1(&textures), textures.layer(TextureLayer(0)).unwrap());
    }

    #[test]
    fn a_non_square_texture_is_rejected() {
        let png = encode(
            4,
            2,
            ColorType::Rgba,
            BitDepth::Eight,
            None,
            None,
            &repeat(&[0; 4], 8),
        );
        assert_rejected(
            &png,
            &TextureProblem::NotSquare {
                width: 4,
                height: 2,
            },
        );
    }

    #[test]
    fn a_non_power_of_two_texture_is_rejected() {
        let png = encode_8bit(3, ColorType::Rgba, &repeat(&[0; 4], 9));
        assert_rejected(&png, &TextureProblem::NotPowerOfTwo { size: 3 });
    }

    #[test]
    fn an_oversized_texture_is_rejected() {
        let size = MAX_TILE_SIZE * 2;
        let png = encode_8bit(size, ColorType::Grayscale, &repeat(&[0], size * size));
        assert_rejected(&png, &TextureProblem::TooLarge { size });
    }

    #[test]
    fn the_largest_allowed_size_is_accepted() {
        let png = encode_8bit(
            MAX_TILE_SIZE,
            ColorType::Grayscale,
            &repeat(&[0], MAX_TILE_SIZE * MAX_TILE_SIZE),
        );
        let (textures, warnings) = build_one("a.png", &png);
        assert!(warnings.is_empty());
        assert_eq!(textures.size, MAX_TILE_SIZE);
    }

    #[test]
    fn garbage_is_undecodable() {
        let (textures, warnings) = build_one("bad.png", b"not a png at all");
        assert_eq!(warnings.len(), 1);
        assert!(matches!(
            warnings[0].problem,
            TextureProblem::Undecodable(_)
        ));
        assert_eq!(textures.layer_count, 2);
    }

    #[test]
    fn a_missing_file_uses_layer_zero_and_warns() {
        let pack = TempPack::new();
        let (textures, warnings) = build_textures(&pack.stack(), &layer_ids(&["gone.png"]));
        assert_eq!(
            warnings,
            vec![Warning {
                texture: id("gone.png"),
                problem: TextureProblem::Missing
            }]
        );
        assert_eq!(layer1(&textures), textures.layer(TextureLayer(0)).unwrap());
        let text = warnings[0].to_string();
        assert!(text.contains("base:textures/block/gone.png"), "{text}");
    }

    #[test]
    fn a_failed_texture_does_not_shrink_the_tile_size_of_the_others() {
        let pack = TempPack::new();
        pack.write(
            "good.png",
            &encode_8bit(32, ColorType::Rgba, &repeat(&[1, 2, 3, 255], 32 * 32)),
        );
        let ids = layer_ids(&["gone.png", "good.png"]);
        let (textures, warnings) = build_textures(&pack.stack(), &ids);
        assert_eq!(warnings.len(), 1);
        assert_eq!(textures.size, 32);
        assert_eq!(textures.layer_count, 3);
        assert_eq!(textures.pixels.len(), 3 * 32 * 32 * BYTES_PER_PIXEL);
    }

    #[test]
    fn layer_zero_is_a_magenta_and_black_checker_with_magenta_top_left() {
        let (textures, _) = build_textures(&PackStack::new(), &layer_ids(&[]));
        assert_eq!(textures.layer_count, 1);
        let size = side(textures.size);
        let layer = textures.layer(TextureLayer(0)).unwrap();
        assert_eq!(layer.len(), size * size * BYTES_PER_PIXEL);
        let at = |x: usize, y: usize| &layer[(y * size + x) * 4..(y * size + x) * 4 + 4];
        let half = size / 2;
        assert_eq!(at(0, 0), MISSING_COLOR_A);
        assert_eq!(at(half - 1, half - 1), MISSING_COLOR_A);
        assert_eq!(at(half, 0), MISSING_COLOR_B);
        assert_eq!(at(0, half), MISSING_COLOR_B);
        assert_eq!(at(half, half), MISSING_COLOR_A);
        assert_eq!(at(size - 1, size - 1), MISSING_COLOR_A);
    }

    #[test]
    fn layer_accessor_stops_at_the_last_layer() {
        let (textures, _) = build_textures(&PackStack::new(), &layer_ids(&[]));
        assert!(textures.layer(TextureLayer(1)).is_none());
    }

    #[test]
    fn the_highest_pack_provides_the_texture() {
        let low = TempPack::new();
        let high = TempPack::new();
        low.write(
            "a.png",
            &encode_8bit(2, ColorType::Rgba, &repeat(&[1, 1, 1, 255], 4)),
        );
        high.write(
            "a.png",
            &encode_8bit(2, ColorType::Rgba, &repeat(&[2, 2, 2, 255], 4)),
        );
        let mut stack = PackStack::new();
        stack.push(DirPack::new(&low.0));
        stack.push(DirPack::new(&high.0));
        let (textures, _) = build_textures(&stack, &layer_ids(&["a.png"]));
        assert_eq!(layer1(&textures), repeat(&[2, 2, 2, 255], 4));
    }

    /// The pack stack of the checked-in base assets.
    fn base_stack() -> PackStack {
        let assets = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("mods")
            .join("base")
            .join("assets");
        let mut stack = PackStack::new();
        stack.push(DirPack::new(assets));
        stack
    }

    #[test]
    fn the_base_assets_load_without_warnings() {
        let (registry, textures, warnings) = load_content(&base_stack()).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        // Missing, grass top, grass side, dirt, stone, sand, cobblestone, planks, log side,
        // log top.
        assert_eq!(textures.layer_count, 10);
        assert_eq!(textures.size, DEFAULT_TILE_SIZE);
        let layers = usize::try_from(textures.layer_count).unwrap();
        assert_eq!(
            textures.pixels.len(),
            layers * side(textures.size) * side(textures.size) * BYTES_PER_PIXEL
        );
        // Air, grass, dirt, stone, sand, cobblestone, planks, log.
        assert_eq!(registry.len(), 8);
    }

    #[test]
    fn load_content_is_the_single_file_case_of_load_content_from() {
        let files = [blocks::base_blocks_file().unwrap()];
        let (registry_a, textures_a, _) = load_content(&base_stack()).unwrap();
        let (registry_b, textures_b, _) = load_content_from(&base_stack(), &files).unwrap();
        // BlockTextures compares size, layer count and every pixel.
        assert_eq!(textures_a, textures_b);
        assert!(!textures_a.pixels.is_empty());
        for name in ["base:grass", "base:dirt", "base:log"] {
            let (a, b) = (registry_a.id(name).unwrap(), registry_b.id(name).unwrap());
            for face in vox_world::Face::ALL {
                assert_eq!(
                    registry_a.face_textures().get(a, face),
                    registry_b.face_textures().get(b, face)
                );
            }
        }
        assert_eq!(registry_a.len(), registry_b.len());
        assert_eq!(registry_a.id("base:stone"), registry_b.id("base:stone"));
    }

    #[test]
    fn base_textures_are_distinct_and_opaque() {
        let (_, textures, _) = load_content(&base_stack()).unwrap();
        let layers: Vec<&[u8]> = (0..textures.layer_count)
            .map(|i| {
                textures
                    .layer(TextureLayer(u16::try_from(i).unwrap()))
                    .unwrap()
            })
            .collect();
        for (i, a) in layers.iter().enumerate() {
            assert!(a.chunks_exact(4).all(|p| p[3] == 255), "layer {i}");
            for b in &layers[i + 1..] {
                assert_ne!(a, b);
            }
        }
    }

    /// CRC-32 (IEEE, as PNG uses), bitwise: tests only, to craft chunks by hand.
    fn crc32(bytes: &[u8]) -> u32 {
        let mut crc = u32::MAX;
        for byte in bytes {
            crc ^= u32::from(*byte);
            for _ in 0..8 {
                crc = if crc & 1 == 1 {
                    (crc >> 1) ^ 0xEDB8_8320
                } else {
                    crc >> 1
                };
            }
        }
        !crc
    }

    /// Adler-32, the checksum a zlib stream ends with.
    fn adler32(bytes: &[u8]) -> u32 {
        let (mut a, mut b) = (1u32, 0u32);
        for byte in bytes {
            a = (a + u32::from(*byte)) % 65521;
            b = (b + a) % 65521;
        }
        (b << 16) | a
    }

    /// A zlib stream holding `data` in one stored (uncompressed) block.
    fn zlib_stored(data: &[u8]) -> Vec<u8> {
        let len = u16::try_from(data.len()).unwrap();
        let mut out = vec![0x78, 0x01, 0x01];
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&(!len).to_le_bytes());
        out.extend_from_slice(data);
        out.extend_from_slice(&adler32(data).to_be_bytes());
        out
    }

    fn chunk(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
        let mut out = u32::try_from(data.len()).unwrap().to_be_bytes().to_vec();
        out.extend_from_slice(kind);
        out.extend_from_slice(data);
        let mut checked = kind.to_vec();
        checked.extend_from_slice(data);
        out.extend_from_slice(&crc32(&checked).to_be_bytes());
        out
    }

    /// A PNG written by hand: header for `width` x `height`, 8-bit `color_code`, `interlace`
    /// method, and `raw` filtered scanlines as the only image data.
    fn crafted(width: u32, height: u32, color_code: u8, interlace: u8, raw: &[u8]) -> Vec<u8> {
        let mut header = width.to_be_bytes().to_vec();
        header.extend_from_slice(&height.to_be_bytes());
        header.extend_from_slice(&[8, color_code, 0, 0, interlace]);
        let mut out = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
        out.extend(chunk(b"IHDR", &header));
        out.extend(chunk(b"IDAT", &zlib_stored(raw)));
        out.extend(chunk(b"IEND", &[]));
        out
    }

    #[test]
    fn a_huge_header_with_tiny_data_is_rejected_from_the_header() {
        // 65536 x 65536 RGBA would be 16 GiB; the data is a few bytes. The tile check runs
        // before any pixel buffer exists, so this returns at once.
        let png = crafted(65536, 65536, 6, 0, &[0, 1, 2, 3, 4]);
        assert_rejected(&png, &TextureProblem::TooLarge { size: 65536 });
        let png = crafted(100_000, 100_000, 6, 0, &[0]);
        assert_rejected(&png, &TextureProblem::NotPowerOfTwo { size: 100_000 });
    }

    #[test]
    fn an_interlaced_png_decodes() {
        // 2x2 gray, Adam7: pass 1 holds (0,0), pass 6 holds (1,0), pass 7 the second row.
        // Every pass has a filter byte (0 = none) per row.
        let raw = [0, 10, 0, 20, 0, 30, 40];
        let png = crafted(2, 2, 0, 1, &raw);
        let (textures, warnings) = build_one("a.png", &png);
        assert!(warnings.is_empty(), "{warnings:?}");
        let px = |v: u8| [v, v, v, 255];
        assert_eq!(
            layer1(&textures),
            [px(10), px(20), px(30), px(40)].concat().as_slice()
        );
    }

    #[test]
    fn a_truncated_png_is_undecodable() {
        // Noise does not compress, so the data stream is long and the cut lands inside it.
        let size = 16;
        let noise: Vec<u8> = (0..size * size * 4)
            .map(|i| u8::try_from(i * 37 % 251).unwrap())
            .collect();
        let png = encode_8bit(size, ColorType::Rgba, &noise);
        let cut = &png[..png.len() * 6 / 10];
        let (textures, warnings) = build_one("a.png", cut);
        assert_eq!(warnings.len(), 1);
        assert!(
            matches!(warnings[0].problem, TextureProblem::Undecodable(_)),
            "{warnings:?}"
        );
        assert_eq!(layer1(&textures), textures.layer(TextureLayer(0)).unwrap());
    }

    #[test]
    fn a_file_over_the_byte_limit_is_refused_before_decoding() {
        let bytes = vec![0; MAX_TEXTURE_FILE_BYTES + 1];
        let (_, warnings) = build_one("a.png", &bytes);
        assert_eq!(
            warnings[0].problem,
            TextureProblem::FileTooLarge {
                bytes: MAX_TEXTURE_FILE_BYTES + 1
            }
        );
    }

    #[test]
    fn transparency_chunk_on_gray_becomes_alpha() {
        // tRNS of gray 8-bit is one 16-bit sample: pixels equal to it are transparent.
        let png = encode(
            2,
            2,
            ColorType::Grayscale,
            BitDepth::Eight,
            None,
            Some(vec![0, 5]),
            &[5, 6, 6, 5],
        );
        let (textures, _) = build_one("a.png", &png);
        let clear = [5, 5, 5, 0];
        let solid = [6, 6, 6, 255];
        assert_eq!(
            layer1(&textures),
            [clear, solid, solid, clear].concat().as_slice()
        );
    }

    #[test]
    fn transparency_chunk_on_rgb_becomes_alpha() {
        let png = encode(
            2,
            2,
            ColorType::Rgb,
            BitDepth::Eight,
            None,
            Some(vec![0, 1, 0, 2, 0, 3]),
            &[1, 2, 3, 9, 9, 9, 9, 9, 9, 1, 2, 3],
        );
        let (textures, _) = build_one("a.png", &png);
        let clear = [1, 2, 3, 0];
        let solid = [9, 9, 9, 255];
        assert_eq!(
            layer1(&textures),
            [clear, solid, solid, clear].concat().as_slice()
        );
    }

    #[test]
    fn sixteen_bit_rgba_keeps_the_high_bytes_and_stays_in_the_decode_limit() {
        // The largest accepted picture at the widest sample size: must not hit the limit.
        let size = MAX_TILE_SIZE;
        let pixel = [0x11, 0xFF, 0x22, 0xFF, 0x33, 0xFF, 0x44, 0xFF];
        let png = encode(
            size,
            size,
            ColorType::Rgba,
            BitDepth::Sixteen,
            None,
            None,
            &repeat(&pixel, size * size),
        );
        let (textures, warnings) = build_one("a.png", &png);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(&layer1(&textures)[..4], [0x11, 0x22, 0x33, 0x44]);
    }

    #[test]
    fn a_directory_in_place_of_the_file_is_unreadable() {
        let pack = TempPack::new();
        fs::create_dir(pack.0.join(BLOCK_DIR).join("a.png")).unwrap();
        let (_, warnings) = build_textures(&pack.stack(), &layer_ids(&["a.png"]));
        assert_eq!(warnings.len(), 1);
        assert!(
            matches!(warnings[0].problem, TextureProblem::Unreadable(_)),
            "{warnings:?}"
        );
    }

    /// Checks that `layer` is the missing picture of `size`: a 2x2 checker, magenta in the
    /// top-left square.
    fn assert_checker(layer: &[u8], size: usize) {
        assert_eq!(layer.len(), size * size * BYTES_PER_PIXEL);
        let cell = size / 2;
        for y in 0..size {
            for x in 0..size {
                let odd = (x / cell + y / cell) % 2 == 1;
                let expected = if odd {
                    MISSING_COLOR_B
                } else {
                    MISSING_COLOR_A
                };
                let start = (y * size + x) * BYTES_PER_PIXEL;
                assert_eq!(&layer[start..start + 4], expected, "pixel {x},{y}");
            }
        }
    }

    #[test]
    fn the_missing_picture_is_a_checker_at_any_tile_size() {
        for size in [2u32, 16, 32] {
            let png = encode_8bit(size, ColorType::Rgba, &repeat(&[1, 2, 3, 255], size * size));
            let (textures, _) = build_one("a.png", &png);
            assert_eq!(textures.size, size);
            assert_checker(textures.layer(TextureLayer(0)).unwrap(), side(size));
        }
    }

    #[test]
    fn more_textures_than_layers_get_no_layer_and_a_warning() {
        let names: Vec<String> = (0..MAX_LAYERS + 2).map(|i| format!("t{i}.png")).collect();
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let ids = layer_ids(&refs);
        assert_eq!(ids.len(), MAX_LAYERS + 3);
        let (textures, warnings) = build_textures(&PackStack::new(), &ids);
        assert_eq!(textures.layer_count as usize, MAX_LAYERS);
        let dropped = warnings
            .iter()
            .filter(|w| w.problem == TextureProblem::NoLayerLeft)
            .count();
        assert_eq!(dropped, 3);
    }

    #[test]
    fn entry_zero_is_ignored_and_an_empty_list_gives_the_missing_layer() {
        let (textures, warnings) = build_textures(&PackStack::new(), &[]);
        assert!(warnings.is_empty());
        assert_eq!(textures.layer_count, 1);
        assert_checker(
            textures.layer(TextureLayer(0)).unwrap(),
            side(DEFAULT_TILE_SIZE),
        );
        // A real file id in slot 0 is neither read nor warned about.
        let (textures, warnings) = build_textures(&PackStack::new(), &[id("whatever.png")]);
        assert!(warnings.is_empty());
        assert_eq!(textures.layer_count, 1);
    }
}
