//! The decoder as it was before it streamed, kept verbatim as the
//! reference `tests/codec_streaming.rs` compares the streaming decoder
//! against: the whole file read into memory, every channel de-interleaved
//! at the source rate, then resampled in one pass. Its own copies of the
//! CAF and WAV readers and of the windowed sinc, so a change to the
//! crate's readers or resamplers cannot move the reference with them; the
//! 3:1 FIR is the writer's (`Resampler48kTo16k`), which the change does
//! not touch.

#![allow(dead_code)]

use std::path::Path;

use steno_audio::codec::CodecError;
use steno_audio::writer::{CafStreamWriter, Resampler48kTo16k, WavReadError, WavStreamWriter};
use steno_core::paths::file_url_path;
use steno_core::{AudioAsset, AudioFormat, AudioLane};
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::DecoderOptions;
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::{MediaSourceStream, MediaSourceStreamOptions};
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

const FRAME_SIZE: usize = 480;
const SAMPLE_RATE: f64 = 48_000.0;
const SAMPLE_RATE_16K: f64 = 16_000.0;

/// `AudioDecoder::decode`: the sidecar when it reads and is not empty,
/// else the master's channel for `lane` at 16 kHz.
pub fn decode(asset: &AudioAsset, lane: AudioLane) -> Result<Vec<f32>, CodecError> {
    if let Some(sidecar) = asset.sidecars_16k.get(&lane)
        && let Some(path) = file_url_path(sidecar)
        && let Ok(samples) = read_16k_mono(&path)
        && !samples.is_empty()
    {
        return Ok(samples);
    }
    let channel = asset
        .lanes
        .iter()
        .position(|l| *l == lane)
        .ok_or(CodecError::LaneNotInAsset(lane))?;
    let path = file_url_path(&asset.url).unwrap();
    decode_path(&path, channel, lane)
}

/// `SymphoniaAudioCodec::decode_path`.
pub fn decode_path(path: &Path, channel: usize, lane: AudioLane) -> Result<Vec<f32>, CodecError> {
    let (sample_rate, samples) = read_channel(path, channel, lane)?;
    Ok(to_16k(&samples, sample_rate))
}

/// `SymphoniaAudioCodec::read_channel`: the rate and the channel.
pub fn read_channel(
    path: &Path,
    channel: usize,
    lane: AudioLane,
) -> Result<(u32, Vec<f32>), CodecError> {
    let (sample_rate, all) = read_all(path)?;
    let channels = all.len();
    let samples = all
        .into_iter()
        .nth(channel)
        .ok_or(CodecError::ChannelMissing {
            lane,
            channel,
            channels,
        })?;
    Ok((sample_rate, samples))
}

fn read_all(path: &Path) -> Result<(u32, Vec<Vec<f32>>), CodecError> {
    if path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("caf"))
        && let Ok((rate, channels)) = read_caf(path)
    {
        return Ok((rate as u32, channels));
    }
    read_all_channels(path)
}

