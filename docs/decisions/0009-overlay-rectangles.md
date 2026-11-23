# 0009. The overlay: a batch of colored rectangles

Status: accepted.

## Context
The game needs an on-screen metrics panel: text and bars over the 3D scene. The renderer has one pipeline for chunk meshes, no textures and no descriptor sets (decisions 0004, 0007). The overlay must stay simple, keep the renderer free of font code, run on plain Vulkan 1.0 and cost next to nothing per frame.

## Decision
- **A rectangle batch.** `Renderer::set_overlay(&[OverlayRect])` replaces the overlay; an empty slice removes it. An `OverlayRect` is `x`, `y`, `width`, `height` in window pixels from the top-left corner of the drawing area, and a non-premultiplied RGBA color in `0.0..=1.0`. The rectangles are drawn after all chunk meshes, in slice order, blended by their alpha. The game turns text (a pixel font) into rectangles; the renderer knows nothing about fonts.
- **Vertex format:** six vertices per rectangle (two triangles, no index buffer), 12 bytes each, little-endian: position as `R32G32_SFLOAT` in pixels (location 0, `vec2`), color as `R8G8B8A8_UNORM` (location 1, `vec4`). Both are required vertex formats in Vulkan 1.0. Colors are clamped to `0..=1`, scaled by 255 and rounded half away from zero (0.5 becomes 128); NaN becomes 0. Serialization is a pure function with tests (`vox-render/src/overlay.rs`).
- **Bad input:** a non-finite coordinate or size (or an edge that overflows to infinity) refuses the whole batch with `RenderError::InvalidOverlay`, and the previous overlay stays, like `InvalidMesh` for a mesh. A rectangle with a width or height of zero or less is skipped silently: it is valid data that covers no pixel (a progress bar at 0 %). A batch is limited to `u32::MAX / 6` rectangles, because the single draw takes a `u32` vertex count; the limit is checked on the slice length before anything is allocated (an upper bound, since skipped rectangles only lower the count), and a larger batch, or one whose vertices cannot be allocated, is refused with `InvalidOverlay` as well.
- **Pixels to clip space:** one push-constant block, vertex stage, 8 bytes: the swapchain image size in pixels as `vec2`, pushed every frame from the current extent. The vertex shader computes `clip = pixel / size * 2 - 1` on both axes. Vulkan clip space has Y down like pixels, so nothing is flipped. Resizing the window needs no rebuild, and a rectangle keeps its pixel size.
- **Pipeline:** a second graphics pipeline with its own layout, owned like the first (`Pipeline` with its own `Drop`): triangle list, no culling, no depth test or depth write (the render pass keeps its depth attachment; the pipeline ignores it), dynamic viewport and scissor, and alpha blending. Color: `src * src_alpha + dst * (1 - src_alpha)`. Alpha: `src_alpha * 1 + dst_alpha * (1 - src_alpha)`, Porter-Duff "over" for coverage, which keeps the image alpha at exactly 1 over the opaque sky and so matches the opaque window composition. Blending is enabled only if the swapchain format has `COLOR_ATTACHMENT_BLEND` with optimal tiling (Vulkan 1.0 guarantees it for the 8-bit formats the renderer prefers). Only optimal tiling counts: pipeline creation looks at both tilings, but each draw needs the feature on the image view it renders to (`VUID-vkCmdDraw-blendEnable-04727`), and swapchain images use optimal tiling. Without it the overlay is drawn opaque, alpha ignored, instead of creating an invalid pipeline.
- **One buffer, one draw:** the vertices of all rectangles live in one host-visible buffer (the existing `Buffer`), created by `set_overlay`, drawn with one `vkCmdDraw` of `6 * n` vertices inside the same render pass and command buffer as the meshes. The overlay is not counted in `RenderStats` (its GPU time is part of `gpu_time`).
- **Deferred freeing:** a replaced or removed overlay's buffer goes to the same retired list as chunk mesh buffers (decision 0007), which now holds `Buffer`s; the same fence proofs free it. No `vkDeviceWaitIdle`.
- **Per-pipeline interface descriptions:** `vox-render/src/interface/` holds one `PipelineInterface` per pipeline (vertex stride, attribute locations and formats, push-constant size and stage). It is the single source of truth for that pipeline's Vulkan vertex input state, its push-constant range and the shader check (decision 0003), so the chunk and overlay shaders are each checked against their own pipeline.

## Reasons
- Rectangles need no textures, samplers or descriptor sets, so the overlay adds one small pipeline and nothing else; text stays in the game, where the font lives.
- One buffer and one draw keep the cost flat however many rectangles there are, and updating a few times per second is one small allocation plus a deferred free.
- Pushing the size per frame is free and makes resizing correct without any bookkeeping.

## Rejected
- **A font atlas texture with descriptor sets:** the usual way to draw text, but it brings images, samplers, descriptor sets and an upload path; much more machinery than a metrics panel needs today.
- **A dear-imgui style immediate-mode renderer:** a large dependency for a few lines of text and bars.
- **Drawing text in the fragment shader:** couples the renderer to one font and its layout rules.

## Risks
- Large text means many rectangles (one per lit pixel run). The game merges runs on a row; if the count grows, a texture atlas is the next step.
- No clipping or scissor per rectangle; parts outside the window are cut off by the viewport only.
- The buffer is host-visible memory with its own allocation, like the meshes.
- No validation layer is installed on the development machine; the valid usage of the blend state, the second layout, the push and the draw was reasoned through by hand.
