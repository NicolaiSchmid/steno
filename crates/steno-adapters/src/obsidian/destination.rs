//! The Obsidian vault folder destination.
//! Swift: `Sources/StenoAdapters/Obsidian/ObsidianFolderDestination.swift`.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use chrono_tz::Tz;
use steno_core::{
    BoundaryResult, DeliveredFile, DeliveryReceipt, Destination, FileOwnership, MeetingExport,
    ObsidianSettings, async_trait,
};
use thiserror::Error;
use uuid::Uuid;

use super::ManagedBlock;
use crate::fs::{AtomicFileWriter, LocalFolderSink, WriteFailure};
use crate::naming::MeetingFolder;
use crate::rendering::{ArtifactRenderer, LinkStyle, RenderOptions};
use crate::runtime::DeliveryLedger;

/// What can go wrong in the vault. The app shows the message verbatim.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ObsidianError {
    #[error("The Obsidian vault at {0} does not exist.")]
    VaultMissing(String),
    #[error("The Obsidian vault at {0} is not writable.")]
    VaultNotWritable(String),
    #[error("The people folder \"{0}\" must be a relative path inside the vault.")]
    PeopleFolderInvalid(String),
    #[error("Could not read {path}: {underlying}")]
    ReadFailed { path: String, underlying: String },
    #[error("Could not write {path}: {underlying}")]
    WriteFailed { path: String, underlying: String },
    #[error("The meeting has no audio mixdown to copy; every other file was written.")]
    AudioUnavailable,
}

/// The Obsidian vault folder destination: `Meetings/<date>-<slug>/` with the
/// folder note, transcript, tasks, `transcript.vtt`, `meeting.json`, the
/// optional audio copy, and one managed block per person page. A person page
/// the previous receipt lists but this delivery no longer renders (a speaker
/// reassigned to somebody else, a renamed person) loses this meeting's line
/// and keeps everything else. The policy (which receipt applies, what may be
/// written, what the receipt says) is [`DeliveryLedger`]'s; this type
/// renders, asks the ledger and writes through [`LocalFolderSink`]. Nothing
/// is deleted but the writer's own temp files.
#[derive(Debug, Clone)]
pub struct ObsidianFolderDestination {
    /// [`ObsidianFolderDestination::DESTINATION_ID`] for the app's stored
    /// settings. A one-off run into another vault (`steno deliver --vault`)
    /// passes its own id so its `Delivery` row and receipt never replace the
    /// stored destination's.
    id: String,
    settings: ObsidianSettings,
    /// Time zone of the folder date and every date in the notes. The
    /// machine's zone in the app; tests pin it.
    time_zone: Tz,
    sink: LocalFolderSink,
}

impl ObsidianFolderDestination {
    pub const DESTINATION_ID: &'static str = "obsidian-folder";

    #[must_use]
    pub fn new(settings: ObsidianSettings, time_zone: Tz) -> Self {
        Self::with_id(settings, time_zone, Self::DESTINATION_ID)
    }

    #[must_use]
    pub fn with_id(settings: ObsidianSettings, time_zone: Tz, id: &str) -> Self {
        let sink = LocalFolderSink::new(PathBuf::from(&settings.vault_path));
        ObsidianFolderDestination {
            id: id.to_owned(),
            settings,
            time_zone,
            sink,
        }
    }

    #[must_use]
    pub fn settings(&self) -> &ObsidianSettings {
        &self.settings
    }

    #[must_use]
    pub fn time_zone(&self) -> Tz {
        self.time_zone
    }

    /// The vault is a writable directory (probed with a file that is created
    /// and removed) and the people folder is a relative path without `..`. A
    /// missing `.obsidian/` is not an error: the folder may be a vault
    /// Obsidian has not opened yet.
    pub fn validate_vault(&self) -> Result<(), ObsidianError> {
        self.check_vault()?;
        let probe = format!(".steno-probe-{}", AtomicFileWriter::random_hex());
        let path = self.sink.path(&probe);
        fs::write(&path, [])
            .and_then(|()| fs::remove_file(&path))
            .map_err(|_| ObsidianError::VaultNotWritable(self.settings.vault_path.clone()))
    }

