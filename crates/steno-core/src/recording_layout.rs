//! Where one meeting's audio files live.
//! Swift: `Sources/StenoCore/Audio/RecordingLayout.swift`.
//!
//! `<audio folder>/<MEETING-UUID>/recording.caf`, one `<lane>.wav` sidecar
//! per lane, `audio.<ext>` for the mixdown, `speakers/<SPEAKER-UUID>.wav`
//! for the sample clips (`speakers/<SPEAKER-UUID>-<RUN-UUID>.wav` for the
//! clips this port's pipeline writes), and, while a meeting is processed,
//! the pipeline's `.processing-runs`. The UUID folder is spelled as Swift's
//! `uuidString`: uppercase, hyphenated.

use std::path::{Path, PathBuf};

use uuid::Uuid;

use crate::json::uuid_string;
use crate::paths::file_url_path;
use crate::{AudioAsset, AudioFormat, AudioLane};

/// One meeting's audio folder; every file name is derived from it (see the
/// module doc). Nothing touches the disk except
/// [`Self::create_directories`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RecordingLayout {
    /// `<audio folder>/<MEETING-UUID>`.
    pub directory: PathBuf,
}

impl RecordingLayout {
    /// The layout of an existing folder, whatever its name.
    #[must_use]
    pub fn from_directory(directory: impl Into<PathBuf>) -> Self {
        Self {
            directory: directory.into(),
        }
    }

    /// The meeting's folder under `audio_folder`, named by its id in
    /// uppercase as Swift's `uuidString` spells it, so the Swift app and
    /// this crate find the same files.
    #[must_use]
    pub fn new(audio_folder: &Path, meeting_id: Uuid) -> Self {
        Self::from_directory(audio_folder.join(uuid_string(meeting_id)))
    }

    /// The folder holding `asset.url`; `None` when the URL is not a file URL.
    #[must_use]
    pub fn from_asset(asset: &AudioAsset) -> Option<Self> {
        let master = file_url_path(&asset.url)?;
        Some(Self::from_directory(master.parent()?))
    }

    /// [`Self::from_asset`] when that folder is named after the asset's
    /// meeting, as [`Self::new`] names it, so no other meeting's files are
    /// in it; `None` for a master in any other folder. Rust only.
    #[must_use]
    pub fn own_folder(asset: &AudioAsset) -> Option<Self> {
        let folder = uuid_string(asset.meeting_id);
        Self::from_asset(asset).filter(|layout| {
            layout
                .directory
                .file_name()
                .is_some_and(|name| name == folder.as_str())
        })
    }

    /// `recording.<ext>`, the 48 kHz master.
    #[must_use]
    pub fn master(&self, format: AudioFormat) -> PathBuf {
        self.directory
            .join(format!("recording.{}", format.file_extension()))
    }

    /// `<lane>.wav`, the lane's 16 kHz sidecar.
    #[must_use]
    pub fn sidecar(&self, lane: AudioLane) -> PathBuf {
        self.directory.join(format!("{}.wav", lane.as_str()))
    }

    /// `audio.<ext>`, the optional export.
    #[must_use]
    pub fn mixdown(&self, format: AudioFormat) -> PathBuf {
        self.directory
            .join(format!("audio.{}", format.file_extension()))
    }

    /// `speakers/`, the speaker sample clips.
    #[must_use]
    pub fn speakers_directory(&self) -> PathBuf {
        self.directory.join("speakers")
    }

    /// `speakers/<SPEAKER-UUID>.wav`, uppercase as the meeting folder.
    #[must_use]
    pub fn sample_clip(&self, speaker_id: Uuid) -> PathBuf {
        self.speakers_directory()
            .join(format!("{}.wav", uuid_string(speaker_id)))
    }

    /// `speakers/<SPEAKER-UUID>-<RUN-UUID>.wav`, the clip one pipeline run
    /// writes for a speaker: a name no other run uses, so a run never
    /// writes over a clip a speaker row names (`steno_pipeline`'s
    /// `sample_clips`). Rust only: Swift writes [`Self::sample_clip`] in
    /// place.
    #[must_use]
    pub fn run_sample_clip(&self, speaker_id: Uuid, run_id: Uuid) -> PathBuf {
        self.speakers_directory().join(format!(
            "{}-{}.wav",
            uuid_string(speaker_id),
            uuid_string(run_id)
        ))
    }

    /// `.processing-runs`, the pipeline's count of runs that ended with the
    /// app while the meeting was processed (`steno_pipeline::crash_loop`).
    /// Rust only.
    #[must_use]
    pub fn processing_runs(&self) -> PathBuf {
        self.directory.join(".processing-runs")
    }

    /// Creates `directory` (and `speakers/` when asked) if needed.
    pub fn create_directories(&self, speakers: bool) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.directory)?;
        if speakers {
            std::fs::create_dir_all(self.speakers_directory())?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_follow_the_swift_layout() {
        let id = Uuid::parse_str("0b6f4b1e-5c2a-4f8e-9d3b-7a1c2e3f4a5b").unwrap();
        let layout = RecordingLayout::new(Path::new("/audio"), id);
        assert_eq!(
            layout.directory,
            PathBuf::from("/audio/0B6F4B1E-5C2A-4F8E-9D3B-7A1C2E3F4A5B")
        );
        assert_eq!(
            layout.master(AudioFormat::Caf48kFloat32),
            layout.directory.join("recording.caf")
        );
        assert_eq!(
            layout.sidecar(AudioLane::Mic),
            layout.directory.join("mic.wav")
        );
        assert_eq!(
            layout.mixdown(AudioFormat::M4aAac),
            layout.directory.join("audio.m4a")
        );
        assert_eq!(
            layout.sample_clip(id),
            layout
                .directory
                .join("speakers/0B6F4B1E-5C2A-4F8E-9D3B-7A1C2E3F4A5B.wav")
        );
        let run = Uuid::parse_str("9a8b7c6d-5e4f-4a3b-8c2d-1e0f9a8b7c6d").unwrap();
        assert_eq!(
            layout.run_sample_clip(id, run),
            layout.directory.join(
                "speakers/0B6F4B1E-5C2A-4F8E-9D3B-7A1C2E3F4A5B-9A8B7C6D-5E4F-4A3B-8C2D-1E0F9A8B7C6D.wav"
            )
        );
    }

    /// Only a folder named after the meeting is the meeting's own.
    #[test]
    fn only_a_folder_named_after_the_meeting_is_its_own() {
        let id = Uuid::parse_str("0b6f4b1e-5c2a-4f8e-9d3b-7a1c2e3f4a5b").unwrap();
        let asset = |master: &Path| AudioAsset {
            id: Uuid::nil(),
            meeting_id: id,
            url: crate::paths::file_url(master, false),
            format: AudioFormat::Wav16kInt16,
            lanes: vec![AudioLane::Mixed],
            sidecars_16k: std::collections::BTreeMap::new(),
            mixdown_url: None,
            retention: crate::AudioRetention::KeepForever,
            expires_at: None,
        };
        let own = RecordingLayout::new(Path::new("/audio"), id);
        assert_eq!(
            RecordingLayout::own_folder(&asset(&own.master(AudioFormat::Wav16kInt16))),
            Some(own)
        );
        assert_eq!(
            RecordingLayout::own_folder(&asset(Path::new("/downloads/call.wav"))),
            None
        );
    }
}
