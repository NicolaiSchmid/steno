//! RIFF/WAVE: the streaming 16 kHz mono Int16 sidecar writer and a reader
//! for any PCM WAV (16-bit integer or 32-bit float, any rate, any channel
//! count). Swift: `Sources/StenoAudio/Writer/WAVStreamWriter.swift` and
//! `WAVFile.swift`.

use std::fs::File;
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use super::{durable, io_error};
use crate::capture::CaptureError;

/// Streams 16 kHz mono Int16 PCM into a RIFF/WAVE file: header with zero
/// sizes first, samples appended per frame, sizes patched by `finish()`.
/// A file whose process died before `finish` has zero sizes; the master
/// CAF is the recoverable copy, and the decoder rebuilds the lane from it.
pub struct WavStreamWriter {
    path: PathBuf,
    sample_rate: u32,
    samples_written: usize,
    file: Option<File>,
    scratch: Vec<u8>,
}

impl std::fmt::Debug for WavStreamWriter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WavStreamWriter")
            .field("path", &self.path)
            .field("samples_written", &self.samples_written)
            .finish_non_exhaustive()
    }
}

impl WavStreamWriter {
    /// `RIFF` size + `WAVE`, the 16-byte `fmt ` chunk with its header, and
    /// the `data` chunk header: 44 bytes before the first sample.
    pub const HEADER_SIZE: usize = 44;
    /// The RIFF size field counts everything after itself: the header minus
    /// the 8-byte `RIFF` chunk header, plus the samples.
    pub const RIFF_SIZE_BEFORE_DATA: usize = Self::HEADER_SIZE - 8;
    /// Int16.
    pub const BYTES_PER_SAMPLE: usize = 2;

    /// Writes a header with zero sizes; `sample_rate` in hertz.
    pub fn create(path: &Path, sample_rate: u32) -> Result<Self, CaptureError> {
        let mut file = File::create(path).map_err(|e| io_error(path, &e))?;
        file.write_all(&Self::header(sample_rate, 0))
            .map_err(|e| io_error(path, &e))?;
        Ok(Self {
            path: path.to_path_buf(),
            sample_rate,
            samples_written: 0,
            file: Some(file),
            scratch: Vec::new(),
        })
    }

    /// Where it writes.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Samples written so far.
    #[must_use]
    pub fn samples_written(&self) -> usize {
        self.samples_written
    }

    /// Appends `samples`.
    pub fn write(&mut self, samples: &[i16]) -> Result<(), CaptureError> {
        let Some(file) = self.file.as_mut() else {
            return Ok(());
        };
        if samples.is_empty() {
            return Ok(());
        }
        let bytes = samples.len() * Self::BYTES_PER_SAMPLE;
        if self.scratch.len() < bytes {
            self.scratch.resize(bytes, 0);
        }
        for (chunk, sample) in self.scratch[..bytes]
            .as_chunks_mut::<2>()
            .0
            .iter_mut()
            .zip(samples)
        {
            *chunk = sample.to_le_bytes();
        }
        file.write_all(&self.scratch[..bytes])
            .map_err(|e| io_error(&self.path, &e))?;
        self.samples_written += samples.len();
        Ok(())
    }

    /// Patches the sizes, syncs and closes; once.
    pub fn finish(&mut self) -> Result<(), CaptureError> {
        let Some(mut file) = self.file.take() else {
            return Ok(());
        };
        file.seek(SeekFrom::Start(0))
            .map_err(|e| io_error(&self.path, &e))?;
        file.write_all(&Self::header(self.sample_rate, self.samples_written))
            .map_err(|e| io_error(&self.path, &e))?;
        durable::sync(&file, File::sync_all).map_err(|e| io_error(&self.path, &e))?;
        Ok(())
    }

    /// Seconds written.
    #[must_use]
    pub fn duration(&self) -> f64 {
        let samples = self.samples_written as f64;
        samples / f64::from(self.sample_rate)
    }

