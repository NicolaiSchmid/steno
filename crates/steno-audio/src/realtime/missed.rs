//! The audio a capture thread missed: a callback's position against where
//! the last one ended (A18 of `.plans/2026-10-07-stable-promotion.md`).
//! Rust only: Swift's IOProc reads no timestamps.
//!
//! Each backend's callback says where its first frame sits on the device's
//! or the graph's clock: PipeWire's `pw_time` ticks, the IOProc's input
//! sample time on macOS, WASAPI's device position. A capture thread the
//! scheduler held up misses cycles: the daemon, the HAL or the engine runs
//! on, the audio of those cycles is gone, and the next callback's position
//! sits past where the last one ended. [`CycleClock::advance`] turns that
//! jump into frames, which the backend writes as silence on every lane
//! before the callback's own frames ([`LaneFrameSink::fill_missed`]), so the
//! master stays on wall time and the lanes stay aligned, and which the
//! session counts in [`CaptureStatistics::missed_seconds`] for the
//! recorder's note about missing audio.
//!
//! A jump of more than [`MISSED_FILL_LIMIT`] is not filled: one that long
//! (a computer that slept, a clock that was reset) is left to the
//! session's handling of time the master does not hold. The master falls
//! short by it, and the recorder's note counts the wall time it lacks. The
//! limit is under [`CaptureSession::STALL_TIMEOUT`], so no jump the fill
//! takes can be a stall whose rebuild writes a gap for the same time.
//!
//! Real-time safe: integer arithmetic on plain fields, nothing else.
//!
//! [`LaneFrameSink::fill_missed`]: super::LaneFrameSink::fill_missed
//! [`CaptureStatistics::missed_seconds`]: crate::capture::CaptureStatistics::missed_seconds
//! [`CaptureSession::STALL_TIMEOUT`]: crate::capture::CaptureSession::STALL_TIMEOUT

use std::time::Duration;

/// The longest jump [`CycleClock::advance`] reports for filling: one
/// second. That is half the session's two-second rings, so the fill fits
/// beside a consumer up to a second behind, and half of
/// [`CaptureSession::STALL_TIMEOUT`], so a jump it fills was never a stall
/// the session rebuilt for. See the module doc for what happens to a
/// longer one.
///
/// [`CaptureSession::STALL_TIMEOUT`]: crate::capture::CaptureSession::STALL_TIMEOUT
pub const MISSED_FILL_LIMIT: Duration = Duration::from_secs(1);

/// Where a capture's last callback ended on its clock, and how that clock
/// counts frames; see the module doc. One per started stream, owned by the
/// thread whose callbacks it follows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CycleClock {
    /// Stream frames per `ticks` clock ticks.
    frames: u64,
    ticks: u64,
    /// The most frames [`Self::advance`] reports.
    limit: usize,
    /// The last callback's position and frames; `None` before the first
    /// and after [`Self::reset`].
    last: Option<(u64, usize)>,
}

impl CycleClock {
    /// A clock whose ticks are the stream's frames (the IOProc's sample
    /// time, WASAPI's device position), at `sample_rate` frames a second,
    /// which sets the limit ([`MISSED_FILL_LIMIT`]).
    #[must_use]
    pub fn new(sample_rate: f64) -> Self {
        // Whole frames of a positive rate; a rate that is not one fills
        // nothing.
        let limit = if sample_rate.is_finite() && sample_rate > 0.0 {
            (sample_rate * MISSED_FILL_LIMIT.as_secs_f64()) as usize
        } else {
            0
        };
        Self {
            frames: 1,
            ticks: 1,
            limit,
            last: None,
        }
    }

    /// From now on `ticks` clock ticks are `frames` stream frames
    /// (PipeWire: the graph's rate against the stream's 48 kHz). A scale
    /// that differs from the one before resets the clock, so a graph that
    /// changed its rate reads as no jump. A zero in either is ignored.
    pub fn rescale(&mut self, frames: u64, ticks: u64) {
        if frames == 0 || ticks == 0 || (frames, ticks) == (self.frames, self.ticks) {
            return;
        }
        (self.frames, self.ticks) = (frames, ticks);
        self.reset();
    }

    /// Forgets the last callback: the next one starts afresh and reports
    /// no jump (a position the backend could not read, a stream that
    /// restarted).
    pub fn reset(&mut self) {
        self.last = None;
    }

