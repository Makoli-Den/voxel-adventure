//! The overlay: colored, semi-transparent rectangles in window pixels, drawn over the scene.
//!
//! The renderer knows nothing about text; the game turns text (or anything else) into
//! rectangles. This module holds the public [`OverlayRect`] and turns a batch of them into
//! the vertex bytes described in `crate::interface::overlay`. It is pure, so it is unit
//! tested without a GPU. See `docs/decisions/0009-overlay-rectangles.md`.

use crate::RenderError;
use crate::interface::overlay::{PUSH_CONSTANT_SIZE, VERTEX_STRIDE};

/// One rectangle of the overlay, in window pixels measured from the top-left corner of
/// the window's drawing area.
///
/// The pixels are those of the current swapchain image, the image the renderer draws
/// into, whose size is read every frame; so a rectangle keeps its size in pixels when the
/// window is resized. Parts outside the image are cut off.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OverlayRect {
    /// Left edge, in pixels from the left side of the drawing area, growing to the right.
    pub x: f32,
    /// Top edge, in pixels from the top of the drawing area, growing downwards.
    pub y: f32,
    /// Width in pixels. A rectangle with a width of zero or less draws nothing.
    pub width: f32,
    /// Height in pixels. A rectangle with a height of zero or less draws nothing.
    pub height: f32,
    /// Red, green, blue and alpha, each from 0.0 to 1.0, not premultiplied.
    ///
    /// Alpha 1.0 is opaque, 0.0 invisible. Each channel is stored in 8 bits: values are
    /// clamped to 0.0..=1.0, rounded to the nearest of 256 steps, and NaN counts as 0.0.
    pub color: [f32; 4],
}

/// Vertices per rectangle: two triangles of three corners, without an index buffer.
pub(crate) const VERTICES_PER_RECT: usize = 6;

/// The 8-bit value that stands for 1.0 in a normalized color channel.
const UNORM8_MAX: f32 = u8::MAX as f32;

/// Why a batch whose vertices a `u32` cannot count is refused.
const TOO_MANY_RECTS: &str = "too many rectangles";

/// Why a batch whose vertices cannot be allocated is refused.
const OUT_OF_MEMORY: &str = "the rectangles do not fit in memory";

/// A batch of rectangles serialized for the GPU.
#[derive(Debug, PartialEq)]
pub(crate) struct OverlayVertices {
    /// The vertices, laid out as `crate::interface::overlay` describes.
    pub(crate) bytes: Vec<u8>,
    /// How many vertices `bytes` holds: [`VERTICES_PER_RECT`] per drawn rectangle.
    pub(crate) count: u32,
}

/// Serializes `rects` in order. Rectangles with a width or height of zero or less are
/// skipped; they cover no pixel.
///
/// # Errors
/// [`RenderError::InvalidOverlay`] if a coordinate or size is not finite (NaN or
/// infinite), or the right or bottom edge overflows to infinity, or the slice has more
/// rectangles than a `u32` vertex count covers (checked before anything is allocated),
/// or its vertices do not fit in memory. The whole batch is refused, like a malformed mesh.
pub(crate) fn overlay_vertices(rects: &[OverlayRect]) -> Result<OverlayVertices, RenderError> {
    let capacity = byte_capacity(rects.len())?;
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(capacity)
        .map_err(|_| RenderError::InvalidOverlay(OUT_OF_MEMORY))?;
    let mut drawn: usize = 0;
    for rect in rects {
        if is_drawn(rect)? {
            write_rect(&mut bytes, rect);
            drawn += 1;
        }
    }
    // Cannot fail: `byte_capacity` proved that even every rectangle drawn fits a `u32`.
    let count = vertex_count(drawn)?;
    Ok(OverlayVertices { bytes, count })
}

/// Bytes the vertices of `rect_count` rectangles take if every one is drawn: an upper
/// bound, since skipped rectangles only lower it.
///
/// # Errors
/// [`RenderError::InvalidOverlay`] if that many rectangles have more vertices than a
/// `u32` counts (the draw call takes a `u32`), or the size overflows `usize`.
fn byte_capacity(rect_count: usize) -> Result<usize, RenderError> {
    let vertices = vertex_count(rect_count)?;
    usize::try_from(vertices)
        .ok()
        .and_then(|vertices| vertices.checked_mul(VERTEX_STRIDE as usize))
        .ok_or(RenderError::InvalidOverlay(TOO_MANY_RECTS))
}

/// The number of vertices of `rect_count` drawn rectangles, if a `u32` counts it.
fn vertex_count(rect_count: usize) -> Result<u32, RenderError> {
    rect_count
        .checked_mul(VERTICES_PER_RECT)
        .and_then(|count| u32::try_from(count).ok())
        .ok_or(RenderError::InvalidOverlay(TOO_MANY_RECTS))
}

