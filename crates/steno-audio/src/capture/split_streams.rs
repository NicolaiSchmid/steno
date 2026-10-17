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
use crate::realtime::{FollowerLane, MISSED_FILL_LIMIT, Missed, Ratio};

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

/// How many delivered frames of clean packets [`DevicePositions`] measures
/// a stream's QPC over before it fills by it: two seconds of the stream's
/// 48 kHz frames.
pub const QPC_CALIBRATION_FRAMES: u64 = 2 * SAMPLE_RATE as u64;

/// The most any one reading of [`DevicePositions`] counts: an hour of
/// 48 kHz frames. A reading past it on a stream that kept delivering is a
/// garbage position or QPC; a real one (a computer that slept) is also wall
/// time the master lacks, which the recorder's note counts anyway.
pub const MOST_READ_FRAMES: u64 = 3_600 * SAMPLE_RATE as u64;

/// The QPC's 100 ns units in the stream's 48 kHz frames.
const QPC: Ratio = Ratio {
    frames: SAMPLE_RATE as u64,
    ticks: 10_000_000,
};

/// The longest jump [`DevicePositions`] fills, in 48 kHz frames
/// ([`MISSED_FILL_LIMIT`]).
const FILL_LIMIT: usize = SAMPLE_RATE as usize * MISSED_FILL_LIMIT.as_secs() as usize;

/// One packet as `IAudioCaptureClient::GetBuffer` hands it over.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Packet {
    /// The packet's 48 kHz frames.
    pub frames: usize,
    /// The device position of its first frame, in a unit Microsoft leaves
    /// open.
    pub position: u64,
    /// The performance counter at its first frame, in 100 ns units; 0 when
    /// the engine gave none.
    pub qpc: u64,
    /// `AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY`, the stream's first packet
    /// aside: the engine lost audio before it.
    pub discontinuity: bool,
    /// `AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR`: the time the position was
    /// taken at, the QPC, is not to be trusted.
    pub timestamp_error: bool,
}

/// One clock read at every packet: its last reading, and how far it
/// advanced over clean pairs of packets against the frames delivered
/// across them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct Counter {
    last: Option<u64>,
    advanced: u64,
    delivered: u64,
    /// The pairs in the sums.
    pairs: u64,
    /// The last advance read, whatever the packets' flags, and whether any
    /// was above zero (device positions only).
    last_advance: Option<u64>,
    moved: bool,
}

impl Counter {
    /// How far `now` is past the last reading; `None` without one, and for
    /// one behind it (a step back wraps past half the range).
    fn advance(&self, now: u64) -> Option<u64> {
        let advance = now.wrapping_sub(self.last?);
        (advance <= u64::MAX / 2).then_some(advance)
    }

    /// A clean pair: the clock advanced `advance` over `frames` delivered.
    fn pair(&mut self, advance: u64, frames: usize) {
        self.advanced = self.advanced.saturating_add(advance);
        self.delivered = self.delivered.saturating_add(frames as u64);
        self.pairs = self.pairs.saturating_add(1);
    }

    /// Whether positions that stood still from the start moved for the
    /// first time by `advance`, the advance before it none; they then start
    /// their sums afresh ([`DevicePositions::packet`]).
    fn first_move(&mut self, advance: Option<u64>) -> bool {
        let moving = advance.is_some_and(|advance| advance > 0);
        let first = moving && !self.moved && self.last_advance == Some(0);
        self.moved |= moving;
        self.last_advance = advance;
        if first {
            self.advanced = 0;
            self.delivered = 0;
            self.pairs = 0;
        }
        first
    }

    /// A clean pair of device positions, as [`Self::pair`], except that a
    /// second pair that advanced less than two thirds of what the first
    /// explains replaces it: nothing but the rate bounds the first pair (no
    /// ratio is measured yet), so an idle no packet flagged can pass in it.
    fn pair_positions(&mut self, advance: u64, frames: usize) {
        if self.pairs == 1
            && 3 * u128::from(advance) * u128::from(self.delivered)
                < 2 * u128::from(self.advanced) * frames as u128
        {
            self.advanced = 0;
            self.delivered = 0;
            self.pairs = 0;
        }
        self.pair(advance, frames);
    }

    /// The frames delivered per tick so far; `None` before the clock moved.
    /// The ticks are counted one short: a position read to the whole tick
    /// at each end puts the sums up to one tick off, so a ratio measured
    /// over a few packets reads a loss over, never under.
    fn measured(&self) -> Option<Ratio> {
        (self.advanced > 0).then_some(Ratio {
            frames: self.delivered,
            ticks: self.advanced.saturating_sub(1).max(1),
        })
    }
}

