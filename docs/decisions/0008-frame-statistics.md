# 0008. Frame statistics: draw counts and GPU time

Status: accepted.

## Context
Optimizing by measurement needs numbers from the running game: how much the renderer draws, and how long the GPU needs for it. The frame rate alone says little, because the game caps it (decision 0005): at the cap a frame that costs the GPU 0.5 ms and one that costs 4 ms look the same. The GPU time per frame is the number that says how close the GPU is to its limit. It does not depend on the frame limiter as long as the GPU has idle time between frames, which is the case with the limiter; when the GPU is the bottleneck the number drifts upwards (see the risks).

## Decision
- **Interface:** `Renderer::stats()` returns a `RenderStats` copy: `chunks_drawn` (renamed `meshes_drawn` in 0015 S5, it counts chunk and tile meshes), `draw_calls` and `triangles` of the most recently recorded frame, and `gpu_time: Option<Duration>` of the most recent frame whose timing has been read. No backend types appear in it.
- **Draw counts** are summed while the frame is recorded: one chunk and one draw call per mesh, `index_count / 3` triangles (`INDICES_PER_TRIANGLE`), with a saturating `u64` sum. A frame without meshes reports zeros; calls that record nothing (minimized window, swapchain rebuild, out-of-date image) keep the previous counts.
- **GPU time** uses timestamp queries, core Vulkan 1.0. They are used only if the queue family's `timestampValidBits` is non-zero and `limits.timestampPeriod` is positive; otherwise no query objects exist and `gpu_time` stays `None`. Each frame slot owns a TIMESTAMP query pool with two queries. The frame's command buffer resets that pool (outside the render pass), writes a TOP_OF_PIPE timestamp first and a BOTTOM_OF_PIPE timestamp last, after the render pass. A successful submit flags the slot as measured. When the next frame in that slot has waited on the slot's fence, which proves the measured frame finished, the two 64-bit results are read without the WAIT flag and the flag is cleared. The difference is masked to the valid bits (so counter wrap-around and undefined high bits do not matter) and multiplied by the period. That conversion is a pure function with unit tests (`vulkan/timestamps.rs`).
- The pools are part of the per-frame resources (`Frames`) and are destroyed with them after the GPU is idle.

## Reasons
- Two timestamps per frame cost next to nothing and need no device feature.
- Reading after the slot's fence never blocks: the fence the frame loop waits on anyway already proves the results are available. The time lags the counts by `FRAMES_IN_FLIGHT` frames, which does not matter for a statistic.
- Flagging a slot only after a successful submit means a slot that was never used, or a frame that failed before its submit, is never read.

## Rejected
- **Pipeline statistics queries** (vertex and fragment shader invocations, primitives): an optional device feature (`pipelineStatisticsQuery`), so not available everywhere; the draw counts the renderer already knows cover the basic need.
- **Reading results with `VK_QUERY_RESULT_WAIT_BIT`** right after the submit: stalls the CPU until the GPU finishes the frame and removes the overlap of frames in flight.
- **Timestamps around every draw:** two queries per chunk per frame, hundreds of pipeline-stage writes, and readback of all of them; too costly for a per-frame statistic. Can be added later as an explicit profiling mode.

## Risks
- `gpu_time` is an **upper bound** of the work, found by the independent review. The start timestamp is at TOP_OF_PIPE, but only the COLOR_ATTACHMENT_OUTPUT stage waits for the swapchain image, so the start can be written before the image is free and the wait is inside the span. When the GPU is the bottleneck, frame N+1 can start while frame N still runs and the spans overlap, so the figure drifts towards the frame period. To measure pure work, a second timestamp at COLOR_ATTACHMENT_OUTPUT could be compared (not done; a hypothesis until measured).
- A measurement that cannot be read keeps the previous value instead of reporting `None`, so a consumer that averages per frame counts the same value twice now and then.
- The span covers waits inside the frame: if the GPU is idle when the frame starts, the start timestamp is written at once, but the color output still waits for the swapchain image (the submit's wait semaphore), so that wait counts as GPU time.
- TOP_OF_PIPE to BOTTOM_OF_PIPE measures the span of the frame's commands, not presentation or compositing.
- The precision depends on the driver's `timestampPeriod`; some drivers report a rounded period, and the counter may tick in steps coarser than a nanosecond.
- The query usage is unchecked by the Khronos validation layer, which is not installed on the development machine; the valid usage was reasoned through by hand.
