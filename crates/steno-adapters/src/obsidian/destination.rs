//! The Obsidian vault folder destination.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex, PoisonError};

use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use steno_core::content_hash::sha256;
use steno_core::paths::file_url_path;
use steno_core::{
    AudioFormat, BoundaryResult, DeliveryReceipt, Destination, FileOwnership, MeetingExport,
    ObsidianSettings, Platform, async_trait,
};
use thiserror::Error;
use uuid::Uuid;

use super::ManagedBlock;
use crate::fs::{AtomicFileWriter, LocalFolderSink};
use crate::naming::{MeetingFolder, Note};
use crate::rendering::{ArtifactRenderer, LinkStyle, PersonPage, RenderOptions};
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
}

/// The start of the vault probe's file name; eight lowercase hex digits
/// follow.
const PROBE_PREFIX: &str = ".steno-probe-";

/// Whether `name` is exactly a probe's name.
fn is_probe_name(name: &str) -> bool {
    name.strip_prefix(PROBE_PREFIX).is_some_and(|hex| {
        hex.len() == 8
            && hex
                .bytes()
                .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
    })
}

/// Whether a probe's remove left it gone; already gone counts.
fn removed(result: std::io::Result<()>) -> bool {
    match result {
        Ok(()) => true,
        Err(error) => error.kind() == std::io::ErrorKind::NotFound,
    }
}

/// The folder's basename, which every note in it is named after.
fn folder_slug(folder: &str) -> String {
    Path::new(folder).file_name().map_or_else(
        || folder.to_owned(),
        |name| name.to_string_lossy().into_owned(),
    )
}

fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or_default()
}

/// The receipt's warning for the notes this delivery kept, each as
/// `(name, copy name)`; `None` when it kept none.
fn kept_warning(kept: &[(String, String)]) -> Option<String> {
    match kept {
        [] => None,
        [(name, copy)] => Some(format!(
            "Kept your changes to {name} and put Steno's version beside it as {copy}; \
             to use Steno's, delete {name} and export again"
        )),
        [rest @ .., (last, _)] => {
            let rest: Vec<&str> = rest.iter().map(|(name, _)| name.as_str()).collect();
            Some(format!(
                "Kept your changes to {} and {last} and put Steno's versions beside them; \
                 to use Steno's, delete your versions and export again",
                rest.join(", ")
            ))
        }
    }
}

/// An audio copy's file name: `audio` or `audio.<ext>`.
fn is_audio(name: &str) -> bool {
    name == "audio" || name.starts_with("audio.")
}

/// The lock every delivery into the vault at `vault_path` holds, one per
/// canonical vault path in the process. Other processes are not covered;
/// only the folder claim holds across processes.
fn vault_lock(vault_path: &str) -> Arc<Mutex<()>> {
    static LOCKS: LazyLock<Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>> = LazyLock::new(Mutex::default);
    let key = fs::canonicalize(vault_path).unwrap_or_else(|_| PathBuf::from(vault_path));
    let mut locks = LOCKS.lock().unwrap_or_else(PoisonError::into_inner);
    Arc::clone(locks.entry(key).or_default())
}

/// The Obsidian vault folder destination: `Meetings/<date>-<slug>/` with
/// the notes, `transcript.vtt`, `meeting.json`, the optional audio copy,
/// and one managed block per person page. Policy is [`DeliveryLedger`]'s;
/// this type renders and writes through [`LocalFolderSink`]. Nothing is
/// deleted but its own temp files and an empty folder it created.
#[derive(Debug, Clone)]
pub struct ObsidianFolderDestination {
    /// [`ObsidianFolderDestination::DESTINATION_ID`] for the stored
    /// settings; a one-off `steno deliver --vault` passes its own id.
    id: String,
    settings: ObsidianSettings,
    /// Time zone of the folder date and every date in the notes.
    time_zone: Tz,
    /// The platform the folder note names a call by
    /// ([`RenderOptions::platform`]).
    platform: Platform,
    sink: LocalFolderSink,
    /// The wall clock that dates a copy written beside an edited note.
    now: fn() -> DateTime<Utc>,
}

