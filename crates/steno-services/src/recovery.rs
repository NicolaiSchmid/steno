//! Recovery of an interrupted recording. A meeting is written `recording`
//! when its capture starts and gets its asset only when the capture stops,
//! so a recording ended by a crash, a `kill -9`, a power loss or a logout
//! cut short has a row and no asset. The master on disk is readable to its
//! last whole frame (`steno_audio::writer::caf`), so the launch rebuilds the
//! asset from the master's header and queues the meeting through the Mac
//! intake, with the end reason `failed` ("the capture failed; the recording
//! so far was kept"). The recorder does the same for a stop whose capture
//! failed. A phone meeting arrives whole and `queued`, so a phone row left
//! `recording` was written by another process: it is queued with the
//! asset that names its upload, or failed when it has none. Rust only:
//! Swift had no recovery.
//!
//! A row is failed only when its master is provably not there. The launch
//! looks first in the two folders that decide it: the one the recorder
//! recorded the meeting into before it wrote the row (the record in
//! [`crate::audio_folders`]), so a recording is found where it started
//! after the user picked another folder while it ran, and the settings'
//! audio folder. Either one missing or unreadable (an unmounted
//! volume, a permission not granted yet, an I/O error) keeps the row
//! `recording` for the next launch, and so does a record that cannot be
//! read. Then it looks in the known folders ([`crate::audio_folders`])
//! and the folder of every stored asset (`other_folders`), skipping one
//! that is missing or unreadable, so a folder retired for good does not
//! keep a row forever.
//!
//! The launch also leaves a recording alone while its master is still
//! written: the Swift app and an older Rust build share the database and
//! take no lock, so a row left `recording` may be one another process is
//! still writing. A master modified within
//! [`LiveRecordingCheck::fresh_within`] is watched until it is that old;
//! an old crash's master is recovered at once. A master whose modification
//! time is ahead of the clock (a clock set back, a volume whose clock runs
//! ahead) counts as still written: its row stays `recording` until the
//! clock passes that time, and a later launch recovers it.
//!
//! A recording can also be left with no row at all: a phone upload copied
//! into its meeting folder before a crash or a failed commit (the phone's
//! retry admits it into a new folder), a row a power loss took before it
//! was durable, a local recording whose meeting was deleted while it
//! recorded. After the rows left `recording`, the launch adopts each such
//! master where it is, as a meeting with its folder's id (`adopt_orphans`),
//! and the main window says so. Only a master this install wrote is
//! adopted: the record in [`crate::audio_folders`] names it from before
//! its row until a launch finds that row durable, so a meeting folder
//! another install put into a shared audio folder (a second computer
//! syncing it, a second database over it) is left alone. So is one the
//! Swift app or an earlier release wrote, which kept no record: the
//! launch logs how many it left. Nothing in the handover inbox is
//! adopted: it holds files, not meeting folders, a receipt accounts for
//! each, and the phone still has its copy.
//!
//! During the rollback window a recording can come back once: a Rust
//! recording killed, the Swift app's launch fails its row, and the user
//! deletes it there. The Swift app removes only what an asset names, so
//! the master stays, and its entry too; the next launch here adopts it
//! again. A delete here removes it for good.
//!
//! | Item | What it does |
//! |------|--------------|
//! | `Interrupted` | What the launch lists before anything can record: the rows left `recording`, the recorded and the known folders |
//! | `reconcile_interrupted` | The launch's pass over those rows, with what it found in `Reconciled` |
//! | `recover` | Finds, salvages and queues one meeting; the recorder calls it after a failed stop |
//! | `queue_upload` | Queues a phone meeting left `recording` with its stored asset |
//! | `other_folders`, `find_master` | Where else a master may be, and which folder holds it (`Lookup`) |
//! | `salvage` | The asset a master's header and sidecars give, as its capture would have handed it over |
//! | `adopt_orphans`, `orphans` | The launch's adoption of the masters in the audio folders that have no row and that the record names |
//! | `meeting_folders` | The audio folders a meeting's folder may be in, which a delete removes it from when no asset names it |
//! | [`LiveRecordingCheck`] | When a master counts as still written; the app's field, so tests inject the clock, and the recorder's check before a delete |
//!
//! All but [`LiveRecordingCheck`] are this crate's own.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use chrono::Utc;
use steno_audio::writer::{CafHeader, CafReadError, WavStreamWriter};
use steno_core::{
    AudioAsset, AudioFormat, AudioLane, AudioRetention, Meeting, MeetingSource, RecordingEndReason,
    RecordingLayout, Store, StoreError,
    paths::{file_url, file_url_path},
};
use steno_pipeline::{LocalRecordingIntake, LocalRecordingIntakeError, RecordingResult};
use uuid::Uuid;

/// Why a meeting's master cannot be recovered; the meeting fails as it
/// did before recovery.
#[derive(Debug, thiserror::Error)]
pub(crate) enum Unrecoverable {
    /// The writer never created the master, or it is gone; for a phone
    /// meeting, no asset names its upload.
    #[error("no audio folder holds the master")]
    NoMaster,
    /// Not a CAF the writer wrote: malformed, or another format.
    #[error(transparent)]
    Unreadable(CafReadError),
    /// The master's channels are not the lanes the source records.
    #[error("the master has {found} channels; the meeting records {expected} lanes")]
    WrongChannels { found: usize, expected: usize },
    /// A header and no whole frame.
    #[error("the master holds no audio")]
    Empty,
}

/// Why a recovery did not queue the meeting.
#[derive(Debug, thiserror::Error)]
pub(crate) enum RecoveryError {
    #[error(transparent)]
    Unrecoverable(#[from] Unrecoverable),
    /// A folder the master may be in, or the master itself, could not be
    /// read; the meeting stays `recording` for the next launch.
    #[error("a folder the recording may be in cannot be read now")]
    Unreachable,
    /// The asset was rebuilt but the intake could not save it; the meeting
    /// stays `recording` (`LocalRecordingIntake::complete`).
    #[error(transparent)]
    NotSaved(#[from] LocalRecordingIntakeError),
}

/// The lanes a meeting from `source` records, in master channel order, as
/// the recorder configures its capture; a phone's upload is one mixed lane
/// (never salvaged: [`queue_upload`]).
fn lanes(source: MeetingSource) -> Vec<AudioLane> {
    match source {
        MeetingSource::MacCall => steno_audio::CaptureMode::Call.lanes(),
        MeetingSource::MacInPerson => steno_audio::CaptureMode::InPerson.lanes(),
        MeetingSource::Phone => vec![AudioLane::Mixed],
    }
}

/// Where the master of `meeting_id` lives in `audio_folder`.
pub(crate) fn master_path(audio_folder: &Path, meeting_id: Uuid) -> PathBuf {
    RecordingLayout::new(audio_folder, meeting_id).master(AudioFormat::Caf48kFloat32)
}

/// `folders` each once, in order, without those in `except`.
pub(crate) fn distinct(
    folders: impl IntoIterator<Item = PathBuf>,
    except: &[PathBuf],
) -> Vec<PathBuf> {
    let mut distinct: Vec<PathBuf> = Vec::new();
    for folder in folders {
        if !except.contains(&folder) && !distinct.contains(&folder) {
            distinct.push(folder);
        }
    }
    distinct
}

/// The audio folders the folder of meeting `meeting_id` may be in, each
/// once: the one it was recorded into ([`crate::audio_folders::recorded`]),
/// the settings' one and the known ones. A record, settings or list that
/// cannot be read gives none. A delete removes the meeting's folder in
/// each when no asset names it (a meeting left `recording`, one that
/// failed before its asset was saved), so the launch does not adopt its
/// master again (`Recorder::left_recording`).
pub(crate) fn meeting_folders(
    store: &Store,
    support_directory: &Path,
    meeting_id: Uuid,
) -> Vec<PathBuf> {
    let recorded = crate::audio_folders::recorded(support_directory)
        .ok()
        .and_then(|mut recorded| recorded.remove(&meeting_id));
    let current = store
        .settings()
        .ok()
        .and_then(|settings| file_url_path(&settings.audio_folder));
    let known = crate::audio_folders::known(support_directory).unwrap_or_default();
    distinct(recorded.into_iter().chain(current).chain(known), &[])
}

/// The folders a master may be in besides the two that decide a meeting
/// (the recorded one and the settings'), each once: the `known` folders
/// ([`crate::audio_folders::known`]), then the folder of each stored asset
/// (the folder above its meeting's folder).
pub(crate) fn other_folders(store: &Store, known: &[PathBuf]) -> Result<Vec<PathBuf>, StoreError> {
    let assets = store.asset_urls()?.into_iter().filter_map(|url| {
        file_url_path(&url).and_then(|master| master.parent()?.parent().map(Path::to_path_buf))
    });
    Ok(distinct(known.iter().cloned().chain(assets), &[]))
}

/// Where a meeting's master was looked for, and what was found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Lookup {
    /// The audio folder that holds the master.
    Found(PathBuf),
    /// No folder holds it, and each deciding folder was read.
    Absent,
    /// No folder holds it, but a deciding folder is missing or could not
    /// be read, so the master may be there.
    Unreachable,
}

/// Looks for the master of `meeting_id` in each of `deciding`, then in
/// each of `others`. A deciding folder that is missing or cannot be read
/// makes a miss [`Lookup::Unreachable`]: an unmounted volume is missing
/// until it is mounted again. Any other such folder is skipped.
#[must_use]
pub(crate) fn find_master(deciding: &[PathBuf], others: &[PathBuf], meeting_id: Uuid) -> Lookup {
    let mut unreachable = false;
    let searched = deciding
        .iter()
        .map(|folder| (folder, true))
        .chain(others.iter().map(|folder| (folder, false)));
    for (folder, decides) in searched {
        match std::fs::metadata(master_path(folder, meeting_id)) {
            Ok(metadata) if metadata.is_file() => return Lookup::Found(folder.clone()),
            Ok(_) => {}
            Err(error)
                if error.kind() == std::io::ErrorKind::NotFound
                    && std::fs::read_dir(folder).is_ok() => {}
            Err(_) => unreachable |= decides,
        }
    }
    if unreachable {
        Lookup::Unreachable
    } else {
        Lookup::Absent
    }
}

/// The recording the master of `meeting_id` in `layout` holds, as
/// its capture would have handed it over: the asset rebuilt from the
/// master's header alone (whole frames only) with the lanes `source`
/// records, the default retention (`complete` sets the real one), and the
/// end reason `failed`. A master that is there but cannot be read now (a
/// permission, an I/O error) is [`RecoveryError::Unreachable`], so its row
/// is kept, not failed. A sidecar whose writer died is finished from its
/// length ([`WavStreamWriter::recover`]) and listed when it holds samples;
/// one that cannot be is left out, and the decoder rebuilds that lane
/// from the master.
pub(crate) fn salvage(
    layout: &RecordingLayout,
    meeting_id: Uuid,
    source: MeetingSource,
) -> Result<RecordingResult, RecoveryError> {
    let lanes = lanes(source);
    let master = layout.master(AudioFormat::Caf48kFloat32);
    let header = CafHeader::read(&master).map_err(|error| match error {
        CafReadError::Io(_) if master.try_exists().is_ok_and(|exists| !exists) => {
            Unrecoverable::NoMaster.into()
        }
        CafReadError::Io(_) => RecoveryError::Unreachable,
        error => Unrecoverable::Unreadable(error).into(),
    })?;
    if header.channel_count != lanes.len() {
        return Err(Unrecoverable::WrongChannels {
            found: header.channel_count,
            expected: lanes.len(),
        }
        .into());
    }
    if header.frame_count == 0 {
        return Err(Unrecoverable::Empty.into());
    }
    let sidecars_16k = lanes
        .iter()
        .filter_map(|lane| {
            let path = layout.sidecar(*lane);
            match WavStreamWriter::recover(&path) {
                Ok(samples) if samples > 0 => Some((*lane, file_url(&path, false))),
                Ok(_) => None,
                Err(error) => {
                    tracing::debug!(lane = lane.as_str(), %error, "a sidecar was left out");
                    None
                }
            }
        })
        .collect();
    Ok(RecordingResult {
        asset: AudioAsset {
            id: Uuid::new_v4(),
            meeting_id,
            url: file_url(&master, false),
            format: AudioFormat::Caf48kFloat32,
            lanes,
            sidecars_16k,
            mixdown_url: None,
            retention: AudioRetention::KeepForever,
            expires_at: None,
        },
        duration: header.duration(),
        end_reason: RecordingEndReason::Failed,
    })
}

/// Looks for the master of meeting `meeting_id` in `folders`, each one
/// deciding ([`find_master`]), [salvages](salvage) it and queues the
/// meeting through `intake`, under the settings' default retention.
pub(crate) async fn recover(
    intake: &LocalRecordingIntake,
    folders: &[PathBuf],
    meeting_id: Uuid,
    source: MeetingSource,
) -> Result<Meeting, RecoveryError> {
    match find_master(folders, &[], meeting_id) {
        Lookup::Found(folder) => queue(intake, &folder, meeting_id, source).await,
        Lookup::Unreachable => Err(RecoveryError::Unreachable),
        Lookup::Absent => Err(Unrecoverable::NoMaster.into()),
    }
}

/// [Salvages](salvage) the master of `meeting_id` in `audio_folder` and
/// queues the meeting through `intake`.
async fn queue(
    intake: &LocalRecordingIntake,
    audio_folder: &Path,
    meeting_id: Uuid,
    source: MeetingSource,
) -> Result<Meeting, RecoveryError> {
    let result = salvage(
        &RecordingLayout::new(audio_folder, meeting_id),
        meeting_id,
        source,
    )?;
    Ok(intake.complete(meeting_id, result, None).await?)
}

/// Queues a phone meeting left `recording` through `intake` with the asset
/// that names its upload, kept as stored. This app admits a phone recording
/// whole and `queued`, so such a row was written by another process; the
/// end reason is `failed`, as for every recovered row. One with no asset
/// has nothing to queue ([`Unrecoverable::NoMaster`]), and one whose asset
/// cannot be read now is kept ([`RecoveryError::NotSaved`]).
async fn queue_upload(
    store: &Store,
    intake: &LocalRecordingIntake,
    meeting: &Meeting,
) -> Result<Meeting, RecoveryError> {
    let asset = store
        .asset(meeting.id)
        .map_err(|error| RecoveryError::NotSaved(error.into()))?
        .ok_or(Unrecoverable::NoMaster)?;
    let retention = asset.retention;
    let result = RecordingResult {
        asset,
        duration: meeting.duration,
        end_reason: RecordingEndReason::Failed,
    };
    Ok(intake.complete(meeting.id, result, Some(retention)).await?)
}

/// When the launch counts a master as still being written. The clock and
/// the wait are injectable so a test needs no real wait.
#[derive(Clone)]
pub struct LiveRecordingCheck {
    /// A master modified longer ago than this is a crash's: recovered at
    /// once. A younger one is watched until it is this old, and counts as
    /// still written when its modification time or its size changes
    /// meanwhile. Covers a writer stalled by a device rebuild and a
    /// network volume's cached file attributes.
    pub fresh_within: Duration,
    /// The wall clock the masters' modification times are read against;
    /// [`SystemTime::now`] in the product.
    pub now: Arc<dyn Fn() -> SystemTime + Send + Sync>,
    /// Blocks the calling thread for the given time; [`std::thread::sleep`]
    /// in the product.
    pub wait: Arc<dyn Fn(Duration) + Send + Sync>,
}

impl Default for LiveRecordingCheck {
    /// Ten seconds, with the product's clock and sleep: past a writer's
    /// stall in a device rebuild (its backoff of 1.75 s and the backend's
    /// start, longer over Bluetooth) and a network volume's cached file
    /// attributes; a writer that runs appends a frame every 10 ms.
    fn default() -> Self {
        LiveRecordingCheck {
            fresh_within: Duration::from_secs(10),
            now: Arc::new(SystemTime::now),
            wait: Arc::new(std::thread::sleep),
        }
    }
}

/// A master's modification time and size.
fn stamp(path: &Path) -> Option<(SystemTime, u64)> {
    let metadata = std::fs::metadata(path).ok()?;
    Some((metadata.modified().ok()?, metadata.len()))
}

impl LiveRecordingCheck {
    /// Whether the master at `path` was modified within
    /// [`Self::fresh_within`], or after the clock's now, without waiting;
    /// `false` when it is not there.
    pub(crate) fn is_fresh(&self, path: &Path) -> bool {
        stamp(path).is_some_and(|(modified, _)| self.left(modified) > Duration::ZERO)
    }

