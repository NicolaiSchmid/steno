//! The program's [`AudioDecoder`] for real recordings, in pure Rust.
//! Swift: `Sources/StenoAudio/Codec/AVFoundationAudioCodec.swift`.
//!
//! `decode` returns the lane's 16 kHz sidecar when it is present and
//! complete, else reads the master through `symphonia` (CAF, WAV, m4a/AAC,
//! mp3) and resamples channel n (lane n of the master) to 16 kHz mono: the
//! exact 3:1 FIR for 48 kHz material, a windowed-sinc polyphase resampler
//! ([`sinc::SincResampler`]) for every other rate (the phone records
//! 44.1 kHz). Whole files are decoded to one channel of `f32` at the source
//! rate before resampling; a two-hour 48 kHz lane is 1.4 GB transiently,
//! which the chunked AVFoundation path avoided. Tracked as a parity item.
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
//! - **CAF**: PCM only (what the writer produces); a CAF holding AAC fails.
//! - An unfinished master (data chunk size -1) decodes to its last whole
//!   frame through the crate's own [`CafFile`]
//!   reader before symphonia is tried, as the Swift test demands.

pub mod sinc;

use std::path::Path;

use steno_core::{
    AudioAsset, AudioBuffer16k, AudioDecoder, AudioFormat, AudioLane, BoundaryResult, async_trait,
    paths::path_from_file_url,
};
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::DecoderOptions;
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::{MediaSourceStream, MediaSourceStreamOptions};
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

use crate::writer::{CafFile, WavFile, WavStreamWriter};
use crate::{FRAME_SIZE, SAMPLE_RATE};
use sinc::SincResampler;

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
    #[error("audio conversion failed: {0}")]
    ConversionFailed(String),
    #[error("{0}")]
    Io(String),
}

/// Decoded PCM at the source rate: one channel's samples.
#[derive(Debug, Clone, PartialEq)]
pub struct DecodedChannel {
    pub sample_rate: u32,
    pub channels: usize,
    pub samples: Vec<f32>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct SymphoniaAudioCodec;

impl SymphoniaAudioCodec {
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Channel `channel` of the file at `path`, resampled to 16 kHz mono.
    pub fn decode_path(
        path: &Path,
        channel: usize,
        lane: AudioLane,
    ) -> Result<AudioBuffer16k, CodecError> {
        let decoded = Self::read_channel(path, channel, lane)?;
        Ok(AudioBuffer16k::new(Self::to_16k(
            &decoded.samples,
            decoded.sample_rate,
        )))
    }

    /// One channel at the source rate. The crate's own CAF reader goes
    /// first so an unfinished master reads to its last whole frame.
    pub fn read_channel(
        path: &Path,
        channel: usize,
        lane: AudioLane,
    ) -> Result<DecodedChannel, CodecError> {
        if path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("caf"))
            && let Ok(file) = CafFile::read(path)
        {
            let channels = file.channels.len();
            let samples =
                file.channels
                    .into_iter()
                    .nth(channel)
                    .ok_or(CodecError::ChannelMissing {
                        lane,
                        channel,
                        channels,
                    })?;
            // The writer's rate is whole hertz.
            return Ok(DecodedChannel {
                sample_rate: file.sample_rate as u32,
                channels,
                samples,
            });
        }
        let all = Self::read_all_channels(path)?;
        let channels = all.len();
        let mut iter = all.into_iter();
        let Some((rate, samples)) = iter.nth(channel) else {
            return Err(CodecError::ChannelMissing {
                lane,
                channel,
                channels,
            });
        };
        Ok(DecodedChannel {
            sample_rate: rate,
            channels,
            samples,
        })
    }

