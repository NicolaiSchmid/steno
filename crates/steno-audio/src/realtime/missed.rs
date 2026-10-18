//! The audio a capture thread missed: a callback whose position on the
//! device's or graph's clock sits past where the delivered audio ends.
//! [`CycleClock`] turns that jump into frames; up to [`MISSED_FILL_LIMIT`]
//! is written as zeros on every lane so the master stays on wall time, a
//! longer one is only counted. Real-time safe: integer arithmetic only.

use std::time::Duration;

/// The longest jump [`CycleClock::advance`] reports as [`Missed::fill`]:
/// half the session's two-second rings and of its stall timeout, so no
/// filled jump can also be a stall's gap.
pub const MISSED_FILL_LIMIT: Duration = Duration::from_secs(1);

/// The frames a [`CycleClock`] found missed: those to write as silence and
/// those of a jump too long for that.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Missed {
    pub fill: usize,
    pub unfilled: usize,
}

impl Missed {
    #[must_use]
    pub fn is_none(&self) -> bool {
        self.fill == 0 && self.unfilled == 0
    }

    /// A jump of `gap` frames past where a callback of `last_frames` ended:
    /// nothing for half a callback or less (a resampler's rounding; cycles
    /// are missed whole) or after a callback without frames, else filled
    /// up to `limit` frames and unfilled past it. The one rule every
    /// backend reads its clock by.
    ///
    /// ```
    /// use steno_audio::realtime::Missed;
    ///
    /// assert!(Missed::of_jump(240, 480, 48_000).is_none());
    /// assert_eq!(Missed::of_jump(960, 480, 48_000).fill, 960);
    /// assert_eq!(Missed::of_jump(48_001, 480, 48_000).unfilled, 48_001);
    /// ```
    #[must_use]
    #[inline(always)]
    pub fn of_jump(gap: u64, last_frames: usize, limit: usize) -> Self {
        if last_frames == 0 || gap <= last_frames as u64 / 2 {
            return Self::default();
        }
        // At most the limit, a `usize`; past it, as many as a `usize` holds.
        let frames = usize::try_from(gap).unwrap_or(usize::MAX);
        if gap <= limit as u64 {
            Self {
                fill: frames,
                unfilled: 0,
            }
        } else {
            Self {
                fill: 0,
                unfilled: frames,
            }
        }
    }
}

/// How one clock's ticks count in another's frames: `frames` frames per
/// `ticks` ticks (the graph's rate against the stream's 48 kHz, a device
/// position's unit, the QPC's 100 ns).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ratio {
    pub frames: u64,
    pub ticks: u64,
}

impl Ratio {
    pub const ONE: Self = Self {
        frames: 1,
        ticks: 1,
    };

    /// `ticks` in frames, to the nearest frame; 0 for a ratio of no ticks,
    /// so no reading divides by zero on the audio thread.
    ///
    /// ```
    /// use steno_audio::realtime::Ratio;
    ///
    /// let graph = Ratio { frames: 48_000, ticks: 44_100 };
    /// assert_eq!(graph.read(441), 480);
    /// assert_eq!(Ratio { frames: 1, ticks: 0 }.read(441), 0);
    /// ```
    #[must_use]
    #[inline(always)]
    pub fn read(self, ticks: u64) -> u64 {
        let frames = (u128::from(ticks) * u128::from(self.frames) + u128::from(self.ticks / 2))
            .checked_div(u128::from(self.ticks))
            .unwrap_or(0);
        u64::try_from(frames).unwrap_or(u64::MAX)
    }
}

/// Where the audio a capture delivered ends on its clock, and how that
/// clock counts frames. One per started stream, owned by the thread whose
/// callbacks it follows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CycleClock {
    scale: Ratio,
    limit: usize,
    /// The first callback's position since the last [`Self::reset`], from
    /// which positions count in stream frames; `None` before it.
    origin: Option<u64>,
    /// Where the delivered audio, fills included, ends: stream frames
    /// since `origin`.
    end: u64,
    last_frames: usize,
}

