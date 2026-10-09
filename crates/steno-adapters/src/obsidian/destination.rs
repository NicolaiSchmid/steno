//! The Obsidian vault folder destination.
//! Swift: `Sources/StenoAdapters/Obsidian/ObsidianFolderDestination.swift`.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex, PoisonError, TryLockError};

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

/// A point inside [`ObsidianFolderDestination::deliver_meeting`] that a
/// test can stop a delivery at, so a race with a second delivery (or with
/// another process, played by the test) runs in a chosen order instead of
/// by luck. Exported with the `testing` feature only; production code never
/// installs a hook.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(feature = "testing"), allow(dead_code))]
pub enum DeliveryStep<'a> {
    /// Another delivery in this process holds the vault; this one waits for
    /// it before touching anything.
    WaitingForVault,
    /// A delivery is about to claim this meeting folder by creating it: a
    /// first delivery, a redelivery whose pinned folder is gone, or one
    /// whose pinned folder is no longer this meeting's.
    ClaimingFolder(&'a str),
    /// The meeting folder is in place and its files are about to be
    /// written.
    WritingFolder(&'a str),
    /// The person page at this path has been read (or found missing) and
    /// is about to be written with this meeting's line merged in.
    WritingPersonPage(&'a str),
}

/// The hook a test installs with
/// [`ObsidianFolderDestination::with_step_hook`] (feature `testing`).
#[cfg(feature = "testing")]
#[derive(Clone)]
struct StepHook(Arc<dyn Fn(DeliveryStep<'_>) + Send + Sync>);

#[cfg(feature = "testing")]
impl std::fmt::Debug for StepHook {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("StepHook")
    }
}

/// The folder's basename, which every note in it is named after. Swift's
/// `lastPathComponent`: a pinned `Meetings/x/` still names `x`.
fn folder_slug(folder: &str) -> String {
    Path::new(folder).file_name().map_or_else(
        || folder.to_owned(),
        |name| name.to_string_lossy().into_owned(),
    )
}

/// The last component of the vault-relative `path`.
fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or_default()
}

/// The receipt's warning for the notes this delivery kept, each as `(name,
/// copy name)` ([`ObsidianFolderDestination::write_note`]): for one, a
/// sentence naming both files; for several, one naming the notes only.
/// `None` when it kept none.
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

/// The lock every delivery into the vault at `vault_path` holds from start
/// to end, one per vault in the process, so two deliveries merge a shared
/// person page, claim meeting folders and sweep temp files one after the
/// other. Keyed by the canonical path (the configured one when it cannot be
/// resolved), so two spellings of one vault share a lock. Another process
/// on the vault is not covered; only the folder claim holds across
/// processes.
fn vault_lock(vault_path: &str) -> Arc<Mutex<()>> {
    static LOCKS: LazyLock<Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>> = LazyLock::new(Mutex::default);
    let key = fs::canonicalize(vault_path).unwrap_or_else(|_| PathBuf::from(vault_path));
    let mut locks = LOCKS.lock().unwrap_or_else(PoisonError::into_inner);
    Arc::clone(locks.entry(key).or_default())
}

/// The Obsidian vault folder destination: `Meetings/<date>-<slug>/` with the
/// folder note, transcript, tasks, `transcript.vtt`, `meeting.json`, the
/// optional audio copy, and one managed block per person page. A person page
/// the previous receipt lists but this delivery no longer renders (a speaker
/// reassigned to somebody else, a renamed person) loses this meeting's line
/// and keeps everything else. The policy (which receipt applies, what may be
/// written, what the receipt says) is [`DeliveryLedger`]'s; this type
/// renders, asks the ledger and writes through [`LocalFolderSink`]. Nothing
/// is deleted but the writer's own temp files and a meeting folder this
/// delivery created and left empty. Deliveries into one vault run one at a
/// time in a process (`vault_lock`).
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
    /// The platform the folder note names a call by
    /// ([`RenderOptions::platform`]): the one the app runs on, which
    /// recorded every call it stores; tests pin it.
    platform: Platform,
    sink: LocalFolderSink,
    /// The tests' [`DeliveryStep`] hook; `None` outside them.
    #[cfg(feature = "testing")]
    steps: Option<StepHook>,
    /// The wall clock that dates a copy written beside an edited note;
    /// tests pin it.
    now: fn() -> DateTime<Utc>,
}

impl ObsidianFolderDestination {
    pub const DESTINATION_ID: &'static str = "obsidian-folder";

    /// The receipt's warning when the audio copy is on, the file it copies
    /// is gone (the retention sweep removes it with the master) and the
    /// meeting folder holds no audio file: the notes are delivered without
    /// it. Swift fails with `audioUnavailable` instead (parity note in the
    /// plan).
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
            #[cfg(feature = "testing")]
            steps: None,
            now: Utc::now,
        }
    }

    /// The same destination dating the copies it writes beside edited
    /// notes with `now`: the tests pin the clock.
    #[must_use]
    pub fn with_now(mut self, now: fn() -> DateTime<Utc>) -> Self {
        self.now = now;
        self
    }

    /// The same destination naming its calls after `platform`: the golden
    /// tests render every platform's note on every OS.
    #[must_use]
    pub fn with_platform(mut self, platform: Platform) -> Self {
        self.platform = platform;
        self
    }

    /// The same destination calling `hook` at every [`DeliveryStep`] it
    /// reaches; the hook may block to hold the delivery there.
    #[cfg(feature = "testing")]
    #[must_use]
    pub fn with_step_hook(
        mut self,
        hook: impl Fn(DeliveryStep<'_>) + Send + Sync + 'static,
    ) -> Self {
        self.steps = Some(StepHook(Arc::new(hook)));
        self
    }

    /// Calls the tests' hook; without the `testing` feature every step
    /// passes straight through.
    #[cfg_attr(not(feature = "testing"), allow(clippy::unused_self, unused_variables))]
    fn reached(&self, step: DeliveryStep<'_>) {
        #[cfg(feature = "testing")]
        if let Some(StepHook(hook)) = &self.steps {
            hook(step);
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

    #[must_use]
    pub fn platform(&self) -> Platform {
        self.platform
    }

    /// The vault is a writable directory (probed with a file that is created
    /// and removed) and the people folder is a plain relative path (no `..`,
    /// no `.`). A missing `.obsidian/` is not an error: the folder may be a
    /// vault Obsidian has not opened yet.
    pub fn validate_vault(&self) -> Result<(), ObsidianError> {
        self.check_vault()?;
        let probe = format!(".steno-probe-{}", AtomicFileWriter::random_hex());
        let path = self.sink.path(&probe);
        fs::write(&path, [])
            .and_then(|()| fs::remove_file(&path))
            .map_err(|_| ObsidianError::VaultNotWritable(self.settings.vault_path.clone()))
    }

    /// [`Destination::deliver`] without the boundary error wrapper and on
    /// the calling thread, where the tests call it. Blocks while another
    /// delivery into the same vault runs in this process (`vault_lock`). A
    /// delivery that kept a note the user edited ([`Self::write_note`]) or
    /// went without the audio it should copy ([`Self::NO_AUDIO_WARNING`])
    /// succeeds and says so in the receipt's `warnings`.
    pub fn deliver_meeting(
        &self,
        meeting: &MeetingExport,
        previous: Option<&DeliveryReceipt>,
    ) -> Result<DeliveryReceipt, ObsidianError> {
        let lock = vault_lock(&self.settings.vault_path);
        // `try_lock` first only so a test hook sees that this delivery
        // waits. A delivery that panicked leaves nothing behind the lock to
        // repair, so a poisoned lock is taken as it is.
        let _vault = match lock.try_lock() {
            Ok(guard) => guard,
            Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
            Err(TryLockError::WouldBlock) => {
                self.reached(DeliveryStep::WaitingForVault);
                lock.lock().unwrap_or_else(PoisonError::into_inner)
            }
        };
        self.check_vault()?;
        let mut ledger = DeliveryLedger::new(previous, &self.settings.vault_path, |root| {
            self.sink.is_root(root)
        });
        let (folder, created) = self.folder_for(meeting, &mut ledger)?;
        let written = self.write_meeting(meeting, &folder, &mut ledger);
        if written.is_err() && created {
            // Only an empty folder is removed, so the next attempt claims
            // the same name instead of reading it as no longer this
            // meeting's; one that holds a file stays.
            let _ = fs::remove_dir(self.sink.path(&folder));
        }
        written?;
        Ok(ledger.receipt(&folder, ArtifactRenderer::VERSION))
    }

    /// The meeting folder of this delivery, and whether this delivery
    /// created it. Without a pinned folder it claims one
    /// ([`Self::claim_folder`]). A pinned folder that is gone is claimed
    /// again by creating it (`symlink_metadata`, so a dangling link counts
    /// as there). One that is there, or that something else created first,
    /// is kept when [`Self::keeps_pin`] says so; otherwise the delivery
    /// claims a folder as a first one does and moves its receipt there.
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

    /// Writes the meeting's files into `folder`, its person pages and its
    /// audio copy; what it could not do goes into the receipt's warnings.
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
        self.reached(DeliveryStep::WritingFolder(folder));

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

    /// Writes every rendered page into `people_folder`: whole when the page
    /// is new, else with this meeting's line merged into its managed block;
    /// then drops the line from the previous receipt's pages this delivery
    /// did not render. File names are written as the model holds them, NFC
    /// (`Anna Müller.md`); Foundation writes them NFD on APFS, which treats
    /// the two as one file, and the Swift and Rust receipts both store NFC.
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
            self.reached(DeliveryStep::WritingPersonPage(&path));
            if let Some(existing) = existing {
                // A page that is not UTF-8 text cannot be merged without
                // changing bytes outside the block, so it is left as it
                // is and reported.
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

    /// Copies the mixdown into the meeting folder, or for a phone recording
    /// (`M4aAac`) without one its AAC file. When that file is gone, an
    /// audio file already in the folder is the audio, and a receipt entry
    /// for one that is gone is dropped. `Ok(false)` when there is no audio;
    /// a file that cannot be checked is [`ObsidianError::ReadFailed`].
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

    /// [`write_owned`](Self::write_owned) for a note the user may edit in
    /// the vault (every rendered file but the audio copy). A note the user
    /// edited ([`Self::is_edited`]) is left as it is: the new render goes
    /// beside it ([`Self::copy_path`]), and the receipt keeps the note's
    /// entry when Steno may write there ([`DeliveryLedger::keep`]), so the
    /// next delivery sees the edit too. Returns the copy it wrote, which the
    /// receipt's warning names with the note and how to get Steno's version
    /// (`kept_warning`). Rust only: Swift overwrites the note.
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

    /// Whether what is at `path` is the user's to keep: a directory (or a
    /// link to one), or a file whose bytes are neither this render nor bytes
    /// Steno wrote there or to a copy beside it ([`DeliveryLedger::wrote`]).
    /// Without a receipt that lists them (none applies, or it does not list
    /// the path) any other bytes count as edited, so an unedited note of an
    /// earlier render gets a copy too. A file that already holds this render
    /// is not edited: a delivery that failed or was not saved after writing
    /// heals on the next one. Neither is a note the user moved a copy over,
    /// also once the render changes. A path with nothing at it is not
    /// edited.
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

    /// Where the new render `data` of the edited note at `path` goes: the
    /// newest copy an earlier delivery wrote beside it
    /// ([`DeliveryLedger::listed_copies`]) that is there and not edited
    /// ([`Self::may_reuse`]); else a new one dated today in the
    /// destination's time zone, numbered from 2 within the day while
    /// anything (a dangling link too) holds the name, unless what holds it
    /// is a file with this render already (a delivery that failed after
    /// writing it). A listed copy the user deleted leaves the receipt, so it
    /// is not written again under its old date.
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

    /// Whether the copy at `copy` may take the new render `data`: a file
    /// (not a dangling link) is there, and it is not edited.
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

    /// Drops this meeting's line from every managed-block page of the
    /// previous receipt that this delivery did not render, so a person who
    /// left the meeting no longer lists it. A page that is gone from disk is
    /// skipped and a page that is not UTF-8 text is left as it is; neither
    /// fails the delivery, since there is nothing of ours to correct in the
    /// first case and no safe way to in the second. An unchanged page is not
    /// rewritten. A listed page that is the same file as a rendered one
    /// ([`LocalFolderSink::same_file`]) is not stale: on a case-insensitive
    /// vault a person renamed from `anna` to `Anna` renders `Anna.md`, which
    /// is the receipt's `anna.md`, and removing the line there would drop
    /// it from the page just written. The path stays in the receipt either
    /// way, as every file the app wrote does.
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

    /// The folder of a delivery without one: the scope's path with the
    /// ledger's collision rule ([`DeliveryLedger::claim_folder`]), each
    /// candidate claimed by creating it under `Meetings/` (created as
    /// needed), and whether this delivery created it (a candidate holding
    /// this meeting's `meeting.json` is reused). A candidate whose
    /// `meeting.json` cannot be read counts as taken.
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

    /// Creates the gone pinned `folder` again, its parent first: `Ok(true)`
    /// when this delivery created it, `Ok(false)` when something took the
    /// name first.
    fn recreate(&self, folder: &str) -> Result<bool, ObsidianError> {
        if let Some(parent) = Path::new(folder).parent() {
            let parent = parent.to_string_lossy();
            self.writing(&parent, || self.sink.create_directory(&parent))?;
        }
        self.create(folder)
    }

    /// Claims `folder` by creating it, its parent already there: `Ok(true)`
    /// when this delivery created it, `Ok(false)` when anything was at the
    /// path.
    fn create(&self, folder: &str) -> Result<bool, ObsidianError> {
        self.reached(DeliveryStep::ClaimingFolder(folder));
        match self.sink.create_new_directory(folder) {
            Err(error) if error.kind() == ErrorKind::AlreadyExists => Ok(false),
            created => self.writing(folder, || created.map(|()| true)),
        }
    }

    /// Whether a pinned folder that is there is still this meeting's: a
    /// directory whose `meeting.json` names this meeting, or, with
    /// `meeting.json` missing, one where the `transcript.vtt` header, the
    /// folder note's `steno_id` or a copy of `meeting.json` the receipt
    /// lists ([`DeliveryLedger::listed_copies`]) names it, so a user who
    /// deleted the edited files Steno kept gets Steno's back in place. The
    /// user moved or deleted this meeting's folder, and a meeting with the
    /// same date and title may have claimed the name since; a folder that
    /// is not this meeting's is never written, so the cost is a duplicate
    /// folder. A `meeting.json`, note or copy that cannot be read is
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

    /// Whether `transcript.vtt` (its `WEBVTT - Steno <id>` header) or the
    /// folder note (its `steno_id` frontmatter line) in `folder` names `id`.
    /// A note that is missing or not UTF-8 text names no meeting; one that
    /// cannot be read is [`ObsidianError::ReadFailed`].
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

    /// The meeting whose `meeting.json` is in `folder`: `None` when the
    /// file is missing or names no meeting. Any other read error is
    /// [`ObsidianError::ReadFailed`], so a file another program holds open
    /// fails the delivery instead of passing for another meeting's.
    fn meeting_of(&self, folder: &str) -> Result<Option<Uuid>, ObsidianError> {
        self.meeting_in(&format!("{folder}/{}", MeetingFolder::JSON))
    }

    /// The meeting the `meeting.json` (or copy of it) at `path` names,
    /// [`Self::meeting_of`]'s rule.
    fn meeting_in(&self, path: &str) -> Result<Option<Uuid>, ObsidianError> {
        let Some(data) = self.reading(path, || self.sink.read(path))? else {
            return Ok(None);
        };
        let probe: Option<serde_json::Value> = serde_json::from_slice(&data).ok();
        Ok(probe.and_then(|probe| probe.pointer("/meeting/id")?.as_str()?.parse().ok()))
    }

    /// Whether an `audio` or `audio.<ext>` file is in the meeting folder on
    /// disk.
    fn audio_is_in_the_vault(&self, folder: &str) -> bool {
        self.sink
            .file_names(folder)
            .iter()
            .any(|name| is_audio(name))
    }

    /// The vault exists and the people folder, if any, passes
    /// [`DeliveryLedger::is_plain_relative`] plus three checks on top: no `.`
    /// or empty component (the ledger's rule folds those away), no `\`, no
    /// surrounding whitespace. Swift's `checkVault` accepts `./People`
    /// (parity list in the plan).
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

    /// A `WriteFailure` names the same absolute target the sink resolves
    /// `path` to, so one mapping serves it and the plain `io::Error`s.
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

#[async_trait]
impl Destination for ObsidianFolderDestination {
    fn id(&self) -> &str {
        &self.id
    }

    async fn validate(&self) -> BoundaryResult<()> {
        Ok(self.validate_vault()?)
    }

    /// [`ObsidianFolderDestination::deliver_meeting`] on tokio's blocking
    /// pool: it waits on the vault lock and does blocking file I/O (the
    /// `fsync`ed writes, the audio copy), which would otherwise park a
    /// runtime worker. A panic inside it is raised again here, so the
    /// pipeline's `catch_unwind` still sees it. A task the runtime
    /// cancelled at shutdown comes back as an error.
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