    /// Every channel of the file through symphonia, with the rate.
    fn read_all_channels(path: &Path) -> Result<Vec<(u32, Vec<f32>)>, CodecError> {
        let file = std::fs::File::open(path)
            .map_err(|e| CodecError::Io(format!("{}: {e}", path.display())))?;
        let stream = MediaSourceStream::new(Box::new(file), MediaSourceStreamOptions::default());
        let mut hint = Hint::new();
        if let Some(extension) = path.extension().and_then(|e| e.to_str()) {
            hint.with_extension(extension);
        }
        let probed = symphonia::default::get_probe()
            .format(
                &hint,
                stream,
                &FormatOptions::default(),
                &MetadataOptions::default(),
            )
            .map_err(|e| CodecError::UnsupportedFormat(format!("{}: {e}", path.display())))?;
        let mut format = probed.format;
        let track = format
            .default_track()
            .ok_or_else(|| CodecError::UnsupportedFormat("no audio track".into()))?;
        let track_id = track.id;
        let mut decoder = symphonia::default::get_codecs()
            .make(&track.codec_params, &DecoderOptions::default())
            .map_err(|e| CodecError::UnsupportedFormat(e.to_string()))?;
        let mut channels: Vec<Vec<f32>> = Vec::new();
        let mut rate = track.codec_params.sample_rate.unwrap_or(0);
        let mut sample_buffer: Option<SampleBuffer<f32>> = None;
        loop {
            let packet = match format.next_packet() {
                Ok(packet) => packet,
                Err(SymphoniaError::IoError(e))
                    if e.kind() == std::io::ErrorKind::UnexpectedEof =>
                {
                    break;
                }
                Err(SymphoniaError::ResetRequired) => break,
                Err(e) => return Err(CodecError::ConversionFailed(e.to_string())),
            };
            if packet.track_id() != track_id {
                continue;
            }
            let audio = match decoder.decode(&packet) {
                Ok(audio) => audio,
                Err(SymphoniaError::DecodeError(_)) => continue,
                Err(e) => return Err(CodecError::ConversionFailed(e.to_string())),
            };
            let spec = *audio.spec();
            rate = spec.rate;
            let count = spec.channels.count();
            if channels.len() != count {
                channels = vec![Vec::new(); count];
            }
            let buffer = sample_buffer
                .get_or_insert_with(|| SampleBuffer::<f32>::new(audio.capacity() as u64, spec));
            if buffer.capacity() < audio.capacity() * count {
                *buffer = SampleBuffer::<f32>::new(audio.capacity() as u64, spec);
            }
            buffer.copy_interleaved_ref(audio);
            for (index, sample) in buffer.samples().iter().enumerate() {
                channels[index % count].push(*sample);
            }
        }
        if channels.is_empty() || rate == 0 {
            return Err(CodecError::UnsupportedFormat(format!(
                "{}: no decodable audio",
                path.display()
            )));
        }
        Ok(channels.into_iter().map(|c| (rate, c)).collect())
    }

    /// `samples` at `rate` to 16 kHz: exact length `round(len * 16000 /
    /// rate)`, trimmed or zero-padded so the 16 kHz lane lasts exactly as
    /// long as the master and segment counts stay stable.
    #[must_use]
    pub fn to_16k(samples: &[f32], rate: u32) -> Vec<f32> {
        // Lengths are exact in f64; the result is a small positive count.
        let expected =
            (samples.len() as f64 * AudioBuffer16k::SAMPLE_RATE / f64::from(rate)).round() as usize;
        let mut output = if f64::from(rate) == AudioBuffer16k::SAMPLE_RATE {
            samples.to_vec()
        } else if f64::from(rate) == SAMPLE_RATE {
            Self::decimate_48k(samples)
        } else {
            SincResampler::new(f64::from(rate), AudioBuffer16k::SAMPLE_RATE).resample(samples)
        };
        output.resize(expected, 0.0);
        output
    }

