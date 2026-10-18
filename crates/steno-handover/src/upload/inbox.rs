//! The directory where uploads live until the intake takes them:
//!
//! ```text
//! <inbox>/<recordingID>.partial            chunks being received
//! <inbox>/<recordingID>.metadata.json      the announced `RecordingMetadata`
//! <inbox>/<recordingID>.metadata.json.tmp  the metadata while it is written
//! <inbox>/<recordingID>.<ext>              verified, waiting for the intake
//! ```

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use steno_core::busy_file;
use steno_core::json::parse_uuid;
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

    /// Where the metadata is written before it is renamed into place; a
    /// crash leaves it behind and `discard` removes it.
    #[must_use]
    pub fn metadata_temporary(&self, recording_id: Uuid) -> PathBuf {
        self.metadata(recording_id).with_extension("json.tmp")
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

    /// Starts a recording: an empty partial file unless one is there (never
    /// truncated), and the metadata sidecar beside it. Called only under the
    /// engine's files lock.
    pub fn begin(&self, metadata: &RecordingMetadata) -> std::io::Result<()> {
        self.prepare()?;
        receiving_file::create(&self.partial(metadata.recording_id))?;
        let json = serde_json::to_vec(metadata)?;
        write_atomically(
            &self.metadata(metadata.recording_id),
            &self.metadata_temporary(metadata.recording_id),
            &json,
        )
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

    /// Renames the complete partial to its final name, retried on Windows while
    /// another handle briefly holds a file. A failure leaves the partial in
    /// place and `complete` answers 500, so the phone keeps its copy.
    pub fn promote(&self, recording_id: Uuid, format: AudioFormat) -> std::io::Result<PathBuf> {
        let destination = self.verified(recording_id, format);
        if destination.exists() {
            match busy_file::retried(|| std::fs::remove_file(&destination)) {
                Err(error) if error.kind() != std::io::ErrorKind::NotFound => return Err(error),
                _ => {}
            }
        }
        busy_file::rename(&self.partial(recording_id), &destination)?;
        Ok(destination)
    }

    /// Removes every file of the recording.
    pub fn discard(&self, recording_id: Uuid) {
        let verified = AudioFormat::ALL
            .iter()
            .map(|format| self.verified(recording_id, *format));
        let files = [
            self.partial(recording_id),
            self.metadata(recording_id),
            self.metadata_temporary(recording_id),
        ];
        for file in files.into_iter().chain(verified) {
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
                parse_uuid(stem)
            })
            .collect()
    }
}

/// Write to `temporary`, then rename it over `path`, retried on Windows
/// while another handle briefly holds a file.
fn write_atomically(path: &Path, temporary: &Path, bytes: &[u8]) -> std::io::Result<()> {
    std::fs::write(temporary, bytes)?;
    busy_file::rename(temporary, path)
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone as _;

    use super::*;

    #[test]
    fn begin_promote_and_discard_leave_exactly_the_files_they_name() {
        let directory = tempfile::tempdir().unwrap();
        let inbox = Inbox::new(directory.path().join("inbox"));
        let id = Uuid::new_v4();
        let metadata = RecordingMetadata {
            recording_id: id,
            started_at: chrono::Utc.timestamp_opt(1_789_990_000, 0).unwrap(),
            duration_seconds: 61.5,
            byte_count: 3,
            sha256: vec![7; 32],
            chunk_size: 64 * 1024,
            format: AudioFormat::M4aAac,
            device_name: "Phone".to_owned(),
        };
        let names = || {
            let mut names: Vec<String> = std::fs::read_dir(&inbox.directory)
                .unwrap()
                .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
                .collect();
            names.sort();
            names
        };
        inbox.begin(&metadata).unwrap();
        assert_eq!(
            names(),
            [format!("{id}.metadata.json"), format!("{id}.partial")]
        );
        assert_eq!(inbox.load_metadata(id), Some(metadata.clone()));

        // A crash between the write and the rename leaves the temporary.
        std::fs::write(inbox.metadata_temporary(id), b"{").unwrap();
        let verified = inbox.promote(id, AudioFormat::M4aAac).unwrap();
        assert_eq!(verified, inbox.verified(id, AudioFormat::M4aAac));
        assert!(!inbox.has_partial(id));
        assert_eq!(inbox.recording_ids().into_iter().collect::<Vec<_>>(), [id]);
        let gone = inbox.promote(id, AudioFormat::M4aAac).unwrap_err();
        assert_eq!(gone.kind(), std::io::ErrorKind::NotFound, "no partial");
        let whole = std::fs::read(inbox.metadata(id)).unwrap();
        std::fs::write(inbox.metadata(id), &whole[..whole.len() / 2]).unwrap();
        assert_eq!(inbox.load_metadata(id), None, "a truncated sidecar");
        inbox.discard(id);
        assert_eq!(names(), Vec::<String>::new());
    }
}
