//! Which capture stream feeds which lane when the microphone and the system
//! audio arrive as two streams (WASAPI), the latency arithmetic for them,
//! and which of the engine's answers are trusted. Pure, so it is tested on
//! every OS; buffer 0 is the master stream's packet, buffer 1 the
//! follower's mono staging copy ([`FollowerLane`]).

use std::ops::RangeInclusive;

use steno_core::AudioLane;

use super::configuration::CaptureError;
use super::layout::StreamLayout;
use crate::SAMPLE_RATE;
use crate::realtime::{FollowerLane, MISSED_FILL_LIMIT, Missed, Ratio};

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

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            StreamSource::Microphone => "mic",
            StreamSource::System => "system",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SplitStreamPlan {
    pub layout: StreamLayout,
    /// The stream whose thread produces into the sink: the microphone when
    /// a lane needs it, else the system stream.
    pub master: StreamSource,
    /// The system stream as the follower, when both lanes are recorded.
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
            // The follower's mono staging copy sits in the place of a tap.
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
/// a stream's QPC over before it fills by it: two seconds.
pub const QPC_CALIBRATION_FRAMES: u64 = 2 * SAMPLE_RATE as u64;

/// The most any one reading of [`DevicePositions`] counts: an hour. A
/// larger reading on a stream that kept delivering is garbage.
pub const MOST_READ_FRAMES: u64 = 3_600 * SAMPLE_RATE as u64;

/// The QPC's 100 ns units in the stream's 48 kHz frames.
const QPC: Ratio = Ratio {
    frames: SAMPLE_RATE as u64,
    ticks: 10_000_000,
};

/// The longest jump [`DevicePositions`] fills ([`MISSED_FILL_LIMIT`]).
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
    /// `AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY`: the engine lost audio
    /// before it.
    pub discontinuity: bool,
    /// `AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR`: the QPC is not to be trusted.
    pub timestamp_error: bool,
}

/// One clock read at every packet: its last reading, and its advance over
/// clean pairs of packets against the frames delivered across them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct Counter {
    last: Option<u64>,
    advanced: u64,
    delivered: u64,
    pairs: u64,
    /// The last advance read, and whether any was above zero.
    last_advance: Option<u64>,
    moved: bool,
}

impl Counter {
    /// How far `now` is past the last reading; `None` without one or for one
    /// behind it.
    fn advance(&self, now: u64) -> Option<u64> {
        let advance = now.wrapping_sub(self.last?);
        (advance <= u64::MAX / 2).then_some(advance)
    }

    fn pair(&mut self, advance: u64, frames: usize) {
        self.advanced = self.advanced.saturating_add(advance);
        self.delivered = self.delivered.saturating_add(frames as u64);
        self.pairs = self.pairs.saturating_add(1);
    }

    /// Whether positions that stood still from the start moved for the
    /// first time by `advance`; their sums then start afresh.
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

    /// [`Self::pair`] for device positions, except that a second pair that
    /// advanced under two thirds of what the first explains replaces it:
    /// only the rate bounds the first pair, so an idle can pass in it.
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
    /// Ticks are counted one short, so a ratio over few packets reads a
    /// loss over, never under.
    fn measured(&self) -> Option<Ratio> {
        (self.advanced > 0).then_some(Ratio {
            frames: self.delivered,
            ticks: self.advanced.saturating_sub(1).max(1),
        })
    }
}

fn within(read: u64, frames: u64, percent: u64) -> bool {
    u128::from(read.abs_diff(frames)) * 100 <= u128::from(frames) * u128::from(percent)
}

