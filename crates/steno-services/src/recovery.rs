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
//! A row is failed only when its master is provably not there: the launch
//! looks in the settings' audio folder and in every folder an asset's
//! master sits in ([`audio_folders`]), since the user may have picked
//! another folder while the recording ran. A folder that is missing or
//! cannot be read (an unmounted volume, a permission not granted yet, an
//! I/O error) keeps the row `recording` for the next launch.
//!
//! The launch also leaves a recording alone while its master is still
//! written: the Swift app and an older Rust build share the database and
//! take no lock, so a row left `recording` may be one another process is
//! still writing. A master modified within
//! [`LiveRecordingCheck::fresh_within`] is watched until it is that old;
//! an old crash's master is recovered at once.

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
pub enum Unrecoverable {
    /// A phone meeting: its audio arrives whole through the handover.
    #[error("a {} meeting is not captured on this computer", .0.as_str())]
    NotCaptured(MeetingSource),
    /// The writer never created the master, or it is gone.
    #[error("no audio folder holds the master")]
    NoMaster,
    /// Not a CAF the writer wrote, or a read failed.
    #[error(transparent)]
    Unreadable(#[from] CafReadError),
    /// The master's channels are not the lanes the source records.
    #[error("the master has {found} channels; the meeting records {expected} lanes")]
    WrongChannels { found: usize, expected: usize },
    /// A header and no whole frame.
    #[error("the master holds no audio")]
    Empty,
}

/// Why a recovery did not queue the meeting.
#[derive(Debug, thiserror::Error)]
pub enum RecoveryError {
    #[error(transparent)]
    Unrecoverable(#[from] Unrecoverable),
    /// A folder the master may be in could not be read; the meeting stays
    /// `recording` for the next launch.
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
fn master_path(audio_folder: &Path, meeting_id: Uuid) -> PathBuf {
    RecordingLayout::new(audio_folder, meeting_id).master(AudioFormat::Caf48kFloat32)
}

/// Every audio folder a master may be in, each once: the one `settings`
/// name first (`None` when it is not a file URL), then the folder of each
/// stored asset (the folder above its meeting's folder), so a recording
/// started before the user picked another folder is found where it was
/// written.
pub fn audio_folders(store: &Store, current: Option<PathBuf>) -> Result<Vec<PathBuf>, StoreError> {
    let mut folders: Vec<PathBuf> = current.into_iter().collect();
    for url in store.asset_urls()? {
        let folder =
            file_url_path(&url).and_then(|master| master.parent()?.parent().map(Path::to_path_buf));
        if let Some(folder) = folder
            && !folders.contains(&folder)
        {
            folders.push(folder);
        }
    }
    Ok(folders)
}

/// Where a meeting's master was looked for, and what was found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lookup {
    /// The audio folder that holds the master.
    Found(PathBuf),
    /// Every folder was read and none holds it.
    Absent,
    /// None holds it, but a folder is missing or could not be read, so
    /// the master may be there.
    Unreachable,
}

/// Looks for the master of `meeting_id` in each of `folders`, in order.
/// A folder that is missing counts as unreadable: an unmounted volume
/// is missing until it is mounted again.
#[must_use]
pub fn find_master(folders: &[PathBuf], meeting_id: Uuid) -> Lookup {
    let mut unreachable = folders.is_empty();
    for folder in folders {
        match std::fs::metadata(master_path(folder, meeting_id)) {
            Ok(metadata) if metadata.is_file() => return Lookup::Found(folder.clone()),
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if std::fs::read_dir(folder).is_err() {
                    unreachable = true;
                }
            }
            Err(_) => unreachable = true,
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
/// end reason `failed`. A sidecar whose writer died is finished from its
/// length ([`WavStreamWriter::recover`]) and listed when it holds samples;
/// one that cannot be is left out, and the decoder rebuilds that lane
/// from the master.
pub fn salvage(
    audio_folder: &Path,
    meeting_id: Uuid,
    source: MeetingSource,
) -> Result<RecordingResult, Unrecoverable> {
    let lanes = lanes(source)?;
    let layout = RecordingLayout::new(audio_folder, meeting_id);
    let master = layout.master(AudioFormat::Caf48kFloat32);
    if !master.is_file() {
        return Err(Unrecoverable::NoMaster);
    }
    let header = CafHeader::read(&master)?;
    if header.channel_count != lanes.len() {
        return Err(Unrecoverable::WrongChannels {
            found: header.channel_count,
            expected: lanes.len(),
        });
    }
    if header.frame_count == 0 {
        return Err(Unrecoverable::Empty);
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

/// Looks for the master of meeting `meeting_id` in `folders`
/// ([`find_master`]), [salvages](salvage) it and queues the meeting
/// through `intake`, under the settings' default retention.
pub async fn recover(
    intake: &LocalRecordingIntake,
    folders: &[PathBuf],
    meeting_id: Uuid,
    source: MeetingSource,
) -> Result<Meeting, RecoveryError> {
    let folder = match find_master(folders, meeting_id) {
        Lookup::Found(folder) => folder,
        Lookup::Unreachable => return Err(RecoveryError::Unreachable),
        Lookup::Absent => return Err(Unrecoverable::NoMaster.into()),
    };
    let result = salvage(&folder, meeting_id, source)?;
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
    /// Ten seconds: the writer appends a frame every 10 ms.
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

/// What the launch did with the meetings a process left `recording`.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Reconciled {
    /// Recovered and queued.
    pub recovered: Vec<Uuid>,
    /// Their master is still written: left `recording`.
    pub live: Vec<Uuid>,
    /// A folder their master may be in could not be read: left
    /// `recording` for the next launch.
    pub unreachable: Vec<Uuid>,
    /// Recovered but not saved: left `recording` for the next launch.
    pub kept: Vec<Uuid>,
    /// Nothing to recover: failed with Swift's reason.
    pub failed: Vec<Uuid>,
}

/// The launch's reconciliation of `meetings`, the rows a process left
/// `recording` (listed by the caller before anything could start a
/// recording here). Each master is looked for in [`audio_folders`]. A
/// meeting whose master is still written, or that may be in a folder that
/// cannot be read now, is left alone; one with a master is recovered and
/// queued through `intake`; the rest are failed with
/// [`Store::INTERRUPTED_RECORDING_REASON`] through
/// [`Store::fail_recordings`], which skips a row another process has
/// moved on meanwhile. Blocks for up to
/// [`LiveRecordingCheck::fresh_within`] when a master is fresh, so the
/// caller runs it off the main thread. Swift:
/// `MeetingStore.failInterruptedRecordings` in `AppController.launch`, the
/// failing part alone.
pub fn reconcile_interrupted(
    store: &Store,
    intake: &LocalRecordingIntake,
    meetings: &[Meeting],
    check: &LiveRecordingCheck,
    runtime: &tokio::runtime::Handle,
) -> Reconciled {
    let mut reconciled = Reconciled::default();
    if meetings.is_empty() {
        return reconciled;
    }
    let folders = store.settings().and_then(|settings| {
        let current = file_url_path(&settings.audio_folder);
        let named = current.is_some();
        audio_folders(store, current).map(|folders| (folders, named))
    });
    let (folders, named) = match folders {
        Ok(folders) => folders,
        Err(error) => {
            // Nothing can be told apart without the folders; the next
            // launch tries again.
            tracing::warn!(%error, "interrupted recordings left for the next launch");
            reconciled.kept = meetings.iter().map(|meeting| meeting.id).collect();
            return reconciled;
        }
    };
    // Settings whose audio folder this build cannot read may name the one
    // a master is in: only a master found elsewhere is recovered.
    let lookups: Vec<Lookup> = meetings
        .iter()
        .map(|meeting| match find_master(&folders, meeting.id) {
            Lookup::Absent if !named => Lookup::Unreachable,
            lookup => lookup,
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
        let folder = match lookup {
            Lookup::Found(_) if written => {
                tracing::warn!(%meeting_id, "a recording another process is writing was left alone");
                reconciled.live.push(meeting_id);
                continue;
            }
            Lookup::Found(folder) => folder,
            Lookup::Unreachable => {
                tracing::warn!(%meeting_id, "an interrupted recording's folder cannot be read now; the next launch tries again");
                reconciled.unreachable.push(meeting_id);
                continue;
            }
            Lookup::Absent => {
                tracing::warn!(%meeting_id, "an interrupted recording has no audio on disk");
                unrecovered.push(meeting_id);
                continue;
            }
        };
        let recovered = crate::block_on(
            runtime,
            recover(intake, &[folder], meeting_id, meeting.source),
        );
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
    if unrecovered.is_empty() {
        return reconciled;
    }
    match store.fail_recordings(
        &unrecovered,
        Store::INTERRUPTED_RECORDING_REASON,
        Utc::now(),
    ) {
        Ok(failed) => reconciled.failed = failed,
        Err(error) => tracing::warn!(%error, "interrupted recordings could not be marked"),
    }
    reconciled
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use steno_audio::writer::{CafStreamWriter, RecordingWriter, RecordingWriting as _, WavFile};
    use steno_audio::{FRAME_SIZE, SAMPLE_RATE};
    use steno_core::testing::WriteLockHold;
    use steno_core::{MeetingState, MeetingStateKind};
    use steno_pipeline::RetentionSweep;

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
                .begin(source, None, None, &[], Utc::now())
                .unwrap()
        }

        /// The production writer for `meeting`, as its capture opens it.
        fn writer(&self, meeting: &Meeting) -> RecordingWriter {
            let layout = RecordingLayout::new(&self.audio_folder(), meeting.id);
            RecordingWriter::new(&layout, &lanes(meeting.source).unwrap(), false).unwrap()
        }

        fn reconcile(&self, meetings: &[Meeting], check: &LiveRecordingCheck) -> Reconciled {
            reconcile_interrupted(
                &self.store,
                &self.intake(),
                meetings,
                check,
                &tokio::runtime::Handle::current(),
            )
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
            &RetentionSweep::new(harness.store.clone()),
            meetings,
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
        let set_folder = |folder: &Path| {
            let mut settings = harness.store.settings().unwrap();
            settings.audio_folder = file_url(folder, true);
            harness.store.save_settings(&settings).unwrap();
        };
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
}
