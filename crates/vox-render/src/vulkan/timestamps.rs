//! GPU frame timing with timestamp queries (core Vulkan 1.0).
//!
//! Each frame slot owns a query pool with two timestamps: one written before the
//! frame's first command, one after its last. [`TimestampClock`] turns the two raw
//! counter values into a duration; it is pure arithmetic and unit tested. See
//! decision 0008.

use std::time::Duration;

use ash::vk;

/// The query that holds the timestamp written before the frame's commands.
pub const FRAME_START_QUERY: u32 = 0;
/// The query that holds the timestamp written after the frame's commands.
pub const FRAME_END_QUERY: u32 = 1;
/// Queries in each frame slot's pool: the start and the end timestamp.
pub const TIMESTAMPS_PER_FRAME: u32 = FRAME_END_QUERY + 1;
// The one reset of `0..TIMESTAMPS_PER_FRAME` must cover both queries, start before end.
const _: () = assert!(FRAME_START_QUERY == 0 && FRAME_START_QUERY < FRAME_END_QUERY);

/// Width of the widest timestamp counter: a query result is a `u64`, and Vulkan allows
/// `timestampValidBits` of 36 up to 64 (or 0 for "no timestamps").
const MAX_TIMESTAMP_BITS: u32 = u64::BITS;
/// Nanoseconds per second, to turn the counter's nanoseconds into seconds for `Duration`.
const NANOS_PER_SECOND: f64 = 1e9;

/// How to read the timestamp counter of the queue the renderer uses.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TimestampClock {
    /// The bits of a timestamp that carry the counter; the bits above are undefined.
    mask: u64,
    /// Nanoseconds per counter tick (`limits.timestampPeriod`).
    period_ns: f64,
}

impl TimestampClock {
    /// The clock of a queue whose timestamps have `valid_bits` meaningful bits and tick
    /// every `period_ns` nanoseconds, or `None` if it cannot measure: the queue writes no
    /// timestamps (`valid_bits == 0`) or the period is not a positive finite number.
    pub fn new(valid_bits: u32, period_ns: f32) -> Option<Self> {
        if valid_bits == 0 || !period_ns.is_finite() || period_ns <= 0.0 {
            return None;
        }
        // A driver reporting more bits than a result has gets the full width.
        let bits = valid_bits.min(MAX_TIMESTAMP_BITS);
        // `bits` is 1..=64, so the shift is 0..=63 and cannot overflow; 64 bits keep
        // every bit, where `(1 << 64) - 1` would overflow.
        let mask = u64::MAX >> (MAX_TIMESTAMP_BITS - bits);
        Some(Self {
            mask,
            period_ns: f64::from(period_ns),
        })
    }

    /// The time from timestamp `start` to the later timestamp `end`, or `None` if it
    /// does not fit a `Duration`.
    ///
    /// The counter may wrap between the two: the difference is taken modulo the counter
    /// width, which is exact as long as less than one full counter period passed: with
    /// 36 bits at one tick per nanosecond that is about 69 seconds, far longer than a frame.
    pub fn elapsed(&self, start: u64, end: u64) -> Option<Duration> {
        let ticks = end.wrapping_sub(start) & self.mask;
        // Rounds above 2^53 ticks, far beyond any frame time; never overflows.
        let seconds = ticks as f64 * self.period_ns / NANOS_PER_SECOND;
        Duration::try_from_secs_f64(seconds).ok()
    }
}

/// Resets the frame's timestamps and writes the start timestamp.
///
/// # Safety
/// `command_buffer` must be a primary command buffer of `device` in the recording state,
/// outside a render pass, from a pool whose queue family has non-zero
/// `timestampValidBits`. `pool` must be a timestamp query pool of `device` with
/// [`TIMESTAMPS_PER_FRAME`] queries that no pending submission uses.
pub unsafe fn record_frame_start(
    device: &ash::Device,
    command_buffer: vk::CommandBuffer,
    pool: vk::QueryPool,
) {
    // SAFETY: the caller guarantees the recording state outside a render pass, the
    // pool's type and size, and that no pending work uses it. The reset covers both
    // queries and comes first, so both are unavailable when written, as Vulkan requires.
    unsafe {
        device.cmd_reset_query_pool(
            command_buffer,
            pool,
            FRAME_START_QUERY,
            TIMESTAMPS_PER_FRAME,
        );
        device.cmd_write_timestamp(
            command_buffer,
            vk::PipelineStageFlags::TOP_OF_PIPE,
            pool,
            FRAME_START_QUERY,
        );
    }
}

