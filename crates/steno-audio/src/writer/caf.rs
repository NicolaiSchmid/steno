//! A crash-tolerant CAF writer and the reader for what it writes.
//! Swift: `Sources/StenoAudio/Writer/CAFFile.swift`.
//!
//! Float32 little-endian PCM: `caff` header, `desc` chunk, then a `data`
//! chunk whose size is -1 ("to the end of the file", the streaming form
//! Core Audio's own writers use) while recording. Every `write` appends
//! whole frames, so a process killed mid-recording leaves a file any CAF
//! reader opens up to the last frame written; `finish()` patches the real
//! size in. Big-endian chunk headers, as the CAF specification requires;
//! no AudioToolbox, so the writer and its tests run on every OS.

use std::fs::File;
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use super::bytes::{ReadAt, WindowedFile};
use super::{durable, io_error};
use crate::capture::CaptureError;

/// Streams Float32 PCM into a CAF whose data size stays -1 until `finish`;
/// see the module doc.
pub struct CafStreamWriter {
    path: PathBuf,
    sample_rate: f64,
    channels: usize,
    frames_written: usize,
    file: Option<File>,
    /// Interleaved frames as little-endian bytes, reused per write.
    scratch: Vec<u8>,
}

impl std::fmt::Debug for CafStreamWriter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CafStreamWriter")
            .field("path", &self.path)
            .field("channels", &self.channels)
            .field("frames_written", &self.frames_written)
            .finish_non_exhaustive()
    }
}

impl CafStreamWriter {
    /// `caff`, version, flags.
    pub const FILE_HEADER_SIZE: usize = 8;
    /// Chunk type (4) and chunk size (8), before every chunk body.
    pub const CHUNK_HEADER_SIZE: usize = 12;
    /// The `desc` chunk body: an `AudioStreamBasicDescription` without the
    /// reserved field.
    pub const DESC_CHUNK_SIZE: usize = 32;
    /// The `data` chunk body starts with the edit count; the samples follow.
    pub const EDIT_COUNT_SIZE: usize = 4;
    /// `kCAFLinearPCMFormatFlagIsFloat | kCAFLinearPCMFormatFlagIsLittleEndian`.
    pub const FLOAT_LITTLE_ENDIAN_FLAGS: u32 = 0b11;
    /// Float32.
    pub const BYTES_PER_SAMPLE: usize = 4;
    /// Everything before the first sample: 68 bytes.
    pub const HEADER_SIZE: usize = Self::FILE_HEADER_SIZE
        + Self::CHUNK_HEADER_SIZE
        + Self::DESC_CHUNK_SIZE
        + Self::CHUNK_HEADER_SIZE
        + Self::EDIT_COUNT_SIZE;
    /// Where the `data` chunk's size field sits: after the type tag of the
    /// chunk that follows the `desc` chunk.
    pub const DATA_SIZE_OFFSET: u64 =
        (Self::FILE_HEADER_SIZE + Self::CHUNK_HEADER_SIZE + Self::DESC_CHUNK_SIZE + 4) as u64;

    /// Writes the header: `channels` interleaved Float32 at `sample_rate`.
    pub fn create(path: &Path, sample_rate: f64, channels: usize) -> Result<Self, CaptureError> {
        assert!(channels > 0, "a CAF needs at least one channel");
        let mut file = File::create(path).map_err(|e| io_error(path, &e))?;
        file.write_all(&Self::header(sample_rate, channels))
            .map_err(|e| io_error(path, &e))?;
        Ok(Self {
            path: path.to_path_buf(),
            sample_rate,
            channels,
            frames_written: 0,
            file: Some(file),
            scratch: Vec::new(),
        })
    }

    /// Where it writes.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Frames written so far.
    #[must_use]
    pub fn frames_written(&self) -> usize {
        self.frames_written
    }

