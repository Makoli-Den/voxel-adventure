//! A hard-coded demo scene: one small chunk and a fixed camera looking at it.
//!
//! Temporary. It exists only so the renderer has something real to draw until world
//! generation, a player camera and input exist; then this module is deleted.

use glam::Vec3;
use glam::camera::rh::{proj::vulkan, view};
use vox_world::{BlockId, Chunk, LocalPos};

const GRASS: BlockId = BlockId(1);
const DIRT: BlockId = BlockId(2);
const STONE: BlockId = BlockId(3);

/// Side length of the demo slab in blocks.
const SLAB_SIDE: usize = 12;
/// Height of the slab's grass layer; the stone lies below it.
const SLAB_TOP: usize = 5;

/// A 12x6x12 slab of stone with grass on top, plus a dirt staircase on it, so the
/// camera sees tops, sides and steps.
pub fn chunk() -> Chunk {
    let mut chunk = Chunk::new();
    for x in 0..SLAB_SIDE {
        for z in 0..SLAB_SIDE {
            for y in 0..SLAB_TOP {
                place(&mut chunk, x, y, z, STONE);
            }
            place(&mut chunk, x, SLAB_TOP, z, GRASS);
        }
    }
    // Four steps that get lower towards +X, facing the camera.
    for x in 0..4 {
        for z in 4..8 {
            for y in 1..=(4 - x) {
                place(&mut chunk, x, SLAB_TOP + y, z, DIRT);
            }
        }
    }
    chunk
}

/// Sets one block; the demo coordinates are all inside the chunk.
fn place(chunk: &mut Chunk, x: usize, y: usize, z: usize, block: BlockId) {
    if let Some(pos) = LocalPos::new(x, y, z) {
        chunk.set(pos, block);
    }
}

/// Where the demo camera stands: above the +X/+Z corner of the slab.
const EYE: Vec3 = Vec3::new(26.0, 20.0, 28.0);
/// The point the camera looks at, near the middle of the slab.
const TARGET: Vec3 = Vec3::new(6.0, 4.0, 6.0);

/// View-projection matrix for a window of `width` by `height` pixels, looking at the
/// slab from above its +X/+Z corner. Vulkan clip space: Y down, depth 0..1.
pub fn camera(width: u32, height: u32) -> [[f32; 4]; 4] {
    // A zero side only happens while minimized, when nothing is drawn anyway.
    let aspect = width.max(1) as f32 / height.max(1) as f32;
    // glam's Vulkan projection already flips Y and maps depth to 0..1.
    let projection = vulkan::perspective(60_f32.to_radians(), aspect, 0.1, 200.0);
    let view = view::look_at_mat4(EYE, TARGET, Vec3::Y);
    (projection * view).to_cols_array_2d()
}

#[cfg(test)]
mod tests {
    use glam::Mat4;

    use super::*;

    /// Normalized device coordinates of a world point seen through the demo camera.
    fn ndc(point: Vec3) -> Vec3 {
        let clip = Mat4::from_cols_array_2d(&camera(800, 600)) * point.extend(1.0);
        clip.truncate() / clip.w
    }

    #[test]
    fn target_is_centered_and_inside_the_depth_range() {
        let target = ndc(TARGET);
        assert!(target.x.abs() < 1e-4 && target.y.abs() < 1e-4);
        assert!((0.0..1.0).contains(&target.z));
    }

    #[test]
    fn world_up_points_up_on_screen() {
        // Vulkan's NDC Y points down, so "higher in the world" must be a smaller Y.
        assert!(ndc(TARGET + Vec3::Y).y < 0.0);
    }
}
