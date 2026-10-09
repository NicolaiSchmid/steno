//! Writes the master and the sidecars of one recording.
//! Swift: `Sources/StenoAudio/Writer/RecordingWriter.swift`.
//!
//! The master is `recording.caf` (48 kHz Float32, one channel per lane),
//! plus one 16 kHz Int16 WAV sidecar per lane through
//! [`Resampler48kTo16k`], plus `mic.raw.caf` when asked, all into
//! `RecordingLayout`'s directory, which `new` creates and never reuses.
//! Owned by the writer thread; one `write` per 10 ms frame, `finish()`
//! patches sizes and returns the files. All scratch buffers are allocated
//! in `new`.
//!
//! The sidecars lag the master by the resampler's group delay: the 192-tap
//! linear-phase FIR delays by 95.5 input samples, so every sidecar sample
//! sits 2.0 ms (32 samples at 16 kHz) after the master sample it belongs
//! to. Harmless for transcripts, and deliberate, so do not "fix" a 2 ms
//! offset by hand.

use std::collections::BTreeMap;
use std::path::PathBuf;

use steno_core::{AudioFormat, AudioLane, RecordingLayout};

use super::caf::CafStreamWriter;
use super::resampler::Resampler48kTo16k;
use super::wav::WavStreamWriter;
use crate::capture::CaptureError;
use crate::{FRAME_SIZE, SAMPLE_RATE};

/// One processed frame handed to the writer: `frame_count` samples per lane
/// in the session's lane order, plus the raw microphone when kept. A view
/// over the writer thread's buffers, valid for the duration of `write`.
#[derive(Debug)]
pub struct LaneFrames<'a> {
    /// Samples per lane.
    pub frame_count: usize,
    /// One slice per lane, in the session's lane order.
    pub lanes: &'a [&'a [f32]],
    /// The microphone before cancellation, when kept.
    pub raw_mic: Option<&'a [f32]>,
}

/// The files one recording produced.
#[derive(Debug, Clone, PartialEq)]
pub struct RecordingFiles {
    /// The 48 kHz CAF.
    pub master: PathBuf,
    /// One 16 kHz WAV per lane.
    pub sidecars_16k: BTreeMap<AudioLane, PathBuf>,
    /// `mic.raw.caf`, when kept.
    pub raw_mic: Option<PathBuf>,
    /// Seconds in the master.
    pub duration: f64,
}

/// What the writer thread and the session need from the file writer.
/// [`RecordingWriter`] is the production implementation; tests wrap it to
/// inject the I/O failures a full disk produces.
pub trait RecordingWriting: Send {
    /// The files and the duration written so far; valid before `finish()`
    /// and after a failed one, so the session can still hand out the asset.
    fn files(&self) -> RecordingFiles;
    /// One frame for every lane.
    fn write(&mut self, frames: &LaneFrames<'_>) -> Result<(), CaptureError>;
    /// Makes what the files hold so far durable (`File::sync_data`, see
    /// [`CafStreamWriter::sync`]), so a power loss keeps it; the writer
    /// thread calls it every
    /// [`SYNC_INTERVAL_FRAMES`](super::writer_thread::SYNC_INTERVAL_FRAMES)
    /// frames. A failure is logged once and handed back at the stop, and
    /// the recording goes on; only a failed write ends it, and a sync
    /// failure is reported only when nothing else ended the recording. Rust
    /// only: Swift synced at the close alone.
    fn sync(&mut self) -> std::io::Result<()>;
    /// Patches the headers, syncs and closes the files; once. A sync that
    /// fails here, the Mac's `fsync` fallback too, is a failure: the audio
    /// may not be on disk.
    fn finish(&mut self) -> Result<RecordingFiles, CaptureError>;
    /// Whether this writer created the meeting's folder, so a start that
    /// fails after it may remove the folder. `false` unless the writer
    /// says so: a folder no writer claims is never removed. Rust only.
    fn created_directory(&self) -> bool {
        false
    }
}

/// The production writer: the master CAF, one sidecar per lane through the
/// 3:1 FIR, the raw microphone when kept; see the module doc.
pub struct RecordingWriter {
    layout: RecordingLayout,
    lanes: Vec<AudioLane>,
    master: CafStreamWriter,
    sidecars: Vec<WavStreamWriter>,
    resamplers: Vec<Resampler48kTo16k>,
    raw_mic: Option<CafStreamWriter>,
    interleaved: Vec<f32>,
    sidecar_scratch: Vec<i16>,
    is_finished: bool,
}

impl std::fmt::Debug for RecordingWriter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RecordingWriter")
            .field("layout", &self.layout)
            .field("lanes", &self.lanes)
            .finish_non_exhaustive()
    }
}