    /// Appends `frame_count` interleaved frames from `interleaved`
    /// (`frame_count * channels` samples).
    pub fn write(&mut self, interleaved: &[f32], frame_count: usize) -> Result<(), CaptureError> {
        let Some(file) = self.file.as_mut() else {
            return Ok(());
        };
        if frame_count == 0 {
            return Ok(());
        }
        let samples = frame_count * self.channels;
        let byte_count = samples * Self::BYTES_PER_SAMPLE;
        if self.scratch.len() < byte_count {
            self.scratch.resize(byte_count, 0);
        }
        for (chunk, sample) in self.scratch[..byte_count]
            .as_chunks_mut::<4>()
            .0
            .iter_mut()
            .zip(&interleaved[..samples])
        {
            *chunk = sample.to_le_bytes();
        }
        file.write_all(&self.scratch[..byte_count])
            .map_err(|e| io_error(&self.path, &e))?;
        self.frames_written += frame_count;
        Ok(())
    }

    /// Makes every frame written so far durable with `File::sync_data`
    /// (the samples and the file size, not the timestamps): `fdatasync` on
    /// Linux, `F_FULLFSYNC` on the Mac, which also flushes the drive's
    /// cache (5 to 14 ms per 5 s of audio on an internal SSD), and
    /// `FlushFileBuffers` on Windows; on the Mac a plain `fsync` when the
    /// filesystem refuses `F_FULLFSYNC` (the `durable` module). Nothing after
    /// `finish`. Rust only: Swift synced at the close alone.
    pub fn sync(&mut self) -> std::io::Result<()> {
        self.file
            .as_ref()
            .map_or(Ok(()), |file| durable::sync(file, File::sync_data))
    }

    /// Patches the data chunk size (edit count plus samples), flushes and
    /// closes.
    pub fn finish(&mut self) -> Result<(), CaptureError> {
        let Some(mut file) = self.file.take() else {
            return Ok(());
        };
        let size = (Self::EDIT_COUNT_SIZE
            + self.frames_written * self.channels * Self::BYTES_PER_SAMPLE)
            as u64;
        file.seek(SeekFrom::Start(Self::DATA_SIZE_OFFSET))
            .map_err(|e| io_error(&self.path, &e))?;
        file.write_all(&size.to_be_bytes())
            .map_err(|e| io_error(&self.path, &e))?;
        durable::sync(&file, File::sync_all).map_err(|e| io_error(&self.path, &e))?;
        Ok(())
    }

    /// Seconds written so far.
    #[must_use]
    pub fn duration(&self) -> f64 {
        // Exact in f64 for any recording.
        let frames = self.frames_written as f64;
        frames / self.sample_rate
    }

    /// `caff` file header, `desc` chunk and the `data` chunk header with
    /// size -1 and edit count 0.
    #[must_use]
    pub fn header(sample_rate: f64, channels: usize) -> Vec<u8> {
        let mut data = Vec::with_capacity(Self::HEADER_SIZE);
        data.extend_from_slice(b"caff");
        data.extend_from_slice(&1u16.to_be_bytes()); // file version
        data.extend_from_slice(&0u16.to_be_bytes()); // file flags
        data.extend_from_slice(b"desc");
        data.extend_from_slice(&(Self::DESC_CHUNK_SIZE as u64).to_be_bytes());
        data.extend_from_slice(&sample_rate.to_bits().to_be_bytes());
        data.extend_from_slice(b"lpcm");
        data.extend_from_slice(&Self::FLOAT_LITTLE_ENDIAN_FLAGS.to_be_bytes());
        data.extend_from_slice(&((channels * Self::BYTES_PER_SAMPLE) as u32).to_be_bytes()); // bytes per packet
        data.extend_from_slice(&1u32.to_be_bytes()); // frames per packet
        data.extend_from_slice(&(channels as u32).to_be_bytes());
        data.extend_from_slice(&((Self::BYTES_PER_SAMPLE * 8) as u32).to_be_bytes()); // bits per channel
        data.extend_from_slice(b"data");
        data.extend_from_slice(&(-1i64).to_be_bytes()); // size: to the end, until finish()
        data.extend_from_slice(&0u32.to_be_bytes()); // edit count
        debug_assert_eq!(data.len(), Self::HEADER_SIZE);
        data
    }
}