    pub fn deliver_meeting(
        &self,
        meeting: &MeetingExport,
        previous: Option<&DeliveryReceipt>,
    ) -> Result<DeliveryReceipt, ObsidianError> {
        self.check_vault()?;
        let mut ledger = DeliveryLedger::new(previous, &self.settings.vault_path);
        let folder = ledger
            .pinned_folder()
            .map_or_else(|| self.resolve_folder(meeting), str::to_owned);
        let slug = folder.rsplit('/').next().unwrap_or(&folder).to_owned();
        let options = RenderOptions {
            link_style: LinkStyle::Wikilink,
            person_pages: self.settings.people_folder.is_some(),
            task_tag: self.settings.task_tag.clone(),
            time_zone: self.time_zone,
        };
        let renderer = ArtifactRenderer::new();

        self.writing(&folder, || self.sink.create_directory(&folder))?;
        AtomicFileWriter::remove_stale_temporaries(&self.sink.path(&folder));

        let artifacts = renderer
            .render_meeting_files(meeting, &options, Some(&slug))
            .map_err(|error| ObsidianError::WriteFailed {
                path: self.absolute(&format!("{folder}/{}", MeetingFolder::JSON)),
                underlying: error.to_string(),
            })?;
        for artifact in artifacts {
            self.write_owned(
                &mut ledger,
                &format!("{folder}/{}", artifact.file_name),
                &artifact.data,
            )?;
        }

        if let Some(people_folder) = &self.settings.people_folder {
            self.writing(people_folder, || self.sink.create_directory(people_folder))?;
            AtomicFileWriter::remove_stale_temporaries(&self.sink.path(people_folder));
            let mut rendered_pages: HashSet<String> = HashSet::new();
            for page in renderer.render_person_pages(meeting, &options, Some(&slug)) {
                let path = format!("{people_folder}/{}", page.file_name);
                rendered_pages.insert(path.clone());
                let mut data = page.page.into_bytes();
                if let Some(existing) = self.reading(&path, || self.sink.read(&path))? {
                    // A page that is not UTF-8 text cannot be merged without
                    // changing bytes outside the block, so it is left as it
                    // is and reported.
                    let Ok(text) = String::from_utf8(existing) else {
                        return Err(ObsidianError::ReadFailed {
                            path: self.absolute(&path),
                            underlying: "not UTF-8 text; the page was left unchanged".to_owned(),
                        });
                    };
                    data = ManagedBlock::merge(&page.line, meeting.meeting.id, &text).into_bytes();
                }
                self.writing(&path, || self.sink.write(&data, &path))?;
                ledger.record(&path, FileOwnership::ManagedBlock, &data);
            }
            let previous_files = ledger.previous().map(|receipt| receipt.files.clone());
            self.remove_meeting_line(
                previous_files.as_deref().unwrap_or(&[]),
                &rendered_pages,
                meeting.meeting.id,
                &mut ledger,
            )?;
        }

        if self.settings.include_audio {
            // The mixdown is copied while it exists; after the retention sweep
            // the copy already in the vault (in the receipt or on disk) is the
            // audio. Only when neither is there has the meeting no audio to
            // deliver.
            let mixdown = meeting
                .audio
                .as_ref()
                .and_then(|audio| audio.mixdown_url.as_deref())
                .and_then(file_url_path)
                .filter(|path| path.exists());
            if let Some(mixdown) = mixdown {
                let data = self
                    .reading(&mixdown.to_string_lossy(), || fs::read(&mixdown).map(Some))?
                    .unwrap_or_default();
                let extension = mixdown
                    .extension()
                    .map(|extension| extension.to_string_lossy().into_owned())
                    .unwrap_or_default();
                self.write_owned(
                    &mut ledger,
                    &format!("{folder}/{}", MeetingFolder::audio_file(&extension)),
                    &data,
                )?;
            } else if !self.audio_is_in_the_vault(&folder, &ledger) {
                return Err(ObsidianError::AudioUnavailable);
            }
        }

        Ok(ledger.receipt(&folder, ArtifactRenderer::VERSION))
    }

