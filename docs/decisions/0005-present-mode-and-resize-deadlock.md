# 0005. MAILBOX presentation, because FIFO can deadlock while the window is resized

Status: accepted.

## Context
Decision 0002 chose the FIFO present mode, the only one every driver must support. While testing camera controls on Windows, the game was found to hang for good when its window was resized in quick succession from another process (as window managers, snapping and scripts do): Windows reports "not responding", and the log stops inside the present call.

## What was measured
The test starts the game, changes the window size from another process, and asks whether the game still responds. "Slow" means 4 sizes, 1.2 s apart; "rapid" means 20 random sizes, 100 ms apart. All numbers come from one machine (Windows 11, RTX 4070 SUPER, debug builds), so they show a tendency, not a statistic.

| Present mode | Test | Runs that hung |
|---|---|---|
| FIFO (what `main` had before this change) | slow | 2 of 8 |
| FIFO, a later build | slow | 0 of 12 |
| FIFO | rapid | 3 of 10, and 1 of 12 in another series |
| FIFO with frame pacing (a sleeping loop instead of a spinning one) | rapid | 2 of 20 |
| IMMEDIATE | slow | 0 of 14 |
| MAILBOX | rapid | 0 of 14, 0 of 16 with frame pacing, and 0 of 16 on the combined branch |

Summed up, rapid resizing hung in 6 of 42 runs with FIFO and in 0 of 46 runs with MAILBOX. If the hang rate were really 14 percent with MAILBOX too, 46 clean runs in a row would happen about once in a thousand tries. The FIFO row with frame pacing rules out one alternative explanation: the earlier FIFO runs used a spinning loop, the MAILBOX runs a sleeping one, but FIFO hangs with the sleeping loop too. The IMMEDIATE row was measured only with the slow test, where FIFO also hung 0 of 12 on the later build, so it points the same way but proves little.

Once hung, the game never recovered: a 30-second wait produced no output at all.

## Explanation (a model, not a proof)
During a resize the compositor may stop taking new frames until the window thread has handled the size message. With FIFO the present call can wait for the compositor. The window thread is the one that calls present, so it never gets to the size message: both wait for each other. MAILBOX never queues a frame behind the compositor, so present returns at once and the loop reaches the message. There was no debugger on the machine, so the exact blocking call inside the driver was not confirmed.

## Decision
- Prefer **MAILBOX** when the surface offers it, otherwise FIFO. MAILBOX does not tear: the newest finished frame is shown at every refresh. IMMEDIATE was rejected because it tears.
- MAILBOX does not wait for the refresh, so the game caps its own frame rate (`pacing.rs`): twice the monitor's refresh rate, at most 240 per second, 60 Hz assumed when unknown. The loop sleeps between frames, which keeps the processor and GPU cool and lets window events through promptly. The timer is not exact: with a target of 330 frames per second (a 165 Hz monitor, before the cap of 240 existed) the game measured 299. The cap lowers the target on fast monitors; the measured shortfall at 240 was not re-measured.

## Rejected
- **FIFO with a check that the swapchain matches the window.** A stale-swapchain theory (the surface reports its new size late) was tested with logging, did not explain the hang, and was removed. Without the check the swapchain size settled in 12 of 12 runs.
- **IMMEDIATE:** visible tearing and a GPU running flat out.
- **A dedicated render thread** (so the window thread never blocks in present) would remove the cause for every present mode. It is the larger change and is kept for when measurements demand it.

## Risks
- On a driver that offers no MAILBOX the game falls back to FIFO and may still hang during an external resize. A render thread would fix that case.
- The measurements come from one machine and a synthetic resize. A resize by dragging the window border runs inside the window thread and was not tested.
- With MAILBOX the game draws up to twice per refresh, so some frames are never shown.
- The refresh rate is read when the window opens and when it moves; a monitor that changes its rate while the window stays put keeps the old cap until the next move.
