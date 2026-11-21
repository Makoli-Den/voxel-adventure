//! Frame metrics: average FPS and the "1% low" and "0.1% low" figures, worked out from
//! recorded frame times.
//!
//! The definitions are the ones capture tools use for the "average of the lows":
//! - **average FPS** is the number of frames divided by the time they took;
//! - **1% low** is the FPS over the slowest 1% of the frames: that many frames divided by
//!   the sum of their frame times;
//! - **0.1% low** is the same for the slowest 0.1% of the frames.
//!
//! The share is rounded up to a whole number of frames, at least one, so with fewer than 100
//! frames the 1% low is the single worst frame, and with fewer than 1000 frames so is the
//! 0.1% low. The lows say how often the game stutters, which the average hides.
//!
//! Three times are recorded per frame:
//! - the **interval** since the previous frame started (what the player sees, capped by the
//!   frame limiter). It gives the FPS figures. Every stall counts, because the player sees it:
//!   dragging or resizing the window, which blocks the loop, and the rebuild of the swapchain
//!   after a resize. Only the pause of a minimized window is left out;
//! - the **work** time the game spent producing the frame: moving the camera and the renderer's
//!   `render_frame`, which includes waiting for the GPU. Updating the window title is not
//!   part of it, so that cost lands in the next frame's interval;
//! - the **GPU** time, when the renderer can measure it. It is an upper bound (see
//!   `RenderStats`), and it is the latest known value for each frame: a frame whose own
//!   measurement was not ready repeats the previous one.
//!
//! Work and GPU time say how much room is left below the frame limiter.

use std::collections::VecDeque;
use std::fmt;
use std::time::Duration;

/// The "1% low" looks at the slowest frame out of this many.
const ONE_PERCENT_DENOMINATOR: usize = 100;
/// The "0.1% low" looks at the slowest frame out of this many.
const ONE_PER_MILLE_DENOMINATOR: usize = 1000;
/// The label of the 1% low in the printed figures.
const LOW_1_PERCENT_LABEL: &str = "1% low";
/// The label of the 0.1% low in the printed figures.
const LOW_0_1_PERCENT_LABEL: &str = "0.1% low";
/// Milliseconds in a second, for printing times. They are all shown in milliseconds, so the
/// numbers can be compared at a glance and the text stays plain ASCII.
const MILLIS_PER_SECOND: f64 = 1000.0;
/// How much recent time the live figures cover.
const LIVE_WINDOW: Duration = Duration::from_secs(10);
/// How many frames the summary of the whole run keeps: about half an hour at 300 frames per
/// second. A `Sample` is 12 bytes, so this is about 7 MB, up to twice that while the buffer
/// grows. A longer run keeps its most recent frames.
const MAX_RUN_FRAMES: usize = 600_000;
/// How often the live figures are refreshed.
pub const REFRESH_INTERVAL: Duration = Duration::from_millis(500);

/// One recorded frame. Stored as `f32` seconds to keep a long run small: a frame time is
/// far below the precision limit, and the sums are done in `f64`.
#[derive(Clone, Copy, Debug)]
struct Sample {
    interval: f32,
    work: f32,
    /// `NAN` when the GPU time is unknown.
    gpu: f32,
}

/// How long a [`FrameHistory`] remembers frames.
#[derive(Clone, Copy, Debug)]
enum Retention {
    /// Frames of the last `Duration` of play time.
    Window(Duration),
    /// The last this many frames.
    Frames(usize),
}

/// The recent frames, from which [`Summary`] is worked out.
#[derive(Debug)]
pub struct FrameHistory {
    samples: VecDeque<Sample>,
    retention: Retention,
    /// Sum of the intervals of the frames in `samples`, in seconds.
    covered: f64,
}

impl FrameHistory {
    /// A history of the last ten seconds, for the live figures in the window title.
    pub fn live() -> Self {
        Self::new(Retention::Window(LIVE_WINDOW))
    }

    /// A history of the whole run (up to [`MAX_RUN_FRAMES`]), for the summary at exit.
    pub fn whole_run() -> Self {
        Self::new(Retention::Frames(MAX_RUN_FRAMES))
    }

    fn new(retention: Retention) -> Self {
        Self {
            samples: VecDeque::new(),
            retention,
            covered: 0.0,
        }
    }

