//! Recovery of an interrupted recording. A meeting gets its asset only when
//! its capture stops, so a crash leaves a `recording` row with no asset; the
//! launch rebuilds the asset from the master's header and queues it, fails
//! the row only when its master is provably not there, and leaves a master
//! still being written alone. It also adopts masters this install wrote that
//! have no row at all (`adopt_orphans`).

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use chrono::Utc;
use steno_audio::writer::{CafHeader, CafReadError, CafStreamWriter, WavStreamWriter};
use steno_core::{
    AdmissionCommit, AudioAsset, AudioFormat, AudioLane, AudioRetention, Meeting, MeetingSource,
    RecordingEndReason, RecordingLayout, Store, StoreError,
    paths::{file_url, file_url_path},
};
use steno_pipeline::{
    LocalRecordingIntake, LocalRecordingIntakeError, NotedUpload, RecordingResult,
};
use uuid::Uuid;

/// Why a meeting's master cannot be recovered; the meeting fails.
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
    /// stays `recording`.
    #[error(transparent)]
    NotSaved(#[from] LocalRecordingIntakeError),
}

/// The lanes a meeting from `source` records, in master channel order; a
/// phone's upload is one mixed lane (never salvaged).
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
/// once: the recorded one, the settings' one and the known ones (none when
/// unreadable). A delete removes the meeting's folder in each when no asset
/// names it, so the launch does not adopt its master again.
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

/// The folders a master may be in besides the two deciding ones, each
/// once: the `known` folders, then each stored asset's folder.
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
    /// No folder holds it, but a deciding folder is missing or unreadable,
    /// so the master may be there.
    Unreachable,
}

/// Looks for the master of `meeting_id` in each of `deciding`, then each of
/// `others`. A deciding folder that is missing or unreadable makes a miss
/// [`Lookup::Unreachable`]; any other such folder is skipped.
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

/// The recording the master of `meeting_id` in `layout` holds, as its
/// capture would have handed it over: the asset rebuilt from the header
/// (whole frames only), the default retention and the end reason `failed`.
/// A master that cannot be read now is [`RecoveryError::Unreachable`], so
/// its row is kept. A sidecar whose writer died is finished from its length
/// ([`WavStreamWriter::recover`]) or left out, and the decoder rebuilds
/// that lane from the master.
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

/// Looks for the master of `meeting_id` in `folders` (each one deciding),
/// salvages it and queues the meeting through `intake`.
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

/// Salvages the master of `meeting_id` in `audio_folder` and queues the
/// meeting through `intake`.
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

/// Queues a phone meeting left `recording` (written by another process)
/// with the stored asset that names its upload, end reason `failed`. One with
/// no asset is [`Unrecoverable::NoMaster`]; one whose asset cannot be read
/// now is kept.
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
    /// still written when its modification time or size changes meanwhile.
    pub fresh_within: Duration,
    /// The clock modification times are read against.
    pub now: Arc<dyn Fn() -> SystemTime + Send + Sync>,
    /// Blocks the calling thread for the given time.
    pub wait: Arc<dyn Fn(Duration) + Send + Sync>,
}

impl Default for LiveRecordingCheck {
    /// Ten seconds: past a writer's stall in a device rebuild and a network
    /// volume's cached attributes; a running writer appends every 10 ms.
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
    /// [`Self::fresh_within`] or after now, without waiting.
    pub(crate) fn is_fresh(&self, path: &Path) -> bool {
        stamp(path).is_some_and(|(modified, _)| self.left(modified) > Duration::ZERO)
    }

    /// How much younger than [`Self::fresh_within`] a master modified at
    /// `modified` is; a time ahead of the clock counts as just modified.
    fn left(&self, modified: SystemTime) -> Duration {
        let age = (self.now)()
            .duration_since(modified)
            .unwrap_or(Duration::ZERO);
        self.fresh_within.saturating_sub(age)
    }

    /// Whether each of `masters` is still written. A fresh one is watched
    /// until it is [`Self::fresh_within`] old; one that changes meanwhile,
    /// or is still young after that long (a time ahead of the clock), is.
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

/// What the launch lists before anything here can start a recording: the
/// meetings left `recording`, their recorded folders and the known ones.
#[derive(Debug, Clone)]
pub(crate) struct Interrupted {
    pub(crate) meetings: Vec<Meeting>,
    /// `None` when the record or the meetings cannot be read: then a meeting
    /// whose master is not found is kept, and no entry is forgotten.
    pub(crate) recorded: Option<BTreeMap<Uuid, PathBuf>>,
    pub(crate) known_folders: Vec<PathBuf>,
    /// Where the record is.
    pub(crate) support_directory: PathBuf,
}

impl Interrupted {
    /// Lists them; whatever cannot be read is logged and left out.
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
    /// A folder their master may be in could not be read.
    pub(crate) unreachable: Vec<Uuid>,
    /// Left `recording` for the next launch: not saved, not marked failed,
    /// or every meeting when the settings or assets could not be read.
    pub(crate) kept: Vec<Uuid>,
    /// Nothing to recover: failed.
    pub(crate) failed: Vec<Uuid>,
}

/// The launch's reconciliation of `interrupted`. A meeting whose master is
/// still written, or may be in an unreadable folder, is left alone; one with
/// a master is recovered and queued; the rest are failed with
/// [`Store::INTERRUPTED_RECORDING_REASON`]. Settled entries are then
/// forgotten from the record (`forget_settled`). Blocks up to
/// [`LiveRecordingCheck::fresh_within`], so run it off the main thread.
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
            // Nothing can be told apart; the next launch tries again.
            tracing::warn!(%error, "interrupted recordings left for the next launch");
            reconciled.kept = meetings.iter().map(|meeting| meeting.id).collect();
            return reconciled;
        }
    };
    // A phone meeting's audio is its upload: no master is looked for.
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
                // A folder this build cannot read may hold the master: only
                // a master found is recovered.
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
    // Warn names the meeting; an error, which can name its folder, is debug.
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