/// Whether `rect` covers any area, or an error if one of its numbers is not finite.
fn is_drawn(rect: &OverlayRect) -> Result<bool, RenderError> {
    let right = rect.x + rect.width;
    let bottom = rect.y + rect.height;
    let numbers = [rect.x, rect.y, rect.width, rect.height, right, bottom];
    if !numbers.iter().all(|number| number.is_finite()) {
        return Err(RenderError::InvalidOverlay(
            "a rectangle has a coordinate or size that is not finite",
        ));
    }
    Ok(rect.width > 0.0 && rect.height > 0.0)
}

/// Appends the six vertices of `rect`: the triangles top-left, top-right, bottom-right
/// and top-left, bottom-right, bottom-left. The winding does not matter; the overlay
/// pipeline culls nothing.
fn write_rect(bytes: &mut Vec<u8>, rect: &OverlayRect) {
    let (left, top) = (rect.x, rect.y);
    let (right, bottom) = (rect.x + rect.width, rect.y + rect.height);
    let corners: [[f32; 2]; VERTICES_PER_RECT] = [
        [left, top],
        [right, top],
        [right, bottom],
        [left, top],
        [right, bottom],
        [left, bottom],
    ];
    let color = rect.color.map(unorm8);
    for [x, y] in corners {
        bytes.extend_from_slice(&x.to_le_bytes());
        bytes.extend_from_slice(&y.to_le_bytes());
        bytes.extend_from_slice(&color);
    }
}

/// One color channel as an 8-bit normalized value: clamped to 0.0..=1.0, scaled to 255
/// and rounded to the nearest integer, halves away from zero (0.5 becomes 128). NaN
/// becomes 0.
fn unorm8(channel: f32) -> u8 {
    if channel.is_nan() {
        return 0;
    }
    // Lossless: the clamped, rounded value lies in 0.0..=255.0.
    (channel.clamp(0.0, 1.0) * UNORM8_MAX).round() as u8
}

