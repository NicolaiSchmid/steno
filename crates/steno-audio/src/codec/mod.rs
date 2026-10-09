//! The program's [`AudioDecoder`] for real recordings, in pure Rust.
//! Swift: `Sources/StenoAudio/Codec/AVFoundationAudioCodec.swift`.
//!
//! `decode` returns the lane's 16 kHz sidecar when it is present, complete
//! and as long as the master, else reads the master (the crate's own CAF
//! reader for the writer's masters, `symphonia` for CAF, WAV, m4a/AAC and
//! mp3) and resamples channel n (lane n of the master) to 16 kHz mono: the
//! exact 3:1 FIR for 48 kHz material, a windowed-sinc polyphase resampler
//! ([`sinc::SincResampler`]) for every other rate (the phone records
//! 44.1 kHz).
//!
//! The file streams through, as the AVFoundation codec's 32 768-frame
//! chunks did: a block of 32 768 frames at a time from a CAF, a packet at
//! a time through symphonia, a block at a time from a sidecar, each
//! resampled as it arrives ([`LaneResampler`]). A decode holds the 16 kHz
//! lane and a working set under a megabyte, never the lane at the source
//! rate or the file's bytes; a mixdown writes as it goes and holds neither.
//! The samples are those one pass over the whole file gives, bit for bit:
//! `tests/codec_streaming.rs` holds the decoder against its whole-file
//! form, `tests/codec_memory.rs` bounds the working set. A stream whose
//! rate or channel count changes mid-file keeps what came before: each
//! shape's samples are resampled at their own rate and appended, so the
//! lane keeps all of the file, as Swift's one decode did.
//!
//! `decode` takes the sidecar only when it is the master's length at
//! 16 kHz, and falls back to it when the master then fails.
//!
//! Where symphonia cannot follow AVFoundation, documented here and in the
//! plan's parity list:
//!
//! - **No AAC encoder in pure Rust.** `mixdown` writes a 16 kHz mono Int16
//!   WAV (`audio.wav`) and `mixdown_format()` says `Wav16kInt16`, so the
//!   persist stage names the file right. The Swift codec writes AAC 64 kbps.
//! - **AAC-LC only.** HE-AAC files (unlikely from the iOS recorder, which
//!   uses `AVAudioRecorder` with `kAudioFormatMPEG4AAC`) decode as LC and
//!   sound wrong. Not seen in practice.
//! - **Encoder priming.** AVFoundation trimmed the encoder's priming
//!   samples; symphonia 0.5 trims them for MP3 (the LAME tag, with
//!   `enable_gapless`) but not for MP4, whose edit list it parses and
//!   ignores. For AAC in MP4 the decoder finds the priming itself
//!   ([`priming`]: the edit list, else iTunes' gapless tag, else the
//!   2 112 samples AVFoundation assumes, but only in the layout of the
//!   phone's `AVAudioRecorder`, whose files need it; any other undeclared
//!   file keeps every sample) and drops exactly that many frames from
//!   the start, by the packets' timestamps, so the lane starts on the
//!   first sample the encoder was given (1 024 samples at 44.1 kHz, 23 ms,
//!   for an ffmpeg encode; 2 112, 48 ms, for Apple's; `tests/codec.rs`).
//!   Every other input decodes as before, bit for bit. The padding after
//!   the last sample stays (under a packet of silence), as the
//!   exact-length rule counts what was decoded.
//! - **A damaged packet.** AVFoundation conceals a packet its decoder
//!   cannot read; symphonia returns an error, which stopped the whole
//!   decode. The decoder replaces such a packet by silence of its length
//!   (its duration in the container's timing, 1 024 frames for AAC), so the
//!   audio after it keeps its time, logs it (its index and timestamp, never
//!   its content) and counts it: [`AudioBuffer16k::damaged_parts`], which
//!   the pipeline shows on the meeting. The first packet after a damaged
//!   one starts from a cleared overlap, so its frames differ from a clean
//!   decode's; the later ones equal it bit for bit, except in the bands the
//!   encoder filled with noise, whose generator has moved on (within
//!   2 * 10^-3 on the test tone, `tests/codec.rs`). A file with more
//!   damaged packets than whole ones fails ([`MAX_DAMAGED_SHARE`]), so a
//!   corrupt file never becomes a meeting of silence.
//! - **CAF**: PCM only (what the writer produces); a CAF holding AAC fails.
//! - An unfinished master (data chunk size -1) decodes to its last whole
//!   frame through the crate's own CAF reader (the chunk walk
//!   [`CafFile`](crate::writer::CafFile) uses) before symphonia is
//!   tried, as the Swift test demands.