    /// Records a frame. A frame with a zero interval is ignored, since no FPS can be
    /// worked out from it.
    pub fn push(&mut self, interval: Duration, work: Duration, gpu: Option<Duration>) {
        if interval.is_zero() {
            return;
        }
        self.samples.push_back(Sample {
            interval: interval.as_secs_f32(),
            work: work.as_secs_f32(),
            gpu: gpu.map_or(f32::NAN, |time| time.as_secs_f32()),
        });
        self.covered += f64::from(interval.as_secs_f32());
        self.forget_old_frames();
    }

    /// Drops the oldest frames that no longer fit the retention, but never the newest one.
    fn forget_old_frames(&mut self) {
        while self.samples.len() > 1 && self.exceeds_retention() {
            if let Some(oldest) = self.samples.pop_front() {
                self.covered -= f64::from(oldest.interval);
            }
        }
    }

    fn exceeds_retention(&self) -> bool {
        match self.retention {
            Retention::Window(window) => self.covered > window.as_secs_f64(),
            Retention::Frames(limit) => self.samples.len() > limit,
        }
    }

    /// The figures over the remembered frames, or `None` if there are none yet.
    ///
    /// Sorts a copy of the frame times, which for the live window (a few thousand frames)
    /// takes well under a millisecond; it is meant to run a couple of times per second.
    pub fn summary(&self) -> Option<Summary> {
        let frames = self.samples.len();
        if frames == 0 {
            return None;
        }
        let mut slowest_first: Vec<f64> = self
            .samples
            .iter()
            .map(|sample| f64::from(sample.interval))
            .collect();
        slowest_first.sort_unstable_by(|a, b| b.total_cmp(a));

        let covered: f64 = slowest_first.iter().sum();
        Some(Summary {
            frames,
            covered: Duration::from_secs_f64(covered),
            average_fps: frames as f64 / covered,
            low_1_percent: low_fps(&slowest_first, ONE_PERCENT_DENOMINATOR),
            low_0_1_percent: low_fps(&slowest_first, ONE_PER_MILLE_DENOMINATOR),
            worst_interval: Duration::from_secs_f64(slowest_first.first().copied().unwrap_or(0.0)),
            average_work: Duration::from_secs_f64(
                self.average(|sample| Some(sample.work)).unwrap_or(0.0),
            ),
            worst_work: Duration::from_secs_f64(self.worst_work()),
            average_gpu: self
                .average(|sample| (!sample.gpu.is_nan()).then_some(sample.gpu))
                .map(Duration::from_secs_f64),
        })
    }

    /// The mean of `value` over the frames where it is `Some`, in seconds.
    fn average(&self, value: impl Fn(&Sample) -> Option<f32>) -> Option<f64> {
        let (sum, count) = self
            .samples
            .iter()
            .filter_map(value)
            .fold((0.0_f64, 0_usize), |(sum, count), v| {
                (sum + f64::from(v), count + 1)
            });
        (count > 0).then(|| sum / count as f64)
    }

    /// The longest work time among the remembered frames, in seconds.
    fn worst_work(&self) -> f64 {
        self.samples
            .iter()
            .map(|sample| f64::from(sample.work))
            .fold(0.0, f64::max)
    }
}

/// The FPS over the slowest one frame in `denominator`, given all the frame times in seconds,
/// slowest first. The count is rounded up with integer arithmetic, so it is exact, and is at
/// least one frame.
fn low_fps(slowest_first: &[f64], denominator: usize) -> f64 {
    let frames = slowest_first.len();
    let count = frames.div_ceil(denominator).clamp(1, frames.max(1));
    let time: f64 = slowest_first.iter().take(count).sum();
    count as f64 / time
}

/// The figures worked out from a [`FrameHistory`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Summary {
    /// Frames the figures are based on.
    pub frames: usize,
    /// Play time those frames cover.
    pub covered: Duration,
    /// Frames divided by the time they took.
    pub average_fps: f64,
    /// FPS over the slowest 1% of the frames.
    pub low_1_percent: f64,
    /// FPS over the slowest 0.1% of the frames.
    pub low_0_1_percent: f64,
    /// The longest interval between two frames.
    pub worst_interval: Duration,
    /// Mean time the game spent producing a frame, including waiting for the GPU.
    pub average_work: Duration,
    /// The longest time the game spent producing a frame.
    pub worst_work: Duration,
    /// Mean of the latest known GPU time of each frame (an upper bound), or `None` if the
    /// renderer cannot measure it.
    pub average_gpu: Option<Duration>,
}

