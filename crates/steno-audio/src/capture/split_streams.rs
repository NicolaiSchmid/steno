//! Which capture stream feeds which lane when the microphone and the system
//! audio arrive as two streams (WASAPI, WP10a), with the latency arithmetic
//! for them. It also decides which of the engine's answers about a stream
//! are trusted ([`stream_sizes`]). Pure, so it is tested on every OS; the
//! Windows backend (`capture::live::wasapi`) builds on it. No Swift
//! counterpart (one IOProc there).
//!
//! The plan describes the two streams in [`StreamLayout`]'s terms, so the
//! master thread delivers through the same
//! [`deliver_slices`](crate::realtime::deliver_slices) as the IOProc and
//! the PipeWire backend: buffer 0 is the master stream's packet, buffer 1 the
//! follower's mono staging copy ([`FollowerLane`]).
//! The microphone stream is mono (the engine downmixes on request), the
//! system stream stereo and folded to the mono system lane.

use std::ops::RangeInclusive;

use steno_core::AudioLane;

use super::configuration::CaptureError;
use super::layout::StreamLayout;
use crate::SAMPLE_RATE;
use crate::realtime::{CycleClock, FollowerLane, Missed};

/// One of the two capture streams.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StreamSource {
    /// The microphone: the default capture endpoint or the selected one.
    Microphone,
    /// What the machine plays: process loopback, or endpoint loopback.
    System,
}

impl StreamSource {
    /// Channels requested from the engine for this stream: mono for the
    /// microphone, stereo for the system audio.
    #[must_use]
    pub fn channels(self) -> usize {
        match self {
            StreamSource::Microphone => 1,
            StreamSource::System => 2,
        }
    }

    /// `mic` or `system`, for thread names and logs.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            StreamSource::Microphone => "mic",
            StreamSource::System => "system",
        }
    }
}

/// The streams a capture opens for its lanes, and where each lane reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SplitStreamPlan {
    /// Buffer 0 is the master's packet, buffer 1 the follower's copy.
    pub layout: StreamLayout,
    /// The stream whose thread produces into the sink: the microphone when
    /// a lane needs it, else the system stream.
    pub master: StreamSource,
    /// The system stream as the follower: both the microphone and the
    /// system lane are recorded.
    pub follower: Option<StreamSource>,
}

impl SplitStreamPlan {
    /// The plan for `lanes`, in sink order.
    ///
    /// ```
    /// use steno_audio::capture::{SplitStreamPlan, StreamSource};
    /// use steno_core::AudioLane;
    ///
    /// let plan = SplitStreamPlan::new(&[AudioLane::Mic, AudioLane::System]).unwrap();
    /// assert_eq!(plan.master, StreamSource::Microphone);
    /// assert_eq!(plan.follower, Some(StreamSource::System));
    ///
    /// let system_only = SplitStreamPlan::new(&[AudioLane::System]).unwrap();
    /// assert_eq!(system_only.master, StreamSource::System);
    /// assert_eq!(system_only.follower, None);
    /// ```
    pub fn new(lanes: &[AudioLane]) -> Result<Self, CaptureError> {
        if lanes.is_empty() {
            return Err(CaptureError::UnexpectedStreamLayout(
                "no lanes to capture".into(),
            ));
        }
        let needs_mic = lanes.contains(&AudioLane::Mic) || lanes.contains(&AudioLane::Mixed);
        let needs_system = lanes.contains(&AudioLane::System);
        let (layout, master, follower) = if needs_mic {
            let mic = StreamSource::Microphone.channels();
            // The follower's staging copy is one mono buffer after the
            // microphone's, in the place of a tap.
            let tap: &[usize] = if needs_system { &[1] } else { &[] };
            let aggregate: Vec<usize> = std::iter::once(mic).chain(tap.iter().copied()).collect();
            let layout = StreamLayout::resolve(lanes, &aggregate, &[vec![mic]], tap, Some(0))?;
            (
                layout,
                StreamSource::Microphone,
                needs_system.then_some(StreamSource::System),
            )
        } else {
            let system = StreamSource::System.channels();
            let layout = StreamLayout::resolve(lanes, &[system], &[], &[system], None)?;
            (layout, StreamSource::System, None)
        };
        Ok(Self {
            layout,
            master,
            follower,
        })
    }