/// Forgets the recorded folder of every entry whose meeting has a row not
/// left `recording`, only after a durable checkpoint: until then a power
/// loss can take the row, and the entry is what lets a later launch adopt
/// its master. An entry with no row stays for [`adopt_orphans`].
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

/// A meeting folder with a master, no meeting row, and an entry in the
/// record of recording folders.
#[derive(Debug, Clone)]
struct Orphan {
    meeting_id: Uuid,
    layout: RecordingLayout,
    master: PathBuf,
    format: AudioFormat,
    modified: SystemTime,
}

/// The meeting id a folder named `name` stands for: a hyphenated UUID, in
/// any case.
fn meeting_folder_id(name: &std::ffi::OsStr) -> Option<Uuid> {
    let name = name.to_str().filter(|name| name.len() == 36)?;
    Uuid::try_parse(name).ok()
}

/// The first master in `layout` that is there, with its modification time.
fn orphan_master(layout: &RecordingLayout) -> Option<(AudioFormat, PathBuf, SystemTime)> {
    AudioFormat::ALL.iter().find_map(|&format| {
        let master = layout.master(format);
        let metadata = std::fs::metadata(&master).ok().filter(|metadata| {
            metadata.is_file() && may_hold_audio(format, &master, metadata.len())
        })?;
        Some((format, master, metadata.modified().ok()?))
    })
}

/// Whether `master`, `len` bytes long, may hold audio: for a CAF, the header
/// [`CafStreamWriter`] writes plus one whole frame; any other format unless
/// empty.
fn may_hold_audio(format: AudioFormat, master: &Path, len: u64) -> bool {
    match format {
        AudioFormat::Caf48kFloat32 => {
            let header = CafStreamWriter::HEADER_SIZE as u64;
            let sample = CafStreamWriter::BYTES_PER_SAMPLE as u64;
            // The header is read only once it is whole and a sample follows.
            len >= header + sample && {
                let channels = CafHeader::read(master).map_or(1, |caf| caf.channel_count);
                len >= header + channels as u64 * sample
            }
        }
        AudioFormat::M4aAac | AudioFormat::Wav16kInt16 => len > 0,
    }
}

/// Whether `folder` provably holds no master of `meeting_id`: its meeting
/// folder has none that may hold audio, or is missing while `folder` lists
/// other entries. An empty or unreadable folder may still hold one.
fn holds_no_master(folder: &Path, meeting_id: Uuid) -> bool {
    let layout = RecordingLayout::new(folder, meeting_id);
    match std::fs::metadata(&layout.directory) {
        Ok(metadata) if metadata.is_dir() => AudioFormat::ALL.iter().all(|&format| {
            let master = layout.master(format);
            match std::fs::metadata(&master) {
                Ok(metadata) => {
                    metadata.is_file() && !may_hold_audio(format, &master, metadata.len())
                }
                Err(error) => error.kind() == std::io::ErrorKind::NotFound,
            }
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            std::fs::read_dir(folder).is_ok_and(|mut entries| entries.any(|entry| entry.is_ok()))
        }
        _ => false,
    }
}

/// The meeting folders in `folders` that hold a master, whose id no row has
/// and the record names, each id once. Unreadable folders are skipped. A
/// master the record does not name (another install's, or the Swift app's)
/// is left alone and logged in one line.
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
        tracing::warn!(
            count = unrecorded.len(),
            first = %first_ids(&unrecorded),
            "recordings with no meeting that this install has no record of are left alone"
        );
    }
    found
}

/// How many ids each of the launch's two summary lines names.
const IDS_LOGGED: usize = 3;