    /// How much younger than [`Self::fresh_within`] a master modified at
    /// `modified` is; zero once it is that old. A time ahead of the clock
    /// counts as just modified.
    fn left(&self, modified: SystemTime) -> Duration {
        let age = (self.now)()
            .duration_since(modified)
            .unwrap_or(Duration::ZERO);
        self.fresh_within.saturating_sub(age)
    }

    /// Whether each of `masters` is still written. A master that is
    /// missing or old is not. A fresh one is watched, all of them through
    /// one wait at a time, until it is [`Self::fresh_within`] old; one
    /// whose modification time or size changes meanwhile, or that is still
    /// young after waiting `fresh_within` in all (a modification time
    /// ahead of the clock), is. Nothing is waited for when no master is
    /// fresh.
    fn still_written(&self, masters: &[Option<PathBuf>]) -> Vec<bool> {
        let mut written = vec![false; masters.len()];
        let mut watched: Vec<(usize, &Path, (SystemTime, u64))> = masters
            .iter()
            .enumerate()
            .filter_map(|(index, path)| {
                let path = path.as_deref()?;
                let stamp = stamp(path)?;
                (self.left(stamp.0) > Duration::ZERO).then_some((index, path, stamp))
            })
            .collect();
        let mut waited = Duration::ZERO;
        while let Some(longest) = watched.iter().map(|(_, _, stamp)| self.left(stamp.0)).max() {
            let budget = self.fresh_within.saturating_sub(waited);
            if budget.is_zero() {
                for (index, _, _) in watched {
                    written[index] = true;
                }
                break;
            }
            let wait = longest.min(budget);
            (self.wait)(wait);
            waited += wait;
            watched.retain(|&(index, path, first)| {
                if stamp(path) != Some(first) {
                    written[index] = true;
                    return false;
                }
                self.left(first.0) > Duration::ZERO
            });
        }
        written
    }
}

/// What the launch lists before anything in this process can start a
/// recording: the meetings a process left `recording`, the folder each was
/// recorded into and the known audio folders ([`crate::audio_folders`]).
#[derive(Debug, Clone)]
pub(crate) struct Interrupted {
    pub(crate) meetings: Vec<Meeting>,
    /// `None` when the record cannot be read: a meeting may then be
    /// anywhere, and one whose master is not found is kept. Also `None`
    /// when the meetings cannot be listed, so no entry is forgotten as
    /// settled ([`reconcile_interrupted`]).
    pub(crate) recorded: Option<BTreeMap<Uuid, PathBuf>>,
    pub(crate) known_folders: Vec<PathBuf>,
    /// Where the record is, so the entries of the meetings settled here
    /// are forgotten.
    pub(crate) support_directory: PathBuf,
}

impl Interrupted {
    /// Lists them from `store` and `support_directory`; whatever cannot be
    /// read is logged and left out (a meeting list that cannot be read is
    /// empty, so nothing is recovered or failed).
    pub(crate) fn list(store: &Store, support_directory: &Path) -> Self {
        let listed = store.meetings_in_states(&[steno_core::MeetingStateKind::Recording]);
        // The error can name the user's folder: debug alone.
        let recorded = match &listed {
            Ok(_) => crate::audio_folders::recorded(support_directory)
                .inspect_err(|error| {
                    tracing::warn!(
                        "the recording folders could not be read; recordings not found are kept"
                    );
                    tracing::debug!(%error, "recording folders not read");
                })
                .ok(),
            // Without the rows, no entry can be told settled.
            Err(_) => None,
        };
        let meetings = listed.unwrap_or_else(|error| {
            tracing::warn!(%error, "interrupted recordings could not be listed");
            Vec::new()
        });
        let known_folders =
            crate::audio_folders::known(support_directory).unwrap_or_else(|error| {
                tracing::debug!(%error, "the known audio folders could not be read");
                Vec::new()
            });
        Interrupted {
            meetings,
            recorded,
            known_folders,
            support_directory: support_directory.to_path_buf(),
        }
    }
}

/// What the launch did with the meetings a process left `recording`.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct Reconciled {
    /// Recovered and queued.
    pub(crate) recovered: Vec<Uuid>,
    /// Their master is still written: left `recording`.
    pub(crate) live: Vec<Uuid>,
    /// A folder their master may be in could not be read: left
    /// `recording` for the next launch.
    pub(crate) unreachable: Vec<Uuid>,
    /// Left `recording` for the next launch: recovered but not saved,
    /// unrecovered but not marked failed, or, every meeting, when the
    /// settings or the assets could not be read, so no folder could be
    /// told apart.
    pub(crate) kept: Vec<Uuid>,
    /// Nothing to recover: failed with Swift's reason.
    pub(crate) failed: Vec<Uuid>,
}

/// The launch's reconciliation of `interrupted`, listed by the caller
/// before anything could start a recording here. Each master is looked for
/// as the module doc says ([`find_master`]). A meeting whose master is
/// still written, or that may be in a folder that cannot be read now, is
/// left alone; one with a master is recovered and queued through `intake`;
/// the rest are failed with [`Store::INTERRUPTED_RECORDING_REASON`] through
/// [`Store::fail_recordings`], which skips a row another process has moved
/// on meanwhile. The record forgets every listed entry whose meeting has a
/// row no longer left `recording`, once a durable checkpoint has run
/// (`forget_settled`), also when no row was left `recording`. Blocks
/// for up to [`LiveRecordingCheck::fresh_within`] when a master is fresh,
/// so the caller runs it off the main thread. Swift:
/// `MeetingStore.failInterruptedRecordings` in `AppController.launch`, the
/// failing part alone.
pub(crate) fn reconcile_interrupted(
    store: &Store,
    intake: &LocalRecordingIntake,
    interrupted: &Interrupted,
    check: &LiveRecordingCheck,
    runtime: &tokio::runtime::Handle,
) -> Reconciled {
    let reconciled = reconcile_listed(store, intake, interrupted, check, runtime);
    forget_settled(store, interrupted, &reconciled);
    reconciled
}