    /// The streams to open, master first.
    #[must_use]
    pub fn streams(&self) -> Vec<StreamSource> {
        std::iter::once(self.master).chain(self.follower).collect()
    }
}

/// A WASAPI duration (100 ns units, `REFERENCE_TIME`) in frames at
/// `sample_rate`, rounded to the nearest frame; 0 for a negative duration.
#[must_use]
pub fn frames_from_hundred_nanoseconds(duration: i64, sample_rate: f64) -> usize {
    if duration <= 0 {
        return 0;
    }
    (duration as f64 * sample_rate / 10_000_000.0).round() as usize
}

/// How many delivered frames [`DevicePositions`] measures the device
/// positions over before it decides whether to trust them: two seconds of
/// the stream's 48 kHz frames.
pub const POSITION_CALIBRATION_FRAMES: u64 = 2 * SAMPLE_RATE as u64;

/// What [`DevicePositions`] decided about a stream's device positions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PositionVerdict {
    /// Fewer than [`POSITION_CALIBRATION_FRAMES`] delivered: nothing is
    /// filled yet.
    Measuring,
    /// The positions advance by one per delivered frame, or by the engine's
    /// rate against 48 kHz, within 1 %: a discontinuity's jump is filled
    /// and counted.
    Fills,
    /// Neither, or the engine's rate is unknown: a discontinuity is counted
    /// and its loss shows only in the recording's length against wall time
    /// (the recorder's note reads that too).
    CountsOnly,
}

/// The device positions of one WASAPI stream (`GetBuffer`'s), read for the
/// audio its thread missed (A18, the design in
/// [`realtime::missed`](crate::realtime::missed)). Microsoft's
/// documentation leaves their unit open under `AUTOCONVERTPCM`: they may
/// count the 48 kHz frames the stream asked for or the engine's frames at
/// the shared-mode mix format's rate (`engine_rate`; `None` when the client
/// gives no mix format, as process loopback may). Read in the wrong unit,
/// a jump on a 16 kHz hands-free headset is written three times over, and
/// above 48 kHz a jump is missed. So the positions are measured first: over
/// the first [`POSITION_CALIBRATION_FRAMES`] of clean packets (no
/// discontinuity, a valid position), how far they advance against the
/// frames delivered. Within 1 % of one a frame, or of the engine's rate
/// against 48 kHz, the jumps are read in that unit with a [`CycleClock`]
/// and filled; otherwise, or with no mix format, nothing is filled
/// ([`PositionVerdict`]). The sums run on to the stop, which logs them.
/// Real-time safe: integer arithmetic on plain fields.
///
/// ```
/// use steno_audio::capture::split_streams::{DevicePositions, PositionVerdict};
///
/// // 10 ms packets of 48 kHz frames on a 44.1 kHz engine whose positions
/// // count 441 a packet: two seconds measured, then a packet flagged as a
/// // discontinuity half a second on.
/// let mut positions = DevicePositions::new(Some(44_100));
/// let mut position = 0;
/// for _ in 0..=200 {
///     assert!(positions.packet(Some(position), 480, false).is_none());
///     position += 441;
/// }
/// assert_eq!(positions.verdict(), PositionVerdict::Fills);
/// assert_eq!(positions.packet(Some(position + 22_050), 480, true).fill, 24_000);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DevicePositions {
    engine_rate: Option<u32>,
    verdict: PositionVerdict,
    /// Reads the jumps once the verdict is [`PositionVerdict::Fills`].
    clock: CycleClock,
    /// The last packet's position and frames; `None` before the first and
    /// after a position the engine flagged invalid.
    last: Option<(u64, usize)>,
    /// How far the positions advanced across clean packets, and the frames
    /// delivered across them.
    advanced: u64,
    delivered: u64,
}

impl DevicePositions {
    /// The positions of a stream on an engine at `engine_rate`.
    #[must_use]
    pub fn new(engine_rate: Option<u32>) -> Self {
        Self {
            engine_rate,
            verdict: PositionVerdict::Measuring,
            clock: CycleClock::new(SAMPLE_RATE),
            last: None,
            advanced: 0,
            delivered: 0,
        }
    }

