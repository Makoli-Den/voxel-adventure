//! Frame pacing: how often the game draws a frame.
//!
//! The renderer presents with MAILBOX when it can, which does not wait for the monitor's
//! refresh, so without a limit the GPU would draw thousands of identical frames per second.
//! The game therefore draws at a fixed multiple of the refresh rate. MAILBOX shows the newest
//! finished frame at each refresh, so drawing a little faster than the monitor keeps the
//! motion smooth without wasting power.

use std::time::Duration;

/// Frames drawn per monitor refresh.
const FRAMES_PER_REFRESH: f64 = 2.0;

/// Refresh rate assumed when the monitor does not report one, in millihertz (60 Hz).
const DEFAULT_REFRESH_MILLIHERTZ: u32 = 60_000;

/// Time between two frames for a monitor with the given refresh rate in millihertz,
/// as winit reports it. An unknown or zero rate counts as 60 Hz.
pub fn frame_interval(refresh_millihertz: Option<u32>) -> Duration {
    let millihertz = match refresh_millihertz {
        Some(rate) if rate > 0 => rate,
        _ => DEFAULT_REFRESH_MILLIHERTZ,
    };
    let frames_per_second = f64::from(millihertz) / 1000.0 * FRAMES_PER_REFRESH;
    Duration::from_secs_f64(1.0 / frames_per_second)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: Duration, b: Duration) -> bool {
        a.abs_diff(b) < Duration::from_micros(10)
    }

    #[test]
    fn sixty_hertz_gives_one_hundred_twenty_frames_per_second() {
        assert!(close(
            frame_interval(Some(60_000)),
            Duration::from_secs_f64(1.0 / 120.0)
        ));
    }

    #[test]
    fn faster_monitors_get_shorter_intervals() {
        assert!(frame_interval(Some(144_000)) < frame_interval(Some(60_000)));
    }

    #[test]
    fn unknown_rate_counts_as_sixty_hertz() {
        assert_eq!(frame_interval(None), frame_interval(Some(60_000)));
    }

    #[test]
    fn zero_rate_counts_as_sixty_hertz() {
        assert_eq!(frame_interval(Some(0)), frame_interval(Some(60_000)));
    }
}