pub mod priming;
pub mod resample;
pub mod sinc;

use std::path::{Path, PathBuf};

use steno_core::{
    AudioAsset, AudioBuffer16k, AudioDecoder, AudioFormat, AudioLane, BoundaryResult, async_trait,
    paths::file_url_path,
};
use symphonia::core::audio::{Channels, SampleBuffer};
use symphonia::core::codecs::{CODEC_TYPE_AAC, CodecParameters, Decoder, DecoderOptions};
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::{FormatOptions, FormatReader, Packet};
use symphonia::core::io::{MediaSourceStream, MediaSourceStreamOptions};
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;
use symphonia::core::units::TimeBase;

use priming::Priming;
pub use resample::LaneResampler;
use resample::length_at_16k;

use crate::writer::caf::CafReader;
use crate::writer::{WavFile, WavStreamWriter};

/// Why a decode or mixdown failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CodecError {
    /// The asset's `lanes` do not list it.
    #[error("the asset has no {} lane", .0.as_str())]
    LaneNotInAsset(AudioLane),
    /// The lane's channel index is beyond the file's channels.
    #[error("lane {} is channel {channel} but the file has {channels}", lane.as_str())]
    ChannelMissing {
        /// The lane asked for.
        lane: AudioLane,
        /// Its channel in the master.
        channel: usize,
        /// Channels the file has.
        channels: usize,
    },
    /// Symphonia could not probe or decode the container or codec.
    #[error("unsupported audio format: {0}")]
    UnsupportedFormat(String),
    /// Decoding stopped mid-file.
    #[error("audio conversion failed: {0}")]
    ConversionFailed(String),
    /// A read or write failed: the path and the error.
    #[error("{0}")]
    Io(String),
}

/// Decoded PCM at the source rate: one channel's samples.
#[derive(Debug, Clone, PartialEq)]
pub struct DecodedChannel {
    /// Hertz.
    pub sample_rate: u32,
    /// Channels the file has; this is one of them.
    pub channels: usize,
    /// The channel's samples at `sample_rate`.
    pub samples: Vec<f32>,
    /// Packets that could not be decoded, each replaced by silence of its
    /// length: [`AudioBuffer16k::damaged_parts`].
    pub damaged_parts: u32,
}

/// The share of a file's packets that may be damaged: a file in which more
/// than half of the packets cannot be decoded fails with
/// [`CodecError::ConversionFailed`] rather than decode to more silence than
/// sound. Such a file is not a recording with a few bad packets but one
/// this decoder cannot read (a codec it decodes wrongly, a payload
/// overwritten wholesale); the error keeps it, and the meeting says
/// processing failed, where a transcript of a few words would let the
/// retention rule delete the file.
pub const MAX_DAMAGED_SHARE: f64 = 0.5;

/// The decoder, stateless; see the module doc.
#[derive(Debug, Clone, Copy, Default)]
pub struct SymphoniaAudioCodec;