    /// One packet of `frames` at `position` (`None` when the engine flagged
    /// it as invalid, `AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR`), flagged as a
    /// discontinuity or not: the frames missed before it, read only on a
    /// discontinuity and only once the positions are trusted.
    pub fn packet(&mut self, position: Option<u64>, frames: usize, discontinuity: bool) -> Missed {
        let Some(position) = position else {
            self.last = None;
            self.clock.reset();
            return Missed::default();
        };
        if let Some((last, last_frames)) = self.last.replace((position, frames))
            && !discontinuity
        {
            // A position behind the last one is no advance.
            let advanced = position.wrapping_sub(last);
            if advanced <= u64::MAX / 2 {
                self.advanced = self.advanced.saturating_add(advanced);
                self.delivered = self.delivered.saturating_add(last_frames as u64);
            }
        }
        match self.verdict {
            PositionVerdict::Measuring => {
                if self.delivered >= POSITION_CALIBRATION_FRAMES {
                    self.decide();
                    self.clock.advance(position, frames);
                }
                Missed::default()
            }
            PositionVerdict::Fills => {
                let missed = self.clock.advance(position, frames);
                if discontinuity {
                    missed
                } else {
                    Missed::default()
                }
            }
            PositionVerdict::CountsOnly => Missed::default(),
        }
    }

    /// Whether the jumps are filled.
    #[must_use]
    pub fn verdict(&self) -> PositionVerdict {
        self.verdict
    }

    /// The rate the client's mix format names, if it gave one.
    #[must_use]
    pub fn engine_rate(&self) -> Option<u32> {
        self.engine_rate
    }

    /// How far the positions advanced per delivered frame across the clean
    /// packets so far; `None` before two of them.
    #[must_use]
    pub fn ratio(&self) -> Option<f64> {
        (self.delivered > 0).then(|| self.advanced as f64 / self.delivered as f64)
    }

    /// Fills when the sums are within 1 % of one position a frame, or of
    /// the engine's rate against 48 kHz, read in that unit from here on.
    fn decide(&mut self) {
        let within = |measured: u128, expected: u128| measured.abs_diff(expected) * 100 <= expected;
        let (advanced, delivered) = (u128::from(self.advanced), u128::from(self.delivered));
        let scale = match self.engine_rate {
            Some(_) if within(advanced, delivered) => Some((1, 1)),
            Some(rate) if within(advanced * 48_000, delivered * u128::from(rate)) => {
                Some((SAMPLE_RATE as u64, u64::from(rate)))
            }
            _ => None,
        };
        self.verdict = match scale {
            Some((frames, ticks)) => {
                self.clock.rescale(frames, ticks);
                self.clock.reset();
                PositionVerdict::Fills
            }
            None => PositionVerdict::CountsOnly,
        };
    }
}

/// The stop's account of the positions: how far they advanced against the
/// frames delivered, the engine's rate, and the verdict.
///
/// ```
/// use steno_audio::capture::split_streams::DevicePositions;
///
/// let mut positions = DevicePositions::new(None);
/// positions.packet(Some(0), 480, false);
/// positions.packet(Some(160), 480, false);
/// assert_eq!(
///     positions.to_string(),
///     "device positions advanced 160 over 480 frames delivered (0.3333 a frame, \
///      no mix format), still measuring"
/// );
/// ```
impl std::fmt::Display for DevicePositions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "device positions advanced {} over {} frames delivered (",
            self.advanced, self.delivered
        )?;
        match self.ratio() {
            Some(ratio) => write!(f, "{ratio:.4} a frame, ")?,
            None => write!(f, "no ratio, ")?,
        }
        match self.engine_rate {
            Some(rate) => write!(f, "engine at {rate} Hz), ")?,
            None => write!(f, "no mix format), ")?,
        }
        f.write_str(match self.verdict {
            PositionVerdict::Measuring => "still measuring",
            PositionVerdict::Fills => "jumps filled",
            PositionVerdict::CountsOnly => "jumps not filled",
        })
    }
}

/// The input and output latencies a two-stream capture reports, in frames.
/// The session delays the system lane by their sum before echo
/// cancellation; the follower's staging already delays it by
/// `follower_delay` on the master's clock, so that much comes off the sum
/// (from the output side first). Overstating the delay would put the far
/// end after its echo, which the canceller cannot follow; understating it
/// stays inside the 200 ms tail.
#[must_use]
pub fn far_end_latencies(input: usize, output: usize, follower_delay: usize) -> (usize, usize) {
    let output_left = output.saturating_sub(follower_delay);
    let rest = follower_delay.saturating_sub(output);
    (input.saturating_sub(rest), output_left)
}