    fn write_owned(
        &self,
        ledger: &mut DeliveryLedger,
        path: &str,
        data: &[u8],
    ) -> Result<(), ObsidianError> {
        if !ledger.may_write(path, self.sink.exists(path)) {
            return Ok(());
        }
        self.writing(path, || self.sink.write(data, path))?;
        ledger.record(path, FileOwnership::Owned, data);
        Ok(())
    }

    /// Drops this meeting's line from every managed-block page of the
    /// previous receipt that this delivery did not render, so a person who
    /// left the meeting no longer lists it. A page that is gone from disk is
    /// skipped and a page that is not UTF-8 text is left as it is; neither
    /// fails the delivery, since there is nothing of ours to correct in the
    /// first case and no safe way to in the second. An unchanged page is not
    /// rewritten. The path stays in the receipt either way, as every file
    /// the app wrote does.
    fn remove_meeting_line(
        &self,
        previous: &[DeliveredFile],
        rendered: &HashSet<String>,
        meeting_id: Uuid,
        ledger: &mut DeliveryLedger,
    ) -> Result<(), ObsidianError> {
        for file in previous
            .iter()
            .filter(|file| file.ownership == FileOwnership::ManagedBlock)
        {
            let path = &file.relative_path;
            if rendered.contains(path) {
                continue;
            }
            let Some(existing) = self.reading(path, || self.sink.read(path))? else {
                continue;
            };
            let Ok(text) = String::from_utf8(existing) else {
                continue;
            };
            let cleaned = ManagedBlock::remove(meeting_id, &text);
            if cleaned == text {
                continue;
            }
            let data = cleaned.into_bytes();
            self.writing(path, || self.sink.write(&data, path))?;
            ledger.record(path, FileOwnership::ManagedBlock, &data);
        }
        Ok(())
    }

    /// The folder of a first delivery: the scope's path with the ledger's
    /// collision rule, fed by the two things only this transport knows.
    pub(crate) fn resolve_folder(&self, meeting: &MeetingExport) -> String {
        DeliveryLedger::resolve_folder(
            &MeetingFolder::path(&meeting.meeting, self.time_zone),
            meeting.meeting.id,
            |folder| self.sink.exists(folder),
            |folder| {
                let data = self
                    .sink
                    .read(&format!("{folder}/{}", MeetingFolder::JSON))
                    .ok()
                    .flatten()?;
                let probe: serde_json::Value = serde_json::from_slice(&data).ok()?;
                probe
                    .pointer("/meeting/id")?
                    .as_str()
                    .and_then(|id| Uuid::parse_str(id).ok())
            },
        )
    }

    /// An `audio` or `audio.<ext>` file in the meeting folder, listed in the
    /// receipt or present on disk.
    fn audio_is_in_the_vault(&self, folder: &str, ledger: &DeliveryLedger) -> bool {
        fn is_audio(name: &str) -> bool {
            name == "audio" || name.starts_with("audio.")
        }
        ledger.lists(folder, is_audio)
            || self
                .sink
                .file_names(folder)
                .iter()
                .any(|name| is_audio(name))
    }

    /// The vault exists and the people folder, if any, is a relative path
    /// without `..`, `\`, empty components or surrounding whitespace.
    pub(crate) fn check_vault(&self) -> Result<(), ObsidianError> {
        if !self.sink.is_directory("") {
            return Err(ObsidianError::VaultMissing(
                self.settings.vault_path.clone(),
            ));
        }
        let Some(folder) = &self.settings.people_folder else {
            return Ok(());
        };
        let valid = !folder.is_empty()
            && !folder.starts_with('/')
            && !folder.contains('\\')
            && folder == folder.trim()
            && !folder
                .split('/')
                .any(|component| component == ".." || component.is_empty());
        if valid {
            Ok(())
        } else {
            Err(ObsidianError::PeopleFolderInvalid(folder.clone()))
        }
    }