    /// The sidecar path's filter, frame by frame, compensated for its
    /// 95.5-sample group delay so the lane aligns with the master like a
    /// zero-phase conversion would.
    fn decimate_48k(samples: &[f32]) -> Vec<f32> {
        let mut resampler = crate::writer::Resampler48kTo16k::new(FRAME_SIZE);
        let delay_in = (crate::writer::Resampler48kTo16k::TAPS - 1) / 2;
        let mut padded = Vec::with_capacity(samples.len() + FRAME_SIZE * 2);
        // Pre-roll of the group delay so the first output sample lines up
        // with the first input sample; the tail then needs the same more.
        padded.extend(std::iter::repeat_n(0.0f32, 0));
        padded.extend_from_slice(samples);
        padded.extend(std::iter::repeat_n(0.0f32, delay_in + FRAME_SIZE));
        let remainder = padded.len() % FRAME_SIZE;
        if remainder != 0 {
            padded.extend(std::iter::repeat_n(0.0f32, FRAME_SIZE - remainder));
        }
        let mut out_frame = vec![0i16; FRAME_SIZE / 3];
        let mut output: Vec<f32> = Vec::with_capacity(padded.len() / 3);
        for frame in padded.as_chunks::<FRAME_SIZE>().0 {
            resampler.process(frame, &mut out_frame);
            output.extend(out_frame.iter().map(|&s| f32::from(s) / 32767.0));
        }
        // Drop the group delay (95.5 input samples is 31.83 output samples;
        // 32 keeps the sidecar and the master decode within a sample).
        let delay_out = delay_in.div_ceil(3);
        output.drain(..delay_out.min(output.len()));
        output
    }

    /// Averages every channel of `source` to mono and writes 16 kHz Int16
    /// WAV to `destination`.
    pub fn mixdown_path(source: &Path, destination: &Path) -> Result<(), CodecError> {
        let channels: Vec<(u32, Vec<f32>)> = if source
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("caf"))
            && let Ok(file) = CafFile::read(source)
        {
            let rate = file.sample_rate as u32;
            file.channels.into_iter().map(|c| (rate, c)).collect()
        } else {
            Self::read_all_channels(source)?
        };
        let Some((rate, first)) = channels.first() else {
            return Err(CodecError::UnsupportedFormat("no channels".into()));
        };
        let rate = *rate;
        let frames = first.len();
        // Channel counts are tiny.
        let scale = 1.0 / channels.len().max(1) as f32;
        let mut mono = vec![0.0f32; frames];
        for (_, channel) in &channels {
            for (out, sample) in mono.iter_mut().zip(channel) {
                *out += sample * scale;
            }
        }
        let resampled = Self::to_16k(&mono, rate);
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| CodecError::Io(format!("{}: {e}", parent.display())))?;
        }
        let mut writer = WavStreamWriter::create(destination, 16_000)
            .map_err(|e| CodecError::Io(e.to_string()))?;
        // Clamped first, so the cast is exact.
        let ints: Vec<i16> = resampled
            .iter()
            .map(|s| (s.clamp(-1.0, 1.0) * 32767.0).round() as i16)
            .collect();
        writer
            .write(&ints)
            .and_then(|()| writer.finish())
            .map_err(|e| CodecError::Io(e.to_string()))
    }
}

#[async_trait]
impl AudioDecoder for SymphoniaAudioCodec {
    async fn decode(&self, asset: &AudioAsset, lane: AudioLane) -> BoundaryResult<AudioBuffer16k> {
        if let Some(sidecar) = asset.sidecars_16k.get(&lane)
            && let Some(path) = path_from_file_url(sidecar)
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
        let path = path_from_file_url(&asset.url)
            .ok_or_else(|| CodecError::Io(format!("not a file URL: {}", asset.url)))?;
        Ok(Self::decode_path(&path, channel, lane)?)
    }

    /// `Wav16kInt16`: there is no AAC encoder in pure Rust (see the module
    /// doc); the Swift codec says `M4aAac`.
    fn mixdown_format(&self) -> AudioFormat {
        AudioFormat::Wav16kInt16
    }

    async fn mixdown(&self, asset: &AudioAsset, to: &Path) -> BoundaryResult<()> {
        let source = path_from_file_url(&asset.url)
            .ok_or_else(|| CodecError::Io(format!("not a file URL: {}", asset.url)))?;
        if let Some(parent) = to.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| CodecError::Io(format!("{}: {e}", parent.display())))?;
        }
        if to.exists() {
            std::fs::remove_file(to)
                .map_err(|e| CodecError::Io(format!("{}: {e}", to.display())))?;
        }
        if asset.format == AudioFormat::Wav16kInt16 {
            std::fs::copy(&source, to)
                .map_err(|e| CodecError::Io(format!("{}: {e}", to.display())))?;
            return Ok(());
        }
        Ok(Self::mixdown_path(&source, to)?)
    }
}