/// Why a CAF could not be read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CafReadError {
    /// Not a CAF, or a chunk runs past the end.
    #[error("malformed CAF file: {0}")]
    Malformed(String),
    /// Not Float32 little-endian PCM.
    #[error("unsupported CAF format: {0}")]
    UnsupportedFormat(String),
    /// A read failed: the path and the error.
    #[error("{0}")]
    Io(String),
}

/// Reads the CAF files the writer produces (Float32 little-endian PCM, any
/// channel count, data size -1 accepted) into de-interleaved channels, the
/// whole file at once: for the tests and the bench tools. The pipeline's
/// decoder, [`SymphoniaAudioCodec`](crate::codec::SymphoniaAudioCodec),
/// reads a master through the same chunk walk a block at a time.
#[derive(Debug, Clone, PartialEq)]
pub struct CafFile {
    /// Hertz.
    pub sample_rate: f64,
    /// De-interleaved channels.
    pub channels: Vec<Vec<f32>>,
}

impl CafFile {
    /// Frames per channel.
    #[must_use]
    pub fn frame_count(&self) -> usize {
        self.channels.first().map_or(0, Vec::len)
    }

    /// Seconds.
    #[must_use]
    pub fn duration(&self) -> f64 {
        let frames = self.frame_count() as f64;
        frames / self.sample_rate
    }

    /// Reads the whole file.
    pub fn read(path: &Path) -> Result<Self, CafReadError> {
        let data = std::fs::read(path)
            .map_err(|e| CafReadError::Io(format!("{}: {e}", path.display())))?;
        Self::read_bytes(&data)
    }

    /// Parses `data` as a CAF.
    pub fn read_bytes(mut data: &[u8]) -> Result<Self, CafReadError> {
        // A slice cannot fail to read, so no path is needed for the error.
        let layout = CafLayout::read(&mut data, Path::new(""))?;
        let channel_count = layout.channels;
        let bytes_per_sample = CafStreamWriter::BYTES_PER_SAMPLE;
        let mut channels = vec![vec![0.0f32; layout.frames]; channel_count];
        for frame in 0..layout.frames {
            for (channel, lane) in channels.iter_mut().enumerate() {
                let at = layout.start + (frame * channel_count + channel) * bytes_per_sample;
                lane[frame] = f32::from_le_bytes(data[at..at + 4].try_into().unwrap_or([0; 4]));
            }
        }
        Ok(Self {
            sample_rate: layout.sample_rate,
            channels,
        })
    }
}

/// Where a CAF's samples sit, from its chunk headers.
#[derive(Debug, Clone, Copy)]
struct CafLayout {
    sample_rate: f64,
    /// At least one.
    channels: usize,
    /// The file offset of the first sample.
    start: usize,
    /// Whole frames from `start`.
    frames: usize,
}