/// Whether `read` is within `percent` % of `frames`.
fn within(read: u64, frames: u64, percent: u64) -> bool {
    u128::from(read.abs_diff(frames)) * 100 <= u128::from(frames) * u128::from(percent)
}

/// What a WASAPI stream's thread missed (A18, the design in
/// [`realtime::missed`](crate::realtime::missed)), read from the clocks
/// `GetBuffer` gives with each packet ([`Packet`]). This is the one place
/// the rule is stated; the backend, the plans and the gate link here.
///
/// Two clocks come with a packet. The device position counts in a unit
/// Microsoft leaves open under `AUTOCONVERTPCM` (the 48 kHz frames the
/// stream asked for, or the engine's at the mix format's rate, `None` when
/// the client gives no mix format, as process loopback may), and on CI's
/// process loopback it never moves. The QPC counts 100 ns whatever the
/// device. So:
///
/// - **Filled only on a sane QPC.** The QPC is measured over the clean
///   pairs of packets (no discontinuity, no timestamp error on either, an
///   advance at most half a packet past the frames between them). Once
///   [`QPC_CALIBRATION_FRAMES`] are measured within 2 % of real time, and
///   while the last clean pair matched its frames within 2 %, a flagged
///   packet's QPC past the last packet's end is the loss: up to
///   [`MISSED_FILL_LIMIT`] filled, past it unfilled ([`Missed::of_jump`]).
/// - **Counted otherwise.** Without a sane QPC (none, one off by more than
///   2 %, still measuring, or a timestamp error on this packet or the last),
///   a flagged packet counts the largest of its readings past the last
///   packet's end, unfilled: the device position at one a frame, at the mix
///   format's rate against 48 kHz and at the ratio measured so far, and the
///   QPC while the last clean pair's matched its frames within 2 %. A
///   reading in the right unit is the loss, so the largest is the loss or
///   more: an overcount in the note, never wrong silence. Each reading is
///   capped at [`MOST_READ_FRAMES`].
/// - **What no reading sees.** Positions that never move with no sane QPC
///   read nothing; a position behind the last packet's or a unit that
///   neither 48 kHz nor the mix format explains, before its first clean
///   pair, reads too little. On the microphone (the master) the recorder's
///   note counts that loss as wall time the recording lacks, past the
///   clocks' drift allowance (0.1 %); on the system stream (the follower)
///   the zeros the master pads its lane with count it
///   ([`FollowerLane::take_missed`]).
/// - **At the stop.** A thread held up into the stop by an engine that
///   kept its oldest packets leaves no flagged packet. A stream that
///   delivers continuously reads the performance counter against its last
///   packet there ([`Self::missed_until`]), on a sane QPC. Without one,
///   the microphone's loss counts as above and the system stream's is
///   silent; endpoint loopback idles, so it is never read at the stop, and
///   its loss there is silent too. The Windows gate step checks both.
///
/// The device position's own readings stay in the count for one case: a
/// microphone stream without a sane QPC whose losses add up to less than
/// that drift allowance (3.6 s in an hour), which only they count. The
/// sums run on to the stop, which logs them. Real-time safe: integer
/// arithmetic on plain fields.
///
/// ```
/// use steno_audio::capture::split_streams::{DevicePositions, Packet};
///
/// // 10 ms packets with a QPC: two seconds measured, then a packet flagged
/// // as a discontinuity half a second on.
/// let mut positions = DevicePositions::new(Some(48_000));
/// let mut packet = Packet { frames: 480, qpc: 1_000_000_000, ..Packet::default() };
/// for _ in 0..=200 {
///     assert!(positions.packet(packet).is_none());
///     packet.position += 480;
///     packet.qpc += 100_000;
/// }
/// assert!(positions.fills());
/// packet.position += 24_000;
/// packet.qpc += 5_000_000;
/// packet.discontinuity = true;
/// assert_eq!(positions.packet(packet).fill, 24_000);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DevicePositions {
    engine_rate: Option<u32>,
    positions: Counter,
    qpc: Counter,
    /// The last clean pair's QPC advance (one inside the bound) matched its
    /// frames within 2 %.
    qpc_matched: bool,
    /// The last packet's frames and whether it had a timestamp error.
    last_frames: usize,
    last_timestamp_error: bool,
}