/// Writes the end timestamp once every earlier command of the frame has completed.
///
/// # Safety
/// The conditions of [`record_frame_start`] hold, and it was recorded earlier into the
/// same `command_buffer` (so the end query was reset and is still unwritten).
pub unsafe fn record_frame_end(
    device: &ash::Device,
    command_buffer: vk::CommandBuffer,
    pool: vk::QueryPool,
) {
    // SAFETY: see the caller's contract; the end query was reset by `record_frame_start`
    // and not written since.
    unsafe {
        device.cmd_write_timestamp(
            command_buffer,
            vk::PipelineStageFlags::BOTTOM_OF_PIPE,
            pool,
            FRAME_END_QUERY,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A typical desktop GPU: 64-bit counter, one tick per nanosecond.
    fn nanosecond_clock() -> TimestampClock {
        TimestampClock::new(64, 1.0).unwrap()
    }

    #[test]
    fn ticks_times_the_period_is_the_duration() {
        let clock = TimestampClock::new(64, 1.0).unwrap();
        assert_eq!(
            clock.elapsed(1_000, 501_000),
            Some(Duration::from_micros(500))
        );
        let slow = TimestampClock::new(64, 52.08).unwrap();
        let time = slow.elapsed(0, 1_000_000).unwrap();
        // 52.08 ns as an f32 is not exact; allow a nanosecond per thousand.
        let expected_ns = 52_080_000_f64;
        assert!((time.as_nanos() as f64 - expected_ns).abs() < expected_ns / 1e3);
    }

    #[test]
    fn equal_timestamps_take_no_time() {
        assert_eq!(nanosecond_clock().elapsed(7, 7), Some(Duration::ZERO));
    }

    #[test]
    fn a_counter_that_wraps_between_the_timestamps_still_measures_correctly() {
        let clock = nanosecond_clock();
        assert_eq!(
            clock.elapsed(u64::MAX - 9, 10),
            Some(Duration::from_nanos(20))
        );
    }

    #[test]
    fn a_36_bit_counter_ignores_the_undefined_high_bits_and_wraps_at_2_pow_36() {
        let clock = TimestampClock::new(36, 1.0).unwrap();
        let top = (1_u64 << 36) - 1;
        // Wraps from the top of the 36-bit range to 9: 10 ticks plus 9.
        assert_eq!(clock.elapsed(top, 9), Some(Duration::from_nanos(10)));
        // Garbage above bit 36 does not change the result.
        let garbage = 0xABCD_u64 << 36;
        assert_eq!(
            clock.elapsed(garbage | 100, (garbage << 1) | 250),
            Some(Duration::from_nanos(150))
        );
    }

    #[test]
    fn a_64_bit_counter_keeps_every_bit() {
        let clock = nanosecond_clock();
        assert_eq!(clock.mask, u64::MAX);
        assert_eq!(
            clock.elapsed(0, 1 << 40),
            Some(Duration::from_nanos(1 << 40))
        );
    }

    #[test]
    fn more_valid_bits_than_a_result_has_count_as_64() {
        assert_eq!(TimestampClock::new(200, 1.0), TimestampClock::new(64, 1.0));
    }

    #[test]
    fn no_valid_bits_means_no_clock() {
        assert_eq!(TimestampClock::new(0, 1.0), None);
    }

    #[test]
    fn a_period_that_is_not_positive_and_finite_means_no_clock() {
        assert_eq!(TimestampClock::new(64, 0.0), None);
        assert_eq!(TimestampClock::new(64, -1.0), None);
        assert_eq!(TimestampClock::new(64, f32::NAN), None);
        assert_eq!(TimestampClock::new(64, f32::INFINITY), None);
    }

    #[test]
    fn absurdly_large_spans_give_none_or_a_value_without_panicking() {
        let huge_period = TimestampClock::new(64, f32::MAX).unwrap();
        assert_eq!(huge_period.elapsed(0, u64::MAX), None);
        let fine = TimestampClock::new(64, f32::MIN_POSITIVE).unwrap();
        assert!(fine.elapsed(0, u64::MAX).is_some());
        // The widest span at one tick per nanosecond is about 584 years: still fits.
        assert!(nanosecond_clock().elapsed(1, 0).is_some());
    }
}