impl ObsidianFolderDestination {
    pub const DESTINATION_ID: &'static str = "obsidian-folder";

    /// The receipt's warning when the audio copy is on but its source is
    /// gone and the folder holds no audio: the notes go without it.
    pub const NO_AUDIO_WARNING: &'static str =
        "The audio was already removed, so the export has no audio file";

    /// The stored destination for `settings`, with its dates in `time_zone`
    /// and its calls named after [`Platform::CURRENT`].
    ///
    /// ```
    /// use chrono_tz::Tz;
    /// use steno_adapters::ObsidianFolderDestination;
    /// use steno_core::ObsidianSettings;
    ///
    /// let destination = ObsidianFolderDestination::new(
    ///     ObsidianSettings {
    ///         vault_path: "/path/to/vault".to_owned(),
    ///         people_folder: Some("People".to_owned()),
    ///         include_audio: false,
    ///         task_tag: None,
    ///         extra: serde_json::Map::new(),
    ///     },
    ///     Tz::Europe__Berlin,
    /// );
    /// assert_eq!(destination.settings().people_folder.as_deref(), Some("People"));
    /// assert!(destination.validate_vault().is_err(), "no vault at that path");
    /// ```
    #[must_use]
    pub fn new(settings: ObsidianSettings, time_zone: Tz) -> Self {
        Self::with_id(settings, time_zone, Self::DESTINATION_ID)
    }

    /// A destination with its own `Delivery` row id (`steno deliver --vault`).
    #[must_use]
    pub fn with_id(settings: ObsidianSettings, time_zone: Tz, id: &str) -> Self {
        let sink = LocalFolderSink::new(PathBuf::from(&settings.vault_path));
        ObsidianFolderDestination {
            id: id.to_owned(),
            settings,
            time_zone,
            platform: Platform::CURRENT,
            sink,
            now: Utc::now,
        }
    }

    /// The same destination dating copies beside edited notes with `now`.
    #[must_use]
    pub fn with_now(mut self, now: fn() -> DateTime<Utc>) -> Self {
        self.now = now;
        self
    }

    /// The same destination naming its calls after `platform`.
    #[must_use]
    pub fn with_platform(mut self, platform: Platform) -> Self {
        self.platform = platform;
        self
    }

    #[must_use]
    pub fn settings(&self) -> &ObsidianSettings {
        &self.settings
    }

    #[must_use]
    pub fn time_zone(&self) -> Tz {
        self.time_zone
    }

    #[must_use]
    pub fn platform(&self) -> Platform {
        self.platform
    }

    /// The vault is a writable directory and the people folder a plain
    /// relative path. A missing `.obsidian/` is not an error.
    ///
    /// The write is probed with a hidden empty file of a new name at the
    /// root, created only if nothing has that name and removed at once:
    /// - an empty regular probe a crash left behind is removed first;
    ///   anything else of that name is the user's and stays;
    /// - a probe whose remove fails after the create is left for the next
    ///   check, not an error;
    /// - with one leftover that cannot be removed, a fresh probe decides;
    /// - with two or more, the vault is not writable, so leftovers stay at
    ///   two at most.
    pub fn validate_vault(&self) -> Result<(), ObsidianError> {
        self.validate_vault_with(
            &format!("{PROBE_PREFIX}{}", AtomicFileWriter::random_hex()),
            |path| fs::remove_file(path),
        )
    }