    /// The 44-byte RIFF, `fmt ` and `data` headers for a 16-bit mono file.
    #[must_use]
    pub fn header(sample_rate: u32, sample_count: usize) -> Vec<u8> {
        let data_size = (sample_count * Self::BYTES_PER_SAMPLE) as u32;
        let mut data = Vec::with_capacity(Self::HEADER_SIZE);
        data.extend_from_slice(b"RIFF");
        data.extend_from_slice(&(Self::RIFF_SIZE_BEFORE_DATA as u32 + data_size).to_le_bytes());
        data.extend_from_slice(b"WAVE");
        data.extend_from_slice(b"fmt ");
        data.extend_from_slice(&16u32.to_le_bytes()); // fmt chunk size
        data.extend_from_slice(&1u16.to_le_bytes()); // PCM
        data.extend_from_slice(&1u16.to_le_bytes()); // mono
        data.extend_from_slice(&sample_rate.to_le_bytes());
        data.extend_from_slice(&(sample_rate * Self::BYTES_PER_SAMPLE as u32).to_le_bytes()); // bytes per second
        data.extend_from_slice(&(Self::BYTES_PER_SAMPLE as u16).to_le_bytes()); // block align
        data.extend_from_slice(&((Self::BYTES_PER_SAMPLE * 8) as u16).to_le_bytes()); // bits per sample
        data.extend_from_slice(b"data");
        data.extend_from_slice(&data_size.to_le_bytes());
        data
    }
}

/// Why a WAV could not be read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WavReadError {
    /// Not 16-bit integer or 32-bit float PCM.
    #[error("unsupported WAV format: {0}")]
    UnsupportedFormat(String),
    /// Not a RIFF/WAVE file, or a chunk is truncated.
    #[error("malformed WAV file: {0}")]
    Malformed(String),
    /// A read failed: the path and the error.
    #[error("{0}")]
    Io(String),
}

/// A PCM RIFF/WAVE file as de-interleaved channels: what the sidecar writer
/// and common recorders produce.
#[derive(Debug, Clone, PartialEq)]
pub struct WavFile {
    /// Hertz.
    pub sample_rate: u32,
    /// 16 or 32.
    pub bits_per_sample: u16,
    /// 32-bit float rather than 16-bit integer.
    pub is_float: bool,
    /// De-interleaved, as `f32` in -1..1.
    pub channels: Vec<Vec<f32>>,
}

impl WavFile {
    /// Frames per channel.
    #[must_use]
    pub fn frame_count(&self) -> usize {
        self.channels.first().map_or(0, Vec::len)
    }

    /// Seconds.
    #[must_use]
    pub fn duration(&self) -> f64 {
        let frames = self.frame_count() as f64;
        frames / f64::from(self.sample_rate)
    }

    /// Reads the whole file.
    pub fn read(path: &Path) -> Result<Self, WavReadError> {
        let data = std::fs::read(path)
            .map_err(|e| WavReadError::Io(format!("{}: {e}", path.display())))?;
        Self::read_bytes(&data)
    }

    /// Parses `data` as RIFF/WAVE.
    pub fn read_bytes(data: &[u8]) -> Result<Self, WavReadError> {
        if data.len() < 12 || &data[..4] != b"RIFF" || &data[8..12] != b"WAVE" {
            return Err(WavReadError::Malformed("missing RIFF/WAVE tags".into()));
        }
        let mut offset = 12;
        let mut format: Option<(u16, usize, u32, u16)> = None;
        let mut samples: Option<std::ops::Range<usize>> = None;
        while offset + 8 <= data.len() {
            let id = &data[offset..offset + 4];
            let size = le_u32(data, offset + 4) as usize;
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
                    let mut tag = le_u16(data, body);
                    if tag == 0xFFFE && size >= 26 {
                        tag = le_u16(data, body + 24);
                    }
                    format = Some((
                        tag,
                        le_u16(data, body + 2) as usize,
                        le_u32(data, body + 4),
                        le_u16(data, body + 14),
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
        Ok(Self {
            sample_rate: rate,
            bits_per_sample: bits,
            is_float,
            channels,
        })
    }

    /// The strict sidecar reader: 16 kHz mono, as core's `WAVAudioDecoder`.
    pub fn read_16k_mono(path: &Path) -> Result<Vec<f32>, WavReadError> {
        let file = Self::read(path)?;
        if file.sample_rate != 16_000 || file.channels.len() != 1 {
            return Err(WavReadError::UnsupportedFormat(format!(
                "{} Hz, {} channel(s); need 16000 Hz mono",
                file.sample_rate,
                file.channels.len()
            )));
        }
        Ok(file.channels.into_iter().next().unwrap_or_default())
    }
}

fn le_u16(data: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes(data[offset..offset + 2].try_into().unwrap_or([0; 2]))
}

fn le_u32(data: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap_or([0; 4]))
}