impl RecordingWriter {
    /// Creates `layout.directory`, its parents when needed, and refuses
    /// one that exists ([`CaptureError::RecordingExists`]): a meeting's
    /// folder holds one recording, and `File::create` would empty its
    /// files. Rust only: Swift created the folder if needed.
    pub fn new(
        layout: &RecordingLayout,
        lanes: &[AudioLane],
        keep_raw_mic: bool,
    ) -> Result<Self, CaptureError> {
        assert!(!lanes.is_empty(), "a recording needs at least one lane");
        let directory = &layout.directory;
        let failed =
            |e: std::io::Error| CaptureError::WriterFailed(format!("{}: {e}", directory.display()));
        if let Some(parent) = directory.parent() {
            std::fs::create_dir_all(parent).map_err(failed)?;
        }
        std::fs::create_dir(directory).map_err(|e| match e.kind() {
            std::io::ErrorKind::AlreadyExists => CaptureError::RecordingExists(directory.clone()),
            _ => failed(e),
        })?;
        let master = CafStreamWriter::create(
            &layout.master(AudioFormat::Caf48kFloat32),
            SAMPLE_RATE,
            lanes.len(),
        )?;
        let sidecars = lanes
            .iter()
            .map(|lane| {
                WavStreamWriter::create(
                    &layout.sidecar(*lane),
                    WavStreamWriter::SIDECAR_SAMPLE_RATE,
                )
            })
            .collect::<Result<Vec<_>, _>>()?;
        let raw_mic = if keep_raw_mic && lanes.contains(&AudioLane::Mic) {
            Some(CafStreamWriter::create(
                &layout.directory.join("mic.raw.caf"),
                SAMPLE_RATE,
                1,
            )?)
        } else {
            None
        };
        Ok(Self {
            layout: layout.clone(),
            lanes: lanes.to_vec(),
            master,
            sidecars,
            resamplers: lanes
                .iter()
                .map(|_| Resampler48kTo16k::new(FRAME_SIZE))
                .collect(),
            raw_mic,
            interleaved: vec![0.0; FRAME_SIZE * lanes.len()],
            sidecar_scratch: vec![0; FRAME_SIZE / Resampler48kTo16k::FACTOR],
            is_finished: false,
        })
    }

    /// The folder layout it writes into.
    #[must_use]
    pub fn layout(&self) -> &RecordingLayout {
        &self.layout
    }

    /// The lanes, in master channel order.
    #[must_use]
    pub fn lanes(&self) -> &[AudioLane] {
        &self.lanes
    }
}

impl RecordingWriting for RecordingWriter {
    /// The paths are fixed at `new`; the duration is what the master holds.
    fn files(&self) -> RecordingFiles {
        RecordingFiles {
            master: self.master.path().to_path_buf(),
            sidecars_16k: self
                .lanes
                .iter()
                .zip(&self.sidecars)
                .map(|(lane, sidecar)| (*lane, sidecar.path().to_path_buf()))
                .collect(),
            raw_mic: self.raw_mic.as_ref().map(|w| w.path().to_path_buf()),
            duration: self.master.duration(),
        }
    }

    /// `frames.frame_count` must equal [`FRAME_SIZE`].
    fn write(&mut self, frames: &LaneFrames<'_>) -> Result<(), CaptureError> {
        if self.is_finished {
            return Err(CaptureError::WriterFailed("write after finish".into()));
        }
        let lane_count = self.lanes.len();
        if frames.frame_count != FRAME_SIZE || frames.lanes.len() != lane_count {
            return Err(CaptureError::WriterFailed(format!(
                "expected {FRAME_SIZE} frames on {lane_count} lanes, got {} on {}",
                frames.frame_count,
                frames.lanes.len()
            )));
        }
        for (lane, source) in frames.lanes.iter().enumerate() {
            for (index, &sample) in source[..FRAME_SIZE].iter().enumerate() {
                self.interleaved[index * lane_count + lane] = sample;
            }
        }
        // The master first: when the disk fills on this frame, the
        // recoverable copy is never shorter than a sidecar.
        self.master.write(&self.interleaved, FRAME_SIZE)?;
        for (lane, source) in frames.lanes.iter().enumerate() {
            self.resamplers[lane].process(source, &mut self.sidecar_scratch);
            self.sidecars[lane].write(&self.sidecar_scratch)?;
        }
        if let (Some(raw_mic), Some(raw)) = (self.raw_mic.as_mut(), frames.raw_mic) {
            raw_mic.write(raw, FRAME_SIZE)?;
        }
        Ok(())
    }

    /// Syncs every file, the master first, so after a power loss a
    /// recovered recording's sidecars, which its transcript is decoded
    /// from, are as long as its master. A sidecar costs little beside the
    /// master: 16 kHz Int16 is about a sixth of a lane's bytes in it. A
    /// failure on one file still syncs the others, and the first is
    /// returned. That the real sync reaches the disk is the manual
    /// power-loss check's; the tests count the calls.
    fn sync(&mut self) -> std::io::Result<()> {
        let mut result = self.master.sync();
        for sidecar in &mut self.sidecars {
            result = result.and(sidecar.sync());
        }
        if let Some(raw_mic) = self.raw_mic.as_mut() {
            result = result.and(raw_mic.sync());
        }
        result
    }

    /// Always: `new` refuses a folder it did not create.
    fn created_directory(&self) -> bool {
        true
    }

    /// Closes every file, the master first. A failure on one file still
    /// closes the others before it is returned.
    fn finish(&mut self) -> Result<RecordingFiles, CaptureError> {
        if self.is_finished {
            return Err(CaptureError::WriterFailed("finish called twice".into()));
        }
        self.is_finished = true;
        let mut first_error: Option<CaptureError> = None;
        let mut attempt = |result: Result<(), CaptureError>| {
            if let Err(error) = result
                && first_error.is_none()
            {
                first_error = Some(error);
            }
        };
        attempt(self.master.finish());
        for sidecar in &mut self.sidecars {
            attempt(sidecar.finish());
        }
        if let Some(raw_mic) = self.raw_mic.as_mut() {
            attempt(raw_mic.finish());
        }
        match first_error {
            Some(error) => Err(error),
            None => Ok(self.files()),
        }
    }
}
