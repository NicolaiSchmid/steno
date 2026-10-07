//! RIFF/WAVE: the streaming 16 kHz mono Int16 sidecar writer and a reader
//! for any PCM WAV (16-bit integer or 32-bit float, any rate, any channel
//! count). Swift: `Sources/StenoAudio/Writer/WAVStreamWriter.swift` and
//! `WAVFile.swift`.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use super::bytes::{ReadAt, WindowedFile};
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

    /// Makes every sample written so far durable with `File::sync_data`,
    /// as [`CafStreamWriter::sync`](super::CafStreamWriter::sync) does for
    /// the master. Nothing after `finish`. Rust only: Swift synced at the
    /// close alone.
    pub fn sync(&mut self) -> std::io::Result<()> {
        self.file
            .as_ref()
            .map_or(Ok(()), |file| durable::sync(file, File::sync_data))
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

    /// Finishes a sidecar whose writer died before [`Self::finish`]: the
    /// sizes come from the file's length, whole samples only (a sample cut
    /// short at the end is cut off), and the file is synced. Only a file
    /// whose header is this writer's at 16 kHz, sizes aside, is touched
    /// (every sidecar's rate, which [`WavFile::read_16k_mono`] demands); a
    /// finished one is written back as it was. Returns the samples it
    /// holds. Crash recovery calls it, so the lane reads from its sidecar
    /// rather than being rebuilt from the master. Rust only: Swift had no
    /// recovery.
    pub fn recover(path: &Path) -> Result<usize, WavReadError> {
        let io = |e: std::io::Error| WavReadError::Io(format!("{}: {e}", path.display()));
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .map_err(io)?;
        let length = file.metadata().map_err(io)?.len();
        if length < Self::HEADER_SIZE as u64 {
            return Err(WavReadError::Malformed("shorter than its header".into()));
        }
        let mut header = [0u8; Self::HEADER_SIZE];
        file.read_exact(&mut header).map_err(io)?;
        let sample_rate = 16_000;
        let expected = Self::header(sample_rate, 0);
        // Everything but the two sizes: RIFF size at 4, data size at 40.
        if header[..4] != expected[..4] || header[8..40] != expected[8..40] {
            return Err(WavReadError::Malformed(
                "not a 16 kHz 16-bit mono sidecar header".into(),
            ));
        }
        let bytes = length - Self::HEADER_SIZE as u64;
        let samples = usize::try_from(bytes / Self::BYTES_PER_SAMPLE as u64)
            .map_err(|_| WavReadError::Malformed("too long for a WAV file".into()))?;
        let data_size = samples * Self::BYTES_PER_SAMPLE;
        if u32::try_from(Self::RIFF_SIZE_BEFORE_DATA + data_size).is_err() {
            return Err(WavReadError::Malformed("too long for a WAV file".into()));
        }
        file.set_len((Self::HEADER_SIZE + data_size) as u64)
            .map_err(io)?;
        file.seek(SeekFrom::Start(0)).map_err(io)?;
        file.write_all(&Self::header(sample_rate, samples))
            .map_err(io)?;
        file.sync_all().map_err(io)?;
        Ok(samples)
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
    pub fn read_bytes(mut data: &[u8]) -> Result<Self, WavReadError> {
        let layout = WavLayout::read(&mut data)?;
        let bytes_per_sample = layout.bytes_per_sample();
        let bytes_per_frame = layout.bytes_per_frame();
        let mut channels = vec![vec![0.0f32; layout.frames]; layout.channels];
        for frame in 0..layout.frames {
            for (channel, lane) in channels.iter_mut().enumerate() {
                let at = layout.start + frame * bytes_per_frame + channel * bytes_per_sample;
                lane[frame] = layout.sample(&data[at..at + bytes_per_sample]);
            }
        }
        Ok(Self {
            sample_rate: layout.sample_rate,
            bits_per_sample: layout.bits_per_sample,
            is_float: layout.is_float,
            channels,
        })
    }

    /// The strict sidecar reader: 16 kHz mono, as core's `WAVAudioDecoder`.
    /// Reads a block at a time into a buffer of the lane's length, so
    /// nothing but the lane is held.
    pub fn read_16k_mono(path: &Path) -> Result<Vec<f32>, WavReadError> {
        let io = |e: std::io::Error| WavReadError::Io(e.to_string());
        let mut file = WindowedFile::open(path).map_err(io)?;
        let layout = WavLayout::read(&mut file)?;
        if layout.sample_rate != 16_000 || layout.channels != 1 {
            return Err(WavReadError::UnsupportedFormat(format!(
                "{} Hz, {} channel(s); need 16000 Hz mono",
                layout.sample_rate, layout.channels
            )));
        }
        let bytes_per_sample = layout.bytes_per_sample();
        let mut samples = Vec::with_capacity(layout.frames);
        let mut block = vec![0u8; Self::BLOCK_FRAMES * bytes_per_sample];
        let mut read = 0;
        while read < layout.frames {
            let frames = Self::BLOCK_FRAMES.min(layout.frames - read);
            let bytes = &mut block[..frames * bytes_per_sample];
            file.read_at(layout.start + read * bytes_per_sample, bytes)
                .map_err(io)?;
            samples.extend(
                bytes
                    .chunks_exact(bytes_per_sample)
                    .map(|b| layout.sample(b)),
            );
            read += frames;
        }
        Ok(samples)
    }

    /// Seconds, from the headers alone, so a long file is not read: the
    /// data chunk's whole frames over the sample rate.
    pub fn read_duration(path: &Path) -> Result<f64, WavReadError> {
        let mut file = WindowedFile::open(path).map_err(|e| WavReadError::Io(e.to_string()))?;
        let layout = WavLayout::read(&mut file)?;
        Ok(layout.frames as f64 / f64::from(layout.sample_rate.max(1)))
    }

    /// Frames per read in `read_16k_mono`.
    const BLOCK_FRAMES: usize = 32_768;
}

/// Where a WAV's samples sit and how they are coded, from its chunk
/// headers.
#[derive(Debug, Clone, Copy)]
struct WavLayout {
    sample_rate: u32,
    bits_per_sample: u16,
    is_float: bool,
    /// At least one.
    channels: usize,
    /// The file offset of the first sample.
    start: usize,
    /// Whole frames from `start`.
    frames: usize,
}

impl WavLayout {
    /// Walks the chunks of `source`.
    fn read(source: &mut impl ReadAt) -> Result<Self, WavReadError> {
        let len = source.len();
        let mut read = |offset: usize, buffer: &mut [u8]| {
            source
                .read_at(offset, buffer)
                .map_err(|e| WavReadError::Io(e.to_string()))
        };
        let mut tags = [0u8; 12];
        if len >= 12 {
            read(0, &mut tags)?;
        }
        if &tags[..4] != b"RIFF" || &tags[8..12] != b"WAVE" {
            return Err(WavReadError::Malformed("missing RIFF/WAVE tags".into()));
        }
        let mut offset = 12;
        let mut format: Option<(u16, usize, u32, u16)> = None;
        let mut samples: Option<std::ops::Range<usize>> = None;
        while offset + 8 <= len {
            let mut header = [0u8; 8];
            read(offset, &mut header)?;
            let id = &header[..4];
            let size = le_u32(&header, 4) as usize;
            let body = offset + 8;
            if body + size > len {
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
                    let mut data = [0u8; 26];
                    let data = &mut data[..size.min(26)];
                    read(body, data)?;
                    let mut tag = le_u16(data, 0);
                    if tag == 0xFFFE && size >= 26 {
                        tag = le_u16(data, 24);
                    }
                    format = Some((
                        tag,
                        le_u16(data, 2) as usize,
                        le_u32(data, 4),
                        le_u16(data, 14),
                    ));
                }
                b"data" => samples = Some(body..body + size),
                _ => {}
            }
            offset = body + size + (size % 2);
        }
        let Some((tag, channel_count, sample_rate, bits_per_sample)) = format else {
            return Err(WavReadError::Malformed("no fmt chunk".into()));
        };
        let Some(range) = samples else {
            return Err(WavReadError::Malformed("no data chunk".into()));
        };
        let is_float = match (tag, bits_per_sample) {
            (1, 16) => false,
            (3, 32) => true,
            _ => {
                return Err(WavReadError::UnsupportedFormat(format!(
                    "format tag {tag} at {bits_per_sample} bits; need 16-bit integer or 32-bit float"
                )));
            }
        };
        let channels = channel_count.max(1);
        let bytes_per_frame = usize::from(bits_per_sample / 8) * channels;
        Ok(Self {
            sample_rate,
            bits_per_sample,
            is_float,
            channels,
            start: range.start,
            frames: range.len() / bytes_per_frame,
        })
    }

    fn bytes_per_sample(&self) -> usize {
        usize::from(self.bits_per_sample / 8)
    }

    fn bytes_per_frame(&self) -> usize {
        self.bytes_per_sample() * self.channels
    }

    /// One sample's bytes as `f32` in -1..1.
    fn sample(&self, bytes: &[u8]) -> f32 {
        if self.is_float {
            f32::from_le_bytes(bytes.try_into().unwrap_or([0; 4]))
        } else {
            f32::from(i16::from_le_bytes(bytes.try_into().unwrap_or([0; 2]))) / 32768.0
        }
    }
}

fn le_u16(data: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes(data[offset..offset + 2].try_into().unwrap_or([0; 2]))
}

fn le_u32(data: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap_or([0; 4]))
}