impl CycleClock {
    /// A clock whose ticks are the stream's frames (the IOProc's sample
    /// time), at `sample_rate` frames a second.
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
            scale: Ratio::ONE,
            limit,
            origin: None,
            end: 0,
            last_frames: 0,
        }
    }

    /// From now on `ticks` clock ticks are `frames` stream frames. A scale
    /// that differs from the one before resets the clock, so a graph that
    /// changed its rate reads as no jump. A zero in either is ignored.
    pub fn rescale(&mut self, frames: u64, ticks: u64) {
        let scale = Ratio { frames, ticks };
        if frames == 0 || ticks == 0 || scale == self.scale {
            return;
        }
        self.scale = scale;
        self.reset();
    }

    /// Forgets where the audio ends: the next callback starts afresh and
    /// reports no jump.
    pub fn reset(&mut self) {
        self.origin = None;
    }

    /// Whether a callback of `frames` differs in length from the last one
    /// by more than an eighth of it: a graph whose quantum changed, at
    /// which PipeWire's graph clock jumps while the wall clock does not.
    /// `false` before the first callback.
    #[must_use]
    pub fn resized(&self, frames: usize) -> bool {
        let last = self.last_frames;
        self.origin.is_some() && last.abs_diff(frames).saturating_mul(8) > last
    }

    /// One callback whose first frame sits at `position` and which delivers
    /// `frames`: the frames missed before it, when `position` passed the end
    /// of the delivered audio by more than half the last callback (up to the
    /// limit to fill, past it unfilled). Nothing for the first callback or
    /// after a reset. A position at or behind the end (a cycle delivered
    /// twice) moves the end on by `frames`, so those frames count against
    /// the next jump; one far behind (a clock that went back) starts afresh.
    /// Positions wrap.
    #[inline(always)]
    pub fn advance(&mut self, position: u64, frames: usize) -> Missed {
        let last_frames = std::mem::replace(&mut self.last_frames, frames);
        let Some(at) = self.at(position) else {
            self.origin = Some(position);
            self.end = frames as u64;
            return Missed::default();
        };
        let mut missed = Missed::default();
        if at > self.end {
            missed = self.jump(at - self.end, last_frames);
            self.end = at;
        } else if self.end - at > self.limit as u64 {
            self.origin = Some(position);
            self.end = 0;
        }
        self.end = self.end.saturating_add(frames as u64);
        missed
    }

    /// The frames missed after the last callback, for a stream stopped
    /// while the clock reads `position`, less the callback in flight at the
    /// stop. Changes nothing.
    #[must_use]
    pub fn missed_until(&self, position: u64) -> Missed {
        let Some(at) = self.at(position) else {
            return Missed::default();
        };
        let in_flight = self.end.saturating_add(self.last_frames as u64);
        if at <= in_flight {
            return Missed::default();
        }
        self.jump(at - in_flight, self.last_frames)
    }

    /// `position` in stream frames since the origin; `None` before the
    /// first callback and for a position behind it.
    #[inline(always)]
    fn at(&self, position: u64) -> Option<u64> {
        let since = position.wrapping_sub(self.origin?);
        (since <= u64::MAX / 2).then(|| self.scale.read(since))
    }

    #[inline(always)]
    fn jump(&self, gap: u64, last_frames: usize) -> Missed {
        Missed::of_jump(gap, last_frames, self.limit)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(clock: &mut CycleClock, cycles: &[(u64, usize)]) -> Vec<usize> {
        cycles
            .iter()
            .map(|&(position, frames)| clock.advance(position, frames).fill)
            .collect()
    }

    fn unfilled(frames: usize) -> Missed {
        Missed {
            fill: 0,
            unfilled: frames,
        }
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
        assert_eq!(clock.advance(1_000_000, 480).fill, 0, "the first cycle");
        assert_eq!(clock.advance(1_000_480, 480).fill, 0);
        clock.reset();
        assert_eq!(clock.advance(5_000_000, 480).fill, 0, "after a reset");
        assert_eq!(clock.advance(5_000_480, 0).fill, 0);
        assert_eq!(
            clock.advance(5_009_600, 480).fill,
            0,
            "a cycle without frames says nothing of the next"
        );
        assert_eq!(clock.advance(5_010_080, 480).fill, 0);
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
    fn a_clock_that_goes_back_starts_afresh_and_a_repeated_cycle_counts_against_the_next_jump() {
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
            clock.advance(2_880, 480).fill,
            480,
            "two cycles missed, less the one delivered twice"
        );
        assert_eq!(clock.advance(3_360, 480).fill, 0);
    }

    #[test]
    fn a_buffer_kept_from_a_stall_counts_against_the_next_jump() {
        // As measured on PipeWire in a throttled container: five cycles
        // apart, then a second buffer at the same position.
        let mut clock = CycleClock::new(48_000.0);
        assert_eq!(
            run(
                &mut clock,
                &[
                    (0, 1_024),
                    (5_120, 1_024),
                    (5_120, 1_024),
                    (6_144, 1_024),
                    (7_168, 1_024),
                    (10_240, 1_024),
                    (11_264, 1_024)
                ]
            ),
            [0, 4_096, 0, 0, 0, 1_024, 0],
            "the kept buffer leaves one cycle of the next jump of three to fill"
        );
        // A clock stuck at one position while 102 callbacks deliver: more
        // than the limit behind the end, it starts afresh, and the frames
        // before that count against nothing.
        let mut clock = CycleClock::new(48_000.0);
        for _ in 0..102 {
            assert_eq!(clock.advance(0, 480).fill, 0);
        }
        assert_eq!(clock.advance(480, 480).fill, 0);
        assert_eq!(
            clock.advance(1_440, 480).fill,
            480,
            "a cycle missed after it"
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
    fn a_jump_over_the_limit_is_unfilled_without_a_bound_and_one_at_it_is_filled() {
        let mut clock = CycleClock::new(48_000.0);
        assert_eq!(
            run(&mut clock, &[(0, 480), (480 + 48_000, 480)]),
            [0, 48_000],
            "a second, the limit"
        );
        assert_eq!(
            clock.advance(48_960 + 48_001, 480),
            unfilled(48_001),
            "a frame over it is counted, not filled"
        );
        assert!(
            clock.advance(97_441, 480).is_none(),
            "and the clock follows on from there"
        );
        let mut stopped = CycleClock::new(48_000.0);
        stopped.advance(0, 480);
        assert_eq!(
            stopped.advance(480 + 120_000, 480),
            unfilled(120_000),
            "2.5 s, past the session's stall timeout"
        );
        let mut slept = CycleClock::new(48_000.0);
        slept.advance(0, 480);
        assert_eq!(
            slept.advance(480 + 48_000 * 3_600, 480),
            unfilled(48_000 * 3_600),
            "an hour asleep writes no hour of zeros, and counts it"
        );
        let mut slow = CycleClock::new(16_000.0);
        assert_eq!(
            run(&mut slow, &[(0, 160), (160 + 16_000, 160)]),
            [0, 16_000],
            "the limit is a second at the stream's rate"
        );
        assert_eq!(slow.advance(16_320 + 16_001, 160), unfilled(16_001));
    }

    #[test]
    fn at_the_stop_the_clock_reads_what_passed_the_end_less_the_callback_in_flight() {
        let mut clock = CycleClock::new(48_000.0);
        assert!(clock.missed_until(1_000).is_none(), "nothing delivered yet");
        run(&mut clock, &[(0, 480), (480, 480)]);
        // The audio ends at 960; the callback in flight takes it to 1 440.
        for (position, fill) in [(960, 0), (1_440, 0), (1_680, 0), (1_681, 241), (2_400, 960)] {
            assert_eq!(clock.missed_until(position).fill, fill, "at {position}");
        }
        assert_eq!(
            clock.missed_until(1_440 + 48_000),
            Missed {
                fill: 48_000,
                unfilled: 0
            }
        );
        assert_eq!(clock.missed_until(1_440 + 120_000), unfilled(120_000));
        assert!(
            clock.missed_until(u64::MAX - 10).is_none(),
            "behind the first callback"
        );
        assert!(clock.advance(960, 480).is_none(), "it changed nothing");
        clock.reset();
        assert!(clock.missed_until(1_000_000).is_none(), "after a reset");
        // On another scale: nanosecond ticks, as PipeWire's stop reads them.
        let mut clock = CycleClock::new(48_000.0);
        clock.rescale(48_000, 1_000_000_000);
        clock.advance(5_000_000_000, 480);
        assert!(
            clock.missed_until(5_020_000_000).is_none(),
            "one cycle late"
        );
        assert_eq!(
            clock.missed_until(5_600_000_000).fill,
            28_800 - 960,
            "0.6 s on, less the cycle delivered and the one in flight"
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
            clock.advance(3_087, 480).fill,
            480,
            "the same scale keeps the clock"
        );
        clock.rescale(48_000, 96_000);
        assert_eq!(
            clock.advance(9_000_000, 480).fill,
            0,
            "a new rate starts afresh"
        );
        assert_eq!(clock.advance(9_000_960, 480).fill, 0);
        clock.rescale(0, 96_000);
        assert_eq!(
            clock.advance(9_001_920, 480).fill,
            0,
            "a zero scale is ignored"
        );
        assert_eq!(clock.advance(9_003_840, 480).fill, 480);
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
            assert_eq!(clock.advance(position, frames).fill, 0, "cycle {cycle}");
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
    fn a_zero_or_odd_rate_fills_nothing_and_counts_the_jump() {
        for rate in [0.0, -48_000.0, f64::NAN] {
            let mut clock = CycleClock::new(rate);
            clock.advance(0, 480);
            assert_eq!(clock.advance(1_920, 480), unfilled(1_440), "{rate}");
        }
    }
}
