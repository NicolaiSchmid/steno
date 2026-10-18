//! When a capture's first callback came, against the capture's start. The
//! IO thread stores the first callback's host time once; the backend reads
//! it at the stop and logs the offset. A first callback later than
//! [`FIRST_CALLBACK_ALLOWANCE`] is audio missed before it, counted
//! unfilled ([`FirstCallback::missed`]).

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// How late after the device's start a first callback may come without
/// counting as audio missed; a normal one is well inside it.
pub const FIRST_CALLBACK_ALLOWANCE: Duration = Duration::from_millis(200);

/// The first callback's host time.
///
/// ```
/// use std::time::Duration;
///
/// use steno_audio::realtime::FirstCallback;
///
/// let first = FirstCallback::new();
/// assert_eq!(first.offset(1_000, |ticks| ticks), None);
/// first.mark(1_250); // the IO thread, each callback
/// first.mark(9_999);
/// assert_eq!(first.offset(1_000, |ticks| ticks), Some(Duration::from_nanos(250)));
/// ```
#[derive(Debug, Default)]
pub struct FirstCallback {
    /// Host time of the first callback; 0 until one came.
    host_time: AtomicU64,
}

impl FirstCallback {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            host_time: AtomicU64::new(0),
        }
    }

    /// The IO thread, every callback: keeps `host_time` when no callback
    /// came before. One relaxed load, and a store on the first; only the
    /// IOProc calls it, so they cannot race. A host time of 0 is kept as 1.
    #[inline(always)]
    pub fn mark(&self, host_time: u64) {
        if self.host_time.load(Ordering::Relaxed) == 0 {
            self.host_time.store(host_time.max(1), Ordering::Relaxed);
        }
    }

    /// The first callback's host time, `None` before one came.
    #[must_use]
    pub fn host_time(&self) -> Option<u64> {
        Some(self.host_time.load(Ordering::Relaxed)).filter(|&t| t != 0)
    }

    /// The first callback's offset from `started` (taken just before the
    /// device started), `to_nanos` converting ticks; `None` when none came.
    /// A callback stamped before `started` is an offset of 0.
    #[must_use]
    pub fn offset(&self, started: u64, to_nanos: impl Fn(u64) -> u64) -> Option<Duration> {
        let first = self.host_time()?;
        Some(Duration::from_nanos(to_nanos(
            first.saturating_sub(started),
        )))
    }

    /// The frames missed before the first callback at `sample_rate`: the
    /// offset beyond [`FIRST_CALLBACK_ALLOWANCE`], all unfilled; nothing
    /// when no callback came (the stall watchdog's gap holds that time).
    ///
    /// ```
    /// use steno_audio::realtime::FirstCallback;
    ///
    /// let first = FirstCallback::new();
    /// first.mark(1_000 + 700_000_000); // 0.7 s after the start, in ns
    /// assert_eq!(first.missed(1_000, |ticks| ticks, 48_000.0), 24_000);
    /// ```
    #[must_use]
    pub fn missed(&self, started: u64, to_nanos: impl Fn(u64) -> u64, sample_rate: f64) -> usize {
        let late = self
            .offset(started, to_nanos)
            .map_or(Duration::ZERO, |offset| {
                offset.saturating_sub(FIRST_CALLBACK_ALLOWANCE)
            });
        // Whole frames of a positive, finite product.
        (late.as_secs_f64() * sample_rate.max(0.0)).round() as usize
    }
}

/// The `info` line for a capture that ran for `ran`: the first callback's
/// `offset`, or that none came; `tap` says whether it had the system lane.
#[must_use]
pub fn first_callback_line(offset: Option<Duration>, ran: Duration, tap: bool) -> String {
    let what = if tap {
        "the capture's first callback (system tap)"
    } else {
        "the capture's first callback (no tap)"
    };
    match offset {
        Some(offset) => format!(
            "{what} came {:.1} ms after its start",
            offset.as_secs_f64() * 1_000.0
        ),
        None => format!("{what} never came in {:.1} s", ran.as_secs_f64()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_mark_stays() {
        let first = FirstCallback::new();
        assert_eq!(first.host_time(), None);
        first.mark(500);
        first.mark(400);
        first.mark(900);
        assert_eq!(first.host_time(), Some(500));
    }

    #[test]
    fn a_host_time_of_zero_still_counts_as_a_callback() {
        let first = FirstCallback::new();
        first.mark(0);
        assert_eq!(first.host_time(), Some(1));
        first.mark(7);
        assert_eq!(first.host_time(), Some(1));
    }

    /// Apple silicon's host clock: 125 / 3 nanoseconds a tick.
    #[test]
    fn the_offset_is_converted_from_ticks() {
        let to_nanos = |ticks: u64| ticks * 125 / 3;
        let first = FirstCallback::new();
        let started = 1_000_000;
        assert_eq!(first.offset(started, to_nanos), None);
        first.mark(started + 2_400_000);
        assert_eq!(
            first.offset(started, to_nanos),
            Some(Duration::from_millis(100))
        );
        // A cycle stamped before the start.
        let early = FirstCallback::new();
        early.mark(started - 10);
        assert_eq!(early.offset(started, to_nanos), Some(Duration::ZERO));
    }

    /// Only the time past the allowance counts, unfilled.
    #[test]
    fn a_late_first_callback_counts_what_came_before_it_past_the_allowance() {
        let to_nanos = |ticks: u64| ticks * 125 / 3;
        let started = 1_000_000;
        let first = FirstCallback::new();
        assert_eq!(first.missed(started, to_nanos, 48_000.0), 0, "none came");
        // 0.5 s after the start: 0.3 s past the allowance.
        first.mark(started + 12_000_000);
        assert_eq!(first.missed(started, to_nanos, 48_000.0), 14_400);
        assert_eq!(first.missed(started, to_nanos, 16_000.0), 4_800);
        let on_time = FirstCallback::new();
        on_time.mark(started + 4_800_000); // 200 ms
        assert_eq!(on_time.missed(started, to_nanos, 48_000.0), 0);
        let early = FirstCallback::new();
        early.mark(started - 10);
        assert_eq!(early.missed(started, to_nanos, 48_000.0), 0);
    }

    #[test]
    fn the_line_says_the_offset_or_that_none_came() {
        assert_eq!(
            first_callback_line(
                Some(Duration::from_micros(12_340)),
                Duration::from_secs(20),
                true
            ),
            "the capture's first callback (system tap) came 12.3 ms after its start"
        );
        assert_eq!(
            first_callback_line(None, Duration::from_millis(20_500), false),
            "the capture's first callback (no tap) never came in 20.5 s"
        );
    }
}