    /// Whether a callback of `frames` differs in length from the last one
    /// by more than an eighth of it, more than a resampler's rounding: a
    /// graph whose quantum changed. PipeWire's graph clock jumps at such a
    /// change by up to two of the new quanta while the wall clock does not
    /// (measured from 256 to 2 048 frames), so the PipeWire backend starts
    /// the clock afresh there ([`Self::reset`]). `false` before the first
    /// callback.
    #[must_use]
    pub fn resized(&self, frames: usize) -> bool {
        self.last
            .is_some_and(|(_, last)| last.abs_diff(frames).saturating_mul(8) > last)
    }

    /// One callback whose first frame sits at `position` on the clock and
    /// which delivers `frames`: returns the frames missed before it, to be
    /// filled with silence. That is how far `position` passed the end of
    /// the last callback, in stream frames, when that is more than half the
    /// last callback's frames (cycles are missed whole; this leaves room
    /// for a resampler's rounding) and at most the limit. It is 0 for the
    /// first callback, after a reset, after a callback without frames, for
    /// a jump over the limit, and for a position at or behind where the
    /// last callback ended (a clock that went back, a cycle delivered
    /// twice). The clock then follows on from this callback. Positions
    /// wrap: one just past `u64::MAX` follows on from one just before.
    #[inline(always)]
    pub fn advance(&mut self, position: u64, frames: usize) -> usize {
        let missed = match self.last {
            Some((last, delivered)) if delivered > 0 => {
                let elapsed = position.wrapping_sub(last);
                // More than half the ring of positions ahead is behind.
                if elapsed > u64::MAX / 2 {
                    0
                } else {
                    let missed = self.frames_in(elapsed).saturating_sub(delivered);
                    if missed > delivered / 2 && missed <= self.limit {
                        missed
                    } else {
                        0
                    }
                }
            }
            _ => 0,
        };
        self.last = Some((position, frames));
        missed
    }

