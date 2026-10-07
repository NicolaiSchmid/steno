//! The program's [`AudioDecoder`] for real recordings, in pure Rust.
//! Swift: `Sources/StenoAudio/Codec/AVFoundationAudioCodec.swift`.
//!
//! `decode` returns the lane's 16 kHz sidecar when it is present and
//! complete, else reads the master (the crate's own CAF reader for the
//! writer's masters, `symphonia` for CAF, WAV, m4a/AAC and mp3) and
//! resamples channel n (lane n of the master) to 16 kHz mono: the exact
//! 3:1 FIR for 48 kHz material, a windowed-sinc polyphase resampler
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
//! rate or channel count changes mid-file starts the lane again at the
//! change (the whole-file form did that on a channel count change, and
//! resampled everything at the last rate on a rate change).
//!
//! Where symphonia cannot follow AVFoundation, documented here and in the
//! plan's parity list:
//!
//! - **No AAC encoder in pure Rust.** `mixdown` writes a 16 kHz mono Int16
//!   WAV (`audio.wav`) and `mixdown_format()` says `Wav16kInt16`, so the
//!   persist stage names the file right. The Swift codec writes AAC 64 kbps.
//! - **AAC-LC only.** HE-AAC files (unlikely from the iOS recorder, which
//!   uses `AVAudioFile` with `kAudioFormatMPEG4AAC`) decode as LC and sound
//!   wrong. Not seen in practice.
//! - **Encoder priming.** AVFoundation trimmed the encoder's priming
//!   samples; symphonia 0.5 trims them for MP3 (the LAME tag, with
//!   `enable_gapless`) but not for MP4, whose edit list it parses and
//!   ignores. An AAC lane therefore starts with the priming (1 024
//!   samples at 44.1 kHz, 23 ms, for an ffmpeg encode; measured in
//!   `tests/codec.rs`) and runs that much late. Tracked as a parity item.
//! - **CAF**: PCM only (what the writer produces); a CAF holding AAC fails.
//! - An unfinished master (data chunk size -1) decodes to its last whole
//!   frame through the crate's own CAF reader (the chunk walk
//!   [`CafFile`](crate::writer::CafFile) uses) before symphonia is
//!   tried, as the Swift test demands.

pub mod resample;
pub mod sinc;

use std::path::{Path, PathBuf};

use steno_core::{
    AudioAsset, AudioBuffer16k, AudioDecoder, AudioFormat, AudioLane, BoundaryResult, async_trait,
    paths::file_url_path,
};
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{Decoder, DecoderOptions};
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::{FormatOptions, FormatReader};
use symphonia::core::io::{MediaSourceStream, MediaSourceStreamOptions};
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

pub use resample::LaneResampler;

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
}

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
        let mut output = Vec::new();
        let mut resampler = None;
        let spec = frames.stream(|event| {
            match event {
                Event::Start(spec) => {
                    output = Vec::with_capacity(frames_at_16k(frames_hint(spec), spec.rate));
                    resampler = Some(LaneResampler::new(spec.rate));
                }
                Event::Frames(spec, samples) => {
                    if let Some(resampler) = resampler.as_mut()
                        && channel < spec.channels
                    {
                        let lane = samples.iter().skip(channel).step_by(spec.channels);
                        resampler.push(lane.copied(), &mut output);
                    }
                }
            }
            Ok(())
        })?;
        if channel >= spec.channels {
            return Err(CodecError::ChannelMissing {
                lane,
                channel,
                channels: spec.channels,
            });
        }
        if let Some(resampler) = resampler {
            resampler.finish(&mut output);
        }
        Ok(AudioBuffer16k::new(output))
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
                Event::Start(spec) => samples = Vec::with_capacity(frames_hint(spec)),
                Event::Frames(spec, block) => {
                    if channel < spec.channels {
                        samples.extend(block.iter().skip(channel).step_by(spec.channels));
                    }
                }
            }
            Ok(())
        })?;
        if channel >= spec.channels {
            return Err(CodecError::ChannelMissing {
                lane,
                channel,
                channels: spec.channels,
            });
        }
        Ok(DecodedChannel {
            sample_rate: spec.rate,
            channels: spec.channels,
            samples,
        })
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
        let mut frames = Frames::open(source)?;
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent).map_err(|e| io_error(parent, &e))?;
        }
        let mut mixdown = Mixdown {
            destination,
            writer: None,
            resampler: None,
            pending: Vec::new(),
            written: 0,
            ints: Vec::new(),
        };
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
    (frames as f64 * AudioBuffer16k::SAMPLE_RATE / f64::from(rate.max(1))).round() as usize
        + AudioBuffer16k::SAMPLE_RATE as usize
}

/// The frames to reserve for: the container's count, but never more than
/// `MAX_HINT` (a corrupt header should not reserve gigabytes).
fn frames_hint(spec: Spec) -> usize {
    /// Five hours at 48 kHz.
    const MAX_HINT: u64 = 5 * 3_600 * 48_000;
    usize::try_from(spec.frames.unwrap_or(0).min(MAX_HINT)).unwrap_or(0)
}

/// A stream's shape: hertz, interleaved channels, and the frame count the
/// container declares, if it does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Spec {
    rate: u32,
    channels: usize,
    frames: Option<u64>,
}

/// What [`Frames::stream`] hands its callback.
#[derive(Clone, Copy)]
enum Event<'a> {
    /// The stream begins, or begins again with a new shape: drop what was
    /// taken so far.
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
    /// shape. A change of shape mid-stream starts it again: the whole-file
    /// decoder dropped what it had read when the channel count changed,
    /// and resampling earlier samples at a later rate would be noise.
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
                    frames: Some(reader.frame_count() as u64),
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
}