/// The first [`IDS_LOGGED`] of `ids`, joined for a log line.
fn first_ids(ids: &[Uuid]) -> String {
    ids.iter()
        .take(IDS_LOGGED)
        .map(Uuid::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

/// Saves an adopted recording's rows and processes it.
fn save_adopted(
    pipeline: &steno_pipeline::ProcessingPipeline,
    meeting: &Meeting,
    asset: &AudioAsset,
) -> Result<(), RecoveryError> {
    pipeline
        .enqueue_new(meeting, asset)
        .map_err(|failure| RecoveryError::NotSaved(failure.into()))
}

/// The meeting and asset an orphan's master gives: a Mac CAF is salvaged
/// as a call or in-person recording with end reason `failed`; an m4a or WAV
/// is a phone recording with its declared length. It gets the default title,
/// `queued`, and Keep forever (the master outlived its row, so the user kept
/// it).
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
            // One channel is in-person; the salvage as a call fails any
            // other count.
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
            let asset_id = Uuid::new_v4();
            let duration = steno_audio::SymphoniaAudioCodec::declared_duration(master)
                .inspect_err(|error| {
                    tracing::debug!(%meeting_id, %error, "an adopted recording's length is not known");
                })
                .ok()
                .flatten()
                .unwrap_or(0.0);
            let asset = AudioAsset {
                id: asset_id,
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

/// The launch's adoption of recordings with no meeting. A UUID-named meeting
/// folder in any audio folder the launch knows is adopted where it is, as a
/// `queued` meeting with that id, when its master may hold audio, it has no
/// row, and the record ([`crate::audio_folders`]) names it, both at the
/// launch's listing and in a read after the rows, so a meeting deleted
/// meanwhile is not adopted back. Returns the ids adopted.
///
/// A noted phone copy that holds its upload whole is adopted with the
/// upload's ledger row, so the phone's retry gets this meeting. If the
/// ledger already holds the bytes as another existing meeting, the copy is
/// kept untouched; if the user deleted that meeting, the copy is removed
/// ([`remove_copy`]). Any doubt keeps the copy.
///
/// An adopted master's entry stays until a later launch finds its row
/// durable. An entry with no row is forgotten only when its folder provably
/// holds no master, or the master holds no audio. A master still being
/// written, or one that cannot be read or saved, keeps its entry for the
/// next launch.
pub(crate) fn adopt_orphans(
    store: &Store,
    pipeline: &steno_pipeline::ProcessingPipeline,
    interrupted: &Interrupted,
    check: &LiveRecordingCheck,
    zone: chrono::FixedOffset,
    runtime: &tokio::runtime::Handle,
) -> Vec<Uuid> {
    // The record or rows could not be read: no master is this install's.
    let Some(listed_at_launch) = &interrupted.recorded else {
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
    // Only an entry in the launch's listing and a read after the rows is
    // evidence: a delete forgets its entry before its rows go.
    let read_again = match crate::audio_folders::recorded(&interrupted.support_directory) {
        Ok(read_again) => read_again,
        Err(error) => {
            tracing::debug!(%error, "recording folders not read again; none adopted");
            return Vec::new();
        }
    };
    let mut recorded = listed_at_launch.clone();
    recorded.retain(|meeting_id, _| read_again.contains_key(meeting_id));
    let uploads = noted_uploads(&interrupted.support_directory);
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
    let found = orphans(&folders, &ids, &recorded);
    let _entered = runtime.enter();
    let mut adopted_ids = Vec::new();
    // Forgotten whether or not the store is durable.
    let mut gone = Vec::new();
    // Kept with their entries, and warned about in one line.
    let mut unrecovered = Vec::new();
    for orphan in &found {
        let meeting_id = orphan.meeting_id;
        if check.is_fresh(&orphan.master) {
            tracing::warn!(%meeting_id, "a recording with no meeting is still written; the next launch looks again");
            continue;
        }
        let now = (pipeline.dependencies().now)();
        let rows = adopted(orphan, &settings, zone, now);
        let saved =
            rows.and_then(|rows| adopt(store, pipeline, orphan, uploads.get(&meeting_id), &rows));
        // Warn names the meeting; an error, which can name its folder, is debug.
        match saved {
            Ok(AdmissionCommit::Wrote) => {
                tracing::warn!(%meeting_id, "a recording with no meeting was recovered");
                adopted_ids.push(meeting_id);
            }
            Ok(AdmissionCommit::Earlier {
                meeting_id: admitted,
                deleted,
            }) => match answered_by_the_ledger(orphan, admitted, deleted) {
                LedgerAnswer::Forget => gone.push(meeting_id),
                LedgerAnswer::Keep => {}
                LedgerAnswer::Retry => unrecovered.push(meeting_id),
            },
            // A header and no audio: it holds no master either.
            Err(RecoveryError::Unrecoverable(Unrecoverable::Empty)) => {
                tracing::debug!(%meeting_id, "a recording with no meeting holds no audio; its entry is forgotten");
                gone.push(meeting_id);
            }
            Err(error) => {
                tracing::debug!(%meeting_id, %error, "recording with no meeting not recovered");
                unrecovered.push(meeting_id);
            }
        }
    }
    if !unrecovered.is_empty() {
        tracing::warn!(
            count = unrecovered.len(),
            first = %first_ids(&unrecovered),
            "recordings with no meeting could not be recovered; they stay on disk for the next launch"
        );
    }
    for (&meeting_id, folder) in &recorded {
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

/// The uploads noted for the phone copies; none when they cannot be read.
/// Those whose folder has no entry are forgotten.
fn noted_uploads(support_directory: &Path) -> BTreeMap<Uuid, NotedUpload> {
    let uploads = crate::audio_folders::uploads(support_directory).unwrap_or_else(|error| {
        tracing::debug!(%error, "the noted uploads were not read; copies are adopted without them");
        BTreeMap::new()
    });
    if let Err(error) =
        crate::audio_folders::forget_uploads_without_folders(support_directory, &uploads)
    {
        tracing::debug!(%error, "the noted uploads with no folder were not forgotten");
    }
    uploads
}

/// What the launch does with the entries of a noted copy the ledger
/// answered ([`answered_by_the_ledger`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LedgerAnswer {
    /// They go: the copy is removed, or it is the meeting the ledger names.
    Forget,
    /// They stay with the copy: another existing meeting holds its bytes.
    Keep,
    /// They stay for the next launch: the copy's master was not removed.
    Retry,
}

/// What becomes of the noted copy `orphan` when the ledger already held its
/// bytes as `admitted` (`deleted` when the user deleted it): a deleted
/// meeting's copy is removed, another existing meeting's is kept.
fn answered_by_the_ledger(orphan: &Orphan, admitted: Uuid, deleted: bool) -> LedgerAnswer {
    let meeting_id = orphan.meeting_id;
    if !deleted {
        // The retry admitted into this copy since the launch listed the rows.
        if admitted == meeting_id {
            return LedgerAnswer::Forget;
        }
        // At every launch until that meeting's delete: debug alone.
        tracing::debug!(%meeting_id, %admitted, "a copy of a phone recording whose meeting exists is kept with its entries");
        return LedgerAnswer::Keep;
    }
    match remove_copy(orphan) {
        Ok(()) => {
            tracing::info!(%meeting_id, %admitted, "the copy of a phone recording whose meeting the user deleted was removed");
            LedgerAnswer::Forget
        }
        Err(error) => {
            // The error can name the user's folder: debug alone.
            tracing::debug!(%meeting_id, %error, "the copy of a deleted meeting's recording not removed");
            LedgerAnswer::Retry
        }
    }
}

/// Saves `orphan` as `meeting` with `asset` and processes it, with
/// `upload`'s ledger row when the orphan is a phone
/// copy that holds it whole. A copy that cannot be read now is
/// [`RecoveryError::Unreachable`].
fn adopt(
    store: &Store,
    pipeline: &steno_pipeline::ProcessingPipeline,
    orphan: &Orphan,
    upload: Option<&NotedUpload>,
    (meeting, asset): &(Meeting, AudioAsset),
) -> Result<AdmissionCommit, RecoveryError> {
    let upload = match upload {
        Some(upload) if meeting.source == MeetingSource::Phone => held_by(upload, orphan)
            .map_err(|error| {
                tracing::debug!(meeting_id = %meeting.id, %error, "a phone copy could not be read");
                RecoveryError::Unreachable
            })?
            .then_some(upload),
        _ => None,
    };
    let Some(upload) = upload else {
        return save_adopted(pipeline, meeting, asset).map(|()| AdmissionCommit::Wrote);
    };
    let committed = store
        .adopt_upload_durably(
            meeting,
            asset,
            upload.recording_id,
            upload.byte_count,
            &upload.sha256,
        )
        .map_err(|error| RecoveryError::NotSaved(error.into()))?;
    if committed == AdmissionCommit::Wrote {
        #[cfg(test)]
        crate::admission_kill_tests::gate("after-adoption-commit");
        // A pipeline that cannot take it now leaves it `queued` for the
        // next launch's resume.
        if let Err(failure) = pipeline.enqueue_saved(meeting, asset) {
            tracing::warn!(meeting_id = %meeting.id, stage = failure.stage.as_str(), "an adopted phone recording not enqueued");
        }
    }
    Ok(committed)
}

/// Whether `orphan`'s master holds `upload` whole. The tests make a read
/// fail.
fn held_by(upload: &NotedUpload, orphan: &Orphan) -> std::io::Result<bool> {
    #[cfg(test)]
    if UNREADABLE_COPIES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .contains(&orphan.meeting_id)
    {
        return Err(std::io::Error::other("the copy cannot be read"));
    }
    upload.is_held_by(&orphan.master)
}

/// The meetings whose copy's read fails in [`held_by`].
#[cfg(test)]
pub(crate) static UNREADABLE_COPIES: std::sync::Mutex<Vec<Uuid>> =
    std::sync::Mutex::new(Vec::new());

/// Removes `orphan`'s master durably, then its meeting folder if empty.
/// Fails only when the master was not removed. Never removes more than the
/// master and an empty folder.
fn remove_copy(orphan: &Orphan) -> std::io::Result<()> {
    crate::files::remove_durably(&orphan.master)?;
    match std::fs::remove_dir(&orphan.layout.directory) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
            let meeting_id = orphan.meeting_id;
            tracing::warn!(%meeting_id, "the folder of a removed phone copy stays: it holds other files or could not be removed");
            // The error can name the user's folder: debug alone.
            tracing::debug!(%meeting_id, %error, "the folder of a removed phone copy not removed");
        }
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use steno_audio::writer::{RecordingWriter, RecordingWriting as _, WavFile};
    use steno_audio::{FRAME_SIZE, SAMPLE_RATE};
    use steno_core::testing::WriteLockHold;
    use steno_core::{MeetingState, MeetingStateKind};

    use super::*;
    use crate::pipeline::CurrentPipeline;
    use crate::testing::{
        an_hour_later, current_pipeline, fake_dependencies, temp_store, warnings, write_frames,
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

        /// `meetings` as the launch lists them.
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

        /// Points the settings at `folder` without remembering the old one.
        fn set_folder(&self, folder: &Path) {
            let mut settings = self.store.settings().unwrap();
            settings.audio_folder = file_url(folder, true);
            self.store.save_settings(&settings).unwrap();
        }

        fn state(&self, meeting: &Meeting) -> MeetingState {
            self.store.meeting(meeting.id).unwrap().unwrap().state
        }
    }

    /// A check over a clock that moves only when the check waits, running
    /// `during` with the total time waited.
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

    /// A call recording killed mid-meeting (writer dropped without `finish`)
    /// is queued at launch with its sidecars and processed.
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

    /// Whole frames of a torn write are recovered; a header-only master,
    /// wrong channels or no master fail the row.
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

    /// A master still being written leaves its row `recording`; once the
    /// writer stopped, the next launch watches it `fresh_within` and recovers it.
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

    /// A writer that stalls three seconds and goes on keeps its row; its
    /// own stop saves the whole recording.
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

    /// The master is looked for in every folder a recording was written to;
    /// a row whose master may be on a missing folder stays `recording`, one
    /// with all folders there and no master fails.
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

        // No master anywhere, and the settings' folder is not there.
        let unmounted = harness.begin(MeetingSource::MacInPerson);
        set_folder(&harness.dir.path().join("unmounted"));
        let reconciled = harness.reconcile(std::slice::from_ref(&unmounted), &an_hour_later());
        assert_eq!(reconciled.unreachable, [unmounted.id]);
        assert_eq!(reconciled.failed, Vec::<Uuid>::new());
        assert_eq!(harness.state(&unmounted), MeetingState::Recording);

        // Every folder there, empty of it: failed.
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
            .on_writer(|connection| connection.busy_timeout(Duration::from_millis(20)))
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

    /// The folder recorded before the row lets the launch find a master
    /// after the setting moved, and the entry goes once the meeting is
    /// queued.
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

    /// With no row left `recording` the launch still forgets the entries of
    /// meetings that moved on, and of rowless ones whose folder provably
    /// holds no master.
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

    /// A launch that cannot list the rows left `recording` forgets no entry.
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

    /// An entry recorded after the launch listed the record stays; a listed
    /// settled one goes.
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

    /// A phone row left `recording` is queued with its stored asset and
    /// processes; one with no asset fails.
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

    /// A missing recorded folder keeps its row `recording`; a retired known
    /// folder does not keep a row with no master anywhere.
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

    /// Makes `path` unreadable; `false` when it stays readable (root).
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

    /// An unreadable folder, then an unreadable master, keep the row
    /// `recording`; a later launch recovers it once both can be read.
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

    /// An unreadable record keeps a row whose master is in its start
    /// folder; a later launch recovers it.
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

    /// Settings whose audio folder is not a file URL keep the row; once
    /// they name a folder, the same row fails.
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

    /// A master modified ahead of the clock counts as still written after
    /// `fresh_within` of waiting.
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

    /// A master with no row, written by `write` under the audio folder with
    /// its folder recorded first; returns its id.
    fn orphan(harness: &Harness, write: impl FnOnce(&RecordingLayout)) -> Uuid {
        let meeting_id = Uuid::new_v4();
        crate::audio_folders::record(
            &harness.support_directory(),
            meeting_id,
            &harness.audio_folder(),
        )
        .unwrap();
        write(&RecordingLayout::new(&harness.audio_folder(), meeting_id));
        meeting_id
    }

    /// The audio fixture `name` from `Tests/Fixtures/audio`.
    fn audio_fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../Tests/Fixtures/audio")
            .join(name)
    }

    /// A Mac master of `lanes` with `frames` frames, left as a crash leaves it.
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
            std::fs::create_dir_all(&layout.directory).unwrap();
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

    /// Asserts that `meeting` started its length before its master was last
    /// modified, to the millisecond.
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

    /// Rowless call and in-person masters are adopted where they are with
    /// the folder's id, the right source and length, end reason `failed`,
    /// and processed; the second launch forgets their entries.
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

    /// A rowless phone upload (m4a or WAV) is adopted as a phone recording
    /// with its declared length and processed.
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

    /// Only a recorded rowless meeting folder with a master is adopted; empty
    /// masters, non-UUID folders, unrecorded masters and folders with a row
    /// are left as they are.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn only_a_master_with_no_meeting_is_adopted() {
        let harness = Harness::new();
        let empty = orphan(&harness, |layout| {
            std::fs::create_dir_all(&layout.directory).unwrap();
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

    /// A master modified just now is left for the next launch, which adopts
    /// it once it is old.
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

    /// The handover inbox is never adopted from, even listed as known.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_handover_inbox_is_left_alone() {
        let harness = Harness::new();
        let inbox = harness
            .support_directory()
            .join(steno_handover::HandoverConfiguration::INBOX_DIRECTORY_NAME);
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

    /// An adopted master is kept whatever the default retention.
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

    /// An unreadable default retention still queues a recovered recording,
    /// kept forever; the run then fails with its master kept.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_recovered_recording_with_an_unreadable_default_retention_is_kept_forever() {
        let harness = Harness::new();
        let meeting = harness.begin(MeetingSource::MacCall);
        let mut writer = harness.writer(&meeting);
        write_frames(&mut writer, 150);
        drop(writer);
        harness
            .store
            .write(|transaction| {
                transaction.execute(
                    "INSERT OR REPLACE INTO setting (key, value) \
                     VALUES ('defaultRetention', 'not json')",
                    [],
                )?;
                Ok(())
            })
            .unwrap();
        assert!(harness.store.settings().is_err(), "the row is unreadable");

        let recovered = recover(
            &harness.intake(),
            &[harness.audio_folder()],
            meeting.id,
            meeting.source,
        )
        .await
        .unwrap();
        assert_eq!(recovered.state, MeetingState::Queued);
        assert_eq!(
            harness.store.asset(meeting.id).unwrap().unwrap().retention,
            AudioRetention::KeepForever
        );

        harness.pipeline.current().wait_until_idle().await;
        assert!(matches!(
            harness.state(&meeting),
            MeetingState::Failed { .. }
        ));
        let master = RecordingLayout::new(&harness.audio_folder(), meeting.id)
            .master(AudioFormat::Caf48kFloat32);
        assert!(master.is_file(), "{}", master.display());
    }

    /// A second database over the same audio folder adopts and touches
    /// nothing of the first's.
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

    /// The delete of a meeting with no asset removes its folder in each
    /// audio folder, so the next launch does not bring it back.
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

    /// A recovery that panics is caught; the row stays `recording` and the
    /// next launch recovers it.
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

    /// A power loss before any WAL frame reached the disk, reopened by a new
    /// launch over the same folders.
    fn power_loss(harness: Harness) -> Harness {
        let Harness {
            dir,
            store,
            pipeline,
        } = harness;
        let after = dir.path().join("after-power-loss.sqlite");
        std::fs::copy(dir.path().join("steno.sqlite"), &after).unwrap();
        drop((pipeline, store));
        let store = Arc::new(Store::open(&after).unwrap());
        let pipeline = current_pipeline(fake_dependencies(&store, "fake-engine"));
        Harness {
            dir,
            store,
            pipeline,
        }
    }

    /// A power loss before a checkpoint takes the adoption's row but not the
    /// entry, so the next launch adopts the master again.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_power_loss_after_an_adoption_leaves_the_master_to_the_next_launch() {
        let harness = Harness::new();
        harness.store.checkpoint_durably().unwrap();
        let orphan = mac_orphan(&harness, &[AudioLane::Mixed], 50);
        assert_eq!(
            reconcile_at_launch(&harness, &[], &an_hour_later()),
            [orphan]
        );
        harness.pipeline.current().wait_until_idle().await;

        let after = power_loss(harness);
        assert!(after.store.meeting(orphan).unwrap().is_none());
        assert_eq!(reconcile_at_launch(&after, &[], &an_hour_later()), [orphan]);
        after.pipeline.current().wait_until_idle().await;
        assert_eq!(
            after.store.meeting(orphan).unwrap().unwrap().state,
            MeetingState::Ready
        );
    }

    /// Forgetting an adopted entry checkpoints first, so a power loss keeps
    /// the row.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_power_loss_after_the_forgetting_launch_keeps_the_meeting() {
        let harness = Harness::new();
        harness.store.checkpoint_durably().unwrap();
        let orphan = mac_orphan(&harness, &[AudioLane::Mixed], 50);
        assert_eq!(
            reconcile_at_launch(&harness, &[], &an_hour_later()),
            [orphan]
        );
        harness.pipeline.current().wait_until_idle().await;
        assert_eq!(
            reconcile_at_launch(&harness, &[], &an_hour_later()),
            Vec::<Uuid>::new()
        );
        assert!(
            !crate::audio_folders::recorded(&harness.support_directory())
                .unwrap()
                .contains_key(&orphan),
            "the second launch forgets the entry"
        );

        let after = power_loss(harness);
        assert!(after.store.meeting(orphan).unwrap().is_some());
    }

    /// A refused delete records the entry again, so after a power loss the
    /// next launch adopts the master back.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_refused_delete_then_a_power_loss_leaves_the_master_to_the_next_launch() {
        let harness = Harness::new();
        harness.store.checkpoint_durably().unwrap();
        let orphan = mac_orphan(&harness, &[AudioLane::Mixed], 50);
        assert_eq!(
            reconcile_at_launch(&harness, &[], &an_hour_later()),
            [orphan]
        );
        harness.pipeline.current().wait_until_idle().await;
        let mut meeting = harness.store.meeting(orphan).unwrap().unwrap();
        meeting.state = MeetingState::Processing;
        harness.store.save_meeting(&meeting).unwrap();

        // The host's order: the entry, then the rows (refused), then the
        // entry recorded again.
        let support = harness.support_directory();
        let forgotten = crate::audio_folders::forget(&support, &[orphan]).unwrap();
        assert!(harness.store.delete_meeting(orphan).is_err());
        crate::audio_folders::record(&support, orphan, &forgotten[&orphan]).unwrap();

        let after = power_loss(harness);
        assert!(after.store.meeting(orphan).unwrap().is_none());
        assert!(master_path(&after.audio_folder(), orphan).is_file());
        assert_eq!(reconcile_at_launch(&after, &[], &an_hour_later()), [orphan]);
        after.pipeline.current().wait_until_idle().await;
        assert_eq!(
            after.store.meeting(orphan).unwrap().unwrap().state,
            MeetingState::Ready
        );
    }

    /// A meeting deleted while the launch runs is not adopted back, whether
    /// the delete lands before or after the commit.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_meeting_deleted_while_the_launch_runs_is_not_adopted_back() {
        let harness = Harness::new();
        let orphan = mac_orphan(&harness, &[AudioLane::Mixed], 50);
        assert_eq!(
            reconcile_at_launch(&harness, &[], &an_hour_later()),
            [orphan]
        );
        harness.pipeline.current().wait_until_idle().await;
        let launch = harness.interrupted(&[]);
        assert!(launch.recorded.as_ref().unwrap().contains_key(&orphan));
        let adopt = || {
            adopt_orphans(
                &harness.store,
                &harness.pipeline.current(),
                &launch,
                &an_hour_later(),
                chrono::FixedOffset::east_opt(0).unwrap(),
                &tokio::runtime::Handle::current(),
            )
        };

        // The delete in the host's order: the entry, the rows, the files.
        crate::audio_folders::forget(&harness.support_directory(), &[orphan]).unwrap();
        assert_eq!(adopt(), Vec::<Uuid>::new());
        harness.store.delete_meeting(orphan).unwrap();
        assert_eq!(adopt(), Vec::<Uuid>::new());
        harness.pipeline.current().wait_until_idle().await;
        assert!(harness.store.meeting(orphan).unwrap().is_none());
        assert!(master_path(&harness.audio_folder(), orphan).is_file());
    }

    /// A header-only recorded master, or a two-channel one with part of
    /// its first frame, is not adopted; its entry is forgotten and its file
    /// stays.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_header_only_master_with_no_meeting_is_forgotten() {
        let harness = Harness::new();
        let header_only = mac_orphan(&harness, &[AudioLane::Mixed], 0);
        let master = master_path(&harness.audio_folder(), header_only);
        assert_eq!(CafHeader::read(&master).unwrap().frame_count, 0);
        let partial = mac_orphan(&harness, &[AudioLane::Mic, AudioLane::System], 0);
        let partial_master = master_path(&harness.audio_folder(), partial);
        let mut bytes = std::fs::read(&partial_master).unwrap();
        bytes.extend_from_slice(&[0; CafStreamWriter::BYTES_PER_SAMPLE + 1]);
        std::fs::write(&partial_master, &bytes).unwrap();
        let before = files_in(&harness.audio_folder());

        assert_eq!(
            reconcile_at_launch(&harness, &[], &an_hour_later()),
            Vec::<Uuid>::new()
        );
        assert_eq!(harness.store.all_meetings().unwrap(), Vec::<Meeting>::new());
        assert!(
            crate::audio_folders::recorded(&harness.support_directory())
                .unwrap()
                .is_empty()
        );
        assert_eq!(files_in(&harness.audio_folder()), before);
    }

    /// A master cut inside its header is treated as header-only; a
    /// three-channel master keeps its entry and is logged as unrecovered.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_master_cut_in_its_header_is_forgotten_and_unreadable_ones_are_counted() {
        let harness = Harness::new();
        let cut = orphan(&harness, |layout| {
            let mut writer = RecordingWriter::new(layout, &[AudioLane::Mixed], false).unwrap();
            write_frames(&mut writer, 0);
            drop(writer);
            let master = layout.master(AudioFormat::Caf48kFloat32);
            let bytes = std::fs::read(&master).unwrap();
            std::fs::write(&master, &bytes[..30]).unwrap();
        });
        let three_channels = || {
            orphan(&harness, |layout| {
                std::fs::create_dir_all(&layout.directory).unwrap();
                let master = layout.master(AudioFormat::Caf48kFloat32);
                let mut writer = CafStreamWriter::create(&master, 48_000.0, 3).unwrap();
                writer.write(&[0.25; 3 * 480], 480).unwrap();
            })
        };
        let unreadable = [three_channels(), three_channels()];
        let cut_master = master_path(&harness.audio_folder(), cut);
        let cut_bytes = std::fs::read(&cut_master).unwrap();

        for launch in 1..=2 {
            let (adopted, logged) =
                warnings(|| reconcile_at_launch(&harness, &[], &an_hour_later()));
            assert_eq!(adopted, Vec::<Uuid>::new());
            let recorded = crate::audio_folders::recorded(&harness.support_directory()).unwrap();
            assert!(!recorded.contains_key(&cut), "launch {launch}");
            assert!(unreadable.iter().all(|id| recorded.contains_key(id)));
            let lines: Vec<&str> = logged
                .lines()
                .filter(|line| line.contains("recordings with no meeting"))
                .collect();
            let [line] = lines.as_slice() else {
                panic!("launch {launch}, one line: {logged}");
            };
            assert!(line.contains("could not be recovered"), "{line}");
            assert!(line.contains("count=2"), "{line}");
            assert!(!logged.contains(&cut.to_string()), "{logged}");
        }
        assert_eq!(std::fs::read(&cut_master).unwrap(), cut_bytes);
    }

    /// An entry whose folder is empty, missing or unreadable stays; once the
    /// volume is back the launch adopts it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_entry_whose_folder_cannot_be_told_empty_stays_recorded() {
        let harness = Harness::new();
        let mount = harness.dir.path().join("volume");
        std::fs::create_dir_all(&mount).unwrap();
        let unmounted = Uuid::new_v4();
        let gone = Uuid::new_v4();
        crate::audio_folders::record(&harness.support_directory(), unmounted, &mount).unwrap();
        crate::audio_folders::record(
            &harness.support_directory(),
            gone,
            &harness.dir.path().join("gone"),
        )
        .unwrap();
        assert_eq!(
            reconcile_at_launch(&harness, &[], &an_hour_later()),
            Vec::<Uuid>::new()
        );
        let recorded = crate::audio_folders::recorded(&harness.support_directory()).unwrap();
        assert!(recorded.contains_key(&unmounted) && recorded.contains_key(&gone));

        #[cfg(unix)]
        {
            let locked = harness.dir.path().join("locked");
            std::fs::create_dir_all(locked.join("other")).unwrap();
            let unreadable = Uuid::new_v4();
            crate::audio_folders::record(&harness.support_directory(), unreadable, &locked)
                .unwrap();
            if lock_out(&locked, || std::fs::read_dir(&locked).is_ok()) {
                let adopted = reconcile_at_launch(&harness, &[], &an_hour_later());
                set_mode(&locked, 0o755);
                assert_eq!(adopted, Vec::<Uuid>::new());
                assert!(
                    crate::audio_folders::recorded(&harness.support_directory())
                        .unwrap()
                        .contains_key(&unreadable)
                );
            }
        }

        let layout = RecordingLayout::new(&mount, unmounted);
        let mut writer = RecordingWriter::new(&layout, &[AudioLane::Mixed], false).unwrap();
        write_frames(&mut writer, 50);
        drop(writer);
        assert_eq!(
            reconcile_at_launch(&harness, &[], &an_hour_later()),
            [unmounted]
        );
        harness.pipeline.current().wait_until_idle().await;
    }

    /// Unrecorded masters are logged in one line; a folder whose id has a
    /// row is not looked into.
    #[test]
    fn unrecorded_masters_are_logged_once_and_a_rows_folder_is_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let audio = dir.path().join("audio");
        let master = |id: Uuid| {
            let layout = RecordingLayout::new(&audio, id);
            std::fs::create_dir_all(&layout.directory).unwrap();
            std::fs::write(layout.master(AudioFormat::Caf48kFloat32), [1; 100]).unwrap();
        };
        let unrecorded: Vec<Uuid> = (0..5).map(|_| Uuid::new_v4()).collect();
        unrecorded.iter().copied().for_each(master);
        let (row, orphan) = (Uuid::new_v4(), Uuid::new_v4());
        master(row);
        master(orphan);
        let recorded = BTreeMap::from([(row, audio.clone()), (orphan, audio.clone())]);

        let (found, logged) = warnings(|| {
            orphans(
                std::slice::from_ref(&audio),
                &HashSet::from([row]),
                &recorded,
            )
        });

        let ids: Vec<Uuid> = found.iter().map(|orphan| orphan.meeting_id).collect();
        assert_eq!(ids, [orphan]);
        let lines: Vec<&str> = logged.lines().collect();
        let [line] = lines.as_slice() else {
            panic!("one line: {logged}");
        };
        assert!(line.contains("count=5"), "{line}");
        let named = unrecorded
            .iter()
            .filter(|id| line.contains(&id.to_string()))
            .count();
        assert_eq!(named, IDS_LOGGED, "{line}");
        assert!(!line.contains(&row.to_string()), "{line}");
    }

    /// Unrecorded meeting folders with masters and sidecars; returns every
    /// file of theirs with its bytes, each time it is called.
    fn unrecorded_orphans(harness: &Harness) -> impl Fn() -> BTreeMap<PathBuf, Vec<u8>> {
        let mut folders = Vec::new();
        for (lanes, finish) in [
            (vec![AudioLane::Mic, AudioLane::System], false),
            (vec![AudioLane::Mixed], false),
            (vec![AudioLane::Mic, AudioLane::System], true),
        ] {
            let layout = RecordingLayout::new(&harness.audio_folder(), Uuid::new_v4());
            let mut writer = RecordingWriter::new(&layout, &lanes, false).unwrap();
            write_frames(&mut writer, 40);
            if finish {
                writer.finish().unwrap();
            }
            folders.push(layout.directory);
        }
        for (name, format) in [
            ("tone-440-44k1-500ms.m4a", AudioFormat::M4aAac),
            ("conversation-mic-6s.wav", AudioFormat::Wav16kInt16),
        ] {
            let layout = RecordingLayout::new(&harness.audio_folder(), Uuid::new_v4());
            std::fs::create_dir_all(&layout.directory).unwrap();
            std::fs::copy(audio_fixture(name), layout.master(format)).unwrap();
            folders.push(layout.directory);
        }
        let known = harness.dir.path().join("known");
        crate::audio_folders::remember(&harness.support_directory(), &known).unwrap();
        let layout = RecordingLayout::new(&known, Uuid::new_v4());
        let mut writer = RecordingWriter::new(&layout, &[AudioLane::Mixed], false).unwrap();
        write_frames(&mut writer, 40);
        drop(writer);
        folders.push(layout.directory);
        move || folders.iter().flat_map(|folder| files_in(folder)).collect()
    }

    /// Unrecorded masters are never deleted or changed, by a launch, a
    /// retention sweep or another meeting's delete.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn unadopted_orphans_are_never_deleted_or_changed() {
        let harness = Harness::new();
        let mut settings = harness.store.settings().unwrap();
        settings.default_retention = AudioRetention::DeleteAfterProcessing;
        harness.store.save_settings(&settings).unwrap();
        let snapshot = unrecorded_orphans(&harness);
        let before = snapshot();
        assert!(
            before.keys().any(|path| path.ends_with("mic.wav")),
            "{:?}",
            before.keys()
        );

        // A meeting saved as a stop saves it, and one failed with no asset.
        let saved = harness.begin(MeetingSource::MacInPerson);
        harness.record(&saved, &harness.audio_folder());
        let mut writer = harness.writer(&saved);
        write_frames(&mut writer, 30);
        let files = writer.finish().unwrap();
        let mut result = finished(&saved, &files);
        result.asset.retention = AudioRetention::DeleteAfterProcessing;
        harness
            .intake()
            .complete(saved.id, result, None)
            .await
            .unwrap();
        let failed = harness.begin(MeetingSource::MacInPerson);
        harness.record(&failed, &harness.audio_folder());
        harness.intake().fail(failed.id, "refused").unwrap();

        assert_eq!(
            reconcile_at_launch(&harness, &[], &an_hour_later()),
            Vec::<Uuid>::new()
        );
        harness.pipeline.current().wait_until_idle().await;
        steno_pipeline::RetentionSweep::new(harness.store.clone())
            .run(Utc::now() + chrono::Duration::days(1))
            .unwrap();
        assert_eq!(snapshot(), before);

        let mut list = steno_host::main_window::MeetingListViewModel::new(
            chrono::FixedOffset::east_opt(0).unwrap(),
        );
        assert!(list.delete(
            saved.id,
            &harness.store,
            &steno_host::services::RealFileSystem,
            None,
            false,
        ));
        let left = steno_host::services::LeftRecording {
            folders: meeting_folders(&harness.store, &harness.support_directory(), failed.id),
            still_written: false,
        };
        assert!(left.folders.contains(&harness.dir.path().join("known")));
        assert!(list.delete(
            failed.id,
            &harness.store,
            &steno_host::services::RealFileSystem,
            Some(&left),
            false,
        ));
        assert_eq!(
            reconcile_at_launch(&harness, &[], &an_hour_later()),
            Vec::<Uuid>::new()
        );
        harness.pipeline.current().wait_until_idle().await;

        assert_eq!(harness.store.all_meetings().unwrap(), Vec::<Meeting>::new());
        assert_eq!(snapshot(), before);
    }
}