impl SymphoniaAudioCodec {
    /// There is no state.
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
            damaged_parts: frames.damaged_parts(),
            ..AudioBuffer16k::new(decode.finish())
        })
    }

    /// One channel at the source rate, whole: for the tests and the bench
    /// tools; the pipeline's [`decode_path`](Self::decode_path) never holds
    /// it.
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
            damaged_parts: frames.damaged_parts(),
        })
    }

    /// Seconds in the file at `path` as its container declares them (an
    /// m4a's track header, a WAV's data chunk), read without decoding it;
    /// `None` when the container does not say. Without the AAC priming the
    /// decode drops, so it matches the decode up to the encoder's padding
    /// at the end (under a packet), and capped at five hours, as
    /// `reserved_frames` caps a declared length, so a corrupt header cannot
    /// move a start absurdly far back. For a recording that has no stored
    /// duration, such as one the launch adopts from the audio folder. Rust
    /// only.
    pub fn declared_duration(path: &Path) -> Result<Option<f64>, CodecError> {
        let frames = SymphoniaFrames::open(path)?;
        let primed = frames.priming.map_or(0, |trim| trim.frames);
        Ok(frames
            .frames
            .zip(frames.rate.filter(|rate| *rate > 0))
            .map(|(declared, rate)| {
                capped(declared.saturating_sub(primed), rate) as f64 / f64::from(rate)
            }))
    }

    /// `samples` at `rate` to 16 kHz in one pass: exact length `round(len *
    /// 16000 / rate)`, trimmed or zero-padded so the 16 kHz lane lasts
    /// exactly as long as the master and segment counts stay stable.
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

    /// `mixdown_path` from an open source.
    fn mix(mut frames: Frames, destination: &Path) -> Result<(), CodecError> {
        create_parent(destination)?;
        let mut mixdown = Mixdown::new(destination);
        let result = frames
            .stream(|event| mixdown.take(event))
            .and_then(|_| mixdown.finish());
        if result.is_err()
            && let Some(writer) = mixdown.writer.take()
        {
            // Closed first, which Windows needs to remove it; best effort,
            // the error says what went wrong.
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
    /// Five hours, in seconds.
    const MAX_DECLARED_SECONDS: u64 = 5 * 3_600;
    frames.min(MAX_DECLARED_SECONDS * u64::from(rate))
}

/// The frames to reserve for a stream: its length, but a length the
/// container declares is capped at five hours at the stream's rate (a
/// corrupt header must not reserve gigabytes, and a failed allocation
/// aborts the app), so the 16 kHz reservation stays under five hours at
/// 16 kHz whatever the rate. A length measured from the file is not
/// capped.
fn reserved_frames(spec: Spec) -> usize {
    match spec.length {
        Length::Measured(frames) => frames,
        Length::Declared(frames) => {
            usize::try_from(capped(frames.unwrap_or(0), spec.rate)).unwrap_or(0)
        }
    }
}

/// A stream's shape: hertz, interleaved channels, and its length in
/// frames.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Spec {
    rate: u32,
    channels: usize,
    length: Length,
}

/// How a stream's frame count is known.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Length {
    /// From the file's size: the crate's CAF reader.
    Measured(usize),
    /// From the container's header, if it says (symphonia): not trusted.
    Declared(Option<u64>),
}

impl Spec {
    /// Whether the stream has samples to take: a channel and a rate.
    fn audible(self) -> bool {
        self.channels > 0 && self.rate > 0
    }