fn read_all_channels(path: &Path) -> Result<(u32, Vec<Vec<f32>>), CodecError> {
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
            &FormatOptions {
                enable_gapless: true,
                ..FormatOptions::default()
            },
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
            Err(SymphoniaError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
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
    Ok((rate, channels))
}

/// `SymphoniaAudioCodec::to_16k`.
pub fn to_16k(samples: &[f32], rate: u32) -> Vec<f32> {
    let expected = (samples.len() as f64 * SAMPLE_RATE_16K / f64::from(rate)).round() as usize;
    let mut output = if f64::from(rate) == SAMPLE_RATE_16K {
        samples.to_vec()
    } else if f64::from(rate) == SAMPLE_RATE {
        decimate_48k(samples)
    } else {
        sinc_resample(f64::from(rate), SAMPLE_RATE_16K, samples)
    };
    output.resize(expected, 0.0);
    output
}

fn decimate_48k(samples: &[f32]) -> Vec<f32> {
    let mut resampler = Resampler48kTo16k::new(FRAME_SIZE);
    let delay_in = (Resampler48kTo16k::TAPS - 1) / 2;
    let mut padded = Vec::with_capacity(samples.len() + FRAME_SIZE * 2);
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
    let delay_out = delay_in.div_ceil(3);
    output.drain(..delay_out.min(output.len()));
    output
}

/// `SincResampler::new(input_rate, output_rate).resample(input)`.
pub fn sinc_resample(input_rate: f64, output_rate: f64, input: &[f32]) -> Vec<f32> {
    const PHASES: usize = 128;
    const TAPS: usize = 64;
    let ratio = input_rate / output_rate;
    let cutoff = 0.45 * output_rate.min(input_rate) / input_rate;
    let taps = TAPS;
    let mut table = Vec::with_capacity((PHASES + 1) * taps);
    for phase in 0..=PHASES {
        let fraction = phase as f64 / PHASES as f64;
        let mut coefficients = Vec::with_capacity(taps);
        let centre = (taps / 2) as f64;
        let denominator = Resampler48kTo16k::bessel_i0(9.0);
        let mut sum = 0.0f64;
        for k in 0..taps {
            let x = k as f64 - centre + 1.0 - fraction;
            let sinc = if x == 0.0 {
                2.0 * cutoff
            } else {
                (2.0 * std::f64::consts::PI * cutoff * x).sin() / (std::f64::consts::PI * x)
            };
            let ratio_k = x / centre;
            let window = if ratio_k.abs() >= 1.0 {
                0.0
            } else {
                Resampler48kTo16k::bessel_i0(9.0 * (1.0 - ratio_k * ratio_k).sqrt()) / denominator
            };
            coefficients.push(sinc * window);
            sum += sinc * window;
        }
        table.extend(coefficients.iter().map(|c| (c / sum) as f32));
    }
    if input.is_empty() {
        return Vec::new();
    }
    let half = taps / 2;
    let count = (input.len() as f64 / ratio).ceil() as usize;
    let mut output = Vec::with_capacity(count);
    for n in 0..count {
        let position = n as f64 * ratio;
        let index = position.floor();
        let fraction = position - index;
        let index = index as usize;
        let phase = (fraction * PHASES as f64) as usize;
        let blend = (fraction * PHASES as f64 - phase as f64) as f32;
        let low = &table[phase * taps..(phase + 1) * taps];
        let high = &table[(phase + 1) * taps..(phase + 2) * taps];
        let mut accumulator = 0.0f32;
        for k in 0..taps {
            let Some(at) = (index + 1 + k).checked_sub(half) else {
                continue;
            };
            if at >= input.len() {
                continue;
            }
            let coefficient = low[k] + (high[k] - low[k]) * blend;
            accumulator += coefficient * input[at];
        }
        output.push(accumulator);
    }
    output
}

/// `SymphoniaAudioCodec::mixdown_path`.
pub fn mixdown_path(source: &Path, destination: &Path) -> Result<(), CodecError> {
    let (rate, channels) = read_all(source)?;
    let Some(first) = channels.first() else {
        return Err(CodecError::UnsupportedFormat("no channels".into()));
    };
    let scale = 1.0 / channels.len() as f32;
    let mut mono = vec![0.0f32; first.len()];
    for channel in &channels {
        for (out, sample) in mono.iter_mut().zip(channel) {
            *out += sample * scale;
        }
    }
    let resampled = to_16k(&mono, rate);
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent).map_err(|e| io_error(parent, &e))?;
    }
    let mut writer =
        WavStreamWriter::create(destination, 16_000).map_err(|e| CodecError::Io(e.to_string()))?;
    let ints: Vec<i16> = resampled
        .iter()
        .map(|s| (s.clamp(-1.0, 1.0) * 32767.0).round() as i16)
        .collect();
    writer
        .write(&ints)
        .and_then(|()| writer.finish())
        .map_err(|e| CodecError::Io(e.to_string()))
}

/// `AudioDecoder::mixdown`.
pub fn mixdown(asset: &AudioAsset, to: &Path) -> Result<(), CodecError> {
    let source = file_url_path(&asset.url).unwrap();
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
    mixdown_path(&source, to)
}

fn io_error(path: &Path, error: &std::io::Error) -> CodecError {
    CodecError::Io(format!("{}: {error}", path.display()))
}

/// `CafFile::read`: the rate and the de-interleaved channels, or why not.
pub fn read_caf(path: &Path) -> Result<(f64, Vec<Vec<f32>>), String> {
    let data = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if data.len() < 8 || &data[..4] != b"caff" {
        return Err("missing caff header".into());
    }
    let mut offset = 8;
    let mut format: Option<(f64, u32, usize, usize, [u8; 4])> = None;
    let mut samples: Option<(usize, usize)> = None;
    while offset + 12 <= data.len() {
        let kind: [u8; 4] = data[offset..offset + 4].try_into().unwrap_or(*b"????");
        let size = i64::from_be_bytes(data[offset + 4..offset + 12].try_into().unwrap_or([0; 8]));
        let body = offset + 12;
        match &kind {
            b"desc" => {
                let desc = CafStreamWriter::DESC_CHUNK_SIZE;
                if size < 0 || (size as u64) < desc as u64 || body + desc > data.len() {
                    return Err("desc chunk too short".into());
                }
                format = Some((
                    f64::from_bits(be_u64(&data, body)),
                    be_u32(&data, body + 12),
                    be_u32(&data, body + 24) as usize,
                    be_u32(&data, body + 28) as usize,
                    data[body + 8..body + 12].try_into().unwrap_or(*b"????"),
                ));
            }
            b"data" => {
                let edit = CafStreamWriter::EDIT_COUNT_SIZE;
                if body + edit > data.len() {
                    return Err("data chunk too short".into());
                }
                let available = data.len() - body - edit;
                let count = if size < 0 {
                    available
                } else {
                    usize::try_from(size)
                        .unwrap_or(usize::MAX)
                        .saturating_sub(edit)
                        .min(available)
                };
                samples = Some((body + edit, count));
            }
            _ => {}
        }
        if size < 0 {
            break;
        }
        offset = body.saturating_add(usize::try_from(size).unwrap_or(usize::MAX));
    }
    let Some((rate, flags, channel_count, bits, format_id)) = format else {
        return Err("no desc chunk".into());
    };
    let Some((start, count)) = samples else {
        return Err("no data chunk".into());
    };
    let wanted = CafStreamWriter::FLOAT_LITTLE_ENDIAN_FLAGS;
    if &format_id != b"lpcm" || flags & wanted != wanted || bits != 32 {
        return Err("unsupported".into());
    }
    let channel_count = channel_count.max(1);
    let frames = count / (4 * channel_count);
    let mut channels = vec![vec![0.0f32; frames]; channel_count];
    for frame in 0..frames {
        for (channel, lane) in channels.iter_mut().enumerate() {
            let at = start + (frame * channel_count + channel) * 4;
            lane[frame] = f32::from_le_bytes(data[at..at + 4].try_into().unwrap_or([0; 4]));
        }
    }
    Ok((rate, channels))
}

