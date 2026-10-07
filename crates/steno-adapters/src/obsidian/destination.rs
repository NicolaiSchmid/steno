//! The Obsidian vault folder destination.
//! Swift: `Sources/StenoAdapters/Obsidian/ObsidianFolderDestination.swift`.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex, PoisonError, TryLockError};

use chrono_tz::Tz;
use steno_core::paths::file_url_path;
use steno_core::{
    BoundaryResult, DeliveryReceipt, Destination, FileOwnership, MeetingExport, ObsidianSettings,
    Platform, async_trait,
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
    #[error("The meeting has no audio mixdown to copy; every other file was written.")]
    AudioUnavailable,
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
    /// A delivery without a folder of its own (a first one, or a redelivery
    /// whose pinned folder is no longer this meeting's) is about to claim
    /// this meeting folder by creating it.
    ClaimingFolder(&'a str),
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
/// is deleted but the writer's own temp files. Deliveries into one vault
/// run one at a time in a process (`vault_lock`).
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
}

impl ObsidianFolderDestination {
    pub const DESTINATION_ID: &'static str = "obsidian-folder";

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
        }
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
    /// delivery into the same vault runs in this process (`vault_lock`).
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
        let mut ledger = DeliveryLedger::new(previous, &self.settings.vault_path);
        let folder = match ledger.pinned_folder().map(str::to_owned) {
            Some(lost) if self.is_lost(&lost, meeting.meeting.id)? => {
                let claimed = self.claim_folder(meeting)?;
                ledger.move_folder(&lost, &claimed);
                claimed
            }
            Some(folder) => folder,
            None => self.claim_folder(meeting)?,
        };
        let slug = folder_slug(&folder);
        let options = RenderOptions {
            link_style: LinkStyle::Wikilink,
            person_pages: self.settings.people_folder.is_some(),
            task_tag: self.settings.task_tag.clone(),
            time_zone: self.time_zone,
            platform: self.platform,
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
            let pages = renderer.render_person_pages(meeting, &options, Some(&slug));
            self.write_person_pages(people_folder, pages, meeting.meeting.id, &mut ledger)?;
        }
        if self.settings.include_audio {
            self.copy_audio(meeting, &folder, &mut ledger)?;
        }

        Ok(ledger.receipt(&folder, ArtifactRenderer::VERSION))
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

    /// Copies the mixdown into the meeting folder while it exists; after the
    /// retention sweep the copy already in the vault (in the receipt or on
    /// disk) is the audio. Only when neither is there has the meeting no
    /// audio to deliver.
    fn copy_audio(
        &self,
        meeting: &MeetingExport,
        folder: &str,
        ledger: &mut DeliveryLedger,
    ) -> Result<(), ObsidianError> {
        let mixdown = meeting
            .audio
            .as_ref()
            .and_then(|audio| audio.mixdown_url.as_deref())
            .and_then(file_url_path)
            .filter(|path| path.exists());
        let Some(mixdown) = mixdown else {
            return if self.audio_is_in_the_vault(folder, ledger) {
                Ok(())
            } else {
                Err(ObsidianError::AudioUnavailable)
            };
        };
        let data = self
            .reading(&mixdown.to_string_lossy(), || fs::read(&mixdown).map(Some))?
            .unwrap_or_default();
        let extension = mixdown
            .extension()
            .map(|extension| extension.to_string_lossy().into_owned())
            .unwrap_or_default();
        self.write_owned(
            ledger,
            &format!("{folder}/{}", MeetingFolder::audio_file(&extension)),
            &data,
        )
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
    /// needed). A candidate whose `meeting.json` cannot be read counts as
    /// taken.
    fn claim_folder(&self, meeting: &MeetingExport) -> Result<String, ObsidianError> {
        let base = MeetingFolder::path(&meeting.meeting, self.time_zone);
        self.writing(MeetingFolder::ROOT, || {
            self.sink.create_directory(MeetingFolder::ROOT)
        })?;
        DeliveryLedger::claim_folder(
            &base,
            meeting.meeting.id,
            |candidate| {
                self.reached(DeliveryStep::ClaimingFolder(candidate));
                match self.sink.create_new_directory(candidate) {
                    Err(error) if error.kind() == ErrorKind::AlreadyExists => Ok(false),
                    created => self.writing(candidate, || created.map(|()| true)),
                }
            },
            |folder| self.meeting_of(folder).ok().flatten(),
        )
    }

    /// The pinned folder is lost when it is there but no longer this
    /// meeting's: its `meeting.json` names another meeting or none, or is
    /// missing and no note's `steno_id` names this meeting. The user moved or
    /// deleted this meeting's folder, and a meeting with the same date and
    /// title may have claimed the name since. A lost folder is never written:
    /// the delivery claims a folder as a first one does and moves its receipt
    /// there, so the cost is a duplicate folder. A folder that is gone is not
    /// lost: the delivery recreates it, as Swift's does.
    fn is_lost(&self, folder: &str, id: Uuid) -> Result<bool, ObsidianError> {
        if !self.sink.exists(folder) {
            return Ok(false);
        }
        if self
            .sink
            .exists(&format!("{folder}/{}", MeetingFolder::JSON))
        {
            return Ok(self.meeting_of(folder)? != Some(id));
        }
        Ok(!self.a_note_names(folder, id))
    }

    /// Whether `transcript.vtt` (its `WEBVTT - Steno <id>` header) or the
    /// folder note (its `steno_id` frontmatter line) in `folder` names `id`.
    /// A note that is missing or cannot be read names no meeting.
    fn a_note_names(&self, folder: &str, id: Uuid) -> bool {
        let read = |name: &str| {
            let data = self.sink.read(&format!("{folder}/{name}")).ok()??;
            String::from_utf8(data).ok()
        };
        let named = |value: &str| value.trim().trim_matches('"').parse::<Uuid>().ok() == Some(id);
        let note = MeetingFolder::note_file(Note::Folder, &folder_slug(folder));
        read(MeetingFolder::VTT).is_some_and(|text| {
            text.lines()
                .next()
                .and_then(|header| header.strip_prefix("WEBVTT - Steno "))
                .is_some_and(named)
        }) || read(&note).is_some_and(|text| {
            text.lines()
                .skip(1)
                .take_while(|line| *line != "---")
                .filter_map(|line| line.strip_prefix("steno_id:"))
                .any(named)
        })
    }

    /// The meeting whose `meeting.json` is in `folder`: `None` when the
    /// file is missing or names no meeting. Any other read error is
    /// [`ObsidianError::ReadFailed`], so a file another program holds open
    /// fails the delivery instead of passing for another meeting's.
    fn meeting_of(&self, folder: &str) -> Result<Option<Uuid>, ObsidianError> {
        let path = format!("{folder}/{}", MeetingFolder::JSON);
        let Some(data) = self.reading(&path, || self.sink.read(&path))? else {
            return Ok(None);
        };
        let probe: Option<serde_json::Value> = serde_json::from_slice(&data).ok();
        Ok(probe.and_then(|probe| probe.pointer("/meeting/id")?.as_str()?.parse().ok()))
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