/// [`reconcile_interrupted`] but the forgetting.
fn reconcile_listed(
    store: &Store,
    intake: &LocalRecordingIntake,
    interrupted: &Interrupted,
    check: &LiveRecordingCheck,
    runtime: &tokio::runtime::Handle,
) -> Reconciled {
    let meetings = &interrupted.meetings;
    let mut reconciled = Reconciled::default();
    if meetings.is_empty() {
        return reconciled;
    }
    let folders = store.settings().and_then(|settings| {
        let current = file_url_path(&settings.audio_folder);
        Ok((current, other_folders(store, &interrupted.known_folders)?))
    });
    let (current, others) = match folders {
        Ok(folders) => folders,
        Err(error) => {
            // Nothing can be told apart without the folders; the next
            // launch tries again.
            tracing::warn!(%error, "interrupted recordings left for the next launch");
            reconciled.kept = meetings.iter().map(|meeting| meeting.id).collect();
            return reconciled;
        }
    };
    // A phone meeting's audio is its upload, which an asset names: no
    // master is looked for (`None`).
    let lookups: Vec<Option<Lookup>> = meetings
        .iter()
        .map(|meeting| {
            if meeting.source == MeetingSource::Phone {
                return None;
            }
            let recorded = interrupted
                .recorded
                .as_ref()
                .and_then(|recorded| recorded.get(&meeting.id).cloned());
            let deciding = distinct(recorded.into_iter().chain(current.clone()), &[]);
            let others = distinct(others.iter().cloned(), &deciding);
            Some(match find_master(&deciding, &others, meeting.id) {
                // Settings whose audio folder this build cannot read, or a
                // record that cannot be read, may name the folder a master
                // is in: only a master found is recovered.
                Lookup::Absent if current.is_none() || interrupted.recorded.is_none() => {
                    Lookup::Unreachable
                }
                lookup => lookup,
            })
        })
        .collect();
    let masters: Vec<Option<PathBuf>> = meetings
        .iter()
        .zip(&lookups)
        .map(|(meeting, lookup)| match lookup {
            Some(Lookup::Found(folder)) => Some(master_path(folder, meeting.id)),
            Some(Lookup::Absent | Lookup::Unreachable) | None => None,
        })
        .collect();
    let written = check.still_written(&masters);
    let mut unrecovered = Vec::new();
    // Warn names the meeting; an error, which can name its folder, goes
    // to debug.
    for ((meeting, lookup), written) in meetings.iter().zip(lookups).zip(written) {
        let meeting_id = meeting.id;
        let recovered = match lookup {
            None => crate::block_on(runtime, queue_upload(store, intake, meeting)),
            Some(Lookup::Found(_)) if written => {
                tracing::warn!(%meeting_id, "a recording another process is writing was left alone");
                reconciled.live.push(meeting_id);
                continue;
            }
            Some(Lookup::Found(folder)) => {
                crate::block_on(runtime, queue(intake, &folder, meeting_id, meeting.source))
            }
            Some(Lookup::Unreachable) => Err(RecoveryError::Unreachable),
            Some(Lookup::Absent) => Err(Unrecoverable::NoMaster.into()),
        };
        match recovered {
            Ok(_) => {
                tracing::warn!(%meeting_id, "an interrupted recording was recovered");
                reconciled.recovered.push(meeting_id);
            }
            Err(RecoveryError::Unrecoverable(error)) => {
                tracing::warn!(%meeting_id, "an interrupted recording could not be recovered");
                tracing::debug!(%meeting_id, %error, "unrecoverable recording");
                unrecovered.push(meeting_id);
            }
            Err(RecoveryError::Unreachable) => {
                tracing::warn!(%meeting_id, "an interrupted recording's folder cannot be read now; the next launch tries again");
                reconciled.unreachable.push(meeting_id);
            }
            Err(RecoveryError::NotSaved(error)) => {
                tracing::warn!(%meeting_id, "a recovered recording could not be saved; the next launch tries again");
                tracing::debug!(%meeting_id, %error, "recovered recording not saved");
                reconciled.kept.push(meeting_id);
            }
        }
    }
    if !unrecovered.is_empty() {
        match store.fail_recordings(
            &unrecovered,
            Store::INTERRUPTED_RECORDING_REASON,
            Utc::now(),
        ) {
            Ok(failed) => reconciled.failed = failed,
            Err(error) => {
                tracing::warn!(%error, "interrupted recordings could not be marked");
                reconciled.kept.append(&mut unrecovered);
            }
        }
    }
    reconciled
}

/// Forgets the recorded folder of every meeting in `interrupted`'s record
/// that `reconciled` did not leave `recording` and that has a row:
/// recovered, failed, adopted by an earlier launch, or saved or moved on
/// before the launch. Only after a durable checkpoint
/// ([`Store::checkpoint_durably`]): a row commits under `synchronous =
/// NORMAL`, so until then a power loss can still take it, and the entry
/// is what lets a later launch adopt its master. A checkpoint that fails
/// (another connection holds the WAL) forgets nothing; a later launch
/// tries again. An entry with no row stays for the launch's adoption
/// ([`adopt_orphans`]), since its master may still be on disk, and so
/// does one whose row cannot be read or that was recorded since the
/// launch listed the record.
fn forget_settled(store: &Store, interrupted: &Interrupted, reconciled: &Reconciled) {
    let Some(recorded) = &interrupted.recorded else {
        return;
    };
    let left = [&reconciled.live, &reconciled.unreachable, &reconciled.kept];
    let settled: Vec<Uuid> = recorded
        .keys()
        .copied()
        .filter(|id| !left.iter().any(|ids| ids.contains(id)))
        .filter(|id| store.meeting(*id).is_ok_and(|row| row.is_some()))
        .collect();
    if settled.is_empty() {
        return;
    }
    if let Err(error) = store.checkpoint_durably() {
        tracing::debug!(%error, "settled recording folders kept: the store is not checkpointed");
        return;
    }
    if let Err(error) = crate::audio_folders::forget(&interrupted.support_directory, &settled) {
        tracing::debug!(%error, "settled recording folders not forgotten");
    }
}

/// A meeting folder the launch found in an audio folder with a master, no
/// meeting row, and an entry in the record of recording folders.
#[derive(Debug, Clone)]
struct Orphan {
    meeting_id: Uuid,
    layout: RecordingLayout,
    master: PathBuf,
    format: AudioFormat,
    modified: SystemTime,
}

/// The meeting id a folder named `name` stands for: a hyphenated UUID, in
/// any case ([`RecordingLayout`] spells it in uppercase).
fn meeting_folder_id(name: &std::ffi::OsStr) -> Option<Uuid> {
    let name = name.to_str().filter(|name| name.len() == 36)?;
    Uuid::try_parse(name).ok()
}

/// The non-empty master in `layout`, the first of the formats the store
/// knows that is there, with its modification time.
fn orphan_master(layout: &RecordingLayout) -> Option<(AudioFormat, PathBuf, SystemTime)> {
    AudioFormat::ALL.iter().find_map(|&format| {
        let master = layout.master(format);
        let metadata = std::fs::metadata(&master)
            .ok()
            .filter(|metadata| metadata.is_file() && metadata.len() > 0)?;
        Some((format, master, metadata.modified().ok()?))
    })
}

/// Whether audio folder `folder` provably holds no master of meeting
/// `meeting_id`: its meeting folder is there and each master in it is
/// missing or empty, or the meeting folder is missing while `folder` lists
/// at least one entry. An empty audio folder (a volume's mount point while
/// it is not mounted), a folder or a master that cannot be read now (a
/// permission, an I/O error) may still hold one.
fn holds_no_master(folder: &Path, meeting_id: Uuid) -> bool {
    let layout = RecordingLayout::new(folder, meeting_id);
    match std::fs::metadata(&layout.directory) {
        Ok(metadata) if metadata.is_dir() => {
            AudioFormat::ALL
                .iter()
                .all(|&format| match std::fs::metadata(layout.master(format)) {
                    Ok(metadata) => metadata.is_file() && metadata.len() == 0,
                    Err(error) => error.kind() == std::io::ErrorKind::NotFound,
                })
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            std::fs::read_dir(folder).is_ok_and(|mut entries| entries.any(|entry| entry.is_ok()))
        }
        _ => false,
    }
}

/// The meeting folders in `folders` that hold a master, whose id no row
/// has (`ids`) and the record of recording folders names (`recorded`),
/// each id once, first folder first. A folder that is missing or cannot be
/// read is skipped, and only a folder whose id has no row is looked into.
/// A master that neither a row nor the record names is another install's
/// (a second computer syncing the same folder, a second database over it)
/// or written before this version kept the record (the Swift app, an
/// earlier release): it is left alone, and the launch logs one line with
/// their count and the first few ids.
fn orphans(
    folders: &[PathBuf],
    ids: &HashSet<Uuid>,
    recorded: &BTreeMap<Uuid, PathBuf>,
) -> Vec<Orphan> {
    let mut seen = HashSet::new();
    let mut found = Vec::new();
    let mut unrecorded = Vec::new();
    for folder in folders {
        let entries = match std::fs::read_dir(folder) {
            Ok(entries) => entries,
            Err(error) => {
                // The error can name the user's folder: debug alone.
                tracing::debug!(%error, "an audio folder was not searched for recordings");
                continue;
            }
        };
        for entry in entries.filter_map(Result::ok) {
            let Some(meeting_id) = meeting_folder_id(&entry.file_name()) else {
                continue;
            };
            if ids.contains(&meeting_id) || seen.contains(&meeting_id) {
                continue;
            }
            let layout = RecordingLayout::from_directory(entry.path());
            let Some((format, master, modified)) = orphan_master(&layout) else {
                continue;
            };
            seen.insert(meeting_id);
            if recorded.contains_key(&meeting_id) {
                found.push(Orphan {
                    meeting_id,
                    layout,
                    master,
                    format,
                    modified,
                });
            } else {
                unrecorded.push(meeting_id);
            }
        }
    }
    if !unrecorded.is_empty() {
        let first: Vec<String> = unrecorded
            .iter()
            .take(UNRECORDED_IDS_LOGGED)
            .map(Uuid::to_string)
            .collect();
        tracing::warn!(
            count = unrecorded.len(),
            first = %first.join(", "),
            "recordings with no meeting that this install has no record of are left alone"
        );
    }
    found
}

/// How many ids of the masters [`orphans`] leaves alone its one log line
/// names.
const UNRECORDED_IDS_LOGGED: usize = 3;