/// `WavFile::read_16k_mono`.
pub fn read_16k_mono(path: &Path) -> Result<Vec<f32>, WavReadError> {
    let (rate, channels) = read_wav(path)?;
    if rate != 16_000 || channels.len() != 1 {
        return Err(WavReadError::UnsupportedFormat(format!(
            "{} Hz, {} channel(s); need 16000 Hz mono",
            rate,
            channels.len()
        )));
    }
    Ok(channels.into_iter().next().unwrap_or_default())
}

/// `WavFile::read`: the rate and the de-interleaved channels.
pub fn read_wav(path: &Path) -> Result<(u32, Vec<Vec<f32>>), WavReadError> {
    let data =
        std::fs::read(path).map_err(|e| WavReadError::Io(format!("{}: {e}", path.display())))?;
    if data.len() < 12 || &data[..4] != b"RIFF" || &data[8..12] != b"WAVE" {
        return Err(WavReadError::Malformed("missing RIFF/WAVE tags".into()));
    }
    let mut offset = 12;
    let mut format: Option<(u16, usize, u32, u16)> = None;
    let mut samples: Option<std::ops::Range<usize>> = None;
    while offset + 8 <= data.len() {
        let id = &data[offset..offset + 4];
        let size = le_u32(&data, offset + 4) as usize;
        let body = offset + 8;
        if body + size > data.len() {
            return Err(WavReadError::Malformed(format!(
                "chunk {} runs past the end of the file",
                String::from_utf8_lossy(id)
            )));
        }
        match id {
            b"fmt " => {
                if size < 16 {
                    return Err(WavReadError::Malformed("fmt chunk too short".into()));
                }
                let mut tag = le_u16(&data, body);
                if tag == 0xFFFE && size >= 26 {
                    tag = le_u16(&data, body + 24);
                }
                format = Some((
                    tag,
                    le_u16(&data, body + 2) as usize,
                    le_u32(&data, body + 4),
                    le_u16(&data, body + 14),
                ));
            }
            b"data" => samples = Some(body..body + size),
            _ => {}
        }
        offset = body + size + (size % 2);
    }
    let Some((tag, channel_count, rate, bits)) = format else {
        return Err(WavReadError::Malformed("no fmt chunk".into()));
    };
    let Some(range) = samples else {
        return Err(WavReadError::Malformed("no data chunk".into()));
    };
    let is_float = match (tag, bits) {
        (1, 16) => false,
        (3, 32) => true,
        _ => {
            return Err(WavReadError::UnsupportedFormat(format!(
                "format tag {tag} at {bits} bits; need 16-bit integer or 32-bit float"
            )));
        }
    };
    let channel_count = channel_count.max(1);
    let bytes_per_sample = usize::from(bits / 8);
    let bytes_per_frame = bytes_per_sample * channel_count;
    let frames = range.len() / bytes_per_frame;
    let mut channels = vec![vec![0.0f32; frames]; channel_count];
    for frame in 0..frames {
        for (channel, lane) in channels.iter_mut().enumerate() {
            let at = range.start + frame * bytes_per_frame + channel * bytes_per_sample;
            lane[frame] = if is_float {
                f32::from_le_bytes(data[at..at + 4].try_into().unwrap_or([0; 4]))
            } else {
                f32::from(i16::from_le_bytes(
                    data[at..at + 2].try_into().unwrap_or([0; 2]),
                )) / 32768.0
            };
        }
    }
    Ok((rate, channels))
}

fn be_u32(data: &[u8], offset: usize) -> u32 {
    u32::from_be_bytes(data[offset..offset + 4].try_into().unwrap_or([0; 4]))
}

fn be_u64(data: &[u8], offset: usize) -> u64 {
    u64::from_be_bytes(data[offset..offset + 8].try_into().unwrap_or([0; 8]))
}

fn le_u16(data: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes(data[offset..offset + 2].try_into().unwrap_or([0; 2]))
}

fn le_u32(data: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap_or([0; 4]))
}