/// What a WASAPI stream's thread missed, read from the clocks `GetBuffer`
/// gives with each packet ([`Packet`]): the device position, in a unit
/// Microsoft leaves open (and frozen on some process loopback), and the
/// QPC, which counts 100 ns. This is the one place the rule is stated.
///
/// - **Filled only on a sane QPC:** measured over clean pairs of packets
///   within 2 % of real time for [`QPC_CALIBRATION_FRAMES`], with the last
///   clean pair within half a packet. A flagged packet's QPC past the last
///   packet's end is then the loss: up to [`MISSED_FILL_LIMIT`] filled,
///   past it unfilled ([`Missed::of_jump`]).
/// - **Counted otherwise:** a flagged packet counts the largest of its
///   readings past the last packet's end, unfilled (the position at one a
///   frame, at the mix format's rate and at the measured ratio, and the QPC
///   while its last clean pair matched): an overcount in the note, never
///   wrong silence. Each reading is capped at [`MOST_READ_FRAMES`].
/// - **What no reading sees:** frozen positions without a sane QPC read
///   nothing. On the microphone the recorder's note counts it as wall time
///   the recording lacks; on the system stream the master's padding zeros
///   count it ([`FollowerLane::take_missed`]).
/// - **At the stop and before the first packet:** a stream that delivers
///   continuously reads the QPC against its last packet
///   ([`Self::missed_until`]) and its start against its first packet
///   ([`Self::missed_before_first`]), on a sane QPC. Endpoint loopback
///   idles, so it reads neither.
///
/// Real-time safe: integer arithmetic on plain fields.
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
    /// The last clean pair's QPC advance matched its frames to within half
    /// a packet, so jitter on one pair leaves it on and a stalled counter
    /// turns it off.
    qpc_matched: bool,
    /// The last packet's frames and whether it had a timestamp error.
    last_frames: usize,
    last_timestamp_error: bool,
    frames: u64,
    start: Start,
}

/// [`DevicePositions`]' reading of its start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Start {
    /// No start given.
    None,
    /// The QPC just before the stream started, and the frames delivered
    /// before a packet with a QPC came.
    Waiting { qpc: u64, frames: u64 },
    /// How far the first packet with a QPC came after the start, in frames
    /// past those delivered before it.
    Read(u64),
}

impl DevicePositions {
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
            frames: 0,
            start: Start::None,
        }
    }

    /// The QPC read just before the stream started, for
    /// [`Self::missed_before_first`]; only for a stream that delivers
    /// continuously.
    pub fn start(&mut self, qpc: u64) {
        self.start = Start::Waiting { qpc, frames: 0 };
    }

    /// One packet: the frames missed before it, read only on a
    /// discontinuity.
    pub fn packet(&mut self, packet: Packet) -> Missed {
        let last_frames = std::mem::replace(&mut self.last_frames, packet.frames);
        self.frames = self.frames.saturating_add(packet.frames as u64);
        let position = self.positions.advance(packet.position);
        // Positions that stood still until now measure from the pair after
        // their first move.
        let moved = self.positions.first_move(position);
        let qpc_now = (!packet.timestamp_error && packet.qpc != 0).then_some(packet.qpc);
        if let Start::Waiting { qpc, frames } = self.start {
            self.start = match qpc_now {
                Some(now) => Start::Read(QPC.read(now.saturating_sub(qpc)).saturating_sub(frames)),
                None => Start::Waiting {
                    qpc,
                    frames: frames.saturating_add(packet.frames as u64),
                },
            };
        }
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
                    self.qpc_matched = read.abs_diff(last_frames as u64) <= last_frames as u64 / 2;
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

    /// A flagged packet's loss past where the last packet ended, by its
    /// position advance and its QPC advance.
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
    /// and once they moved, at most 1.5 times the measured ratio. Endpoint
    /// loopback may skip silence without a flag, which would skew the ratio.
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

    /// Whether a jump is filled: the QPC is sane and its last clean pair
    /// matched.
    #[must_use]
    pub fn fills(&self) -> bool {
        self.qpc_matched && self.qpc_sane() == Some(true)
    }

    /// The frames missed after the last packet, for a stream stopped at QPC
    /// `now`: how far `now` passed the last packet's end, less the packet in
    /// flight, unfilled and capped at [`MOST_READ_FRAMES`]. Only on a sane
    /// QPC ([`Self::fills`]) and for a stream that delivers continuously.
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

    /// The frames missed before the first packet: how far the first packet
    /// with a QPC came after [`Self::start`], past the frames before it,
    /// beyond the start latency (the stream's latency and one period), all
    /// unfilled and capped at [`MOST_READ_FRAMES`]. Only on a sane QPC
    /// ([`Self::qpc_sane`]).
    ///
    /// ```
    /// use steno_audio::capture::split_streams::{DevicePositions, Packet, StreamSizes};
    ///
    /// let sizes = StreamSizes { buffer_frames: 4_800, period_frames: 480, latency_frames: 480 };
    /// let mut positions = DevicePositions::new(Some(48_000));
    /// positions.start(1_000_000_000);
    /// // The first packet half a second after the start.
    /// let mut packet = Packet { frames: 480, qpc: 1_005_000_000, ..Packet::default() };
    /// for _ in 0..=200 {
    ///     positions.packet(packet);
    ///     packet.qpc += 100_000;
    /// }
    /// assert_eq!(positions.missed_before_first(sizes), 24_000 - 960);
    /// ```
    #[must_use]
    pub fn missed_before_first(&self, sizes: StreamSizes) -> usize {
        let (Start::Read(late), Some(true)) = (self.start, self.qpc_sane()) else {
            return 0;
        };
        let latency = sizes.latency_frames.saturating_add(sizes.period_frames);
        let gap = late.saturating_sub(latency as u64).min(MOST_READ_FRAMES);
        Missed::of_jump(gap, sizes.period_frames, 0).unfilled
    }

    /// Whether the QPC measured within 2 % of real time; `None` before
    /// [`QPC_CALIBRATION_FRAMES`] are measured.
    #[must_use]
    pub fn qpc_sane(&self) -> Option<bool> {
        (self.qpc.delivered >= QPC_CALIBRATION_FRAMES)
            .then(|| within(QPC.read(self.qpc.advanced), self.qpc.delivered, 2))
    }

    /// The stop's verdict on the QPC: [`Self::qpc_sane`], but `Some(false)`
    /// for a stream that delivered [`QPC_CALIBRATION_FRAMES`] with too few
    /// clean QPC pairs to judge. For the stop's log line.
    #[must_use]
    pub fn qpc_judged(&self) -> Option<bool> {
        self.qpc_sane()
            .or((self.frames >= QPC_CALIBRATION_FRAMES).then_some(false))
    }

    /// The rate the client's mix format names, if it gave one.
    #[must_use]
    pub fn engine_rate(&self) -> Option<u32> {
        self.engine_rate
    }

    /// The positions' advance per delivered frame over clean packets;
    /// `None` before two.
    #[must_use]
    pub fn ratio(&self) -> Option<f64> {
        (self.positions.delivered > 0)
            .then(|| self.positions.advanced as f64 / self.positions.delivered as f64)
    }

    /// The QPC's advance in 48 kHz frames per delivered frame over clean
    /// packets; `None` before two.
    #[must_use]
    pub fn qpc_ratio(&self) -> Option<f64> {
        (self.qpc.delivered > 0)
            .then(|| QPC.read(self.qpc.advanced) as f64 / self.qpc.delivered as f64)
    }
}