impl CafLayout {
    /// Walks the chunks of `source`; `path` names the file in a read error.
    fn read(source: &mut impl ReadAt, path: &Path) -> Result<Self, CafReadError> {
        let len = source.len();
        let mut read = |offset: usize, buffer: &mut [u8]| {
            source
                .read_at(offset, buffer)
                .map_err(|e| CafReadError::Io(format!("{}: {e}", path.display())))
        };
        let mut magic = [0u8; 4];
        if len >= 8 {
            read(0, &mut magic)?;
        }
        if &magic != b"caff" {
            return Err(CafReadError::Malformed("missing caff header".into()));
        }
        let mut offset = 8;
        let mut format: Option<(f64, u32, usize, usize, [u8; 4])> = None;
        let mut samples: Option<(usize, usize)> = None;
        while offset + 12 <= len {
            let mut header = [0u8; 12];
            read(offset, &mut header)?;
            let kind: [u8; 4] = header[..4].try_into().unwrap_or(*b"????");
            let size = i64::from_be_bytes(header[4..].try_into().unwrap_or([0; 8]));
            let body = offset + 12;
            match &kind {
                b"desc" => {
                    let desc = CafStreamWriter::DESC_CHUNK_SIZE;
                    if size < 0 || (size as u64) < desc as u64 || body + desc > len {
                        return Err(CafReadError::Malformed("desc chunk too short".into()));
                    }
                    let mut data = [0u8; CafStreamWriter::DESC_CHUNK_SIZE];
                    read(body, &mut data)?;
                    format = Some((
                        f64::from_bits(be_u64(&data, 0)),
                        be_u32(&data, 12),
                        be_u32(&data, 24) as usize,
                        be_u32(&data, 28) as usize,
                        data[8..12].try_into().unwrap_or(*b"????"),
                    ));
                }
                b"data" => {
                    let edit = CafStreamWriter::EDIT_COUNT_SIZE;
                    if body + edit > len {
                        return Err(CafReadError::Malformed("data chunk too short".into()));
                    }
                    let available = len - body - edit;
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
        let Some((sample_rate, flags, channel_count, bits, format_id)) = format else {
            return Err(CafReadError::Malformed("no desc chunk".into()));
        };
        let Some((start, count)) = samples else {
            return Err(CafReadError::Malformed("no data chunk".into()));
        };
        let wanted = CafStreamWriter::FLOAT_LITTLE_ENDIAN_FLAGS;
        if &format_id != b"lpcm"
            || flags & wanted != wanted
            || bits != CafStreamWriter::BYTES_PER_SAMPLE * 8
        {
            return Err(CafReadError::UnsupportedFormat(format!(
                "{} flags {flags} {bits}-bit; need Float32 little-endian",
                String::from_utf8_lossy(&format_id)
            )));
        }
        let channels = channel_count.max(1);
        Ok(Self {
            sample_rate,
            channels,
            start,
            frames: count / (CafStreamWriter::BYTES_PER_SAMPLE * channels),
        })
    }
}

/// Reads a CAF like [`CafFile`] does, a block of frames at a time, so a
/// two-hour master never sits in memory whole. The decoder reads masters
/// through it.
pub(crate) struct CafReader {
    file: WindowedFile,
    path: PathBuf,
    layout: CafLayout,
    /// Frames read so far.
    position: usize,
    bytes: Vec<u8>,
}

impl CafReader {
    /// Reads the chunk headers; the samples wait for `read_frames`.
    pub(crate) fn open(path: &Path) -> Result<Self, CafReadError> {
        let mut file = WindowedFile::open(path)
            .map_err(|e| CafReadError::Io(format!("{}: {e}", path.display())))?;
        let layout = CafLayout::read(&mut file, path)?;
        Ok(Self {
            file,
            path: path.to_path_buf(),
            layout,
            position: 0,
            bytes: Vec::new(),
        })
    }

    /// Hertz.
    pub(crate) fn sample_rate(&self) -> f64 {
        self.layout.sample_rate
    }

    /// Interleaved channels, at least one.
    pub(crate) fn channel_count(&self) -> usize {
        self.layout.channels
    }

    /// Whole frames in the file.
    pub(crate) fn frame_count(&self) -> usize {
        self.layout.frames
    }

    /// Replaces `samples` with the next frames, at most `max_frames`,
    /// interleaved; returns how many frames, zero at the end.
    pub(crate) fn read_frames(
        &mut self,
        max_frames: usize,
        samples: &mut Vec<f32>,
    ) -> Result<usize, CafReadError> {
        let frames = max_frames.min(self.layout.frames - self.position);
        let bytes_per_frame = CafStreamWriter::BYTES_PER_SAMPLE * self.layout.channels;
        let offset = self.layout.start + self.position * bytes_per_frame;
        self.bytes.resize(frames * bytes_per_frame, 0);
        self.file
            .read_at(offset, &mut self.bytes)
            .map_err(|e| CafReadError::Io(format!("{}: {e}", self.path.display())))?;
        samples.clear();
        samples.extend(
            self.bytes
                .as_chunks::<4>()
                .0
                .iter()
                .map(|bytes| f32::from_le_bytes(*bytes)),
        );
        self.position += frames;
        Ok(frames)
    }
}

fn be_u32(data: &[u8], offset: usize) -> u32 {
    u32::from_be_bytes(data[offset..offset + 4].try_into().unwrap_or([0; 4]))
}

fn be_u64(data: &[u8], offset: usize) -> u64 {
    u64::from_be_bytes(data[offset..offset + 8].try_into().unwrap_or([0; 8]))
}