    /// `ChannelMissing` when the stream has no channel `channel`.
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
    /// Interleaved frames.
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
    /// Frames per read from a CAF: the AVFoundation codec's chunk.
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
    /// shape. A change of shape mid-stream is a new `Start`: resampling
    /// earlier samples at a later rate would be noise, so each shape's
    /// samples are taken at their own.
    fn stream(
        &mut self,
        mut each: impl FnMut(Event<'_>) -> Result<(), CodecError>,
    ) -> Result<Spec, CodecError> {
        match self {
            Self::Caf { reader, block } => {
                // The rate is a whole number of hertz for any recording.
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

    /// The packets [`stream`](Self::stream) replaced by silence; the
    /// crate's CAF reader reads PCM, which has none.
    fn damaged_parts(&self) -> u32 {
        match self {
            Self::Caf { .. } => 0,
            Self::Symphonia(frames) => u32::try_from(frames.damage.damaged).unwrap_or(u32::MAX),
        }
    }
}

/// Symphonia's demuxer and decoder over one file.
struct SymphoniaFrames {
    path: PathBuf,
    format: Box<dyn FormatReader>,
    decoder: Box<dyn Decoder>,
    track_id: u32,
    /// The track's frame count, as its header declares it.
    frames: Option<u64>,
    /// The encoder priming to drop: AAC in MP4 only.
    priming: Option<Trim>,
    /// The track's rate in hertz, as its header declares it.
    rate: Option<u32>,
    /// The track's shape as its header declares it, for the silence of a
    /// damaged packet before any packet decoded.
    declared: Spec,
    /// The packets' timestamp unit, which their durations are in.
    time_base: Option<TimeBase>,
    /// The track's packets so far, and the damaged ones among them.
    damage: Damage,
}

/// A track's packets read and how many of them could not be decoded.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Damage {
    packets: u64,
    damaged: u64,
}

impl Damage {
    /// More of the packets damaged than [`MAX_DAMAGED_SHARE`] allows.
    fn too_much(self) -> bool {
        // Exact for any packet count a file can hold.
        #[allow(clippy::cast_precision_loss)]
        let share = self.damaged as f64 / self.packets.max(1) as f64;
        share > MAX_DAMAGED_SHARE
    }
}

/// The frames before a track's first presented sample, and how to place a
/// packet against them.
#[derive(Debug, Clone, Copy)]
struct Trim {
    /// Frames at `rate` to drop from the start.
    frames: u64,
    rate: u32,
    /// The packets' timestamp unit.
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

impl SymphoniaFrames {
    fn open(path: &Path) -> Result<Self, CodecError> {
        let file = std::fs::File::open(path).map_err(|e| io_error(path, &e))?;
        let stream = MediaSourceStream::new(Box::new(file), MediaSourceStreamOptions::default());
        let mut hint = Hint::new();
        if let Some(extension) = path.extension().and_then(|e| e.to_str()) {
            hint.with_extension(extension);
        }
        let probed = symphonia::default::get_probe()
            .format(
                &hint,
                stream,
                // Gapless: trims the encoder delay and padding where the
                // container declares them (MP3's LAME tag; not MP4 yet).
                &FormatOptions {
                    enable_gapless: true,
                    ..FormatOptions::default()
                },
                &MetadataOptions::default(),
            )
            .map_err(|e| CodecError::UnsupportedFormat(format!("{}: {e}", path.display())))?;
        let format = probed.format;
        let track = format
            .default_track()
            .ok_or_else(|| CodecError::UnsupportedFormat("no audio track".into()))?;
        let track_id = track.id;
        let params = &track.codec_params;
        let frames = params.n_frames;
        let priming = Trim::for_track(path, params);
        let rate = params.sample_rate;
        let declared = Spec {
            rate: params.sample_rate.unwrap_or(0),
            channels: params.channels.map_or(0, Channels::count),
            length: Length::Declared(frames),
        };
        let time_base = params.time_base;
        let decoder = symphonia::default::get_codecs()
            .make(&track.codec_params, &DecoderOptions::default())
            .map_err(|e| CodecError::UnsupportedFormat(e.to_string()))?;
        Ok(Self {
            path: path.to_path_buf(),
            format,
            decoder,
            track_id,
            frames,
            priming,
            rate,
            declared,
            time_base,
            damage: Damage::default(),
        })
    }

    /// Packet by packet. A packet that does not decode becomes silence of
    /// its length (see the module doc); a file with no decodable audio, or
    /// with more damaged packets than [`MAX_DAMAGED_SHARE`] allows, is an
    /// error.
    fn stream(
        &mut self,
        mut each: impl FnMut(Event<'_>) -> Result<(), CodecError>,
    ) -> Result<Spec, CodecError> {
        let mut spec: Option<Spec> = None;
        let mut buffer: Option<SampleBuffer<f32>> = None;
        let mut silence: Vec<f32> = Vec::new();
        self.damage = Damage::default();
        loop {
            let packet = match self.format.next_packet() {
                Ok(packet) => packet,
                Err(SymphoniaError::IoError(e))
                    if e.kind() == std::io::ErrorKind::UnexpectedEof =>
                {
                    break;
                }
                Err(SymphoniaError::ResetRequired) => break,
                Err(e) => return Err(CodecError::ConversionFailed(e.to_string())),
            };
            if packet.track_id() != self.track_id {
                continue;
            }
            let index = self.damage.packets;
            self.damage.packets += 1;
            let (packet_spec, frames, samples): (Spec, usize, &[f32]) =
                match self.decoder.decode(&packet) {
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
                        let packet_spec = Spec {
                            rate: shape.rate,
                            channels,
                            length: Length::Declared(self.frames),
                        };
                        (packet_spec, frames, buffer.samples())
                    }
                    Err(error) => {
                        self.damage.damaged += 1;
                        // The overlap of the packet before is no longer this
                        // packet's to finish.
                        self.decoder.reset();
                        let shape = spec.unwrap_or(self.declared);
                        let frames = self.packet_frames(&packet, shape.rate);
                        tracing::warn!(
                            packet = index,
                            ts = packet.ts(),
                            frames,
                            %error,
                            "a packet that does not decode is replaced by silence of its length"
                        );
                        let length = frames * shape.channels;
                        if silence.len() < length {
                            silence.resize(length, 0.0);
                        }
                        (shape, frames, &silence[..length])
                    }
                };
            let primed = self
                .priming
                .map_or(0, |trim| trim.frames_before(packet.ts(), frames));
            let audible = packet_spec.audible();
            if spec != Some(packet_spec) {
                spec = Some(packet_spec);
                if audible {
                    each(Event::Start(packet_spec))?;
                }
            }
            if audible {
                each(Event::Frames(
                    packet_spec,
                    &samples[primed * packet_spec.channels..],
                ))?;
            }
        }
        if self.damage.too_much() {
            return Err(CodecError::ConversionFailed(format!(
                "{}: {} of {} packets do not decode",
                self.path.display(),
                self.damage.damaged,
                self.damage.packets
            )));
        }
        spec.filter(|spec| spec.audible()).ok_or_else(|| {
            CodecError::UnsupportedFormat(format!("{}: no decodable audio", self.path.display()))
        })
    }

    /// A packet's length in frames at `rate`, from its duration in the
    /// container's timing; at most a second, so a corrupt duration cannot
    /// grow the lane (no codec here has packets longer than 8 192 frames).
    fn packet_frames(&self, packet: &Packet, rate: u32) -> usize {
        let duration = u128::from(packet.dur());
        let frames = self.time_base.map_or(duration, |base| {
            duration * u128::from(base.numer) * u128::from(rate) / u128::from(base.denom.max(1))
        });
        usize::try_from(frames.min(u128::from(rate))).unwrap_or(0)
    }
}

/// One channel's decode between blocks: the 16 kHz lane so far and the
/// current shape's resampler.
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
                // A new shape: what came before stays, resampled at its
                // own rate.
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
    /// 16 kHz samples not yet written.
    pending: Vec<f32>,
    /// 16 kHz samples written since the current shape started.
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
                // A new shape: what came before is finished and written at
                // its own rate, and the file goes on.
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
                // Channel counts are tiny.
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

    /// Writes the first `count` pending samples.
    fn write(&mut self, count: usize) -> Result<(), CodecError> {
        let Some(writer) = self.writer.as_mut() else {
            return Ok(());
        };
        self.ints.clear();
        // Clamped first, so the cast is exact.
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

    /// Flushes the current shape's resampler into the file.
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
    /// master that then fails (an I/O error mid-file, a channel it lacks)
    /// falls back to the sidecar after all, which beats no transcript.
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

    /// `Wav16kInt16`: there is no AAC encoder in pure Rust (see the module
    /// doc); the Swift codec says `M4aAac`.
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
/// master's length at 16 kHz: the writer writes the master and the
/// sidecars a frame at a time, so a finished sidecar holds the master's
/// frames resampled. One that disagrees is not trusted, and the master,
/// the copy that is never shorter, is decoded instead: the sidecar's
/// 32-bit size fields wrap after 37.3 hours, and a sidecar that missed a
/// frame the master kept (a full disk) would lose it. A master whose
/// length cannot be read cheaply (not the writer's CAF) leaves the
/// sidecar trusted.
fn sidecar(asset: &AudioAsset, lane: AudioLane) -> Option<Vec<f32>> {
    let samples = sidecar_samples(asset, lane)?;
    if let Some(master) = file_url_path(&asset.url)
        && let Ok(reader) = CafReader::open(&master)
    {
        // The rate is a whole number of hertz for any recording.
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

/// The lane's 16 kHz sidecar when it reads and is not empty, whatever its
/// length.
fn sidecar_samples(asset: &AudioAsset, lane: AudioLane) -> Option<Vec<f32>> {
    let path = file_url_path(asset.sidecars_16k.get(&lane)?)?;
    WavFile::read_16k_mono(&path)
        .ok()
        .filter(|samples| !samples.is_empty())
}

/// The asset's master as a path; the URL must be a file URL.
fn master_path(asset: &AudioAsset) -> Result<PathBuf, CodecError> {
    file_url_path(&asset.url)
        .ok_or_else(|| CodecError::Io(format!("not a file URL: {}", asset.url)))
}

/// Creates the folder `path` goes in, if needed.
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

    /// A length the container declares reserves at most five hours at
    /// 16 kHz, whatever the rate; a length measured from the file
    /// reserves all of it.
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
    /// before: each shape's samples are resampled at their own rate and
    /// appended, in the lane and in the mixdown, as one decode of the
    /// whole file in Swift kept them all.
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

    /// A mixdown that fails mid-stream removes what it wrote: the master is
    /// cut short under the open reader, after its first block, as a disk
    /// error mid-file would stop it.
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