    /// `ticks` clock ticks in stream frames, to the nearest frame.
    #[inline(always)]
    fn frames_in(&self, ticks: u64) -> usize {
        let frames = (u128::from(ticks) * u128::from(self.frames) + u128::from(self.ticks / 2))
            / u128::from(self.ticks);
        usize::try_from(frames).unwrap_or(usize::MAX)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 48 kHz clock fed one callback per entry of `cycles` (position,
    /// frames); what each reported.
    fn run(clock: &mut CycleClock, cycles: &[(u64, usize)]) -> Vec<usize> {
        cycles
            .iter()
            .map(|&(position, frames)| clock.advance(position, frames))
            .collect()
    }

    #[test]
    fn steady_cycles_miss_nothing_and_a_jump_reports_the_cycles_it_skipped() {
        let mut clock = CycleClock::new(48_000.0);
        assert_eq!(
            run(
                &mut clock,
                &[(0, 480), (480, 480), (960, 480), (2_400, 480), (2_880, 480)]
            ),
            [0, 0, 0, 960, 0],
            "two 480-frame cycles between 960 and 2 400"
        );
    }

    #[test]
    fn the_first_cycle_and_one_after_a_reset_or_an_empty_cycle_report_nothing() {
        let mut clock = CycleClock::new(48_000.0);
        assert_eq!(clock.advance(1_000_000, 480), 0, "the first cycle");
        assert_eq!(clock.advance(1_000_480, 480), 0);
        clock.reset();
        assert_eq!(clock.advance(5_000_000, 480), 0, "after a reset");
        assert_eq!(clock.advance(5_000_480, 0), 0);
        assert_eq!(
            clock.advance(5_009_600, 480),
            0,
            "a cycle without frames says nothing of the next"
        );
        assert_eq!(clock.advance(5_010_080, 480), 0);
    }

    #[test]
    fn a_jump_is_exact_to_the_frame_and_needs_more_than_half_a_cycle() {
        let mut clock = CycleClock::new(48_000.0);
        assert_eq!(run(&mut clock, &[(0, 480), (480 + 241, 480)]), [0, 241]);
        let mut clock = CycleClock::new(48_000.0);
        assert_eq!(
            run(&mut clock, &[(0, 480), (480 + 240, 480)]),
            [0, 0],
            "half a cycle is rounding, not a missed cycle"
        );
        let mut clock = CycleClock::new(48_000.0);
        assert_eq!(run(&mut clock, &[(0, 480), (480 + 481, 480)]), [0, 481]);
    }

    #[test]
    fn a_clock_that_goes_back_or_repeats_a_cycle_reports_nothing_and_follows_on() {
        let mut clock = CycleClock::new(48_000.0);
        assert_eq!(
            run(
                &mut clock,
                &[
                    (48_000, 480),
                    (480, 480),
                    (960, 480),
                    (960, 480),
                    (1_440, 480)
                ]
            ),
            [0, 0, 0, 0, 0]
        );
        assert_eq!(
            clock.advance(2_880, 480),
            960,
            "it follows the new position"
        );
    }

    #[test]
    fn positions_wrap_past_the_largest() {
        let mut clock = CycleClock::new(48_000.0);
        let before = u64::MAX - 239;
        assert_eq!(
            run(
                &mut clock,
                &[
                    (before, 480),
                    (before.wrapping_add(480), 480),
                    (before.wrapping_add(1_920), 480)
                ]
            ),
            [0, 0, 960]
        );
    }

    #[test]
    fn a_jump_over_the_limit_is_not_filled_and_one_at_it_is() {
        let mut clock = CycleClock::new(48_000.0);
        assert_eq!(
            run(&mut clock, &[(0, 480), (480 + 48_000, 480)]),
            [0, 48_000],
            "a second, the limit"
        );
        assert_eq!(
            run(&mut clock, &[(48_960 + 48_001, 480), (97_441, 480)]),
            [0, 0],
            "a frame over it, and the clock follows on from there"
        );
        let mut slept = CycleClock::new(48_000.0);
        assert_eq!(
            run(&mut slept, &[(0, 480), (480 + 48_000 * 3_600, 480)]),
            [0, 0],
            "an hour asleep writes no hour of zeros"
        );
        let mut slow = CycleClock::new(16_000.0);
        assert_eq!(
            run(
                &mut slow,
                &[(0, 160), (160 + 16_000, 160), (16_320 + 16_001, 160)]
            ),
            [0, 16_000, 0],
            "the limit is a second at the stream's rate"
        );
    }

    #[test]
    fn a_graph_at_another_rate_counts_in_stream_frames_and_a_rate_change_resets() {
        // 44.1 kHz ticks, 48 kHz frames: 441 ticks are 480 frames.
        let mut clock = CycleClock::new(48_000.0);
        clock.rescale(48_000, 44_100);
        assert_eq!(
            run(
                &mut clock,
                &[(0, 480), (441, 480), (882, 481), (2_205, 480)]
            ),
            [0, 0, 0, 959],
            "three cycles after one of 481 frames: two missed, less the frame it held over"
        );
        clock.rescale(48_000, 44_100);
        assert_eq!(
            clock.advance(3_087, 480),
            480,
            "the same scale keeps the clock"
        );
        clock.rescale(48_000, 96_000);
        assert_eq!(clock.advance(9_000_000, 480), 0, "a new rate starts afresh");
        assert_eq!(clock.advance(9_000_960, 480), 0);
        clock.rescale(0, 96_000);
        assert_eq!(clock.advance(9_001_920, 480), 0, "a zero scale is ignored");
        assert_eq!(clock.advance(9_003_840, 480), 480);
    }

    #[test]
    fn a_resampled_graph_rounds_without_reporting_a_jump() {
        // 1024-tick quanta at 44.1 kHz are 1114.56 frames at 48 kHz: the
        // adapter delivers 1114 or 1115.
        let mut clock = CycleClock::new(48_000.0);
        clock.rescale(48_000, 44_100);
        let mut position = 0;
        for cycle in 0..1_000u64 {
            let frames = if cycle % 2 == 0 { 1_114 } else { 1_115 };
            assert_eq!(clock.advance(position, frames), 0, "cycle {cycle}");
            position += 1_024;
        }
    }

    #[test]
    fn a_callback_of_another_length_is_a_resize_and_rounding_is_not() {
        let mut clock = CycleClock::new(48_000.0);
        assert!(!clock.resized(1_024), "nothing yet");
        clock.advance(0, 1_024);
        assert!(!clock.resized(1_024));
        assert!(
            !clock.resized(1_025) && !clock.resized(1_023),
            "a resampler's frame"
        );
        assert!(!clock.resized(1_152) && !clock.resized(896), "an eighth");
        assert!(clock.resized(1_153) && clock.resized(895));
        assert!(clock.resized(256) && clock.resized(2_048) && clock.resized(0));
    }

    #[test]
    fn a_zero_or_odd_rate_fills_nothing() {
        for rate in [0.0, -48_000.0, f64::NAN] {
            let mut clock = CycleClock::new(rate);
            assert_eq!(run(&mut clock, &[(0, 480), (1_920, 480)]), [0, 0], "{rate}");
        }
    }
}