/// The overlay's push constants for an image of `width` by `height` pixels: the two
/// sides as little-endian `f32`, the `vec2` the vertex shader divides by.
// HOT PATH: runs once per frame while an overlay is shown; 8 bytes on the stack.
pub(crate) fn window_size_bytes(width: u32, height: u32) -> [u8; PUSH_CONSTANT_SIZE as usize] {
    let mut bytes = [0; PUSH_CONSTANT_SIZE as usize];
    // Exact: image sides are far below 2^24, where f32 starts to round integers.
    let (width, height) = (width as f32, height as f32);
    let (first, second) = bytes.split_at_mut(size_of::<f32>());
    first.copy_from_slice(&width.to_le_bytes());
    second.copy_from_slice(&height.to_le_bytes());
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: f32, y: f32, width: f32, height: f32) -> OverlayRect {
        OverlayRect {
            x,
            y,
            width,
            height,
            color: [1.0, 0.5, 0.0, 0.25],
        }
    }

    fn vertex(x: f32, y: f32, color: [u8; 4]) -> Vec<u8> {
        let mut bytes = x.to_le_bytes().to_vec();
        bytes.extend_from_slice(&y.to_le_bytes());
        bytes.extend_from_slice(&color);
        bytes
    }

    #[test]
    fn one_rectangle_is_six_vertices_of_twelve_bytes() {
        let vertices = overlay_vertices(&[rect(10.0, 20.0, 30.0, 40.0)]).unwrap();
        assert_eq!(vertices.count, 6);
        assert_eq!(vertices.bytes.len(), 6 * 12);
        let color = [255, 128, 0, 64];
        let expected: Vec<u8> = [
            (10.0, 20.0),
            (40.0, 20.0),
            (40.0, 60.0),
            (10.0, 20.0),
            (40.0, 60.0),
            (10.0, 60.0),
        ]
        .into_iter()
        .flat_map(|(x, y)| vertex(x, y, color))
        .collect();
        assert_eq!(vertices.bytes, expected);
    }

    #[test]
    fn positions_are_little_endian_f32() {
        let vertices = overlay_vertices(&[rect(1.0, 2.0, 1.0, 1.0)]).unwrap();
        // 1.0 is 0x3F80_0000 and 2.0 is 0x4000_0000; the low byte comes first.
        assert_eq!(vertices.bytes[..8], [0, 0, 0x80, 0x3F, 0, 0, 0, 0x40]);
    }

    #[test]
    fn rectangles_keep_their_order() {
        let first = rect(0.0, 0.0, 1.0, 1.0);
        let second = rect(5.0, 5.0, 1.0, 1.0);
        let both = overlay_vertices(&[first, second]).unwrap();
        let alone = overlay_vertices(&[second]).unwrap();
        assert_eq!(both.count, 12);
        assert_eq!(both.bytes[6 * 12..], alone.bytes[..]);
    }

    #[test]
    fn colors_are_clamped_and_rounded_to_the_nearest_step() {
        assert_eq!(unorm8(0.0), 0);
        assert_eq!(unorm8(1.0), 255);
        assert_eq!(unorm8(0.5), 128); // 127.5 rounds away from zero
        assert_eq!(unorm8(1.0 / 255.0), 1);
        assert_eq!(unorm8(0.4 / 255.0), 0);
        assert_eq!(unorm8(-3.0), 0);
        assert_eq!(unorm8(7.0), 255);
        assert_eq!(unorm8(f32::INFINITY), 255);
        assert_eq!(unorm8(f32::NEG_INFINITY), 0);
        assert_eq!(unorm8(f32::NAN), 0);
    }

    #[test]
    fn empty_rectangles_are_skipped() {
        let rects = [
            rect(0.0, 0.0, 0.0, 5.0),
            rect(0.0, 0.0, 5.0, 0.0),
            rect(0.0, 0.0, -5.0, 5.0),
            rect(0.0, 0.0, 5.0, -5.0),
            rect(3.0, 4.0, 1.0, 1.0),
        ];
        let vertices = overlay_vertices(&rects).unwrap();
        assert_eq!(vertices.count, 6);
        assert_eq!(vertices.bytes[..8], vertex(3.0, 4.0, [0; 4])[..8]);
    }

    #[test]
    fn no_rectangles_give_no_vertices() {
        let vertices = overlay_vertices(&[]).unwrap();
        assert_eq!(vertices.count, 0);
        assert!(vertices.bytes.is_empty());
        let only_empty = overlay_vertices(&[rect(1.0, 1.0, 0.0, 0.0)]).unwrap();
        assert_eq!(only_empty.count, 0);
        assert!(only_empty.bytes.is_empty());
    }

    #[test]
    fn a_tiny_rectangle_and_one_outside_the_window_are_kept() {
        let vertices =
            overlay_vertices(&[rect(0.0, 0.0, 1.0, 1.0), rect(-50.0, -50.0, 1e6, 10.0)]).unwrap();
        assert_eq!(vertices.count, 12);
    }

    #[test]
    fn non_finite_numbers_reject_the_whole_batch() {
        let good = rect(0.0, 0.0, 1.0, 1.0);
        for bad in [
            rect(f32::NAN, 0.0, 1.0, 1.0),
            rect(0.0, f32::INFINITY, 1.0, 1.0),
            rect(0.0, 0.0, f32::NAN, 1.0),
            rect(0.0, 0.0, 1.0, f32::NEG_INFINITY),
            // Finite on its own, but the right edge overflows to infinity.
            rect(f32::MAX, 0.0, f32::MAX, 1.0),
            // NaN width: checked before the "empty" test, so it is not silently skipped.
            rect(0.0, 0.0, f32::NAN, -1.0),
        ] {
            let result = overlay_vertices(&[good, bad]);
            assert!(
                matches!(result, Err(RenderError::InvalidOverlay(_))),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn the_vertex_limit_is_the_largest_count_a_u32_holds() {
        // u32::MAX / 6 rectangles have 4_294_967_292 vertices; one more is too many.
        let most = u32::MAX as usize / VERTICES_PER_RECT;
        assert_eq!(vertex_count(most).unwrap(), 4_294_967_292);
        assert!(matches!(
            vertex_count(most + 1),
            Err(RenderError::InvalidOverlay(TOO_MANY_RECTS))
        ));
        assert!(matches!(
            vertex_count(usize::MAX),
            Err(RenderError::InvalidOverlay(_))
        ));
    }

    #[test]
    fn the_byte_size_is_checked_before_anything_is_allocated() {
        assert_eq!(byte_capacity(0).unwrap(), 0);
        assert_eq!(byte_capacity(2).unwrap(), 2 * 6 * 12);
        let most = u32::MAX as usize / VERTICES_PER_RECT;
        assert!(matches!(
            byte_capacity(most + 1),
            Err(RenderError::InvalidOverlay(TOO_MANY_RECTS))
        ));
        assert!(matches!(
            byte_capacity(usize::MAX),
            Err(RenderError::InvalidOverlay(_))
        ));
    }

    #[test]
    fn a_nan_color_is_not_an_error() {
        let mut nan_color = rect(0.0, 0.0, 1.0, 1.0);
        nan_color.color = [f32::NAN; 4];
        let vertices = overlay_vertices(&[nan_color]).unwrap();
        assert_eq!(vertices.bytes[8..12], [0, 0, 0, 0]);
    }

    #[test]
    fn window_size_is_two_little_endian_floats() {
        let bytes = window_size_bytes(1280, 720);
        assert_eq!(bytes[..4], 1280.0_f32.to_le_bytes());
        assert_eq!(bytes[4..], 720.0_f32.to_le_bytes());
    }
}
