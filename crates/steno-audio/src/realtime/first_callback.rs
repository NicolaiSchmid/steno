//! When a capture's first callback came, against the capture's start
//! (stable plan A9; A10's step 2 reads it). On the Mac a call capture's
//! aggregate runs only while a process its tap includes drives the
//! output, so the capture starts a silent output of its own first (A10);
//! should that output not start, the IOProc waits for another app to play
//! and a call's first seconds are missing from the recording. The IO
//! thread stores the first callback's host time once; the backend reads
//! it off that thread when the capture stops and logs the offset at
//! `info`. No Swift equivalent.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// The first callback's host time; see the module doc.
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
    /// No callback yet.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            host_time: AtomicU64::new(0),
        }
    }

    /// The IO thread, every callback: keeps `host_time` (the HAL's clock,
    /// in its ticks) when no callback came before. One relaxed load a
    /// callback and one store on the first; nothing else. One thread calls
    /// it (the IOProc), so the load and the store cannot race. A host time
    /// of 0 is kept as 1, since 0 means none.
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

    /// The first callback's offset from `started` (a host time taken just
    /// before the device was started), `to_nanos` turning the clock's ticks
    /// into nanoseconds; `None` when no callback came. A callback stamped
    /// before `started` counts as an offset of 0.
    #[must_use]
    pub fn offset(&self, started: u64, to_nanos: impl Fn(u64) -> u64) -> Option<Duration> {
        let first = self.host_time()?;
        Some(Duration::from_nanos(to_nanos(
            first.saturating_sub(started),
        )))
    }
}

/// The `info` line for a capture that ran for `ran`: the first callback's
/// `offset` from its start, or that none came; `tap` says whether the
/// capture had the system lane, whose IOProc waits on the silent output
/// having started.
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