/// The shared-mode buffer a stream asks for, as a `REFERENCE_TIME` (100 ns
/// units): 100 ms, so a capture thread that is late by several periods
/// loses nothing. Microsoft's `Initialize` page asks event-driven
/// shared-mode clients for 0 here, while its own loopback sample passes a
/// duration; the engine treats it as a minimum either way, and the buffer
/// it chose is read back ([`stream_sizes`]).
pub const BUFFER_DURATION: i64 = 1_000_000;

/// The device periods trusted (`REFERENCE_TIME`, 100 ns units): 1 ms up to
/// the buffer asked for.
pub const TRUSTED_PERIODS: RangeInclusive<i64> = 10_000..=BUFFER_DURATION;

/// The period taken when the engine's is not trusted (`REFERENCE_TIME`,
/// 100 ns units): 10 ms, the usual one.
pub const DEFAULT_PERIOD: i64 = 100_000;

/// The stream latencies trusted (`REFERENCE_TIME`, 100 ns units): up to
/// 200 ms.
pub const TRUSTED_LATENCIES: RangeInclusive<i64> = 0..=2_000_000;

/// The largest buffer trusted, in frames: one second, so the follower's
/// staging holds what it queues while the master runs a whole buffer late.
pub const MAX_BUFFER_FRAMES: usize = FollowerLane::CAPACITY;

/// What a capture stream runs with, in frames at 48 kHz.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamSizes {
    /// The stream's buffer, the largest packet expected.
    pub buffer_frames: usize,
    /// Frames per packet: the engine's period, or a polled stream's poll.
    pub period_frames: usize,
    /// The stream's latency, 0 when not known.
    pub latency_frames: usize,
}

/// The sizes a stream runs with, from what its engine answered (`period`
/// and `latency` as `REFERENCE_TIME`, in 100 ns units):
/// `buffer_frames` from `GetBufferSize`, `period` from `GetDevicePeriod`
/// (a polled stream passes its poll interval), `latency` from
/// `GetStreamLatency`, `None` where the call failed. The process-loopback
/// client is reported to answer `GetBufferSize` with 0 or a huge value and
/// no error, and may implement neither of the others, so no answer is
/// taken on trust:
///
/// - a period outside [`TRUSTED_PERIODS`] is [`DEFAULT_PERIOD`] (the period
///   sizes the follower's jitter buffer);
/// - a latency outside [`TRUSTED_LATENCIES`] is 0, like an unknown one:
///   understating the far-end delay stays inside the echo canceller's
///   tail, overstating it does not ([`far_end_latencies`]);
/// - the buffer, which sizes the capture threads' scratch, is clamped to
///   between one period and [`MAX_BUFFER_FRAMES`] (a larger packet is
///   split, never refused).
///
/// ```
/// use steno_audio::capture::split_streams::{StreamSizes, stream_sizes};
///
/// // A 10 ms period, 30 ms of latency and a 100 ms buffer, as reported.
/// assert_eq!(
///     stream_sizes(4_800, Some(100_000), Some(300_000)),
///     StreamSizes { buffer_frames: 4_800, period_frames: 480, latency_frames: 1_440 },
/// );
/// ```
#[must_use]
pub fn stream_sizes(
    buffer_frames: usize,
    period: Option<i64>,
    latency: Option<i64>,
) -> StreamSizes {
    let period = period
        .filter(|period| TRUSTED_PERIODS.contains(period))
        .unwrap_or(DEFAULT_PERIOD);
    let period_frames = frames_from_hundred_nanoseconds(period, SAMPLE_RATE).max(1);
    let latency = latency
        .filter(|latency| TRUSTED_LATENCIES.contains(latency))
        .unwrap_or(0);
    StreamSizes {
        buffer_frames: buffer_frames.clamp(period_frames, MAX_BUFFER_FRAMES),
        period_frames,
        latency_frames: frames_from_hundred_nanoseconds(latency, SAMPLE_RATE),
    }
}
