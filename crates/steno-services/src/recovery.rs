//! Recovery of an interrupted recording. A meeting is written `recording`
//! when its capture starts and gets its asset only when the capture stops,
//! so a recording ended by a crash, a `kill -9`, a power loss or a logout
//! cut short has a row and no asset. The master on disk is readable to its
//! last whole frame (`steno_audio::writer::caf`), so the launch rebuilds the
//! asset from the master's header and queues the meeting through the Mac
//! intake, with the end reason `failed` ("the capture failed; the recording
//! so far was kept"). The recorder does the same for a stop whose capture
//! failed. Rust only: Swift had no recovery.
//!
//! A row is failed only when its master is provably not there. The launch
//! looks first in the two folders that decide it: the one the recorder
//! recorded the meeting into before it wrote the row (the record in
//! [`crate::audio_folders`]), so a recording is found where it started
//! after the user picked another folder while it ran, and the settings'
//! audio folder. Either one missing or unreadable (an unmounted
//! volume, a permission not granted yet, an I/O error) keeps the row
//! `recording` for the next launch, and so does a record that cannot be
//! read. Then it looks in the known folders
//! ([`crate::audio_folders::known`]) and the folder of every stored asset
//! (`other_folders`), skipping one that is missing or unreadable, so a
//! folder retired for good does not keep a row forever.
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
//! | Item | What it does |
//! |------|--------------|
//! | `Interrupted` | What the launch lists before anything can record: the rows left `recording`, the recorded and the known folders |
//! | `reconcile_interrupted` | The launch's pass over those rows, with what it found in `Reconciled` |
//! | `recover` | Finds, salvages and queues one meeting; the recorder calls it after a failed stop |
//! | `other_folders`, `find_master` | Where else a master may be, and which folder holds it (`Lookup`) |
//! | `salvage` | The asset a master's header and sidecars give, as its capture would have handed it over |
//! | [`LiveRecordingCheck`] | When a master counts as still written; the app's field, so tests inject the clock, and the recorder's check before a delete |
//!
//! All but [`LiveRecordingCheck`] are this crate's own.

use std::collections::BTreeMap;
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
    /// A phone meeting: its audio arrives whole through the handover.
    #[error("a {} meeting is not captured on this computer", .0.as_str())]
    NotCaptured(MeetingSource),
    /// The writer never created the master, or it is gone.
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
/// the recorder configures its capture.
fn lanes(source: MeetingSource) -> Result<Vec<AudioLane>, Unrecoverable> {
    match source {
        MeetingSource::MacCall => Ok(steno_audio::CaptureMode::Call.lanes()),
        MeetingSource::MacInPerson => Ok(steno_audio::CaptureMode::InPerson.lanes()),
        MeetingSource::Phone => Err(Unrecoverable::NotCaptured(source)),
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

/// The recording the master of `meeting_id` in `audio_folder` holds, as
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
    audio_folder: &Path,
    meeting_id: Uuid,
    source: MeetingSource,
) -> Result<RecordingResult, RecoveryError> {
    let lanes = lanes(source)?;
    let layout = RecordingLayout::new(audio_folder, meeting_id);
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
    let result = salvage(audio_folder, meeting_id, source)?;
    Ok(intake.complete(meeting_id, result, None).await?)
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
/// on meanwhile. The record forgets every listed entry whose meeting is no
/// longer left `recording`, also when no row was left `recording`. Blocks
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
    forget_settled(interrupted, &reconciled);
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
    let lookups: Vec<Lookup> = meetings
        .iter()
        .map(|meeting| {
            let recorded = interrupted
                .recorded
                .as_ref()
                .and_then(|recorded| recorded.get(&meeting.id).cloned());
            let deciding = distinct(recorded.into_iter().chain(current.clone()), &[]);
            let others = distinct(others.iter().cloned(), &deciding);
            match find_master(&deciding, &others, meeting.id) {
                // Settings whose audio folder this build cannot read, or a
                // record that cannot be read, may name the folder a master
                // is in: only a master found is recovered.
                Lookup::Absent if current.is_none() || interrupted.recorded.is_none() => {
                    Lookup::Unreachable
                }
                lookup => lookup,
            }
        })
        .collect();
    let masters: Vec<Option<PathBuf>> = meetings
        .iter()
        .zip(&lookups)
        .map(|(meeting, lookup)| match lookup {
            Lookup::Found(folder) => Some(master_path(folder, meeting.id)),
            Lookup::Absent | Lookup::Unreachable => None,
        })
        .collect();
    let written = check.still_written(&masters);
    let mut unrecovered = Vec::new();
    // Warn names the meeting; an error, which can name its folder, goes
    // to debug.
    for ((meeting, lookup), written) in meetings.iter().zip(lookups).zip(written) {
        let meeting_id = meeting.id;
        let recovered = match lookup {
            Lookup::Found(_) if written => {
                tracing::warn!(%meeting_id, "a recording another process is writing was left alone");
                reconciled.live.push(meeting_id);
                continue;
            }
            Lookup::Found(folder) => {
                crate::block_on(runtime, queue(intake, &folder, meeting_id, meeting.source))
            }
            Lookup::Unreachable => Err(RecoveryError::Unreachable),
            Lookup::Absent => Err(Unrecoverable::NoMaster.into()),
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
/// that `reconciled` did not leave `recording`: recovered, failed, or
/// moved on before the launch. An entry recorded since the launch listed
/// the record stays.
fn forget_settled(interrupted: &Interrupted, reconciled: &Reconciled) {
    let Some(recorded) = &interrupted.recorded else {
        return;
    };
    let left = [&reconciled.live, &reconciled.unreachable, &reconciled.kept];
    let settled: Vec<Uuid> = recorded
        .keys()
        .copied()
        .filter(|id| !left.iter().any(|ids| ids.contains(id)))
        .collect();
    if settled.is_empty() {
        return;
    }
    if let Err(error) = crate::audio_folders::forget(&interrupted.support_directory, &settled) {
        tracing::debug!(%error, "settled recording folders not forgotten");
    }
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
            RecordingWriter::new(&layout, &lanes(meeting.source).unwrap(), false).unwrap()
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
    fn reconcile_at_launch(harness: &Harness, meetings: &[Meeting], check: &LiveRecordingCheck) {
        crate::app::reconcile_at_launch(
            &harness.store,
            &harness.pipeline,
            &harness.interrupted(meetings),
            check,
            chrono::FixedOffset::east_opt(0).unwrap(),
            &tokio::runtime::Handle::current(),
        );
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
    /// meetings that moved on: one failed since, and one that is gone.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_launch_with_no_row_left_recording_forgets_the_settled_entries() {
        let harness = Harness::new();
        let failed = harness.begin(MeetingSource::MacCall);
        harness.record(&failed, &harness.audio_folder());
        harness.intake().fail(failed.id, "refused").unwrap();
        let gone = Uuid::new_v4();
        crate::audio_folders::record(&harness.support_directory(), gone, &harness.audio_folder())
            .unwrap();

        assert_eq!(
            harness.reconcile(&[], &an_hour_later()),
            Reconciled::default()
        );
        assert!(
            crate::audio_folders::recorded(&harness.support_directory())
                .unwrap()
                .is_empty()
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