/// The stop's account of the clocks. A position ratio of 0.0000 is
/// positions that never moved.
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

/// The input and output latencies a two-stream capture reports, in frames,
/// less the `follower_delay` the follower's staging already adds (from the
/// output side first). Overstating the delay would put the far end after
/// its echo, which the canceller cannot follow.
#[must_use]
pub fn far_end_latencies(input: usize, output: usize, follower_delay: usize) -> (usize, usize) {
    let output_left = output.saturating_sub(follower_delay);
    let rest = follower_delay.saturating_sub(output);
    (input.saturating_sub(rest), output_left)
}

/// The shared-mode buffer a stream asks for (100 ns units): 100 ms, so a
/// thread late by several periods loses nothing. The engine treats it as a
/// minimum; the buffer it chose is read back ([`stream_sizes`]).
pub const BUFFER_DURATION: i64 = 1_000_000;

/// The device periods trusted (100 ns units): 1 ms up to the buffer.
pub const TRUSTED_PERIODS: RangeInclusive<i64> = 10_000..=BUFFER_DURATION;

/// The period taken when the engine's is not trusted (100 ns units): 10 ms.
pub const DEFAULT_PERIOD: i64 = 100_000;

/// The stream latencies trusted (100 ns units): up to 200 ms.
pub const TRUSTED_LATENCIES: RangeInclusive<i64> = 0..=2_000_000;

/// The largest buffer trusted, in frames: one second, what the follower's
/// staging holds.
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

/// The sizes a stream runs with, from its engine's answers (`period` and
/// `latency` in 100 ns units, `None` where the call failed). Process
/// loopback may answer nonsense without an error, so nothing is trusted:
///
/// - a period outside [`TRUSTED_PERIODS`] is [`DEFAULT_PERIOD`];
/// - a latency outside [`TRUSTED_LATENCIES`] is 0: understating the
///   far-end delay stays inside the echo canceller's tail
///   ([`far_end_latencies`]);
/// - the buffer is clamped between one period and [`MAX_BUFFER_FRAMES`]
///   (a larger packet is split).
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
