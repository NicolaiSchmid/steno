//! The directory where uploads live until the intake takes them:
//!
//! ```text
//! <inbox>/<recordingID>.partial          chunks being received
//! <inbox>/<recordingID>.metadata.json    the announced `RecordingMetadata`
//! <inbox>/<recordingID>.<ext>            verified, waiting for the intake
//! ```
//!
//! The intake deletes the verified file once the meeting is enqueued; the
//! inbox removes the metadata. The sweep removes what no receipt accounts
//! for. Swift: `Upload/Inbox.swift`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use steno_core::{AudioFormat, RecordingMetadata};
use uuid::Uuid;

use super::receiving_file;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Inbox {
    pub directory: PathBuf,
}

impl Inbox {
    #[must_use]
    pub fn new(directory: impl Into<PathBuf>) -> Self {
        Inbox {
            directory: directory.into(),
        }
    }

    #[must_use]
    pub fn partial(&self, recording_id: Uuid) -> PathBuf {
        self.directory
            .join(format!("{}.partial", recording_id.hyphenated()))
    }

    #[must_use]
    pub fn metadata(&self, recording_id: Uuid) -> PathBuf {
        self.directory
            .join(format!("{}.metadata.json", recording_id.hyphenated()))
    }

    #[must_use]
    pub fn verified(&self, recording_id: Uuid, format: AudioFormat) -> PathBuf {
        self.directory.join(format!(
            "{}.{}",
            recording_id.hyphenated(),
            format.file_extension()
        ))
    }

    pub fn prepare(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.directory)
    }

    /// Starts a recording: empty partial file and the metadata beside it.
    pub fn begin(&self, metadata: &RecordingMetadata) -> std::io::Result<()> {
        self.prepare()?;
        receiving_file::create(&self.partial(metadata.recording_id))?;
        let json = serde_json::to_vec(metadata)?;
        write_atomically(&self.metadata(metadata.recording_id), &json)
    }

    #[must_use]
    pub fn load_metadata(&self, recording_id: Uuid) -> Option<RecordingMetadata> {
        let bytes = std::fs::read(self.metadata(recording_id)).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    #[must_use]
    pub fn has_partial(&self, recording_id: Uuid) -> bool {
        self.partial(recording_id).exists()
    }

    #[must_use]
    pub fn has_verified(&self, recording_id: Uuid, format: AudioFormat) -> bool {
        self.verified(recording_id, format).exists()
    }

    /// Renames the complete partial to its final name.
    pub fn promote(&self, recording_id: Uuid, format: AudioFormat) -> std::io::Result<PathBuf> {
        let destination = self.verified(recording_id, format);
        if destination.exists() {
            std::fs::remove_file(&destination)?;
        }
        std::fs::rename(self.partial(recording_id), &destination)?;
        Ok(destination)
    }

    /// Removes every file of the recording.
    pub fn discard(&self, recording_id: Uuid) {
        let mut files = vec![self.partial(recording_id), self.metadata(recording_id)];
        files.extend(
            AudioFormat::ALL
                .iter()
                .map(|format| self.verified(recording_id, *format)),
        );
        for file in files {
            let _ = std::fs::remove_file(file);
        }
    }

    /// Recording ids that have any file in the inbox.
    #[must_use]
    pub fn recording_ids(&self) -> BTreeSet<Uuid> {
        let Ok(entries) = std::fs::read_dir(&self.directory) else {
            return BTreeSet::new();
        };
        entries
            .filter_map(Result::ok)
            .filter_map(|entry| {
                let name = entry.file_name();
                let name = name.to_str()?;
                let stem = name.split_once('.').map_or(name, |(stem, _)| stem);
                crate::pairing::parse_uuid(stem)
            })
            .collect()
    }
}

/// Write to a sibling, then rename over the destination.
fn write_atomically(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let temporary = path.with_extension("json.tmp");
    std::fs::write(&temporary, bytes)?;
    std::fs::rename(&temporary, path)
}