/// A duration in milliseconds, with the fraction.
fn millis(duration: Duration) -> f64 {
    duration.as_secs_f64() * MILLIS_PER_SECOND
}

impl fmt::Display for Summary {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:.0} fps ({LOW_1_PERCENT_LABEL} {:.0}, {LOW_0_1_PERCENT_LABEL} {:.0}) | worst frame {:.1} ms | work {:.2} ms avg, {:.1} ms worst | gpu ",
            self.average_fps,
            self.low_1_percent,
            self.low_0_1_percent,
            millis(self.worst_interval),
            millis(self.average_work),
            millis(self.worst_work),
        )?;
        match self.average_gpu {
            Some(gpu) => write!(f, "{:.2} ms", millis(gpu)),
            None => write!(f, "n/a"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: Duration = Duration::from_millis(1);

    /// A history that keeps everything, so a test controls the number of frames exactly.
    fn history() -> FrameHistory {
        FrameHistory::new(Retention::Frames(usize::MAX))
    }

    fn push_intervals(history: &mut FrameHistory, millis: &[u64]) {
        for &ms in millis {
            history.push(MS * ms as u32, MS, None);
        }
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-3 * b.abs().max(1.0)
    }

    #[test]
    fn an_empty_history_has_no_summary() {
        assert!(history().summary().is_none());
    }

    #[test]
    fn steady_frames_give_the_same_figure_everywhere() {
        let mut h = history();
        push_intervals(&mut h, &[10; 200]);
        let s = h.summary().unwrap();
        assert!(close(s.average_fps, 100.0));
        assert!(close(s.low_1_percent, 100.0));
        assert!(close(s.low_0_1_percent, 100.0));
        assert_eq!(s.frames, 200);
    }

    #[test]
    fn one_slow_frame_in_a_hundred_drives_the_one_percent_low() {
        let mut h = history();
        push_intervals(&mut h, &[10; 99]);
        push_intervals(&mut h, &[100]);
        let s = h.summary().unwrap();
        // 100 frames: the slowest 1% is the single 100 ms frame, so 10 fps.
        assert!(close(s.low_1_percent, 10.0));
        // 99 * 10 ms + 100 ms = 1090 ms for 100 frames.
        assert!(close(s.average_fps, 100.0 / 1.09));
    }

    #[test]
    fn the_share_is_rounded_up_to_whole_frames() {
        let mut h = history();
        // 150 frames: 1% is 1.5, rounded up to the 2 slowest frames (100 ms and 50 ms).
        push_intervals(&mut h, &[10; 148]);
        push_intervals(&mut h, &[100, 50]);
        let s = h.summary().unwrap();
        assert!(close(s.low_1_percent, 2.0 / 0.150));
    }

    #[test]
    fn the_count_of_slowest_frames_steps_up_exactly_at_a_hundred_and_one() {
        // 100 frames: 1% is exactly one frame (the 100 ms one).
        let mut hundred = history();
        push_intervals(&mut hundred, &[10; 98]);
        push_intervals(&mut hundred, &[100, 50]);
        assert!(close(hundred.summary().unwrap().low_1_percent, 10.0));

        // 101 frames: 1% is 1.01, rounded up to two frames (100 ms and 50 ms).
        let mut one_more = history();
        push_intervals(&mut one_more, &[10; 99]);
        push_intervals(&mut one_more, &[100, 50]);
        assert!(close(
            one_more.summary().unwrap().low_1_percent,
            2.0 / 0.150
        ));
    }

    #[test]
    fn the_running_sum_does_not_drift_over_a_long_run() {
        let mut h = FrameHistory::new(Retention::Window(Duration::from_secs(1)));
        for _ in 0..100_000 {
            h.push(7 * MS, MS, None);
        }
        let recomputed: f64 = h.samples.iter().map(|s| f64::from(s.interval)).sum();
        assert!(
            (h.covered - recomputed).abs() < 1e-6,
            "{} against {}",
            h.covered,
            recomputed
        );
        let s = h.summary().unwrap();
        assert!((142..=144).contains(&s.frames), "{} frames", s.frames);
    }

    #[test]
    fn with_few_frames_both_lows_are_the_worst_frame() {
        let mut h = history();
        push_intervals(&mut h, &[10, 10, 40, 10, 10]);
        let s = h.summary().unwrap();
        assert!(close(s.low_1_percent, 25.0));
        assert!(close(s.low_0_1_percent, 25.0));
    }

    #[test]
    fn the_point_one_percent_low_looks_at_fewer_frames_than_the_one_percent_low() {
        let mut h = history();
        // 1000 frames: 0.1% is the single worst frame, 1% is the worst 10.
        push_intervals(&mut h, &[10; 990]);
        push_intervals(&mut h, &[100, 50, 50, 50, 50, 50, 50, 50, 50, 50]);
        let s = h.summary().unwrap();
        assert!(close(s.low_0_1_percent, 10.0));
        assert!(s.low_1_percent > s.low_0_1_percent);
        assert!(close(s.low_1_percent, 10.0 / 0.55));
    }

    #[test]
    fn a_frame_with_a_zero_interval_is_ignored() {
        let mut h = history();
        h.push(Duration::ZERO, MS, None);
        assert!(h.summary().is_none());
    }

    #[test]
    fn the_live_window_forgets_old_frames() {
        let mut h = FrameHistory::new(Retention::Window(Duration::from_secs(1)));
        push_intervals(&mut h, &[10; 300]); // three seconds of play
        let s = h.summary().unwrap();
        assert!(
            s.covered <= Duration::from_millis(1010),
            "covers {:?}",
            s.covered
        );
        assert!((100..=101).contains(&s.frames), "{} frames", s.frames);
    }

    #[test]
    fn a_frame_longer_than_the_window_is_still_kept() {
        let mut h = FrameHistory::new(Retention::Window(Duration::from_millis(50)));
        push_intervals(&mut h, &[200]);
        assert_eq!(h.summary().unwrap().frames, 1);
    }

    #[test]
    fn the_whole_run_history_keeps_the_most_recent_frames() {
        let mut h = FrameHistory::new(Retention::Frames(3));
        push_intervals(&mut h, &[100, 10, 10, 10]);
        let s = h.summary().unwrap();
        assert_eq!(s.frames, 3);
        assert!(
            close(s.average_fps, 100.0),
            "the 100 ms frame was forgotten"
        );
    }

    #[test]
    fn work_and_worst_times_are_reported() {
        let mut h = history();
        h.push(10 * MS, 2 * MS, None);
        h.push(10 * MS, 6 * MS, None);
        let s = h.summary().unwrap();
        assert_eq!(s.worst_interval, 10 * MS);
        assert!(close(s.average_work.as_secs_f64(), 0.004));
        assert!(close(s.worst_work.as_secs_f64(), 0.006));
    }

    #[test]
    fn gpu_time_is_averaged_over_the_frames_that_have_it() {
        let mut h = history();
        h.push(10 * MS, MS, None);
        h.push(10 * MS, MS, Some(2 * MS));
        h.push(10 * MS, MS, Some(4 * MS));
        let gpu = h.summary().unwrap().average_gpu.unwrap();
        assert!(close(gpu.as_secs_f64(), 0.003));
    }

    #[test]
    fn without_any_gpu_time_the_average_is_unknown() {
        let mut h = history();
        push_intervals(&mut h, &[10, 10]);
        assert_eq!(h.summary().unwrap().average_gpu, None);
    }

    #[test]
    fn the_text_names_the_figures_and_shows_an_unknown_gpu_time() {
        let mut h = history();
        push_intervals(&mut h, &[10; 5]);
        let text = h.summary().unwrap().to_string();
        assert!(text.contains("100 fps"), "{text}");
        assert!(text.contains("1% low"), "{text}");
        assert!(text.contains("0.1% low"), "{text}");
        assert!(text.ends_with("gpu n/a"), "{text}");
        assert!(
            text.is_ascii(),
            "the text must survive a legacy console code page: {text}"
        );
        assert!(text.contains(" ms"), "{text}");
    }
}