/// The meeting and asset an orphan's master gives: a CAF the Mac wrote is
/// [salvaged](salvage) as a call (two channels) or an in-person recording
/// (one) with the end reason `failed`, as an interrupted recording is; an
/// m4a or a WAV is a phone recording, with the length its container
/// declares and no end reason. It started its length before the master was
/// last modified (for a phone recording, when its upload was copied in,
/// not when the phone recorded it), and gets the source's default title,
/// the settings' template, `queued`, and the retention Keep forever: the
/// master outlived its row, so it is one the user kept, and a shorter
/// default would delete it once processed. The user can change it.
fn adopted(
    orphan: &Orphan,
    settings: &steno_core::Settings,
    zone: chrono::FixedOffset,
    now: chrono::DateTime<Utc>,
) -> Result<(Meeting, AudioAsset), RecoveryError> {
    let Orphan {
        meeting_id,
        layout,
        master,
        format,
        modified,
    } = orphan;
    let (source, asset, duration, end_reason) = match format {
        AudioFormat::Caf48kFloat32 => {
            // One channel is an in-person recording; the salvage as a call
            // fails any other count and a header it cannot read.
            let source = match CafHeader::read(master) {
                Ok(header) if header.channel_count == 1 => MeetingSource::MacInPerson,
                _ => MeetingSource::MacCall,
            };
            let result = salvage(layout, *meeting_id, source)?;
            (
                source,
                result.asset,
                result.duration,
                Some(result.end_reason),
            )
        }
        AudioFormat::M4aAac | AudioFormat::Wav16kInt16 => {
            let duration = steno_audio::SymphoniaAudioCodec::declared_duration(master)
                .inspect_err(|error| {
                    tracing::debug!(%meeting_id, %error, "an adopted recording's length is not known");
                })
                .ok()
                .flatten()
                .unwrap_or(0.0);
            let asset = AudioAsset {
                id: Uuid::new_v4(),
                meeting_id: *meeting_id,
                url: file_url(master, false),
                format: *format,
                lanes: vec![AudioLane::Mixed],
                sidecars_16k: BTreeMap::new(),
                mixdown_url: None,
                retention: AudioRetention::KeepForever,
                expires_at: None,
            };
            (MeetingSource::Phone, asset, duration, None)
        }
    };
    let ended = chrono::DateTime::<Utc>::from(*modified);
    let started_at = Duration::try_from_secs_f64(duration)
        .ok()
        .and_then(|length| chrono::Duration::from_std(length).ok())
        .and_then(|length| ended.checked_sub_signed(length))
        .unwrap_or(ended);
    let meeting = Meeting {
        id: *meeting_id,
        title: steno_pipeline::intake::default_title(source, started_at, zone),
        started_at,
        duration,
        language: None,
        source,
        calendar_event_id: None,
        tags: Vec::new(),
        state: steno_core::MeetingState::Queued,
        end_reason,
        title_origin: steno_core::TitleOrigin::Default,
        template_id: settings.default_template_id.clone(),
        summary: None,
        scratchpad: String::new(),
        llm_usage: None,
        created_at: now,
        updated_at: now,
    };
    Ok((meeting, asset))
}