/// Symphonia's demuxer and decoder over one file.
struct SymphoniaFrames {
    path: PathBuf,
    format: Box<dyn FormatReader>,
    decoder: Box<dyn Decoder>,
    track_id: u32,
    frames: Option<u64>,
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
        let frames = track.codec_params.n_frames;
        let decoder = symphonia::default::get_codecs()
            .make(&track.codec_params, &DecoderOptions::default())
            .map_err(|e| CodecError::UnsupportedFormat(e.to_string()))?;
        Ok(Self {
            path: path.to_path_buf(),
            format,
            decoder,
            track_id,
            frames,
        })
    }

    /// Packet by packet; corrupt packets are skipped, a file with no
    /// decodable audio is an error.
    fn stream(
        &mut self,
        mut each: impl FnMut(Event<'_>) -> Result<(), CodecError>,
    ) -> Result<Spec, CodecError> {
        let mut spec: Option<Spec> = None;
        let mut buffer: Option<SampleBuffer<f32>> = None;
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
            let audio = match self.decoder.decode(&packet) {
                Ok(audio) => audio,
                Err(SymphoniaError::DecodeError(_)) => continue,
                Err(e) => return Err(CodecError::ConversionFailed(e.to_string())),
            };
            let shape = *audio.spec();
            let count = shape.channels.count();
            let packet_spec = Spec {
                rate: shape.rate,
                channels: count,
                frames: self.frames,
            };
            if spec != Some(packet_spec) {
                spec = Some(packet_spec);
                if count > 0 && shape.rate > 0 {
                    each(Event::Start(packet_spec))?;
                }
            }
            let buffer = buffer
                .get_or_insert_with(|| SampleBuffer::<f32>::new(audio.capacity() as u64, shape));
            if buffer.capacity() < audio.capacity() * count {
                *buffer = SampleBuffer::<f32>::new(audio.capacity() as u64, shape);
            }
            buffer.copy_interleaved_ref(audio);
            if count > 0 && shape.rate > 0 {
                each(Event::Frames(packet_spec, buffer.samples()))?;
            }
        }
        spec.filter(|spec| spec.channels > 0 && spec.rate > 0)
            .ok_or_else(|| {
                CodecError::UnsupportedFormat(format!(
                    "{}: no decodable audio",
                    self.path.display()
                ))
            })
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
    /// 16 kHz samples written since the stream started.
    written: usize,
    ints: Vec<i16>,
}

impl Mixdown<'_> {
    fn take(&mut self, event: Event<'_>) -> Result<(), CodecError> {
        match event {
            Event::Start(spec) => {
                // A new shape starts the file again, as it starts the lane.
                self.writer = Some(
                    WavStreamWriter::create(self.destination, 16_000)
                        .map_err(|e| CodecError::Io(e.to_string()))?,
                );
                self.resampler = Some(LaneResampler::new(spec.rate));
                self.pending.clear();
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

    fn finish(&mut self) -> Result<(), CodecError> {
        if let Some(resampler) = self.resampler.take() {
            resampler.finish(&mut self.pending);
            self.write(self.pending.len())?;
        }
        self.writer
            .as_mut()
            .map_or(Ok(()), WavStreamWriter::finish)
            .map_err(|e| CodecError::Io(e.to_string()))
    }
}

#[async_trait]
impl AudioDecoder for SymphoniaAudioCodec {
    async fn decode(&self, asset: &AudioAsset, lane: AudioLane) -> BoundaryResult<AudioBuffer16k> {
        if let Some(sidecar) = asset.sidecars_16k.get(&lane)
            && let Some(path) = file_url_path(sidecar)
            && let Ok(samples) = WavFile::read_16k_mono(&path)
            && !samples.is_empty()
        {
            return Ok(AudioBuffer16k::new(samples));
        }
        let channel = asset
            .lanes
            .iter()
            .position(|l| *l == lane)
            .ok_or(CodecError::LaneNotInAsset(lane))?;
        Ok(Self::decode_path(&master_path(asset)?, channel, lane)?)
    }

    /// `Wav16kInt16`: there is no AAC encoder in pure Rust (see the module
    /// doc); the Swift codec says `M4aAac`.
    fn mixdown_format(&self) -> AudioFormat {
        AudioFormat::Wav16kInt16
    }

    async fn mixdown(&self, asset: &AudioAsset, to: &Path) -> BoundaryResult<()> {
        let source = master_path(asset)?;
        if let Some(parent) = to.parent() {
            std::fs::create_dir_all(parent).map_err(|e| io_error(parent, &e))?;
        }
        if to.exists() {
            std::fs::remove_file(to).map_err(|e| io_error(to, &e))?;
        }
        if asset.format == AudioFormat::Wav16kInt16 {
            std::fs::copy(&source, to).map_err(|e| io_error(to, &e))?;
            return Ok(());
        }
        Ok(Self::mixdown_path(&source, to)?)
    }
}

/// The asset's master as a path; the URL must be a file URL.
fn master_path(asset: &AudioAsset) -> Result<PathBuf, CodecError> {
    file_url_path(&asset.url)
        .ok_or_else(|| CodecError::Io(format!("not a file URL: {}", asset.url)))
}

fn io_error(path: &Path, error: &std::io::Error) -> CodecError {
    CodecError::Io(format!("{}: {error}", path.display()))
}