    fn reading<T>(
        &self,
        path: &str,
        body: impl FnOnce() -> std::io::Result<T>,
    ) -> Result<T, ObsidianError> {
        body().map_err(|error| ObsidianError::ReadFailed {
            path: self.absolute(path),
            underlying: error.to_string(),
        })
    }

    fn writing<T, E: Into<WriteError>>(
        &self,
        path: &str,
        body: impl FnOnce() -> Result<T, E>,
    ) -> Result<T, ObsidianError> {
        body().map_err(|error| match error.into() {
            WriteError::Atomic(failure) => ObsidianError::WriteFailed {
                path: failure.path,
                underlying: failure.underlying,
            },
            WriteError::Io(error) => ObsidianError::WriteFailed {
                path: self.absolute(path),
                underlying: error.to_string(),
            },
        })
    }

    /// `path` as the message names it: vault-relative paths become absolute,
    /// absolute ones (the mixdown) stay.
    fn absolute(&self, path: &str) -> String {
        if Path::new(path).is_absolute() {
            path.to_owned()
        } else {
            self.sink.path(path).to_string_lossy().into_owned()
        }
    }
}

/// The two ways a write step fails, folded into one message by `writing`.
enum WriteError {
    Atomic(WriteFailure),
    Io(std::io::Error),
}

impl From<WriteFailure> for WriteError {
    fn from(failure: WriteFailure) -> Self {
        WriteError::Atomic(failure)
    }
}

impl From<std::io::Error> for WriteError {
    fn from(error: std::io::Error) -> Self {
        WriteError::Io(error)
    }
}

#[async_trait]
impl Destination for ObsidianFolderDestination {
    fn id(&self) -> &str {
        &self.id
    }

    async fn validate(&self) -> BoundaryResult<()> {
        Ok(self.validate_vault()?)
    }

    async fn deliver(
        &self,
        meeting: &MeetingExport,
        previous: Option<&DeliveryReceipt>,
    ) -> BoundaryResult<DeliveryReceipt> {
        Ok(self.deliver_meeting(meeting, previous)?)
    }
}

/// The local path of a `file://` URL as the store holds it
/// (`steno_core::paths::file_url`): the percent-encoding undone, the host
/// part (empty or `localhost`) dropped, and on Windows the drive path
/// restored with backslashes. `None` for any other scheme.
#[must_use]
pub fn file_url_path(url: &str) -> Option<PathBuf> {
    let rest = url.strip_prefix("file://")?;
    let path = match rest.find('/') {
        Some(0) => rest,
        Some(slash) => &rest[slash..],
        None => return None,
    };
    let mut bytes = Vec::with_capacity(path.len());
    let raw = path.as_bytes();
    let mut index = 0;
    while index < raw.len() {
        if raw[index] == b'%'
            && index + 2 < raw.len()
            && let Ok(byte) = u8::from_str_radix(&path[index + 1..index + 3], 16)
        {
            bytes.push(byte);
            index += 3;
        } else {
            bytes.push(raw[index]);
            index += 1;
        }
    }
    let decoded = String::from_utf8(bytes).ok()?;
    // `file_url` spelt a Windows path with forward slashes behind a leading
    // `/`; both are undone so the path reads back as the OS spells it.
    #[cfg(windows)]
    let decoded = decoded
        .strip_prefix('/')
        .filter(|rest| rest.as_bytes().get(1) == Some(&b':'))
        .unwrap_or(&decoded)
        .replace('/', "\\");
    Some(PathBuf::from(decoded))
}
