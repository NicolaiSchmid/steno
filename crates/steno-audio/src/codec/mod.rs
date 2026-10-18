//! The program's [`AudioDecoder`] for real recordings, in pure Rust:
//! `decode` returns the lane's 16 kHz sidecar when it matches the master,
//! else streams the master (the crate's CAF reader, `symphonia` for CAF,
//! WAV and m4a/AAC) and resamples one channel to 16 kHz mono. AAC priming
//! is trimmed, damaged packets become silence, and `mixdown` writes WAV.

pub mod priming;
pub mod resample;
pub mod salvage;
pub mod sinc;
mod sizes;

use std::path::{Path, PathBuf};

use steno_core::{
    AudioAsset, AudioBuffer16k, AudioDamage, AudioDecoder, AudioFormat, AudioLane, BoundaryResult,
    async_trait, crash_log, paths::file_url_path,
};
use symphonia::core::audio::{AudioBufferRef, SampleBuffer};
use symphonia::core::codecs::{CODEC_TYPE_AAC, CodecParameters, Decoder, DecoderOptions};
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::{FormatOptions, FormatReader, Packet};
use symphonia::core::io::{MediaSourceStream, MediaSourceStreamOptions};
use symphonia::core::units::TimeBase;

use priming::{APPLE_PRIMING, Priming};
pub use resample::LaneResampler;
use resample::length_at_16k;
use salvage::SalvagedPackets;

use crate::writer::caf::CafReader;
use crate::writer::{WavFile, WavStreamWriter};

