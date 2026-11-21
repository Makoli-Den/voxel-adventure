# 0002. Shape of the first Vulkan slice

Status: accepted.

## Context
The first vertical slice must open a window and draw with Vulkan, while keeping the rules from `CLAUDE.md`: rendering behind a narrow interface, `unsafe` isolated, old devices supported.

## Decision
- **Window:** `winit` 0.30. The renderer receives the window through `raw-window-handle` traits and never depends on `winit`.
- **Interface:** a small `Renderer` trait (`resize`, `render_frame`, `device_name`) created by `create_renderer`. The game only sees the trait.
- **Window lifetime:** the renderer holds an `Arc` of the window, so the native window outlives the Vulkan surface without any `unsafe` contract on the caller.
- **Vulkan baseline:** plain Vulkan 1.0, no extensions beyond the swapchain, so old GPUs keep working. Newer features are added later behind capability checks.
- **Presentation:** FIFO present mode only (vsync). It is the only mode every driver must support. Superseded: MAILBOX is preferred, because FIFO can deadlock during a window resize, see decision 0005.
- **Queues:** one queue family that supports both graphics and presentation, which all desktop GPUs offer.
- **Frames in flight:** two, with one "rendering finished" semaphore per swapchain image.
- **First frame:** the swapchain image is cleared with transfer commands, which needs no render pass or pipeline. Superseded: the next slice replaced this with a render pass and a triangle, see decision 0003.
- **Validation:** the Khronos validation layer is enabled automatically in debug builds when it is installed.

## Rejected
- `wgpu` or `vulkano`: see decision 0001.
- Dynamic rendering (Vulkan 1.3): conflicts with the old-device goal.
- Choosing the present mode by preference (mailbox): adds code before measurements show a need. Measurements later showed the need, see decision 0005.

## Risks
- A GPU that supports presentation only on a different queue family than graphics is skipped. Not seen on desktop hardware, can be handled when it matters.
- Very old systems without a Vulkan driver cannot run the game yet. A fallback backend behind the `Renderer` trait is the planned answer.