impl DevicePositions {
    /// The positions of a stream on an engine at `engine_rate`.
    #[must_use]
    pub fn new(engine_rate: Option<u32>) -> Self {
        Self {
            // A mix format at 0 Hz names no unit.
            engine_rate: engine_rate.filter(|rate| *rate > 0),
            positions: Counter::default(),
            qpc: Counter::default(),
            qpc_matched: false,
            last_frames: 0,
            last_timestamp_error: false,
        }
    }

    /// One packet: the frames missed before it, read only on a
    /// discontinuity (see the type's doc).
    pub fn packet(&mut self, packet: Packet) -> Missed {
        let last_frames = std::mem::replace(&mut self.last_frames, packet.frames);
        let position = self.positions.advance(packet.position);
        // Positions that stood still until now (process loopback that
        // starts in silence) measure from the pair after their first move:
        // the pair that spans it holds only part of an advance.
        let moved = self.positions.first_move(position);
        let qpc_now = (!packet.timestamp_error && packet.qpc != 0).then_some(packet.qpc);
        let qpc = qpc_now.and_then(|now| self.qpc.advance(now));
        let missed = if packet.discontinuity {
            self.count(position, qpc, last_frames)
        } else {
            if let Some(advance) = position
                && !moved
                && !packet.timestamp_error
                && !self.last_timestamp_error
                && advance <= self.most_advance(last_frames)
            {
                self.positions.pair_positions(advance, last_frames);
            }
            if let Some(advance) = qpc {
                let read = QPC.read(advance);
                if read <= last_frames as u64 + last_frames as u64 / 2 {
                    self.qpc_matched = within(read, last_frames as u64, 2);
                    self.qpc.pair(advance, last_frames);
                }
            }
            Missed::default()
        };
        self.positions.last = Some(packet.position);
        self.qpc.last = qpc_now;
        self.last_timestamp_error = packet.timestamp_error;
        missed
    }

    /// A flagged packet's loss: past where the last packet of `last_frames`
    /// ended, by its device position's advance `position` and its QPC's
    /// `qpc` (see the type's doc).
    fn count(&self, position: Option<u64>, qpc: Option<u64>, last_frames: usize) -> Missed {
        let gap = |read: u64| {
            read.saturating_sub(last_frames as u64)
                .min(MOST_READ_FRAMES)
        };
        if self.fills()
            && let Some(advance) = qpc
        {
            return Missed::of_jump(gap(QPC.read(advance)), last_frames, FILL_LIMIT);
        }
        let mix = self.engine_rate.map(|rate| Ratio {
            frames: SAMPLE_RATE as u64,
            ticks: u64::from(rate),
        });
        let qpc = qpc.filter(|_| self.qpc_matched && self.qpc_sane() != Some(false));
        let readings = [
            qpc.map(|advance| QPC.read(advance)),
            position.map(|advance| Ratio::ONE.read(advance)),
            position.zip(mix).map(|(advance, mix)| mix.read(advance)),
            position
                .zip(self.positions.measured())
                .map(|(advance, measured)| measured.read(advance)),
        ];
        let most = readings.into_iter().flatten().map(gap).max().unwrap_or(0);
        Missed::of_jump(most, last_frames, 0)
    }

    /// The most a clean packet of `frames` may advance the positions and
    /// still enter the sums: half a packet more than either unit explains,
    /// `(max(1, engine rate / 48 kHz) + 0.5) x frames`, and once the
    /// positions moved, at most 1.5 times what the ratio measured so far
    /// explains. A larger advance on a packet not flagged (endpoint loopback
    /// delivers nothing while nothing plays, and may not flag the packet
    /// after that) would skew the ratio, so it is left out.
    fn most_advance(&self, frames: usize) -> u64 {
        let per_frame = u64::from(self.engine_rate.unwrap_or(0).max(SAMPLE_RATE as u32));
        let by_rate = Ratio {
            frames: 2 * per_frame + SAMPLE_RATE as u64,
            ticks: 2 * SAMPLE_RATE as u64,
        }
        .read(frames as u64);
        let by_measure = self.positions.measured().map_or(u64::MAX, |measured| {
            Ratio {
                frames: 3 * measured.ticks,
                ticks: 2 * measured.frames,
            }
            .read(frames as u64)
            .saturating_add(1)
        });
        by_rate.min(by_measure)
    }