/// The launch's adoption of recordings with no meeting, after the rows
/// left `recording` are reconciled: a meeting folder (named by a UUID) in
/// the settings' audio folder, a folder a recording was recorded into, a
/// known folder or a stored asset's folder, that holds a non-empty master,
/// has no row, and is named in the record of recording folders
/// ([`crate::audio_folders`]), which a recording's start and a phone
/// upload's copy write before the row. Each is adopted where it is, as a
/// `queued` meeting with that id ([`adopted`]), its meeting and asset
/// inserted in one transaction that fails when a row with the id was
/// written meanwhile, and processed
/// ([`ProcessingPipeline::enqueue_new`](steno_pipeline::ProcessingPipeline::enqueue_new)).
/// Its entry stays: the insert commits under `synchronous = NORMAL`, so a
/// power loss can still take it, and a later launch forgets the entry
/// once its row is durable (`forget_settled`). An entry with no row is
/// forgotten here only when its folder provably holds no master
/// (`holds_no_master`); one whose folder is empty, missing or unreadable
/// stays, and is logged at debug. A master modified within
/// [`LiveRecordingCheck::fresh_within`] is left for the next launch:
/// another process (a phone upload still being admitted, the Swift app) may
/// be writing it. One that cannot be read now, or whose rows cannot be
/// saved, is left for the next launch too. A master the record does not
/// name is left alone ([`orphans`]), and nothing in the handover inbox is
/// adopted: it holds files, not meeting folders. Returns the ids adopted.
/// Rust only: Swift had no recovery.
pub(crate) fn adopt_orphans(
    store: &Store,
    pipeline: &steno_pipeline::ProcessingPipeline,
    interrupted: &Interrupted,
    check: &LiveRecordingCheck,
    zone: chrono::FixedOffset,
    runtime: &tokio::runtime::Handle,
) -> Vec<Uuid> {
    // `None`: the record or the rows left `recording` could not be read,
    // so no master can be told this install's.
    let Some(recorded) = &interrupted.recorded else {
        return Vec::new();
    };
    let listed = store
        .settings()
        .and_then(|settings| Ok((settings, store.meeting_ids()?)));
    let (settings, ids) = match listed {
        Ok((settings, ids)) => (settings, ids.into_iter().collect::<HashSet<_>>()),
        Err(error) => {
            tracing::warn!(%error, "recordings with no meeting were not looked for");
            return Vec::new();
        }
    };
    let others = other_folders(store, &interrupted.known_folders).unwrap_or_else(|error| {
        tracing::debug!(%error, "the stored assets' folders were not listed");
        interrupted.known_folders.clone()
    });
    let folders = distinct(
        file_url_path(&settings.audio_folder)
            .into_iter()
            .chain(recorded.values().cloned())
            .chain(others),
        &[],
    );
    let found = orphans(&folders, &ids, recorded);
    let _entered = runtime.enter();
    let mut adopted_ids = Vec::new();
    for orphan in &found {
        let meeting_id = orphan.meeting_id;
        if check.is_fresh(&orphan.master) {
            tracing::warn!(%meeting_id, "a recording with no meeting is still written; the next launch looks again");
            continue;
        }
        let rows = adopted(orphan, &settings, zone, (pipeline.dependencies().now)());
        let saved = rows.and_then(|(meeting, asset)| {
            pipeline
                .enqueue_new(&meeting, &asset)
                .map_err(|failure| RecoveryError::NotSaved(failure.into()))
        });
        // Warn names the meeting; an error, which can name its folder,
        // goes to debug.
        match saved {
            Ok(()) => {
                tracing::warn!(%meeting_id, "a recording with no meeting was recovered");
                adopted_ids.push(meeting_id);
            }
            Err(error) => {
                tracing::warn!(%meeting_id, "a recording with no meeting could not be recovered; it stays on disk");
                tracing::debug!(%meeting_id, %error, "recording with no meeting not recovered");
            }
        }
    }
    // No master to lose: forgotten whether or not the store is durable.
    let mut gone = Vec::new();
    for (&meeting_id, folder) in recorded {
        if ids.contains(&meeting_id) || found.iter().any(|orphan| orphan.meeting_id == meeting_id) {
            continue;
        }
        if holds_no_master(folder, meeting_id) {
            gone.push(meeting_id);
        } else {
            tracing::debug!(%meeting_id, "a recording with no meeting whose folder cannot be told empty is kept for the next launch");
        }
    }
    if !gone.is_empty()
        && let Err(error) = crate::audio_folders::forget(&interrupted.support_directory, &gone)
    {
        tracing::debug!(%error, "recording folders with no master not forgotten");
    }
    adopted_ids
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use steno_audio::writer::{CafStreamWriter, RecordingWriter, RecordingWriting as _, WavFile};
    use steno_audio::{FRAME_SIZE, SAMPLE_RATE};
    use steno_core::testing::WriteLockHold;
    use steno_core::{MeetingState, MeetingStateKind};

    use super::*;
    use crate::pipeline::CurrentPipeline;
    use crate::testing::{
        an_hour_later, current_pipeline, fake_dependencies, temp_store, write_frames,
    };

    struct Harness {
        dir: tempfile::TempDir,
        store: Arc<Store>,
        pipeline: Arc<CurrentPipeline>,
    }

    impl Harness {
        fn new() -> Self {
            let (dir, store) = temp_store();
            let mut settings = store.settings().unwrap();
            settings.audio_folder = file_url(&dir.path().join("audio"), true);
            store.save_settings(&settings).unwrap();
            let pipeline = current_pipeline(fake_dependencies(&store, "fake-engine"));
            Harness {
                dir,
                store,
                pipeline,
            }
        }

        fn audio_folder(&self) -> PathBuf {
            self.dir.path().join("audio")
        }

        fn intake(&self) -> LocalRecordingIntake {
            LocalRecordingIntake::over(
                self.store.clone(),
                self.pipeline.current(),
                chrono::FixedOffset::east_opt(0).unwrap(),
            )
        }

        /// A `recording` row as the recorder's `begin` writes it.
        fn begin(&self, source: MeetingSource) -> Meeting {
            self.intake()
                .begin(Uuid::new_v4(), source, None, None, &[], Utc::now())
                .unwrap()
        }

        /// The production writer for `meeting`, as its capture opens it.
        fn writer(&self, meeting: &Meeting) -> RecordingWriter {
            let layout = RecordingLayout::new(&self.audio_folder(), meeting.id);
            RecordingWriter::new(&layout, &lanes(meeting.source), false).unwrap()
        }

        /// Where the known audio folders are listed.
        fn support_directory(&self) -> PathBuf {
            self.dir.path().join("support")
        }

        /// `meetings` as the launch lists them, with the recorded and the
        /// known folders.
        fn interrupted(&self, meetings: &[Meeting]) -> Interrupted {
            Interrupted {
                meetings: meetings.to_vec(),
                ..Interrupted::list(&self.store, &self.support_directory())
            }
        }

        /// Records `meeting`'s folder as the recorder does before its row.
        fn record(&self, meeting: &Meeting, folder: &Path) {
            crate::audio_folders::record(&self.support_directory(), meeting.id, folder).unwrap();
        }

        /// The folder recorded for `meeting`, if any.
        fn recorded(&self, meeting: &Meeting) -> Option<PathBuf> {
            crate::audio_folders::recorded(&self.support_directory())
                .unwrap()
                .remove(&meeting.id)
        }

        fn reconcile(&self, meetings: &[Meeting], check: &LiveRecordingCheck) -> Reconciled {
            reconcile_interrupted(
                &self.store,
                &self.intake(),
                &self.interrupted(meetings),
                check,
                &tokio::runtime::Handle::current(),
            )
        }

        /// Points the settings at `folder`, as a change in Settings does,
        /// without remembering the folder left.
        fn set_folder(&self, folder: &Path) {
            let mut settings = self.store.settings().unwrap();
            settings.audio_folder = file_url(folder, true);
            self.store.save_settings(&settings).unwrap();
        }

        fn state(&self, meeting: &Meeting) -> MeetingState {
            self.store.meeting(meeting.id).unwrap().unwrap().state
        }
    }

    /// A check over a clock that starts at the real now and moves only
    /// when the check waits: each wait moves it on by the time asked for,
    /// then runs `during` with the time waited in all.
    fn clocked(during: impl Fn(Duration) + Send + Sync + 'static) -> LiveRecordingCheck {
        let start = SystemTime::now();
        let waited = Arc::new(Mutex::new(Duration::ZERO));
        let now = {
            let waited = waited.clone();
            Arc::new(move || start + *waited.lock().unwrap())
        };
        let wait = Arc::new(move |duration| {
            let total = {
                let mut waited = waited.lock().unwrap();
                *waited += duration;
                *waited
            };
            during(total);
        });
        LiveRecordingCheck {
            now,
            wait,
            ..LiveRecordingCheck::default()
        }
    }

    /// A call recording killed mid-meeting: the writer is dropped without
    /// `finish`, as a kill leaves it (the master's size -1, the sidecars'
    /// headers unpatched). The launch queues it with the asset its
    /// capture would have handed over and the `failed` end reason, its
    /// sidecars read back, and the pipeline processes it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_launch_recovers_a_killed_recording_and_processes_it() {
        let harness = Harness::new();
        let meeting = harness.begin(MeetingSource::MacCall);
        let mut writer = harness.writer(&meeting);
        write_frames(&mut writer, 150);
        drop(writer);

        reconcile_at_launch(&harness, std::slice::from_ref(&meeting), &an_hour_later());

        let queued = harness.store.meeting(meeting.id).unwrap().unwrap();
        assert_ne!(queued.state.kind(), MeetingStateKind::Recording);
        assert!(
            !matches!(queued.state, MeetingState::Failed { .. }),
            "{:?}",
            queued.state
        );
        assert_eq!(queued.end_reason, Some(RecordingEndReason::Failed));
        assert_eq!(queued.duration, 1.5);
        let asset = harness.store.asset(meeting.id).unwrap().unwrap();
        let master = master_path(&harness.audio_folder(), meeting.id);
        assert_eq!(asset.url, file_url(&master, false));
        assert_eq!(asset.format, AudioFormat::Caf48kFloat32);
        assert_eq!(asset.lanes, vec![AudioLane::Mic, AudioLane::System]);
        assert_eq!(
            asset.retention,
            harness.store.settings().unwrap().default_retention
        );
        assert_eq!(
            asset.sidecars_16k.keys().copied().collect::<Vec<_>>(),
            [AudioLane::Mic, AudioLane::System]
        );
        for sidecar in asset.sidecars_16k.values() {
            let samples = WavFile::read_16k_mono(&file_url_path(sidecar).unwrap()).unwrap();
            assert_eq!(samples.len(), 150 * FRAME_SIZE / 3);
        }
        harness.pipeline.current().wait_until_idle().await;
        assert_eq!(harness.state(&meeting), MeetingState::Ready);
    }

    /// The launch's background half over `harness`'s pipeline.
    fn reconcile_at_launch(
        harness: &Harness,
        meetings: &[Meeting],
        check: &LiveRecordingCheck,
    ) -> Vec<Uuid> {
        crate::app::reconcile_at_launch(
            &harness.store,
            &harness.pipeline,
            &harness.interrupted(meetings),
            check,
            chrono::FixedOffset::east_opt(0).unwrap(),
            &tokio::runtime::Handle::current(),
        )
    }

    /// A kill inside a write leaves part of a frame: whole frames are
    /// recovered. A master with a header alone, a master whose channels
    /// are not the source's lanes, and no master at all fail the row with
    /// Swift's reason, as before.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn whole_frames_are_recovered_and_a_master_without_audio_fails_as_before() {
        let harness = Harness::new();
        let truncated = harness.begin(MeetingSource::MacInPerson);
        let mut writer = harness.writer(&truncated);
        write_frames(&mut writer, 10);
        drop(writer);
        let master = master_path(&harness.audio_folder(), truncated.id);
        let cut = CafStreamWriter::HEADER_SIZE + (10 * FRAME_SIZE - 1) * 4 + 2;
        std::fs::OpenOptions::new()
            .write(true)
            .open(&master)
            .unwrap()
            .set_len(cut as u64)
            .unwrap();

        let header_only = harness.begin(MeetingSource::MacInPerson);
        drop(harness.writer(&header_only));
        let wrong_channels = harness.begin(MeetingSource::MacCall);
        let layout = RecordingLayout::new(&harness.audio_folder(), wrong_channels.id);
        let mut writer = RecordingWriter::new(&layout, &[AudioLane::Mixed], false).unwrap();
        write_frames(&mut writer, 10);
        drop(writer);
        let no_master = harness.begin(MeetingSource::MacCall);

        let meetings = [
            truncated.clone(),
            header_only.clone(),
            wrong_channels.clone(),
            no_master.clone(),
        ];
        let reconciled = harness.reconcile(&meetings, &an_hour_later());
        assert_eq!(reconciled.recovered, [truncated.id]);
        assert_eq!(
            reconciled.failed,
            [header_only.id, wrong_channels.id, no_master.id]
        );
        let duration = harness
            .store
            .meeting(truncated.id)
            .unwrap()
            .unwrap()
            .duration;
        assert_eq!(duration, 4_799.0 / SAMPLE_RATE);
        for meeting in [&header_only, &wrong_channels, &no_master] {
            assert_eq!(
                harness.state(meeting),
                MeetingState::Failed {
                    reason: Store::INTERRUPTED_RECORDING_REASON.to_owned()
                }
            );
            assert!(harness.store.asset(meeting.id).unwrap().is_none());
        }
        harness.pipeline.current().wait_until_idle().await;
    }

    /// A master another process is still writing (here a writer that
    /// appends while the check waits) leaves its row `recording`, with no
    /// asset. At the next launch the writer has stopped: the master is
    /// watched until it is `fresh_within` old, no longer, and recovered.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_master_that_is_still_written_is_left_recording() {
        let harness = Harness::new();
        let meeting = harness.begin(MeetingSource::MacInPerson);
        let mut writer = harness.writer(&meeting);
        write_frames(&mut writer, 10);
        let writer = Arc::new(Mutex::new(writer));
        let appending = {
            let writer = writer.clone();
            clocked(move |_| write_frames(&mut writer.lock().unwrap(), 1))
        };
        let reconciled = harness.reconcile(std::slice::from_ref(&meeting), &appending);
        assert_eq!(reconciled.live, [meeting.id]);
        assert_eq!(reconciled.failed, Vec::<Uuid>::new());
        assert_eq!(harness.state(&meeting), MeetingState::Recording);
        assert!(harness.store.asset(meeting.id).unwrap().is_none());

        drop(writer);
        let waited = Arc::new(Mutex::new(Duration::ZERO));
        let still = {
            let waited = waited.clone();
            clocked(move |total| *waited.lock().unwrap() = total)
        };
        let reconciled = harness.reconcile(std::slice::from_ref(&meeting), &still);
        assert_eq!(reconciled.recovered, [meeting.id]);
        let waited = *waited.lock().unwrap();
        let fresh_within = LiveRecordingCheck::default().fresh_within;
        assert!(
            waited > fresh_within / 2 && waited <= fresh_within,
            "{waited:?}"
        );
        assert_eq!(
            harness.store.meeting(meeting.id).unwrap().unwrap().duration,
            5_280.0 / SAMPLE_RATE
        );
        harness.pipeline.current().wait_until_idle().await;
    }

    /// A writer that stalls for three seconds (a device rebuild's
    /// backoff) and then goes on is still its owner's: the check waits
    /// past the stall and sees the master change, the row stays
    /// `recording`, and the owner's own stop saves the whole recording.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_stalled_writer_keeps_its_recording() {
        let harness = Harness::new();
        let meeting = harness.begin(MeetingSource::MacInPerson);
        let mut writer = harness.writer(&meeting);
        write_frames(&mut writer, 10);
        let writer = Arc::new(Mutex::new(writer));
        let stalled = {
            let writer = writer.clone();
            clocked(move |total| {
                if total >= Duration::from_secs(3) {
                    write_frames(&mut writer.lock().unwrap(), 1);
                }
            })
        };
        let reconciled = harness.reconcile(std::slice::from_ref(&meeting), &stalled);
        assert_eq!(reconciled.live, [meeting.id]);
        assert_eq!(harness.state(&meeting), MeetingState::Recording);

        drop(stalled);
        let mut writer = Arc::into_inner(writer).unwrap().into_inner().unwrap();
        write_frames(&mut writer, 89);
        let files = writer.finish().unwrap();
        let completed = harness
            .intake()
            .complete(meeting.id, finished(&meeting, &files), None)
            .await
            .unwrap();
        assert_eq!(completed.duration, 1.0, "100 frames");
        assert_eq!(completed.end_reason, Some(RecordingEndReason::Manual));
        harness.pipeline.current().wait_until_idle().await;
    }

    /// The handover a finished in-person capture makes of `files`.
    fn finished(meeting: &Meeting, files: &steno_audio::writer::RecordingFiles) -> RecordingResult {
        RecordingResult {
            asset: AudioAsset {
                id: Uuid::new_v4(),
                meeting_id: meeting.id,
                url: file_url(&files.master, false),
                format: AudioFormat::Caf48kFloat32,
                lanes: vec![AudioLane::Mixed],
                sidecars_16k: std::collections::BTreeMap::new(),
                mixdown_url: None,
                retention: AudioRetention::KeepForever,
                expires_at: None,
            },
            duration: files.duration,
            end_reason: RecordingEndReason::Manual,
        }
    }

    /// The master is looked for in every folder a recording was written
    /// to: after the user picks another folder mid-recording, a master in
    /// the old one (where an earlier recording's asset is) is recovered
    /// from there. A row whose master may be in a folder that is missing
    /// now (an unmounted volume) stays `recording`; one whose folders all
    /// exist and hold no master fails as before.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_master_is_looked_for_in_every_audio_folder() {
        let harness = Harness::new();
        let earlier = harness.begin(MeetingSource::MacInPerson);
        let mut writer = harness.writer(&earlier);
        write_frames(&mut writer, 10);
        let files = writer.finish().unwrap();
        harness
            .intake()
            .complete(earlier.id, finished(&earlier, &files), None)
            .await
            .unwrap();
        let moved = harness.begin(MeetingSource::MacInPerson);
        let mut writer = harness.writer(&moved);
        write_frames(&mut writer, 100);
        drop(writer);
        let set_folder = |folder: &Path| harness.set_folder(folder);
        let elsewhere = harness.dir.path().join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        set_folder(&elsewhere);

        let reconciled = harness.reconcile(std::slice::from_ref(&moved), &an_hour_later());
        assert_eq!(reconciled.recovered, [moved.id]);
        assert_eq!(
            harness.store.asset(moved.id).unwrap().unwrap().url,
            file_url(&master_path(&harness.audio_folder(), moved.id), false)
        );
        assert_eq!(
            harness.store.meeting(moved.id).unwrap().unwrap().duration,
            1.0
        );

        // No master anywhere, and the folder the settings name is not
        // there: the master may be on it.
        let unmounted = harness.begin(MeetingSource::MacInPerson);
        set_folder(&harness.dir.path().join("unmounted"));
        let reconciled = harness.reconcile(std::slice::from_ref(&unmounted), &an_hour_later());
        assert_eq!(reconciled.unreachable, [unmounted.id]);
        assert_eq!(reconciled.failed, Vec::<Uuid>::new());
        assert_eq!(harness.state(&unmounted), MeetingState::Recording);

        // Every folder there, empty of it: failed with Swift's reason.
        set_folder(&elsewhere);
        let reconciled = harness.reconcile(std::slice::from_ref(&unmounted), &an_hour_later());
        assert_eq!(reconciled.failed, [unmounted.id]);
        assert_eq!(
            harness.state(&unmounted),
            MeetingState::Failed {
                reason: Store::INTERRUPTED_RECORDING_REASON.to_owned()
            }
        );
        harness.pipeline.current().wait_until_idle().await;
    }

    /// A stop whose commit stays busy leaves the meeting `recording`, and
    /// the next launch recovers it from the master on disk.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_recording_whose_commit_failed_is_recovered_at_the_next_launch() {
        let harness = Harness::new();
        harness
            .store
            .read(|connection| Ok(connection.busy_timeout(Duration::from_millis(20))?))
            .unwrap();
        let meeting = harness.begin(MeetingSource::MacInPerson);
        let mut writer = harness.writer(&meeting);
        write_frames(&mut writer, 20);
        let files = writer.finish().unwrap();
        let result = finished(&meeting, &files);

        let hold = WriteLockHold::new(&harness.dir.path().join("steno.sqlite"));
        let error = harness
            .intake()
            .complete(meeting.id, result, None)
            .await
            .unwrap_err();
        assert!(error.is_busy(), "{error}");
        drop(hold);
        assert_eq!(harness.state(&meeting), MeetingState::Recording);

        let reconciled = harness.reconcile(std::slice::from_ref(&meeting), &an_hour_later());
        assert_eq!(reconciled.recovered, [meeting.id]);
        let recovered = harness.store.meeting(meeting.id).unwrap().unwrap();
        assert_eq!(recovered.duration, 0.2);
        harness.pipeline.current().wait_until_idle().await;
        assert_eq!(harness.state(&meeting), MeetingState::Ready);
    }

    /// The first recording in a folder the user just chose, which no asset
    /// names yet, then a change of the folder while it ran: the recorder
    /// recorded the folder before the row ([`crate::audio_folders`]), so
    /// the launch finds the master there, though the settings name another
    /// folder that is there and empty, and forgets the entry once the
    /// meeting is queued.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_master_in_the_folder_the_recorder_recorded_is_found_after_the_folder_changed() {
        let harness = Harness::new();
        let meeting = harness.begin(MeetingSource::MacCall);
        harness.record(&meeting, &harness.audio_folder());
        let mut writer = harness.writer(&meeting);
        write_frames(&mut writer, 100);
        drop(writer);
        let elsewhere = harness.dir.path().join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        harness.set_folder(&elsewhere);

        let reconciled = harness.reconcile(std::slice::from_ref(&meeting), &an_hour_later());
        assert_eq!(reconciled.recovered, [meeting.id]);
        assert_eq!(
            harness.store.asset(meeting.id).unwrap().unwrap().url,
            file_url(&master_path(&harness.audio_folder(), meeting.id), false)
        );
        assert_eq!(harness.recorded(&meeting), None);
        harness.pipeline.current().wait_until_idle().await;
    }

    /// A launch with no row left `recording` still forgets the entries of
    /// meetings that moved on: the reconcile one failed since, and the
    /// adoption each one with no row whose folder provably holds no master
    /// (a recording that never wrote one): a meeting folder with no
    /// master, and a meeting folder missing from an audio folder that
    /// lists others. The reconcile keeps those for the adoption, since a
    /// master could have been there.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_launch_with_no_row_left_recording_forgets_the_settled_entries() {
        let harness = Harness::new();
        let failed = harness.begin(MeetingSource::MacCall);
        harness.record(&failed, &harness.audio_folder());
        harness.intake().fail(failed.id, "refused").unwrap();
        let (empty, missing) = (Uuid::new_v4(), Uuid::new_v4());
        std::fs::create_dir_all(RecordingLayout::new(&harness.audio_folder(), empty).directory)
            .unwrap();
        for id in [empty, missing] {
            crate::audio_folders::record(&harness.support_directory(), id, &harness.audio_folder())
                .unwrap();
        }

        assert_eq!(
            harness.reconcile(&[], &an_hour_later()),
            Reconciled::default()
        );
        assert_eq!(
            crate::audio_folders::recorded(&harness.support_directory()).unwrap(),
            BTreeMap::from([
                (empty, harness.audio_folder()),
                (missing, harness.audio_folder())
            ])
        );
        assert_eq!(
            reconcile_at_launch(&harness, &[], &an_hour_later()),
            Vec::<Uuid>::new()
        );
        assert!(
            crate::audio_folders::recorded(&harness.support_directory())
                .unwrap()
                .is_empty()
        );
    }

    /// A launch that cannot list the rows left `recording` forgets no
    /// entry: without the rows, none can be told settled.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_launch_that_cannot_list_the_meetings_forgets_no_entry() {
        let harness = Harness::new();
        let meeting = harness.begin(MeetingSource::MacCall);
        harness.record(&meeting, &harness.audio_folder());
        harness
            .store
            .write(|transaction| {
                Ok(transaction.execute_batch("ALTER TABLE meeting RENAME TO gone")?)
            })
            .unwrap();

        let interrupted = Interrupted::list(&harness.store, &harness.support_directory());
        assert_eq!(interrupted.meetings, Vec::<Meeting>::new());
        assert!(interrupted.recorded.is_none());
        reconcile_interrupted(
            &harness.store,
            &harness.intake(),
            &interrupted,
            &an_hour_later(),
            &tokio::runtime::Handle::current(),
        );
        assert_eq!(harness.recorded(&meeting), Some(harness.audio_folder()));
    }

    /// An entry recorded after the launch listed the record (a recording
    /// started while the reconcile ran) stays; a listed one that settled
    /// goes.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_entry_recorded_during_the_reconcile_stays() {
        let harness = Harness::new();
        let failed = harness.begin(MeetingSource::MacCall);
        harness.record(&failed, &harness.audio_folder());
        harness.intake().fail(failed.id, "refused").unwrap();
        let interrupted = harness.interrupted(&[]);
        let started = harness.begin(MeetingSource::MacCall);
        harness.record(&started, &harness.audio_folder());

        reconcile_interrupted(
            &harness.store,
            &harness.intake(),
            &interrupted,
            &an_hour_later(),
            &tokio::runtime::Handle::current(),
        );
        assert_eq!(harness.recorded(&failed), None);
        assert_eq!(harness.recorded(&started), Some(harness.audio_folder()));
    }

    /// A phone row left `recording` (this app admits a phone recording
    /// `queued`, so another process wrote it) is queued with the asset
    /// that names its upload, kept as stored with the upload where it is,
    /// and processes; one with no asset fails as before.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_phone_row_left_recording_is_queued_with_its_upload() {
        let harness = Harness::new();
        let uploaded = harness.begin(MeetingSource::Phone);
        let layout = RecordingLayout::new(&harness.audio_folder(), uploaded.id);
        std::fs::create_dir_all(&layout.directory).unwrap();
        let upload = layout.master(AudioFormat::M4aAac);
        std::fs::copy(audio_fixture("tone-440-44k1-500ms.m4a"), &upload).unwrap();
        let asset = AudioAsset {
            id: Uuid::new_v4(),
            meeting_id: uploaded.id,
            url: file_url(&upload, false),
            format: AudioFormat::M4aAac,
            lanes: vec![AudioLane::Mixed],
            sidecars_16k: BTreeMap::new(),
            mixdown_url: None,
            retention: AudioRetention::KeepDays(30),
            expires_at: None,
        };
        harness
            .store
            .save_meeting_with_asset(&uploaded, &asset)
            .unwrap();
        let no_upload = harness.begin(MeetingSource::Phone);

        let meetings = [uploaded.clone(), no_upload.clone()];
        let reconciled = harness.reconcile(&meetings, &an_hour_later());
        assert_eq!(reconciled.recovered, [uploaded.id]);
        assert_eq!(reconciled.failed, [no_upload.id]);
        harness.pipeline.current().wait_until_idle().await;
        assert_eq!(harness.state(&uploaded), MeetingState::Ready);
        let stored = harness.store.asset(uploaded.id).unwrap().unwrap();
        assert_eq!(
            (stored.id, &stored.url, stored.retention),
            (asset.id, &asset.url, asset.retention)
        );
        assert!(upload.is_file());
        assert_eq!(
            harness.state(&no_upload),
            MeetingState::Failed {
                reason: Store::INTERRUPTED_RECORDING_REASON.to_owned()
            }
        );
    }

    /// A folder that is gone for good keeps a row only when it decides the
    /// meeting: a recording whose recorded folder is missing (an unmounted
    /// volume) stays `recording`, with its entry, while a known folder
    /// retired since does not keep a row with no master anywhere, which
    /// fails as before.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn only_a_deciding_folder_that_is_gone_keeps_the_row() {
        let harness = Harness::new();
        std::fs::create_dir_all(harness.audio_folder()).unwrap();
        let retired = harness.dir.path().join("retired");
        std::fs::create_dir_all(&retired).unwrap();
        crate::audio_folders::remember(&harness.support_directory(), &retired).unwrap();
        std::fs::remove_dir_all(&retired).unwrap();
        let absent = harness.begin(MeetingSource::MacCall);
        let unmounted = harness.begin(MeetingSource::MacCall);
        let volume = harness.dir.path().join("volume");
        harness.record(&unmounted, &volume);

        let meetings = [absent.clone(), unmounted.clone()];
        let reconciled = harness.reconcile(&meetings, &an_hour_later());
        assert_eq!(reconciled.failed, [absent.id]);
        assert_eq!(reconciled.unreachable, [unmounted.id]);
        assert_eq!(harness.state(&unmounted), MeetingState::Recording);
        assert_eq!(harness.recorded(&unmounted), Some(volume));
    }

    /// Makes `path` unreadable to this user; `false` when it stays
    /// readable (a test run as root), and the caller skips.
    #[cfg(unix)]
    fn lock_out(path: &Path, readable: impl Fn() -> bool) -> bool {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o000)).unwrap();
        if readable() {
            set_mode(path, 0o755);
            return false;
        }
        true
    }

    #[cfg(unix)]
    fn set_mode(path: &Path, mode: u32) {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    }

    /// A folder the launch may not read yet (a permission not granted) and
    /// then a master it may not read keep the row `recording`, never
    /// failed; once both can be read, a later launch recovers it.
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_unreadable_folder_or_master_keeps_the_row_for_a_later_launch() {
        let harness = Harness::new();
        let meeting = harness.begin(MeetingSource::MacInPerson);
        let mut writer = harness.writer(&meeting);
        write_frames(&mut writer, 10);
        drop(writer);
        let folder = harness.audio_folder();
        if !lock_out(&folder, || std::fs::read_dir(&folder).is_ok()) {
            return;
        }
        let reconciled = harness.reconcile(std::slice::from_ref(&meeting), &an_hour_later());
        set_mode(&folder, 0o755);
        assert_eq!(reconciled.unreachable, [meeting.id]);
        assert_eq!(harness.state(&meeting), MeetingState::Recording);

        let master = master_path(&folder, meeting.id);
        assert!(lock_out(&master, || std::fs::File::open(&master).is_ok()));
        let reconciled = harness.reconcile(std::slice::from_ref(&meeting), &an_hour_later());
        set_mode(&master, 0o644);
        assert_eq!(reconciled.unreachable, [meeting.id]);
        assert_eq!(reconciled.failed, Vec::<Uuid>::new());
        assert_eq!(harness.state(&meeting), MeetingState::Recording);

        let reconciled = harness.reconcile(std::slice::from_ref(&meeting), &an_hour_later());
        assert_eq!(reconciled.recovered, [meeting.id]);
        harness.pipeline.current().wait_until_idle().await;
    }

    /// A record the launch cannot read may name the folder a master is in:
    /// a recording whose master is in the folder it started in, after the
    /// setting moved to another (there and empty), is kept, never failed;
    /// once the record can be read, a later launch recovers it.
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_unreadable_record_keeps_a_row_whose_master_is_not_found() {
        let harness = Harness::new();
        let meeting = harness.begin(MeetingSource::MacCall);
        harness.record(&meeting, &harness.audio_folder());
        let mut writer = harness.writer(&meeting);
        write_frames(&mut writer, 100);
        drop(writer);
        let elsewhere = harness.dir.path().join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        harness.set_folder(&elsewhere);
        let record = harness
            .support_directory()
            .join(crate::audio_folders::RECORDED_FILE);
        if !lock_out(&record, || std::fs::read(&record).is_ok()) {
            return;
        }
        let reconciled = harness.reconcile(std::slice::from_ref(&meeting), &an_hour_later());
        set_mode(&record, 0o644);
        assert_eq!(reconciled.unreachable, [meeting.id]);
        assert_eq!(harness.state(&meeting), MeetingState::Recording);

        let reconciled = harness.reconcile(std::slice::from_ref(&meeting), &an_hour_later());
        assert_eq!(reconciled.recovered, [meeting.id]);
        harness.pipeline.current().wait_until_idle().await;
    }

    /// Settings whose audio folder is not a file URL may name the folder
    /// the master is in: with every other folder there and empty of it,
    /// the row is kept, not failed. Once the settings name a folder, the
    /// same row fails with Swift's reason.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_audio_folder_that_is_not_a_file_url_keeps_the_row() {
        let harness = Harness::new();
        let elsewhere = harness.dir.path().join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        crate::audio_folders::remember(&harness.support_directory(), &elsewhere).unwrap();
        let meeting = harness.begin(MeetingSource::MacInPerson);
        let mut settings = harness.store.settings().unwrap();
        settings.audio_folder = "smb://server/recordings".to_owned();
        harness.store.save_settings(&settings).unwrap();

        let reconciled = harness.reconcile(std::slice::from_ref(&meeting), &an_hour_later());
        assert_eq!(reconciled.unreachable, [meeting.id]);
        assert_eq!(harness.state(&meeting), MeetingState::Recording);

        harness.set_folder(&elsewhere);
        let reconciled = harness.reconcile(std::slice::from_ref(&meeting), &an_hour_later());
        assert_eq!(reconciled.failed, [meeting.id]);
    }

    /// A master whose modification time is ahead of the clock (here the
    /// clock reads an hour early) stays young however long it is watched:
    /// after `fresh_within` of waiting it counts as still written, and the
    /// row stays `recording`.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_master_ahead_of_the_clock_is_left_recording() {
        let harness = Harness::new();
        let meeting = harness.begin(MeetingSource::MacInPerson);
        let mut writer = harness.writer(&meeting);
        write_frames(&mut writer, 10);
        drop(writer);
        let start = SystemTime::now() - Duration::from_secs(3_600);
        let waited = Arc::new(Mutex::new(Duration::ZERO));
        let check = LiveRecordingCheck {
            now: {
                let waited = waited.clone();
                Arc::new(move || start + *waited.lock().unwrap())
            },
            wait: {
                let waited = waited.clone();
                Arc::new(move |duration| *waited.lock().unwrap() += duration)
            },
            ..LiveRecordingCheck::default()
        };
        let reconciled = harness.reconcile(std::slice::from_ref(&meeting), &check);
        assert_eq!(reconciled.live, [meeting.id]);
        assert_eq!(*waited.lock().unwrap(), check.fresh_within);
        assert_eq!(harness.state(&meeting), MeetingState::Recording);
    }

    /// A master `recording.<ext>` with no row, as `write` leaves it in a
    /// new meeting folder under the audio folder, its folder recorded first
    /// as the recorder and the phone intake record it; returns its id.
    fn orphan(harness: &Harness, write: impl FnOnce(&RecordingLayout)) -> Uuid {
        let meeting_id = Uuid::new_v4();
        crate::audio_folders::record(
            &harness.support_directory(),
            meeting_id,
            &harness.audio_folder(),
        )
        .unwrap();
        let layout = RecordingLayout::new(&harness.audio_folder(), meeting_id);
        std::fs::create_dir_all(&layout.directory).unwrap();
        write(&layout);
        meeting_id
    }

    /// The audio fixture `name` from `Tests/Fixtures/audio`.
    fn audio_fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../Tests/Fixtures/audio")
            .join(name)
    }

    /// A Mac master of `lanes` with `frames` frames, its writer dropped
    /// as a crash leaves it.
    fn mac_orphan(harness: &Harness, lanes: &[AudioLane], frames: usize) -> Uuid {
        orphan(harness, |layout| {
            let mut writer = RecordingWriter::new(layout, lanes, false).unwrap();
            write_frames(&mut writer, frames);
            drop(writer);
        })
    }

    /// The fixture `name` copied in as a phone's master in `format`.
    fn phone_orphan(harness: &Harness, name: &str, format: AudioFormat) -> Uuid {
        orphan(harness, |layout| {
            std::fs::copy(audio_fixture(name), layout.master(format)).unwrap();
        })
    }

    /// Every file under `folder`, recursively, with its bytes.
    fn files_in(folder: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
        let mut files = BTreeMap::new();
        for entry in std::fs::read_dir(folder).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                files.extend(files_in(&path));
            } else {
                files.insert(path.clone(), std::fs::read(&path).unwrap());
            }
        }
        files
    }

    fn sorted(mut ids: Vec<Uuid>) -> Vec<Uuid> {
        ids.sort();
        ids
    }

    /// Asserts that `meeting` started its length before the master of
    /// `format` in its folder was last modified, to the millisecond.
    fn assert_started_before_the_master_ended(
        harness: &Harness,
        meeting: &Meeting,
        format: AudioFormat,
    ) {
        let master = RecordingLayout::new(&harness.audio_folder(), meeting.id).master(format);
        let modified =
            chrono::DateTime::<Utc>::from(std::fs::metadata(master).unwrap().modified().unwrap());
        let ended = meeting.started_at
            + chrono::Duration::from_std(Duration::from_secs_f64(meeting.duration)).unwrap();
        assert!(
            (ended - modified).num_milliseconds().abs() <= 1,
            "{ended} against {modified}"
        );
    }

    /// A call master and an in-person master with no row (a row a power
    /// loss took before it was durable, a meeting deleted while it
    /// recorded) are adopted at launch where they are: a meeting with the
    /// folder's id, the source the channels say, the master's length, the
    /// end reason `failed`, the default title, started that long before
    /// the master was last modified, its asset rebuilt with the sidecars,
    /// and processed. Their entries stay until the second launch, which
    /// adopts nothing and forgets them.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_mac_master_with_no_meeting_is_adopted_and_processed() {
        let harness = Harness::new();
        let call = mac_orphan(&harness, &[AudioLane::Mic, AudioLane::System], 150);
        let in_person = mac_orphan(&harness, &[AudioLane::Mixed], 50);

        let adopted = reconcile_at_launch(&harness, &[], &an_hour_later());
        assert_eq!(sorted(adopted), sorted(vec![call, in_person]));
        let zone = chrono::FixedOffset::east_opt(0).unwrap();
        for (meeting_id, source, lanes, duration) in [
            (
                call,
                MeetingSource::MacCall,
                vec![AudioLane::Mic, AudioLane::System],
                1.5,
            ),
            (
                in_person,
                MeetingSource::MacInPerson,
                vec![AudioLane::Mixed],
                0.5,
            ),
        ] {
            let meeting = harness.store.meeting(meeting_id).unwrap().unwrap();
            assert_ne!(meeting.state.kind(), MeetingStateKind::Failed);
            assert_eq!(meeting.source, source);
            assert_eq!(meeting.duration, duration);
            assert_eq!(meeting.end_reason, Some(RecordingEndReason::Failed));
            assert_started_before_the_master_ended(&harness, &meeting, AudioFormat::Caf48kFloat32);
            assert_eq!(
                meeting.title,
                steno_pipeline::intake::default_title(source, meeting.started_at, zone)
            );
            let asset = harness.store.asset(meeting_id).unwrap().unwrap();
            assert_eq!(
                asset.url,
                file_url(&master_path(&harness.audio_folder(), meeting_id), false)
            );
            assert_eq!(asset.format, AudioFormat::Caf48kFloat32);
            assert_eq!(
                asset.sidecars_16k.keys().copied().collect::<Vec<_>>(),
                lanes
            );
            assert_eq!(asset.lanes, lanes);
            assert_eq!(asset.retention, AudioRetention::KeepForever);
        }
        harness.pipeline.current().wait_until_idle().await;
        for meeting_id in [call, in_person] {
            assert_eq!(
                harness.store.meeting(meeting_id).unwrap().unwrap().state,
                MeetingState::Ready
            );
        }

        assert_eq!(
            sorted(
                crate::audio_folders::recorded(&harness.support_directory())
                    .unwrap()
                    .into_keys()
                    .collect()
            ),
            sorted(vec![call, in_person]),
            "an adopted recording's folder stays until its row is durable"
        );
        let assets = harness.store.asset_urls().unwrap().len();
        assert_eq!(
            reconcile_at_launch(&harness, &[], &an_hour_later()),
            Vec::<Uuid>::new()
        );
        assert_eq!(harness.store.asset_urls().unwrap().len(), assets);
        assert!(
            crate::audio_folders::recorded(&harness.support_directory())
                .unwrap()
                .is_empty(),
            "the second launch forgets them"
        );
        harness.pipeline.current().wait_until_idle().await;
    }

    /// A phone upload copied into its meeting folder whose row never
    /// committed (a crash between the copy and the commit, a commit and
    /// its `failed` save both failing) is adopted as a phone recording
    /// with no end reason, the length its container declares, a plain
    /// asset in its format, and processed; a WAV master too.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_phone_master_with_no_meeting_is_adopted_as_a_phone_recording() {
        let harness = Harness::new();
        let m4a = phone_orphan(&harness, "tone-440-44k1-500ms.m4a", AudioFormat::M4aAac);
        let wav = phone_orphan(
            &harness,
            "conversation-mic-6s.wav",
            AudioFormat::Wav16kInt16,
        );

        let adopted = reconcile_at_launch(&harness, &[], &an_hour_later());
        assert_eq!(sorted(adopted), sorted(vec![m4a, wav]));
        for (meeting_id, format, seconds) in [
            (m4a, AudioFormat::M4aAac, 0.5..0.6),
            (wav, AudioFormat::Wav16kInt16, 5.99..6.01),
        ] {
            let meeting = harness.store.meeting(meeting_id).unwrap().unwrap();
            assert_eq!(meeting.source, MeetingSource::Phone);
            assert_eq!(meeting.end_reason, None);
            assert!(seconds.contains(&meeting.duration), "{}", meeting.duration);
            assert_started_before_the_master_ended(&harness, &meeting, format);
            assert!(
                meeting.title.starts_with("Phone recording "),
                "{}",
                meeting.title
            );
            let asset = harness.store.asset(meeting_id).unwrap().unwrap();
            let master = RecordingLayout::new(&harness.audio_folder(), meeting_id).master(format);
            assert_eq!(asset.url, file_url(&master, false));
            assert_eq!(asset.format, format);
            assert_eq!(asset.lanes, vec![AudioLane::Mixed]);
            assert!(asset.sidecars_16k.is_empty());
            assert_eq!(asset.retention, AudioRetention::KeepForever);
        }
        harness.pipeline.current().wait_until_idle().await;
        assert_eq!(
            harness.store.meeting(m4a).unwrap().unwrap().state,
            MeetingState::Ready
        );
    }

    /// Only a meeting folder with a master and no row that the record
    /// names is adopted: an empty master, a folder not named by a UUID, a
    /// master the record does not name (another install's, or one from
    /// before the record), and a folder whose meeting has a row in any
    /// state (here `recording` as well, left to the reconcile) are left as
    /// they are.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn only_a_master_with_no_meeting_is_adopted() {
        let harness = Harness::new();
        let empty = orphan(&harness, |layout| {
            std::fs::write(layout.master(AudioFormat::Caf48kFloat32), b"").unwrap();
        });
        let unrecorded = Uuid::new_v4();
        let mut writer = RecordingWriter::new(
            &RecordingLayout::new(&harness.audio_folder(), unrecorded),
            &[AudioLane::Mixed],
            false,
        )
        .unwrap();
        write_frames(&mut writer, 10);
        drop(writer);
        let unrecorded_files =
            files_in(&RecordingLayout::new(&harness.audio_folder(), unrecorded).directory);
        let not_a_meeting = harness.audio_folder().join("not-a-meeting");
        std::fs::create_dir_all(&not_a_meeting).unwrap();
        std::fs::copy(
            audio_fixture("tone-440-44k1-500ms.m4a"),
            not_a_meeting.join("recording.m4a"),
        )
        .unwrap();
        let mut rows = Vec::new();
        for state in [
            MeetingState::Recording,
            MeetingState::Queued,
            MeetingState::Ready,
            MeetingState::Failed {
                reason: "refused".to_owned(),
            },
        ] {
            let meeting = harness.begin(MeetingSource::MacInPerson);
            let mut writer = harness.writer(&meeting);
            write_frames(&mut writer, 10);
            drop(writer);
            harness
                .store
                .set_state(meeting.id, state.clone(), Utc::now())
                .unwrap();
            rows.push((meeting.id, state));
        }

        assert_eq!(
            reconcile_at_launch(&harness, &[], &an_hour_later()),
            Vec::<Uuid>::new()
        );
        assert!(harness.store.meeting(empty).unwrap().is_none());
        assert!(harness.store.meeting(unrecorded).unwrap().is_none());
        assert_eq!(
            files_in(&RecordingLayout::new(&harness.audio_folder(), unrecorded).directory),
            unrecorded_files
        );
        for (meeting_id, state) in rows {
            assert_eq!(
                harness.store.meeting(meeting_id).unwrap().unwrap().state,
                state
            );
            assert!(harness.store.asset(meeting_id).unwrap().is_none());
        }
        assert!(not_a_meeting.join("recording.m4a").is_file());
    }

    /// A master modified just now may be one another process is writing
    /// (a phone upload being admitted, the Swift app): it is left for the
    /// next launch, which adopts it once it is old.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_master_modified_just_now_is_left_for_the_next_launch() {
        let harness = Harness::new();
        let fresh = mac_orphan(&harness, &[AudioLane::Mixed], 10);
        let now = LiveRecordingCheck {
            wait: Arc::new(|_| panic!("an orphan is not waited for")),
            ..LiveRecordingCheck::default()
        };

        assert_eq!(reconcile_at_launch(&harness, &[], &now), Vec::<Uuid>::new());
        assert!(harness.store.meeting(fresh).unwrap().is_none());
        assert!(master_path(&harness.audio_folder(), fresh).is_file());

        assert_eq!(
            reconcile_at_launch(&harness, &[], &an_hour_later()),
            [fresh]
        );
        harness.pipeline.current().wait_until_idle().await;
    }

    /// The handover inbox holds uploads a receipt accounts for, which the
    /// phone still has: its files are left as they are, even with the
    /// inbox and the support directory listed as known audio folders.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_handover_inbox_is_left_alone() {
        let harness = Harness::new();
        let inbox = harness.support_directory().join("handover-inbox");
        std::fs::create_dir_all(&inbox).unwrap();
        let recording_id = Uuid::new_v4().hyphenated().to_string();
        let files = [
            format!("{recording_id}.m4a"),
            format!("{recording_id}.partial"),
            format!("{recording_id}.metadata.json"),
        ]
        .map(|name| inbox.join(name));
        for file in &files {
            std::fs::copy(audio_fixture("tone-440-44k1-500ms.m4a"), file).unwrap();
        }
        for folder in [&harness.support_directory(), &inbox] {
            crate::audio_folders::remember(&harness.support_directory(), folder).unwrap();
        }

        assert_eq!(
            reconcile_at_launch(&harness, &[], &an_hour_later()),
            Vec::<Uuid>::new()
        );
        assert_eq!(harness.store.all_meetings().unwrap(), Vec::<Meeting>::new());
        for file in &files {
            assert!(file.is_file(), "{}", file.display());
        }
    }

    /// An adopted master is kept whatever the default retention: it
    /// outlived its row, so the user kept it, and a default that deletes
    /// after processing would remove it once processed.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_adopted_master_is_kept_whatever_the_default_retention() {
        let harness = Harness::new();
        let mut settings = harness.store.settings().unwrap();
        settings.default_retention = AudioRetention::DeleteAfterProcessing;
        harness.store.save_settings(&settings).unwrap();
        let call = mac_orphan(&harness, &[AudioLane::Mic, AudioLane::System], 150);
        let phone = phone_orphan(&harness, "tone-440-44k1-500ms.m4a", AudioFormat::M4aAac);

        let adopted = reconcile_at_launch(&harness, &[], &an_hour_later());
        assert_eq!(sorted(adopted), sorted(vec![call, phone]));
        harness.pipeline.current().wait_until_idle().await;
        steno_pipeline::RetentionSweep::new(harness.store.clone())
            .run(Utc::now() + chrono::Duration::days(1))
            .unwrap();
        for (meeting_id, format) in [
            (call, AudioFormat::Caf48kFloat32),
            (phone, AudioFormat::M4aAac),
        ] {
            assert_eq!(
                harness.store.asset(meeting_id).unwrap().unwrap().retention,
                AudioRetention::KeepForever
            );
            let master = RecordingLayout::new(&harness.audio_folder(), meeting_id).master(format);
            assert!(master.is_file(), "{}", master.display());
        }
    }

    /// Two databases over one audio folder (two computers syncing it, a
    /// second database the CLI was pointed at): the second one's launch
    /// adopts nothing of the first's, neither a processed meeting nor one
    /// still recording, and touches none of their files, so neither its
    /// retention nor a delete can remove them.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_meeting_folder_of_another_database_is_left_untouched() {
        let first = Harness::new();
        let done = first.begin(MeetingSource::MacCall);
        first.record(&done, &first.audio_folder());
        let mut writer = first.writer(&done);
        write_frames(&mut writer, 150);
        drop(writer);
        reconcile_at_launch(&first, std::slice::from_ref(&done), &an_hour_later());
        first.pipeline.current().wait_until_idle().await;
        assert_eq!(first.state(&done), MeetingState::Ready);
        let live = first.begin(MeetingSource::MacCall);
        first.record(&live, &first.audio_folder());
        let mut writer = first.writer(&live);
        write_frames(&mut writer, 50);
        drop(writer);
        let files = files_in(&first.audio_folder());

        let second = Harness::new();
        second.set_folder(&first.audio_folder());
        let mut settings = second.store.settings().unwrap();
        settings.default_retention = AudioRetention::DeleteAfterProcessing;
        second.store.save_settings(&settings).unwrap();
        assert_eq!(
            reconcile_at_launch(&second, &[], &an_hour_later()),
            Vec::<Uuid>::new()
        );
        second.pipeline.current().wait_until_idle().await;
        steno_pipeline::RetentionSweep::new(second.store.clone())
            .run(Utc::now() + chrono::Duration::days(1))
            .unwrap();

        assert_eq!(second.store.all_meetings().unwrap(), Vec::<Meeting>::new());
        assert_eq!(files_in(&first.audio_folder()), files);
    }

    /// A meeting with no asset (here one whose master has the wrong
    /// channels, failed at launch) keeps its master in its folder. Its
    /// delete removes that folder in each audio folder it may be in, so
    /// the next launch does not bring the meeting back.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_deleted_meeting_with_no_asset_does_not_come_back() {
        let harness = Harness::new();
        let meeting = harness.begin(MeetingSource::MacCall);
        harness.record(&meeting, &harness.audio_folder());
        let layout = RecordingLayout::new(&harness.audio_folder(), meeting.id);
        let mut writer = RecordingWriter::new(&layout, &[AudioLane::Mixed], false).unwrap();
        write_frames(&mut writer, 50);
        drop(writer);
        reconcile_at_launch(&harness, std::slice::from_ref(&meeting), &an_hour_later());
        assert!(matches!(
            harness.state(&meeting),
            MeetingState::Failed { .. }
        ));
        assert!(harness.store.asset(meeting.id).unwrap().is_none());
        assert!(layout.master(AudioFormat::Caf48kFloat32).is_file());

        let left = steno_host::services::LeftRecording {
            folders: meeting_folders(&harness.store, &harness.support_directory(), meeting.id),
            still_written: false,
        };
        let mut list = steno_host::main_window::MeetingListViewModel::new(
            chrono::FixedOffset::east_opt(0).unwrap(),
        );
        assert!(list.delete(
            meeting.id,
            &harness.store,
            &steno_host::services::RealFileSystem,
            Some(&left),
            false,
        ));
        assert_eq!(list.error, None);
        assert!(!layout.directory.exists());

        assert_eq!(
            reconcile_at_launch(&harness, &[], &an_hour_later()),
            Vec::<Uuid>::new()
        );
        assert!(harness.store.meeting(meeting.id).unwrap().is_none());
    }

    /// A recovery that panics (here the live check's wait) is caught: the
    /// launch's task returns, the row it had not finished stays
    /// `recording`, and the next launch recovers it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_recovery_that_panics_leaves_its_rows_for_the_next_launch() {
        let harness = Harness::new();
        let fresh = harness.begin(MeetingSource::MacInPerson);
        let mut writer = harness.writer(&fresh);
        write_frames(&mut writer, 10);
        drop(writer);
        let panicking = LiveRecordingCheck {
            wait: Arc::new(|_| panic!("the check fails")),
            ..LiveRecordingCheck::default()
        };

        reconcile_at_launch(&harness, std::slice::from_ref(&fresh), &panicking);
        assert_eq!(harness.state(&fresh), MeetingState::Recording);
        reconcile_at_launch(&harness, std::slice::from_ref(&fresh), &an_hour_later());
        harness.pipeline.current().wait_until_idle().await;
        assert_eq!(harness.state(&fresh), MeetingState::Ready);
    }
}