/// Why a decode or mixdown failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CodecError {
    #[error("the asset has no {} lane", .0.as_str())]
    LaneNotInAsset(AudioLane),
    #[error("lane {} is channel {channel} but the file has {channels}", lane.as_str())]
    ChannelMissing {
        lane: AudioLane,
        channel: usize,
        channels: usize,
    },
    #[error("unsupported audio format: {0}")]
    UnsupportedFormat(String),
    /// Not an MP4, CAF or WAV file from its first byte; refused before
    /// symphonia reads a byte.
    #[error("Steno cannot read this kind of file; the recording is kept.")]
    UnknownFormat,
    #[error("audio conversion failed: {0}")]
    ConversionFailed(String),
    #[error("{0}")]
    Io(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct DecodedChannel {
    pub sample_rate: u32,
    /// Channels the file has; this is one of them.
    pub channels: usize,
    pub samples: Vec<f32>,
    pub damage: AudioDamage,
}

/// The share of a file's packets that may be damaged: more fails with
/// [`CodecError::ConversionFailed`] rather than decode to a meeting of
/// silence; a failed one keeps its recording and offers Process again.
pub const MAX_DAMAGED_SHARE: f64 = 0.5;

#[derive(Debug, Clone, Copy, Default)]
pub struct SymphoniaAudioCodec;

impl SymphoniaAudioCodec {
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Channel `channel` of the file at `path`, resampled to 16 kHz mono
    /// as it is read; `lane` names the channel in the error when the file
    /// lacks it.
    ///
    /// ```no_run
    /// use std::path::Path;
    ///
    /// use steno_audio::SymphoniaAudioCodec;
    /// use steno_core::AudioLane;
    ///
    /// let lane = SymphoniaAudioCodec::decode_path(Path::new("phone.m4a"), 0, AudioLane::Mixed)?;
    /// println!("{} samples at 16 kHz", lane.len());
    /// # Ok::<(), steno_audio::CodecError>(())
    /// ```
    pub fn decode_path(
        path: &Path,
        channel: usize,
        lane: AudioLane,
    ) -> Result<AudioBuffer16k, CodecError> {
        let mut frames = Frames::open(path)?;
        let mut decode = LaneDecode::new(channel);
        let spec = frames.stream(|event| {
            decode.take(event);
            Ok(())
        })?;
        spec.check_channel(channel, lane)?;
        Ok(AudioBuffer16k {
            damage: frames.damage(),
            ..AudioBuffer16k::new(decode.finish())
        })
    }

    /// One channel at the source rate, whole: for the tests and the bench
    /// tools.
    pub fn read_channel(
        path: &Path,
        channel: usize,
        lane: AudioLane,
    ) -> Result<DecodedChannel, CodecError> {
        let mut frames = Frames::open(path)?;
        let mut samples = Vec::new();
        let spec = frames.stream(|event| {
            match event {
                // At the source rate, so a rate change cannot be kept
                // whole: the last shape wins.
                Event::Start(spec) => samples = Vec::with_capacity(reserved_frames(spec)),
                Event::Frames(spec, block) => {
                    if channel < spec.channels {
                        samples.extend(block.iter().skip(channel).step_by(spec.channels));
                    }
                }
            }
            Ok(())
        })?;
        spec.check_channel(channel, lane)?;
        Ok(DecodedChannel {
            sample_rate: spec.rate,
            channels: spec.channels,
            samples,
            damage: frames.damage(),
        })
    }

    /// Seconds in the file at `path` as its container declares them, read
    /// without decoding it; `None` when the container does not say. Capped
    /// at five hours, so a corrupt header cannot move a start absurdly far
    /// back. For a recording that has no stored duration.
    pub fn declared_duration(path: &Path) -> Result<Option<f64>, CodecError> {
        let frames = SymphoniaFrames::open(path)?;
        let primed = frames.priming.map_or(0, |trim| trim.frames);
        Ok(frames
            .frames
            .zip(frames.params.sample_rate.filter(|rate| *rate > 0))
            .map(|(declared, rate)| {
                capped(declared.saturating_sub(primed), rate) as f64 / f64::from(rate)
            }))
    }

    /// `samples` at `rate` to 16 kHz in one pass: exact length `round(len *
    /// 16000 / rate)`, so the 16 kHz lane lasts exactly as long as the
    /// master.
    ///
    /// # Panics
    ///
    /// When `rate` is zero.
    #[must_use]
    pub fn to_16k(samples: &[f32], rate: u32) -> Vec<f32> {
        let mut resampler = LaneResampler::new(rate);
        let mut output = Vec::with_capacity(frames_at_16k(samples.len(), rate));
        resampler.push(samples.iter().copied(), &mut output);
        resampler.finish(&mut output);
        output
    }

    /// Averages every channel of `source` to mono and writes 16 kHz Int16
    /// WAV to `destination`, a block at a time. A failure removes what was
    /// written.
    pub fn mixdown_path(source: &Path, destination: &Path) -> Result<(), CodecError> {
        Self::mix(Frames::open(source)?, destination)
    }

    fn mix(mut frames: Frames, destination: &Path) -> Result<(), CodecError> {
        create_parent(destination)?;
        let mut mixdown = Mixdown::new(destination);
        let result = frames
            .stream(|event| mixdown.take(event))
            .and_then(|_| mixdown.finish());
        if result.is_err()
            && let Some(writer) = mixdown.writer.take()
        {
            // Closed first, which Windows needs to remove it.
            drop(writer);
            let _ = std::fs::remove_file(destination);
        }
        result
    }
}

/// The 16 kHz samples to reserve for `frames` at `rate`: the exact length
/// plus a second, so the filter's tail and a frame count a little short
/// never grow the buffer.
fn frames_at_16k(frames: usize, rate: u32) -> usize {
    length_at_16k(frames, rate) + AudioBuffer16k::SAMPLE_RATE as usize
}

/// `frames` a container declares at `rate`, capped at five hours: a
/// corrupt header must not claim more.
fn capped(frames: u64, rate: u32) -> u64 {
    const MAX_DECLARED_SECONDS: u64 = 5 * 3_600;
    frames.min(MAX_DECLARED_SECONDS * u64::from(rate))
}

/// The frames to reserve for a stream: its length, but a declared length
/// is capped at five hours (a corrupt header must not reserve gigabytes,
/// and a failed allocation aborts the app). A measured length is not.
fn reserved_frames(spec: Spec) -> usize {
    match spec.length {
        Length::Measured(frames) => frames,
        Length::Declared(frames) => {
            usize::try_from(capped(frames.unwrap_or(0), spec.rate)).unwrap_or(0)
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Spec {
    rate: u32,
    channels: usize,
    length: Length,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Length {
    /// From the file's size: the crate's CAF reader.
    Measured(usize),
    /// From the container's header, if it says: not trusted.
    Declared(Option<u64>),
}

impl Spec {
    fn audible(self) -> bool {
        self.channels > 0 && self.rate > 0
    }

    fn check_channel(self, channel: usize, lane: AudioLane) -> Result<(), CodecError> {
        if channel < self.channels {
            return Ok(());
        }
        Err(CodecError::ChannelMissing {
            lane,
            channel,
            channels: self.channels,
        })
    }
}

/// What [`Frames::stream`] hands its callback.
#[derive(Clone, Copy)]
enum Event<'a> {
    /// The stream begins, or goes on with a new shape: what was taken so
    /// far is finished at the old one.
    Start(Spec),
    Frames(Spec, &'a [f32]),
}

/// A file's frames, block by block: the crate's CAF reader first, so an
/// unfinished master reads to its last whole frame, symphonia for
/// everything else and for a CAF the crate's reader cannot read.
enum Frames {
    Caf { reader: CafReader, block: Vec<f32> },
    Symphonia(Box<SymphoniaFrames>),
}

impl Frames {
    const CAF_BLOCK: usize = 32_768;

    fn open(path: &Path) -> Result<Self, CodecError> {
        if path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("caf"))
            && let Ok(reader) = CafReader::open(path)
        {
            return Ok(Self::Caf {
                reader,
                block: Vec::new(),
            });
        }
        Ok(Self::Symphonia(Box::new(SymphoniaFrames::open(path)?)))
    }

    /// Runs `each` over the whole file and returns the stream's last
    /// shape. A change of shape mid-stream is a new `Start`: each shape's
    /// samples are resampled at their own rate.
    fn stream(
        &mut self,
        mut each: impl FnMut(Event<'_>) -> Result<(), CodecError>,
    ) -> Result<Spec, CodecError> {
        match self {
            Self::Caf { reader, block } => {
                let spec = Spec {
                    rate: reader.sample_rate() as u32,
                    channels: reader.channel_count(),
                    length: Length::Measured(reader.frame_count()),
                };
                if spec.rate == 0 {
                    return Err(CodecError::UnsupportedFormat("a CAF at 0 Hz".into()));
                }
                each(Event::Start(spec))?;
                while reader
                    .read_frames(Self::CAF_BLOCK, block)
                    .map_err(|e| CodecError::Io(e.to_string()))?
                    > 0
                {
                    each(Event::Frames(spec, block))?;
                }
                Ok(spec)
            }
            Self::Symphonia(frames) => frames.stream(each),
        }
    }

    /// What [`stream`](Self::stream) replaced by silence.
    fn damage(&self) -> AudioDamage {
        match self {
            Self::Caf { .. } => AudioDamage::default(),
            Self::Symphonia(frames) => AudioDamage {
                parts: u32::try_from(frames.damage.parts).unwrap_or(u32::MAX),
                seconds: frames.damage.seconds,
                salvaged: matches!(frames.source, Source::Salvaged(_)),
            },
        }
    }
}

/// Symphonia's demuxer, or the salvage's packets, and symphonia's decoder
/// over one file.
struct SymphoniaFrames {
    path: PathBuf,
    source: Source,
    decoder: Box<dyn Decoder>,
    /// The track's codec parameters, which a fresh decoder is made from
    /// after a damaged packet.
    params: CodecParameters,
    track_id: u32,
    frames: Option<u64>,
    priming: Option<Trim>,
    /// The frames of the last packet that decoded, the length of a damaged
    /// packet whose container gives none.
    last_frames: Option<usize>,
    damage: Damage,
}

/// A track's packets read, how many of them could not be decoded, and how
/// long the silence they became lasts.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct Damage {
    packets: u64,
    damaged: u64,
    /// The damaged packets some of whose silence the priming trim left.
    parts: u64,
    seconds: f64,
}

impl Damage {
    fn too_much(self) -> bool {
        #[allow(clippy::cast_precision_loss)]
        let share = self.damaged as f64 / self.packets.max(1) as f64;
        share > MAX_DAMAGED_SHARE
    }
}

#[derive(Default)]
struct Stream {
    spec: Option<Spec>,
    zeros: Vec<f32>,
    leading: Option<Leading>,
}

/// Damaged packets before the first one that decoded, held until that
/// packet gives the stream's shape: symphonia's MP4 reader declares no
/// channel count for AAC.
#[derive(Debug, Clone, Copy)]
struct Leading {
    ts: u64,
    /// Their frames at the declared rate (else at the decoded one).
    frames: usize,
}

/// The frames before a track's first presented sample, and how to place a
/// packet against them.
#[derive(Debug, Clone, Copy)]
struct Trim {
    frames: u64,
    rate: u32,
    time_base: TimeBase,
}

impl Trim {
    /// For AAC in an MP4 file; see [`priming`].
    fn for_track(path: &Path, params: &CodecParameters) -> Option<Self> {
        if params.codec != CODEC_TYPE_AAC {
            return None;
        }
        let priming = Priming::read(path)?;
        let rate = params.sample_rate?;
        let Some(frames) = priming.frames(rate) else {
            let reason = if rate == 0 || priming.timescale == 0 {
                "the AAC track names no rate or no timescale"
            } else {
                "the AAC track starts later than any priming"
            };
            tracing::warn!(
                ?priming,
                rate,
                "{reason}; decoding it from its first sample"
            );
            return None;
        };
        Some(Self {
            frames,
            rate,
            time_base: params.time_base?,
        })
    }

    /// How many of the `frames` a packet at `ts` decodes to fall before
    /// the first presented sample. By timestamp rather than by count, so a
    /// packet skipped as corrupt does not move the cut into the audio.
    fn frames_before(self, ts: u64, frames: usize) -> usize {
        let numerator = u128::from(ts) * u128::from(self.time_base.numer) * u128::from(self.rate);
        let start = numerator / u128::from(self.time_base.denom.max(1));
        let before = u128::from(self.frames).saturating_sub(start);
        usize::try_from(before).map_or(frames, |before| before.min(frames))
    }
}

const LOGGED_DAMAGED_PACKETS: u64 = 10;

/// The most frames of silence one damaged packet becomes, so a corrupt
/// duration cannot grow the lane.
const MAX_PACKET_FRAMES: usize = 8_192;

const FALLBACK_PACKET_FRAMES: usize = 1_024;

/// A packet's length in frames at `rate` from its duration `duration` in
/// `time_base` (in frames when the track names no unit): `fallback` when
/// the duration or the rate is 0, at most [`MAX_PACKET_FRAMES`].
fn packet_frames(duration: u64, time_base: Option<TimeBase>, rate: u32, fallback: usize) -> usize {
    let duration = u128::from(duration);
    let frames = time_base.map_or(duration, |base| {
        duration * u128::from(base.numer) * u128::from(rate) / u128::from(base.denom.max(1))
    });
    if frames == 0 || rate == 0 {
        return fallback.min(MAX_PACKET_FRAMES);
    }
    usize::try_from(frames).map_or(MAX_PACKET_FRAMES, |frames| frames.min(MAX_PACKET_FRAMES))
}

fn emit(
    each: &mut impl FnMut(Event<'_>) -> Result<(), CodecError>,
    spec: &mut Option<Spec>,
    shape: Spec,
    samples: &[f32],
) -> Result<(), CodecError> {
    if *spec != Some(shape) {
        *spec = Some(shape);
        if shape.audible() {
            each(Event::Start(shape))?;
        }
    }
    if shape.audible() && !samples.is_empty() {
        each(Event::Frames(shape, samples))?;
    }
    Ok(())
}

/// `frames` of silence of `shape` handed to `each` through `zeros`, at
/// most [`MAX_PACKET_FRAMES`] at a time; returns the seconds they last.
fn emit_silence(
    each: &mut impl FnMut(Event<'_>) -> Result<(), CodecError>,
    spec: &mut Option<Spec>,
    shape: Spec,
    frames: usize,
    zeros: &mut Vec<f32>,
) -> Result<f64, CodecError> {
    let mut left = frames;
    loop {
        let block = left.min(MAX_PACKET_FRAMES);
        let length = block * shape.channels;
        if zeros.len() < length {
            zeros.resize(length, 0.0);
        }
        emit(each, spec, shape, &zeros[..length])?;
        left -= block;
        if left == 0 {
            break;
        }
    }
    #[allow(clippy::cast_precision_loss)]
    let seconds = frames as f64 / f64::from(shape.rate.max(1));
    Ok(seconds)
}

/// `packet` through `decoder`, a panic inside it caught as a failure. The
/// caller replaces the decoder after any failure, so the state a panic
/// left half-written is never used again. The panic is
/// [`crash_log::expected`], so the app's panic hooks write no crash log.
fn decode_packet<'d>(
    decoder: &'d mut Box<dyn Decoder>,
    packet: &Packet,
) -> Result<AudioBufferRef<'d>, String> {
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
        crash_log::expected(move || {
            let decoder = decoder;
            decoder.decode(packet)
        })
    }));
    match outcome {
        Ok(Ok(audio)) => Ok(audio),
        Ok(Err(error)) => Err(error.to_string()),
        Err(panic) => Err(format!(
            "the decoder panicked: {}",
            crash_log::panic_message(panic.as_ref())
        )),
    }
}

enum Source {
    Format(Box<dyn FormatReader>),
    Salvaged(Box<SalvagedPackets>),
}

impl Source {
    fn next_packet(&mut self, path: &Path) -> Result<Option<Packet>, CodecError> {
        match self {
            Self::Format(format) => match format.next_packet() {
                Ok(packet) => Ok(Some(packet)),
                Err(SymphoniaError::IoError(e))
                    if e.kind() == std::io::ErrorKind::UnexpectedEof =>
                {
                    Ok(None)
                }
                Err(SymphoniaError::ResetRequired) => Ok(None),
                Err(e) => Err(CodecError::ConversionFailed(e.to_string())),
            },
            Self::Salvaged(packets) => packets.next_packet().map_err(|e| io_error(path, &e)),
        }
    }
}

impl SymphoniaFrames {
    /// Symphonia's demuxer over the file at `path`; a phone recording that
    /// it cannot read for want of a `moov` is salvaged ([`salvage`]).
    fn open(path: &Path) -> Result<Self, CodecError> {
        match Self::demux(path) {
            Ok(frames) => Ok(frames),
            Err(error) => match salvage(path) {
                Some(packets) => {
                    tracing::warn!(
                        path = %path.display(),
                        %error,
                        "the recording has no index, as the phone's recorder leaves one it was \
                         killed in; reading its packets without one"
                    );
                    Self::salvaged(path, packets)
                }
                None => Err(error),
            },
        }
    }

    /// The salvage's packets with the phone's settings, less
    /// [`APPLE_PRIMING`].
    fn salvaged(path: &Path, packets: SalvagedPackets) -> Result<Self, CodecError> {
        let params = SalvagedPackets::codec_params();
        Ok(Self {
            path: path.to_path_buf(),
            source: Source::Salvaged(Box::new(packets)),
            decoder: make_decoder(&params)?,
            track_id: salvage::TRACK_ID,
            frames: None,
            priming: Some(Trim {
                frames: APPLE_PRIMING,
                rate: salvage::SAMPLE_RATE,
                time_base: TimeBase::new(1, salvage::SAMPLE_RATE),
            }),
            last_frames: None,
            damage: Damage::default(),
            params,
        })
    }

    fn demux(path: &Path) -> Result<Self, CodecError> {
        // Before symphonia sees a byte: a size it would allocate that the
        // file cannot hold aborts the process.
        let (start, file) = sizes::check(path)?;
        let stream = MediaSourceStream::new(Box::new(file), MediaSourceStreamOptions::default());
        // A panic in the demuxer (symphonia 0.5.5's MP4 reader has
        // unchecked box arithmetic) is caught as its failure. The size check
        // above refuses the known cases; the catch is for the rest.
        let format = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            crash_log::expected(|| demuxer(start, stream))
        }))
        .map_err(|panic| {
            format!(
                "the demuxer panicked: {}",
                crash_log::panic_message(panic.as_ref())
            )
        })
        .and_then(|format| format.map_err(|e| e.to_string()))
        .map_err(|e| CodecError::UnsupportedFormat(format!("{}: {e}", path.display())))?;
        let track = format
            .default_track()
            .ok_or_else(|| CodecError::UnsupportedFormat("no audio track".into()))?;
        let track_id = track.id;
        let params = track.codec_params.clone();
        let decoder = make_decoder(&params)?;
        Ok(Self {
            path: path.to_path_buf(),
            source: Source::Format(format),
            decoder,
            track_id,
            frames: params.n_frames,
            priming: Trim::for_track(path, &params),
            last_frames: None,
            damage: Damage::default(),
            params,
        })
    }

    /// Packet by packet. A packet that does not decode becomes silence of
    /// its length; a file with no decodable audio, or with more damaged
    /// packets than [`MAX_DAMAGED_SHARE`] allows, is an error.
    fn stream(
        &mut self,
        mut each: impl FnMut(Event<'_>) -> Result<(), CodecError>,
    ) -> Result<Spec, CodecError> {
        let mut stream = Stream::default();
        let mut buffer: Option<SampleBuffer<f32>> = None;
        self.damage = Damage::default();
        while let Some(packet) = self.source.next_packet(&self.path)? {
            if packet.track_id() != self.track_id {
                continue;
            }
            let index = self.damage.packets;
            self.damage.packets += 1;
            match decode_packet(&mut self.decoder, &packet) {
                Ok(audio) => {
                    let frames = audio.frames();
                    let shape = *audio.spec();
                    let channels = shape.channels.count();
                    let buffer = buffer.get_or_insert_with(|| {
                        SampleBuffer::<f32>::new(audio.capacity() as u64, shape)
                    });
                    if buffer.capacity() < audio.capacity() * channels {
                        *buffer = SampleBuffer::<f32>::new(audio.capacity() as u64, shape);
                    }
                    buffer.copy_interleaved_ref(audio);
                    let decoded = Spec {
                        rate: shape.rate,
                        channels,
                        length: Length::Declared(self.frames),
                    };
                    let samples = &buffer.samples()[..frames * channels];
                    self.take_decoded(&mut stream, packet.ts(), decoded, samples, &mut each)?;
                }
                Err(failure) => {
                    self.replace_damaged(&mut stream, &packet, index, &failure, &mut each)?;
                }
            }
        }
        if self.damage.damaged > 0 {
            tracing::warn!(
                damaged = self.damage.damaged,
                packets = self.damage.packets,
                seconds = self.damage.seconds,
                "packets that do not decode were replaced by silence"
            );
        }
        if self.damage.too_much() {
            return Err(CodecError::ConversionFailed(format!(
                "{}: {} of {} packets do not decode",
                self.path.display(),
                self.damage.damaged,
                self.damage.packets
            )));
        }
        if let Source::Salvaged(packets) = &self.source {
            if packets.too_little() {
                return Err(CodecError::ConversionFailed(format!(
                    "{}: the recording has no index, and less than half of it reads as packets \
                     ({} read)",
                    self.path.display(),
                    packets.packets()
                )));
            }
            tracing::warn!(
                packets = packets.packets(),
                stopped = ?packets.stopped(),
                "the recording without an index was read up to where it was cut off or could not \
                 be read"
            );
        }
        stream.spec.filter(|spec| spec.audible()).ok_or_else(|| {
            CodecError::UnsupportedFormat(format!("{}: no decodable audio", self.path.display()))
        })
    }

    /// A decoded packet at `ts`, its interleaved `samples` of `shape`
    /// handed on less the priming, after the silence of the damaged packets
    /// held before it.
    fn take_decoded(
        &mut self,
        stream: &mut Stream,
        ts: u64,
        shape: Spec,
        samples: &[f32],
        each: &mut impl FnMut(Event<'_>) -> Result<(), CodecError>,
    ) -> Result<(), CodecError> {
        let frames = samples.len() / shape.channels.max(1);
        self.last_frames = Some(frames);
        if let Some(run) = stream.leading.take() {
            let held = Spec {
                rate: self
                    .params
                    .sample_rate
                    .filter(|&rate| rate > 0)
                    .unwrap_or(shape.rate),
                ..shape
            };
            let primed = self.primed(run.ts, run.frames);
            self.damage.seconds += emit_silence(
                each,
                &mut stream.spec,
                held,
                run.frames - primed,
                &mut stream.zeros,
            )?;
        }
        let primed = self.primed(ts, frames);
        emit(
            each,
            &mut stream.spec,
            shape,
            &samples[primed * shape.channels..],
        )
    }

    /// A packet that did not decode: a fresh decoder, the packet logged and
    /// counted, and its silence emitted, or held while no packet has given
    /// the stream's shape.
    fn replace_damaged(
        &mut self,
        stream: &mut Stream,
        packet: &Packet,
        index: u64,
        failure: &str,
        each: &mut impl FnMut(Event<'_>) -> Result<(), CodecError>,
    ) -> Result<(), CodecError> {
        // A fresh decoder, not a reset: a reset keeps the channel layout
        // a corrupt first packet set, and a panic may have left state half
        // written.
        self.decoder = make_decoder(&self.params)?;
        self.damage.damaged += 1;
        if self.damage.damaged <= LOGGED_DAMAGED_PACKETS {
            tracing::warn!(
                packet = index,
                ts = packet.ts(),
                %failure,
                "a packet that does not decode is replaced by silence of its length"
            );
        }
        let fallback = self.last_frames.unwrap_or(FALLBACK_PACKET_FRAMES);
        match stream.spec {
            None => {
                let rate = self.params.sample_rate.unwrap_or(0);
                let frames = packet_frames(packet.dur(), self.params.time_base, rate, fallback);
                if self.primed(packet.ts(), frames) < frames {
                    self.damage.parts += 1;
                }
                stream
                    .leading
                    .get_or_insert(Leading {
                        ts: packet.ts(),
                        frames: 0,
                    })
                    .frames += frames;
            }
            Some(shape) => {
                let frames =
                    packet_frames(packet.dur(), self.params.time_base, shape.rate, fallback);
                let primed = self.primed(packet.ts(), frames);
                if primed < frames {
                    self.damage.parts += 1;
                }
                self.damage.seconds += emit_silence(
                    each,
                    &mut stream.spec,
                    shape,
                    frames - primed,
                    &mut stream.zeros,
                )?;
            }
        }
        Ok(())
    }

    fn primed(&self, ts: u64, frames: usize) -> usize {
        self.priming
            .map_or(0, |trim| trim.frames_before(ts, frames))
    }
}

/// Symphonia's reader for a file the size walk found to be `start`, at
/// byte 0: no probe, which would scan for a marker the walk did not see.
fn demuxer(
    start: sizes::Start,
    mut stream: MediaSourceStream,
) -> symphonia::core::errors::Result<Box<dyn FormatReader>> {
    use symphonia::core::io::ReadBytes as _;
    use symphonia::default::formats::{CafReader, IsoMp4Reader, WavReader};
    let options = FormatOptions::default();
    Ok(match start {
        sizes::Start::Mp4 => {
            // As symphonia's own probe would leave the stream: past the
            // first box's size, from where the reader steps back 4 bytes.
            stream.ignore_bytes(4)?;
            Box::new(IsoMp4Reader::try_new(stream, &options)?)
        }
        sizes::Start::Caf => Box::new(CafReader::try_new(stream, &options)?),
        // It reads the `RIFF` marker itself (`wave/mod.rs:67`).
        sizes::Start::Wav => Box::new(WavReader::try_new(stream, &options)?),
    })
}

/// The salvage's packets of the file at `path`, when it is a phone
/// recording without a `moov`.
fn salvage(path: &Path) -> Option<SalvagedPackets> {
    SalvagedPackets::open(path).unwrap_or_else(|error| {
        tracing::warn!(path = %path.display(), %error, "the recording could not be read for a salvage");
        None
    })
}

fn make_decoder(params: &CodecParameters) -> Result<Box<dyn Decoder>, CodecError> {
    symphonia::default::get_codecs()
        .make(params, &DecoderOptions::default())
        .map_err(|e| CodecError::UnsupportedFormat(e.to_string()))
}

struct LaneDecode {
    channel: usize,
    output: Vec<f32>,
    resampler: Option<LaneResampler>,
}

impl LaneDecode {
    fn new(channel: usize) -> Self {
        Self {
            channel,
            output: Vec::new(),
            resampler: None,
        }
    }

    fn take(&mut self, event: Event<'_>) {
        match event {
            Event::Start(spec) => match self.resampler.replace(LaneResampler::new(spec.rate)) {
                Some(earlier) => earlier.finish(&mut self.output),
                None => self
                    .output
                    .reserve(frames_at_16k(reserved_frames(spec), spec.rate)),
            },
            Event::Frames(spec, samples) => {
                if let Some(resampler) = self.resampler.as_mut()
                    && self.channel < spec.channels
                {
                    let lane = samples.iter().skip(self.channel).step_by(spec.channels);
                    resampler.push(lane.copied(), &mut self.output);
                }
            }
        }
    }

    fn finish(mut self) -> Vec<f32> {
        if let Some(resampler) = self.resampler.take() {
            resampler.finish(&mut self.output);
        }
        self.output
    }
}

/// The mixdown's state between blocks: the mono lane resampled, held back
/// until settled, then written.
struct Mixdown<'a> {
    destination: &'a Path,
    writer: Option<WavStreamWriter>,
    resampler: Option<LaneResampler>,
    pending: Vec<f32>,
    written: usize,
    ints: Vec<i16>,
}

impl<'a> Mixdown<'a> {
    fn new(destination: &'a Path) -> Self {
        Self {
            destination,
            writer: None,
            resampler: None,
            pending: Vec::new(),
            written: 0,
            ints: Vec::new(),
        }
    }

    fn take(&mut self, event: Event<'_>) -> Result<(), CodecError> {
        match event {
            Event::Start(spec) => {
                self.finish_shape()?;
                if self.writer.is_none() {
                    self.writer = Some(
                        WavStreamWriter::create(self.destination, 16_000)
                            .map_err(|e| CodecError::Io(e.to_string()))?,
                    );
                }
                self.resampler = Some(LaneResampler::new(spec.rate));
                self.written = 0;
                Ok(())
            }
            Event::Frames(spec, samples) => {
                let Some(resampler) = self.resampler.as_mut() else {
                    return Ok(());
                };
                let scale = 1.0 / spec.channels as f32;
                let mono = samples
                    .chunks_exact(spec.channels)
                    .map(|frame| frame.iter().fold(0.0f32, |sum, s| sum + s * scale));
                resampler.push(mono, &mut self.pending);
                let settled = resampler.settled() - self.written;
                self.write(settled)
            }
        }
    }

    fn write(&mut self, count: usize) -> Result<(), CodecError> {
        let Some(writer) = self.writer.as_mut() else {
            return Ok(());
        };
        self.ints.clear();
        self.ints.extend(
            self.pending
                .drain(..count)
                .map(|s| (s.clamp(-1.0, 1.0) * 32767.0).round() as i16),
        );
        self.written += count;
        writer
            .write(&self.ints)
            .map_err(|e| CodecError::Io(e.to_string()))
    }

    fn finish_shape(&mut self) -> Result<(), CodecError> {
        if let Some(resampler) = self.resampler.take() {
            resampler.finish(&mut self.pending);
            self.write(self.pending.len())?;
        }
        Ok(())
    }

    fn finish(&mut self) -> Result<(), CodecError> {
        self.finish_shape()?;
        self.writer
            .as_mut()
            .map_or(Ok(()), WavStreamWriter::finish)
            .map_err(|e| CodecError::Io(e.to_string()))
    }
}

#[async_trait]
impl AudioDecoder for SymphoniaAudioCodec {
    /// The sidecar when it agrees with the master, else the master; a
    /// master that then fails falls back to the sidecar after all.
    async fn decode(&self, asset: &AudioAsset, lane: AudioLane) -> BoundaryResult<AudioBuffer16k> {
        if let Some(samples) = sidecar(asset, lane) {
            return Ok(AudioBuffer16k::new(samples));
        }
        let master = asset
            .lanes
            .iter()
            .position(|l| *l == lane)
            .ok_or(CodecError::LaneNotInAsset(lane))
            .and_then(|channel| Self::decode_path(&master_path(asset)?, channel, lane));
        let error = match master {
            Ok(buffer) => return Ok(buffer),
            Err(error) => error,
        };
        // Read again rather than held through the master's decode, so two
        // copies of the lane are never alive at once.
        let Some(samples) = sidecar_samples(asset, lane) else {
            return Err(error.into());
        };
        tracing::warn!(
            lane = lane.as_str(),
            %error,
            "the master failed to decode; taking the 16 kHz sidecar that disagreed with it"
        );
        Ok(AudioBuffer16k::new(samples))
    }

    /// `Wav16kInt16`: there is no AAC encoder in pure Rust.
    fn mixdown_format(&self) -> AudioFormat {
        AudioFormat::Wav16kInt16
    }

    async fn mixdown(&self, asset: &AudioAsset, to: &Path) -> BoundaryResult<()> {
        let source = master_path(asset)?;
        if to.exists() {
            std::fs::remove_file(to).map_err(|e| io_error(to, &e))?;
        }
        if asset.format != AudioFormat::Wav16kInt16 {
            return Ok(Self::mixdown_path(&source, to)?);
        }
        create_parent(to)?;
        std::fs::copy(&source, to).map_err(|e| io_error(to, &e))?;
        Ok(())
    }
}

/// The lane's 16 kHz sidecar, when it reads, is not empty and is the
/// master's length at 16 kHz. One that disagrees is not trusted (its
/// 32-bit sizes wrap after 37.3 hours, a full disk may cost it a frame),
/// and the master is decoded instead. A master whose length cannot be
/// read cheaply leaves the sidecar trusted.
fn sidecar(asset: &AudioAsset, lane: AudioLane) -> Option<Vec<f32>> {
    let samples = sidecar_samples(asset, lane)?;
    if let Some(master) = file_url_path(&asset.url)
        && let Ok(reader) = CafReader::open(&master)
    {
        let rate = reader.sample_rate() as u32;
        let expected = length_at_16k(reader.frame_count(), rate);
        if rate > 0 && samples.len() != expected {
            tracing::warn!(
                lane = lane.as_str(),
                sidecar = samples.len(),
                master = expected,
                "the 16 kHz sidecar disagrees with the master's length; decoding the master"
            );
            return None;
        }
    }
    Some(samples)
}

fn sidecar_samples(asset: &AudioAsset, lane: AudioLane) -> Option<Vec<f32>> {
    let path = file_url_path(asset.sidecars_16k.get(&lane)?)?;
    WavFile::read_16k_mono(&path)
        .ok()
        .filter(|samples| !samples.is_empty())
}

fn master_path(asset: &AudioAsset) -> Result<PathBuf, CodecError> {
    file_url_path(&asset.url)
        .ok_or_else(|| CodecError::Io(format!("not a file URL: {}", asset.url)))
}

fn create_parent(path: &Path) -> Result<(), CodecError> {
    match path.parent() {
        Some(parent) => std::fs::create_dir_all(parent).map_err(|e| io_error(parent, &e)),
        None => Ok(()),
    }
}

fn io_error(path: &Path, error: &std::io::Error) -> CodecError {
    CodecError::Io(format!("{}: {error}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::writer::CafStreamWriter;

    fn spec(rate: u32, channels: usize) -> Spec {
        Spec {
            rate,
            channels,
            length: Length::Declared(None),
        }
    }

    /// A damaged packet's length: its duration at the stream's rate, else
    /// the last decoded packet's (else one AAC frame), capped.
    #[test]
    fn a_damaged_packets_length_has_a_fallback_and_a_cap() {
        let samples = Some(TimeBase::new(1, 44_100));
        assert_eq!(packet_frames(1_024, samples, 44_100, 7), 1_024);
        let ticks = Some(TimeBase::new(1, 88_200));
        assert_eq!(
            packet_frames(2_048, ticks, 44_100, 7),
            1_024,
            "the time base"
        );
        assert_eq!(packet_frames(1_024, None, 44_100, 7), 1_024, "no time base");
        assert_eq!(packet_frames(0, samples, 44_100, 960), 960, "no duration");
        assert_eq!(packet_frames(1_024, samples, 0, 960), 960, "no rate");
        assert_eq!(
            packet_frames(441_000, samples, 44_100, 7),
            MAX_PACKET_FRAMES
        );
        assert_eq!(packet_frames(u64::MAX, ticks, 96_000, 7), MAX_PACKET_FRAMES);
        assert_eq!(
            packet_frames(0, samples, 44_100, 1 << 20),
            MAX_PACKET_FRAMES
        );
    }

    /// Silence is handed on at most [`MAX_PACKET_FRAMES`] at a time, every
    /// frame and its seconds accounted for.
    #[test]
    fn silence_goes_out_in_capped_blocks() {
        let shape = spec(44_100, 2);
        let frames = 3 * MAX_PACKET_FRAMES + 5;
        let (mut starts, mut pieces) = (0, Vec::new());
        let mut each = |event: Event<'_>| {
            match event {
                Event::Start(_) => starts += 1,
                Event::Frames(_, samples) => {
                    assert!(samples.iter().all(|&s| s == 0.0));
                    pieces.push(samples.len());
                }
            }
            Ok(())
        };
        let (mut current, mut zeros) = (None, Vec::new());
        let seconds = emit_silence(&mut each, &mut current, shape, frames, &mut zeros).unwrap();
        assert_eq!(starts, 1);
        assert_eq!(
            pieces,
            [
                MAX_PACKET_FRAMES * 2,
                MAX_PACKET_FRAMES * 2,
                MAX_PACKET_FRAMES * 2,
                10
            ]
        );
        assert_eq!(zeros.len(), MAX_PACKET_FRAMES * 2, "the zeros stay capped");
        assert!((seconds - frames as f64 / 44_100.0).abs() < 1e-12);
    }

    /// Exactly half the packets damaged still decodes; one more fails.
    #[test]
    fn half_the_packets_damaged_is_not_too_much() {
        let damage = |damaged| Damage {
            packets: 24,
            damaged,
            ..Damage::default()
        };
        assert!(!damage(12).too_much());
        assert!(damage(13).too_much());
        assert!(!damage(0).too_much());
    }

    /// A declared length reserves at most five hours at 16 kHz; a measured
    /// one reserves all of it.
    #[test]
    fn a_declared_length_reserves_at_most_five_hours() {
        let five_hours = 5 * 3_600 * 16_000;
        for rate in [8_000, 44_100, 48_000, 96_000] {
            let declared = Spec {
                length: Length::Declared(Some(u64::MAX / 2)),
                ..spec(rate, 1)
            };
            let reserved = frames_at_16k(reserved_frames(declared), rate);
            assert_eq!(reserved, five_hours + 16_000, "{rate} Hz");
        }
        let six_hours = 6 * 3_600 * 48_000;
        let measured = Spec {
            length: Length::Measured(six_hours),
            ..spec(48_000, 2)
        };
        assert_eq!(reserved_frames(measured), six_hours);
        assert_eq!(reserved_frames(spec(48_000, 1)), 0, "no length declared");
    }

    /// A rate and channel count that change mid-stream keep what came
    /// before, in the lane and in the mixdown.
    #[test]
    fn a_shape_change_mid_stream_appends_to_the_lane() {
        let first: Vec<f32> = (0..4_800).map(|i| (i as f32 * 0.01).sin() * 0.5).collect();
        let second: Vec<f32> = (0..4_410).map(|i| (i as f32 * 0.02).sin() * 0.25).collect();
        let stereo: Vec<f32> = second.iter().flat_map(|&s| [s, s]).collect();
        let events = [
            Event::Start(spec(48_000, 1)),
            Event::Frames(spec(48_000, 1), &first),
            Event::Start(spec(44_100, 2)),
            Event::Frames(spec(44_100, 2), &stereo),
        ];
        let expected = [
            SymphoniaAudioCodec::to_16k(&first, 48_000),
            SymphoniaAudioCodec::to_16k(&second, 44_100),
        ]
        .concat();
        assert_eq!(expected.len(), 1_600 + 1_600);

        let mut decode = LaneDecode::new(0);
        for event in events {
            decode.take(event);
        }
        assert_eq!(decode.finish(), expected);

        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("audio.wav");
        let mut mixdown = Mixdown::new(&destination);
        for event in events {
            mixdown.take(event).unwrap();
        }
        mixdown.finish().unwrap();
        let written = WavFile::read_16k_mono(&destination).unwrap();
        let rounded: Vec<f32> = expected
            .iter()
            .map(|&s| f32::from((s.clamp(-1.0, 1.0) * 32767.0).round() as i16) / 32768.0)
            .collect();
        assert_eq!(written, rounded);
    }

    /// A mixdown that fails mid-stream removes what it wrote.
    #[test]
    fn a_mixdown_that_fails_mid_stream_removes_its_file() {
        let directory = tempfile::tempdir().unwrap();
        let master = directory.path().join("recording.caf");
        let frames = 3 * Frames::CAF_BLOCK;
        let mut writer = CafStreamWriter::create(&master, 48_000.0, 2).unwrap();
        writer.write(&vec![0.25f32; 2 * frames], frames).unwrap();
        writer.finish().unwrap();
        let source = Frames::open(&master).unwrap();
        assert!(matches!(source, Frames::Caf { .. }));
        let length = std::fs::metadata(&master).unwrap().len();
        std::fs::OpenOptions::new()
            .write(true)
            .open(&master)
            .unwrap()
            .set_len(length - 2 * (Frames::CAF_BLOCK * 2 * 4) as u64 + 4)
            .unwrap();

        let destination = directory.path().join("audio.wav");
        let error = SymphoniaAudioCodec::mix(source, &destination).unwrap_err();
        assert!(matches!(error, CodecError::Io(_)), "{error:?}");
        assert!(!destination.exists(), "the partial mixdown is removed");
    }
}