    /// Whether a jump is filled: the QPC is sane, measured within 2 % of
    /// real time and its last clean pair matched (see the type's doc).
    #[must_use]
    pub fn fills(&self) -> bool {
        self.qpc_matched && self.qpc_sane() == Some(true)
    }

    /// The frames missed after the last packet, for a stream stopped while
    /// the performance counter reads `now` (in the QPC's 100 ns units): how
    /// far `now` passed the end of the last packet, less one packet of its
    /// length (the one in flight at the stop), counted unfilled by
    /// [`Missed::of_jump`]'s rule and capped at [`MOST_READ_FRAMES`]. Only
    /// on a sane QPC ([`Self::fills`]) and a last packet that had one;
    /// nothing otherwise. Only for a stream that delivers continuously:
    /// endpoint loopback delivers nothing while nothing plays, so its idle
    /// would read as a loss. Changes nothing.
    ///
    /// ```
    /// use steno_audio::capture::split_streams::{DevicePositions, Packet};
    ///
    /// let mut positions = DevicePositions::new(Some(48_000));
    /// let mut packet = Packet { frames: 480, qpc: 1_000_000_000, ..Packet::default() };
    /// for _ in 0..=200 {
    ///     positions.packet(packet);
    ///     packet.qpc += 100_000;
    /// }
    /// // The stop half a second after the last packet's in-flight successor.
    /// let last = packet.qpc - 100_000;
    /// assert!(positions.missed_until(last + 200_000).is_none());
    /// assert_eq!(positions.missed_until(last + 5_200_000).unfilled, 24_000);
    /// ```
    #[must_use]
    pub fn missed_until(&self, now: u64) -> Missed {
        let Some(advance) = self.qpc.advance(now).filter(|_| self.fills()) else {
            return Missed::default();
        };
        let in_flight = 2 * self.last_frames as u64;
        let gap = QPC
            .read(advance)
            .saturating_sub(in_flight)
            .min(MOST_READ_FRAMES);
        Missed::of_jump(gap, self.last_frames, 0)
    }

    /// Whether the QPC measured within 2 % of real time; `None` before
    /// [`QPC_CALIBRATION_FRAMES`] are measured.
    fn qpc_sane(&self) -> Option<bool> {
        (self.qpc.delivered >= QPC_CALIBRATION_FRAMES)
            .then(|| within(QPC.read(self.qpc.advanced), self.qpc.delivered, 2))
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
        (self.positions.delivered > 0)
            .then(|| self.positions.advanced as f64 / self.positions.delivered as f64)
    }

    /// How far the QPC advanced, in 48 kHz frames, per delivered frame
    /// across the clean packets so far; `None` before two of them.
    #[must_use]
    pub fn qpc_ratio(&self) -> Option<f64> {
        (self.qpc.delivered > 0)
            .then(|| QPC.read(self.qpc.advanced) as f64 / self.qpc.delivered as f64)
    }
}

/// The stop's account of the clocks: how far the positions and the QPC
/// advanced against the frames delivered, the engine's rate, and whether
/// jumps were filled. A position ratio of 0.0000 is positions that never
/// moved.
///
/// ```
/// use steno_audio::capture::split_streams::{DevicePositions, Packet};
///
/// let mut positions = DevicePositions::new(None);
/// positions.packet(Packet { frames: 480, ..Packet::default() });
/// positions.packet(Packet { frames: 480, position: 160, ..Packet::default() });
/// assert_eq!(
///     positions.to_string(),
///     "device positions advanced 160 over 480 frames delivered (0.3333 a frame, \
///      no mix format), QPC no reading, jumps counted, not filled"
/// );
/// ```
impl std::fmt::Display for DevicePositions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "device positions advanced {} over {} frames delivered (",
            self.positions.advanced, self.positions.delivered
        )?;
        match self.ratio() {
            Some(ratio) => write!(f, "{ratio:.4} a frame, ")?,
            None => write!(f, "no ratio, ")?,
        }
        match self.engine_rate {
            Some(rate) => write!(f, "engine at {rate} Hz), ")?,
            None => write!(f, "no mix format), ")?,
        }
        match self.qpc_ratio() {
            Some(ratio) => write!(
                f,
                "QPC {} frames over {} ({ratio:.4} a frame), ",
                QPC.read(self.qpc.advanced),
                self.qpc.delivered
            )?,
            None => write!(f, "QPC no reading, ")?,
        }
        f.write_str(if self.fills() {
            "jumps filled"
        } else {
            "jumps counted, not filled"
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