    /// [`Self::validate_vault`] with the probe's name and the remove passed
    /// in, for tests.
    fn validate_vault_with(
        &self,
        probe: &str,
        remove: impl Fn(&Path) -> std::io::Result<()>,
    ) -> Result<(), ObsidianError> {
        self.check_vault()?;
        let not_writable = || ObsidianError::VaultNotWritable(self.settings.vault_path.clone());
        let stayed = self.remove_probe_leftovers(&remove);
        if stayed >= 2 {
            return Err(not_writable());
        }
        let path = self.sink.path(probe);
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|_| not_writable())?;
        // Another check may have removed it already, which counts.
        if !removed(remove(&path)) && stayed == 1 {
            return Err(not_writable());
        }
        Ok(())
    }

    /// Removes earlier probe leftovers at the vault's root (empty regular
    /// files of the probe pattern, links never followed) and returns how
    /// many stay because their remove failed.
    fn remove_probe_leftovers(&self, remove: impl Fn(&Path) -> std::io::Result<()>) -> usize {
        let Ok(entries) = fs::read_dir(self.sink.root()) else {
            return 0;
        };
        entries
            .flatten()
            .filter(|entry| {
                let path = entry.path();
                entry.file_name().to_str().is_some_and(is_probe_name)
                    && fs::symlink_metadata(&path)
                        .is_ok_and(|metadata| metadata.file_type().is_file() && metadata.len() == 0)
                    && !removed(remove(&path))
            })
            .count()
    }

    /// [`Destination::deliver`] on the calling thread, without the boundary
    /// error. Blocks while another delivery into the vault runs. Kept edited
    /// notes and missing audio succeed with a receipt warning.
    pub fn deliver_meeting(
        &self,
        meeting: &MeetingExport,
        previous: Option<&DeliveryReceipt>,
    ) -> Result<DeliveryReceipt, ObsidianError> {
        let lock = vault_lock(&self.settings.vault_path);
        // A poisoned lock guards nothing to repair, so it is taken as it is.
        let _vault = lock.lock().unwrap_or_else(PoisonError::into_inner);
        self.check_vault()?;
        let mut ledger = DeliveryLedger::new(previous, &self.settings.vault_path, |root| {
            self.sink.is_root(root)
        });
        let (folder, created) = self.folder_for(meeting, &mut ledger)?;
        let written = self.write_meeting(meeting, &folder, &mut ledger);
        if written.is_err() && created {
            // Only an empty folder goes, so the next attempt claims the same
            // name.
            let _ = fs::remove_dir(self.sink.path(&folder));
        }
        written?;
        Ok(ledger.receipt(&folder, ArtifactRenderer::VERSION))
    }

    /// The meeting folder and whether this delivery created it. Without a
    /// pin it claims one; a gone pin is recreated; an existing one is kept
    /// when [`Self::keeps_pin`] says so, else a new folder is claimed.
    fn folder_for(
        &self,
        meeting: &MeetingExport,
        ledger: &mut DeliveryLedger,
    ) -> Result<(String, bool), ObsidianError> {
        let Some(pinned) = ledger.pinned_folder().map(str::to_owned) else {
            return self.claim_folder(meeting);
        };
        if !self.sink.entry_exists(&pinned) && self.recreate(&pinned)? {
            return Ok((pinned, true));
        }
        if self.keeps_pin(ledger, &pinned, meeting.meeting.id)? {
            return Ok((pinned, false));
        }
        let (claimed, created) = self.claim_folder(meeting)?;
        ledger.move_folder(&pinned, &claimed);
        Ok((claimed, created))
    }

    /// Writes the meeting's files, person pages and audio copy; failures go
    /// into the receipt's warnings.
    fn write_meeting(
        &self,
        meeting: &MeetingExport,
        folder: &str,
        ledger: &mut DeliveryLedger,
    ) -> Result<(), ObsidianError> {
        let slug = folder_slug(folder);
        let options = RenderOptions {
            link_style: LinkStyle::Wikilink,
            person_pages: self.settings.people_folder.is_some(),
            task_tag: self.settings.task_tag.clone(),
            time_zone: self.time_zone,
            platform: self.platform,
        };
        let renderer = ArtifactRenderer::new();

        self.writing(folder, || self.sink.create_directory(folder))?;
        AtomicFileWriter::remove_stale_temporaries(&self.sink.path(folder));

        let artifacts = renderer
            .render_meeting_files(meeting, &options, Some(&slug))
            .map_err(|error| ObsidianError::WriteFailed {
                path: self.absolute(&format!("{folder}/{}", MeetingFolder::JSON)),
                underlying: error.to_string(),
            })?;
        let mut kept = Vec::new();
        for artifact in artifacts {
            let path = format!("{folder}/{}", artifact.file_name);
            if let Some(copy) = self.write_note(ledger, &path, &artifact.data)? {
                kept.push((artifact.file_name, file_name(&copy).to_owned()));
            }
        }
        if let Some(warning) = kept_warning(&kept) {
            ledger.warn(warning);
        }

        if let Some(people_folder) = &self.settings.people_folder {
            let pages = renderer.render_person_pages(meeting, &options, Some(&slug));
            self.write_person_pages(people_folder, pages, meeting.meeting.id, ledger)?;
        }
        if self.settings.include_audio && !self.copy_audio(meeting, folder, ledger)? {
            ledger.warn(Self::NO_AUDIO_WARNING.to_owned());
        }
        Ok(())
    }

    /// Writes every rendered page into `people_folder`, whole or merged
    /// into its managed block, then drops this meeting's line from stale
    /// pages. File names stay NFC, as the receipts store them.
    fn write_person_pages(
        &self,
        people_folder: &str,
        pages: Vec<PersonPage>,
        meeting_id: Uuid,
        ledger: &mut DeliveryLedger,
    ) -> Result<(), ObsidianError> {
        self.writing(people_folder, || self.sink.create_directory(people_folder))?;
        AtomicFileWriter::remove_stale_temporaries(&self.sink.path(people_folder));
        let mut rendered: HashSet<String> = HashSet::new();
        for page in pages {
            let path = format!("{people_folder}/{}", page.file_name);
            rendered.insert(path.clone());
            let mut data = page.page.into_bytes();
            let existing = self.reading(&path, || self.sink.read(&path))?;
            if let Some(existing) = existing {
                // A non-UTF-8 page cannot be merged safely; left and reported.
                let Ok(text) = String::from_utf8(existing) else {
                    return Err(ObsidianError::ReadFailed {
                        path: self.absolute(&path),
                        underlying: "not UTF-8 text; the page was left unchanged".to_owned(),
                    });
                };
                data = ManagedBlock::merge(&page.line, meeting_id, &text).into_bytes();
            }
            self.writing(&path, || self.sink.write(&data, &path))?;
            ledger.record(&path, FileOwnership::ManagedBlock, &data);
        }
        self.remove_meeting_line(&rendered, meeting_id, ledger)
    }

    /// Copies the mixdown, or a phone recording's AAC file, into the folder.
    /// An audio file already there stands in for a gone source. `Ok(false)`
    /// when there is no audio.
    fn copy_audio(
        &self,
        meeting: &MeetingExport,
        folder: &str,
        ledger: &mut DeliveryLedger,
    ) -> Result<bool, ObsidianError> {
        let source = meeting
            .audio
            .as_ref()
            .and_then(|audio| match &audio.mixdown_url {
                Some(mixdown) => file_url_path(mixdown),
                None if audio.format == AudioFormat::M4aAac => file_url_path(&audio.url),
                None => None,
            });
        let source = match source {
            Some(path) if self.reading(&path.to_string_lossy(), || path.try_exists())? => path,
            _ => {
                let gone: Vec<String> = ledger
                    .files()
                    .keys()
                    .filter(|path| {
                        DeliveryLedger::name_in(path, folder).is_some_and(is_audio)
                            && !self.sink.entry_exists(path)
                    })
                    .cloned()
                    .collect();
                for path in &gone {
                    ledger.forget(path);
                }
                return Ok(self.audio_is_in_the_vault(folder));
            }
        };
        let data = self
            .reading(&source.to_string_lossy(), || fs::read(&source).map(Some))?
            .unwrap_or_default();
        let extension = source
            .extension()
            .map(|extension| extension.to_string_lossy().into_owned())
            .unwrap_or_default();
        self.write_owned(
            ledger,
            &format!("{folder}/{}", MeetingFolder::audio_file(&extension)),
            &data,
        )?;
        Ok(true)
    }

    /// [`write_owned`](Self::write_owned) for a note the user may edit. An
    /// edited note ([`Self::is_edited`]) stays; the render goes beside it
    /// ([`Self::copy_path`]) and the receipt keeps the note's entry.
    /// Returns the copy it wrote.
    fn write_note(
        &self,
        ledger: &mut DeliveryLedger,
        path: &str,
        data: &[u8],
    ) -> Result<Option<String>, ObsidianError> {
        if !self.is_edited(ledger, path, data)? {
            self.write_owned(ledger, path, data)?;
            return Ok(None);
        }
        let copy = self.copy_path(ledger, path, data)?;
        self.writing(&copy, || self.sink.write(data, &copy))?;
        ledger.record(&copy, FileOwnership::Owned, data);
        ledger.keep(path, data);
        Ok(Some(copy))
    }

    /// Whether what is at `path` is the user's to keep: a directory, or a
    /// file holding neither this render nor bytes Steno wrote there or to a
    /// copy ([`DeliveryLedger::wrote`]). Without a listing receipt any other
    /// bytes count as edited. A missing path is not edited.
    fn is_edited(
        &self,
        ledger: &DeliveryLedger,
        path: &str,
        data: &[u8],
    ) -> Result<bool, ObsidianError> {
        if self.sink.is_directory(path) {
            return Ok(true);
        }
        let Some(on_disk) = self.reading(path, || self.sink.read(path))? else {
            return Ok(false);
        };
        Ok(on_disk != data && !ledger.wrote(path, &sha256(&on_disk)))
    }

    /// Where the new render of an edited note goes: the newest unedited
    /// listed copy ([`DeliveryLedger::listed_copies`]), else a new copy
    /// dated today, numbered from 2 while the name is taken by anything
    /// but this same render.
    fn copy_path(
        &self,
        ledger: &mut DeliveryLedger,
        path: &str,
        data: &[u8],
    ) -> Result<String, ObsidianError> {
        for copy in ledger.listed_copies(path) {
            if self.may_reuse(ledger, &copy, data)? {
                return Ok(copy);
            }
            if !self.sink.entry_exists(&copy) {
                ledger.forget(&copy);
            }
        }
        let today = (self.now)()
            .with_timezone(&self.time_zone)
            .format("%Y-%m-%d")
            .to_string();
        let mut number = 1;
        let mut candidate = DeliveryLedger::copy_beside_path(path, &today, number);
        while self.sink.entry_exists(&candidate) && !self.may_reuse(ledger, &candidate, data)? {
            number += 1;
            candidate = DeliveryLedger::copy_beside_path(path, &today, number);
        }
        Ok(candidate)
    }

    /// Whether `copy` is an unedited file that may take the render `data`.
    fn may_reuse(
        &self,
        ledger: &DeliveryLedger,
        copy: &str,
        data: &[u8],
    ) -> Result<bool, ObsidianError> {
        Ok(self.sink.exists(copy) && !self.is_edited(ledger, copy, data)?)
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

    /// Drops this meeting's line from the previous receipt's managed-block
    /// pages this delivery did not render. Gone and non-UTF-8 pages are
    /// skipped, unchanged pages not rewritten, and a page that is the same
    /// file as a rendered one ([`LocalFolderSink::same_file`], a case-only
    /// rename) is not stale.
    fn remove_meeting_line(
        &self,
        rendered: &HashSet<String>,
        meeting_id: Uuid,
        ledger: &mut DeliveryLedger,
    ) -> Result<(), ObsidianError> {
        for path in &ledger.stale_managed_pages(rendered) {
            if rendered.iter().any(|page| self.sink.same_file(path, page)) {
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

    /// Claims a new folder with the ledger's collision rule
    /// ([`DeliveryLedger::claim_folder`]), creating each candidate; one
    /// holding this meeting's `meeting.json` is reused, an unreadable one
    /// is taken. Returns whether this delivery created it.
    fn claim_folder(&self, meeting: &MeetingExport) -> Result<(String, bool), ObsidianError> {
        let base = MeetingFolder::path(&meeting.meeting, self.time_zone);
        self.writing(MeetingFolder::ROOT, || {
            self.sink.create_directory(MeetingFolder::ROOT)
        })?;
        let mut created = false;
        let folder = DeliveryLedger::claim_folder(
            &base,
            meeting.meeting.id,
            |candidate| {
                created = self.create(candidate)?;
                Ok(created)
            },
            |folder| self.meeting_of(folder).ok().flatten(),
        )?;
        Ok((folder, created))
    }

    /// Creates the gone pinned `folder` and its parent; `Ok(false)` when
    /// something took the name first.
    fn recreate(&self, folder: &str) -> Result<bool, ObsidianError> {
        if let Some(parent) = Path::new(folder).parent() {
            let parent = parent.to_string_lossy();
            self.writing(&parent, || self.sink.create_directory(&parent))?;
        }
        self.create(folder)
    }

    /// Claims `folder` by creating it; `Ok(false)` when anything is there.
    fn create(&self, folder: &str) -> Result<bool, ObsidianError> {
        match self.sink.create_new_directory(folder) {
            Err(error) if error.kind() == ErrorKind::AlreadyExists => Ok(false),
            created => self.writing(folder, || created.map(|()| true)),
        }
    }

    /// Whether an existing pinned folder is still this meeting's: its
    /// `meeting.json`, or without one the `transcript.vtt` header, the
    /// note's `steno_id` or a listed copy of `meeting.json`, names it. A
    /// folder that is not is never written. Unreadable files are
    /// [`ObsidianError::ReadFailed`].
    fn keeps_pin(
        &self,
        ledger: &DeliveryLedger,
        folder: &str,
        id: Uuid,
    ) -> Result<bool, ObsidianError> {
        if !self.sink.is_directory(folder) {
            return Ok(false);
        }
        let json = format!("{folder}/{}", MeetingFolder::JSON);
        if self.sink.exists(&json) {
            return Ok(self.meeting_in(&json)? == Some(id));
        }
        if self.a_note_carries(folder, id)? {
            return Ok(true);
        }
        for copy in ledger.listed_copies(&json) {
            if self.meeting_in(&copy)? == Some(id) {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Whether `transcript.vtt`'s header or the folder note's `steno_id`
    /// names `id`. Unreadable files are [`ObsidianError::ReadFailed`].
    fn a_note_carries(&self, folder: &str, id: Uuid) -> Result<bool, ObsidianError> {
        let read = |name: &str| -> Result<Option<String>, ObsidianError> {
            let path = format!("{folder}/{name}");
            let data = self.reading(&path, || self.sink.read(&path))?;
            Ok(data.and_then(|data| String::from_utf8(data).ok()))
        };
        let named = |value: &str| value.trim().trim_matches('"').parse::<Uuid>().ok() == Some(id);
        let vtt = read(MeetingFolder::VTT)?;
        if vtt.is_some_and(|text| {
            text.lines()
                .next()
                .and_then(|header| header.strip_prefix("WEBVTT - Steno "))
                .is_some_and(named)
        }) {
            return Ok(true);
        }
        let note = read(&MeetingFolder::note_file(
            Note::Folder,
            &folder_slug(folder),
        ))?;
        Ok(note.is_some_and(|text| {
            text.lines()
                .skip(1)
                .take_while(|line| *line != "---")
                .filter_map(|line| line.strip_prefix("steno_id:"))
                .any(named)
        }))
    }

    /// The meeting `folder`'s `meeting.json` names; `None` when missing.
    /// Other read errors are [`ObsidianError::ReadFailed`], so a locked
    /// file never passes for another meeting's.
    fn meeting_of(&self, folder: &str) -> Result<Option<Uuid>, ObsidianError> {
        self.meeting_in(&format!("{folder}/{}", MeetingFolder::JSON))
    }

    /// The meeting the `meeting.json` (or copy) at `path` names.
    fn meeting_in(&self, path: &str) -> Result<Option<Uuid>, ObsidianError> {
        let Some(data) = self.reading(path, || self.sink.read(path))? else {
            return Ok(None);
        };
        let probe: Option<serde_json::Value> = serde_json::from_slice(&data).ok();
        Ok(probe.and_then(|probe| probe.pointer("/meeting/id")?.as_str()?.parse().ok()))
    }

    /// Whether an `audio` or `audio.<ext>` file is in the folder.
    fn audio_is_in_the_vault(&self, folder: &str) -> bool {
        self.sink
            .file_names(folder)
            .iter()
            .any(|name| is_audio(name))
    }

    /// The vault exists and the people folder passes
    /// [`DeliveryLedger::is_plain_relative`] with no `.` or empty
    /// component, no `\`, and no surrounding whitespace.
    pub(crate) fn check_vault(&self) -> Result<(), ObsidianError> {
        if !self.sink.is_directory("") {
            return Err(ObsidianError::VaultMissing(
                self.settings.vault_path.clone(),
            ));
        }
        let Some(folder) = &self.settings.people_folder else {
            return Ok(());
        };
        let valid = DeliveryLedger::is_plain_relative(folder)
            && !folder.contains('\\')
            && folder == folder.trim()
            && !folder
                .split('/')
                .any(|component| component == "." || component.is_empty());
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

    /// A `WriteFailure` names the absolute target the sink resolves.
    fn writing<T, E: std::fmt::Display>(
        &self,
        path: &str,
        body: impl FnOnce() -> Result<T, E>,
    ) -> Result<T, ObsidianError> {
        body().map_err(|error| ObsidianError::WriteFailed {
            path: self.absolute(path),
            underlying: error.to_string(),
        })
    }

    /// `path` as messages name it: vault-relative paths become absolute.
    fn absolute(&self, path: &str) -> String {
        if Path::new(path).is_absolute() {
            path.to_owned()
        } else {
            self.sink.path(path).to_string_lossy().into_owned()
        }
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

    /// [`ObsidianFolderDestination::deliver_meeting`] on tokio's blocking
    /// pool. A panic inside is raised again here; a task cancelled at
    /// shutdown is an error.
    async fn deliver(
        &self,
        meeting: &MeetingExport,
        previous: Option<&DeliveryReceipt>,
    ) -> BoundaryResult<DeliveryReceipt> {
        let destination = self.clone();
        let meeting = meeting.clone();
        let previous = previous.cloned();
        let delivered = tokio::task::spawn_blocking(move || {
            destination.deliver_meeting(&meeting, previous.as_ref())
        })
        .await;
        match delivered {
            Ok(receipt) => Ok(receipt?),
            Err(error) => match error.try_into_panic() {
                Ok(panic) => std::panic::resume_unwind(panic),
                Err(cancelled) => Err(cancelled.into()),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn destination_at(vault: &Path) -> ObsidianFolderDestination {
        ObsidianFolderDestination::new(
            ObsidianSettings {
                vault_path: vault.to_string_lossy().into_owned(),
                people_folder: None,
                include_audio: false,
                task_tag: None,
                extra: serde_json::Map::new(),
            },
            chrono_tz::Europe::Berlin,
        )
    }

    fn names(vault: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(vault)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    fn refuse_remove(_: &Path) -> std::io::Result<()> {
        Err(std::io::Error::from(ErrorKind::PermissionDenied))
    }

    /// A root refusing deletes passes once, then refuses, and holds two
    /// leftovers at most, both gone once a remove works.
    #[test]
    fn a_root_that_refuses_deletes_is_refused_and_holds_two_leftovers_at_most() {
        let directory = tempfile::tempdir().unwrap();
        let vault = directory.path();
        fs::write(vault.join("Inbox.md"), "- [ ] call back").unwrap();
        let destination = destination_at(vault);
        let refused = Err(ObsidianError::VaultNotWritable(
            vault.to_string_lossy().into_owned(),
        ));

        assert_eq!(
            destination.validate_vault_with(".steno-probe-0000abcd", refuse_remove),
            Ok(())
        );
        assert_eq!(names(vault), [".steno-probe-0000abcd", "Inbox.md"]);
        for probe in [
            ".steno-probe-1111abcd",
            ".steno-probe-2222abcd",
            ".steno-probe-3333abcd",
        ] {
            assert_eq!(
                destination.validate_vault_with(probe, refuse_remove),
                refused
            );
            assert_eq!(
                names(vault),
                [".steno-probe-0000abcd", ".steno-probe-1111abcd", "Inbox.md"]
            );
        }
        destination.validate_vault().unwrap();
        assert_eq!(names(vault), ["Inbox.md"]);
        assert_eq!(
            fs::read_to_string(vault.join("Inbox.md")).unwrap(),
            "- [ ] call back"
        );
    }
}
