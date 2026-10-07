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
//! The launch leaves a recording alone while its master still grows: the
//! Swift app and an older Rust build share the database and take no lock,
//! so a row left `recording` may be one another process is still writing.
//! Only a master modified within [`LiveRecordingCheck::fresh_within`] is
//! sampled twice; an old crash's master is recovered at once.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use chrono::Utc;
use steno_audio::writer::{CafHeader, CafReadError, WavStreamWriter};
use steno_core::{
    AudioAsset, AudioFormat, AudioLane, AudioRetention, Meeting, MeetingSource, RecordingEndReason,
    RecordingLayout, Store,
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
    /// The settings name no audio folder this build can read.
    #[error("the audio folder is not a file URL")]
    NoAudioFolder,
    /// The writer never created the master, or it is gone.
    #[error("no master at {}", .0.display())]
    NoMaster(PathBuf),
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
#[must_use]
pub fn master_path(audio_folder: &Path, meeting_id: Uuid) -> PathBuf {
    RecordingLayout::new(audio_folder, meeting_id).master(AudioFormat::Caf48kFloat32)
}

/// The recording the master of meeting `meeting_id`, recorded from
/// `source`, holds in `audio_folder`, as its capture
/// would have handed it over: the asset rebuilt from the master's header
/// alone (whole frames only), the lanes of the meeting's source, the
/// default retention (`retention` is set by `complete`), and the end reason
/// `failed`. A sidecar whose writer died is finished from its length
/// ([`WavStreamWriter::recover`]) and listed when it holds samples; one
/// that cannot be is left out, and the decoder rebuilds that lane from the
/// master.
pub fn salvage(
    audio_folder: &Path,
    meeting_id: Uuid,
    source: MeetingSource,
) -> Result<RecordingResult, Unrecoverable> {
    let lanes = lanes(source)?;
    let layout = RecordingLayout::new(audio_folder, meeting_id);
    let master = layout.master(AudioFormat::Caf48kFloat32);
    if !master.is_file() {
        return Err(Unrecoverable::NoMaster(master));
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

/// [Salvages](salvage) the master of meeting `meeting_id` in
/// `audio_folder` and queues the meeting through `intake`, under the
/// settings' default retention.
pub async fn recover(
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
    /// A master modified longer ago than this is a crash's: recovered at once.
    pub fresh_within: Duration,
    /// How long apart a fresh master's size is sampled.
    pub sample_gap: Duration,
    pub now: Arc<dyn Fn() -> SystemTime + Send + Sync>,
    pub wait: Arc<dyn Fn(Duration) + Send + Sync>,
}

impl Default for LiveRecordingCheck {
    /// Ten seconds and 1.5 s: the writer appends a frame every 10 ms.
    fn default() -> Self {
        LiveRecordingCheck {
            fresh_within: Duration::from_secs(10),
            sample_gap: Duration::from_millis(1_500),
            now: Arc::new(SystemTime::now),
            wait: Arc::new(std::thread::sleep),
        }
    }
}

impl LiveRecordingCheck {
    /// Whether each of `masters` grew between two samples; a master that
    /// is missing, old, or the same size both times is not live. One
    /// wait covers every fresh master, and none is waited for when no
    /// master is fresh.
    fn growing(&self, masters: &[PathBuf]) -> Vec<bool> {
        let now = (self.now)();
        let first: Vec<Option<u64>> = masters
            .iter()
            .map(|path| {
                let metadata = std::fs::metadata(path).ok()?;
                let modified = metadata.modified().ok()?;
                // A time ahead of the clock counts as fresh.
                let fresh = now
                    .duration_since(modified)
                    .map_or(true, |age| age < self.fresh_within);
                fresh.then_some(metadata.len())
            })
            .collect();
        if first.iter().all(Option::is_none) {
            return vec![false; masters.len()];
        }
        (self.wait)(self.sample_gap);
        masters
            .iter()
            .zip(first)
            .map(|(path, first)| {
                first.is_some_and(|first| {
                    std::fs::metadata(path).is_ok_and(|metadata| metadata.len() != first)
                })
            })
            .collect()
    }
}

/// What the launch did with the meetings a process left `recording`.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Reconciled {
    /// Recovered and queued.
    pub recovered: Vec<Uuid>,
    /// Their master still grows: left `recording`.
    pub live: Vec<Uuid>,
    /// Recovered but not saved: left `recording` for the next launch.
    pub kept: Vec<Uuid>,
    /// Nothing to recover: failed with Swift's reason.
    pub failed: Vec<Uuid>,
}

/// The launch's reconciliation of `meetings`, the rows a process left
/// `recording` (listed by the caller before anything could start a
/// recording here): each whose master still grows is left alone, each
/// with a master is recovered and queued through `intake`, and the rest
/// are failed with [`Store::INTERRUPTED_RECORDING_REASON`] through
/// [`Store::fail_recordings`], which skips a row another process has
/// moved on meanwhile. Blocks for [`LiveRecordingCheck::sample_gap`] when
/// a master is fresh, so the caller runs it off the main thread. Swift:
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
    let audio_folder = match store.settings() {
        Ok(settings) => file_url_path(&settings.audio_folder),
        Err(error) => {
            // Nothing can be told apart without the folder; the next launch
            // tries again.
            tracing::warn!(%error, "interrupted recordings left for the next launch");
            reconciled.kept = meetings.iter().map(|meeting| meeting.id).collect();
            return reconciled;
        }
    };
    let masters: Vec<PathBuf> = meetings
        .iter()
        .map(|meeting| {
            audio_folder
                .as_deref()
                .map(|folder| master_path(folder, meeting.id))
                .unwrap_or_default()
        })
        .collect();
    let growing = check.growing(&masters);
    let mut unrecovered = Vec::new();
    for (meeting, growing) in meetings.iter().zip(growing) {
        if growing {
            tracing::info!(meeting_id = %meeting.id, "a recording another process is writing was left alone");
            reconciled.live.push(meeting.id);
            continue;
        }
        let recovered = match audio_folder.as_deref() {
            Some(folder) => {
                crate::block_on(runtime, recover(intake, folder, meeting.id, meeting.source))
            }
            None => Err(Unrecoverable::NoAudioFolder.into()),
        };
        // Warn names the meeting; the error, which can name its folder,
        // goes to debug.
        match recovered {
            Ok(_) => {
                tracing::info!(meeting_id = %meeting.id, "an interrupted recording was recovered");
                reconciled.recovered.push(meeting.id);
            }
            Err(RecoveryError::Unrecoverable(error)) => {
                tracing::warn!(meeting_id = %meeting.id, "an interrupted recording could not be recovered");
                tracing::debug!(meeting_id = %meeting.id, %error, "unrecoverable recording");
                unrecovered.push(meeting.id);
            }
            Err(RecoveryError::NotSaved(error)) => {
                tracing::warn!(meeting_id = %meeting.id, "a recovered recording could not be saved; the next launch tries again");
                tracing::debug!(meeting_id = %meeting.id, %error, "recovered recording not saved");
                reconciled.kept.push(meeting.id);
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

    use steno_audio::testing::AudioFixtures;
    use steno_audio::writer::{
        CafStreamWriter, LaneFrames, RecordingWriter, RecordingWriting, WavFile,
    };
    use steno_audio::{FRAME_SIZE, SAMPLE_RATE};
    use steno_core::{MeetingState, MeetingStateKind};
    use steno_pipeline::RetentionSweep;

    use super::*;
    use crate::pipeline::CurrentPipeline;
    use crate::testing::{current_pipeline, fake_dependencies, temp_store};

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

    /// `frames` frames of a tone on every lane of `writer`.
    fn write_frames(writer: &mut RecordingWriter, frames: usize) {
        let lanes = writer.lanes().len();
        let tone = AudioFixtures::tone(440.0, 0.01, 0.5);
        let slices: Vec<&[f32]> = (0..lanes).map(|_| &tone[..FRAME_SIZE]).collect();
        for _ in 0..frames {
            writer
                .write(&LaneFrames {
                    frame_count: FRAME_SIZE,
                    lanes: &slices,
                    raw_mic: None,
                })
                .unwrap();
        }
    }

    /// A check whose clock reads an hour after now: every master is a
    /// crash's, and none is waited for.
    fn an_hour_later() -> LiveRecordingCheck {
        LiveRecordingCheck {
            now: Arc::new(|| SystemTime::now() + Duration::from_secs(3_600)),
            wait: Arc::new(|_| panic!("an old master is not waited for")),
            ..LiveRecordingCheck::default()
        }
    }

    /// A check that finds every master fresh and runs `between` in place
    /// of the wait.
    fn sampled(between: impl Fn() + Send + Sync + 'static) -> LiveRecordingCheck {
        LiveRecordingCheck {
            now: Arc::new(SystemTime::now),
            wait: Arc::new(move |_| between()),
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
    /// appends between the two samples) leaves its row `recording`, with
    /// no asset; a fresh master that did not grow is recovered.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_master_that_still_grows_is_left_recording() {
        let harness = Harness::new();
        let meeting = harness.begin(MeetingSource::MacInPerson);
        let mut writer = harness.writer(&meeting);
        write_frames(&mut writer, 10);
        let writer = Arc::new(Mutex::new(writer));
        let appending = {
            let writer = writer.clone();
            sampled(move || write_frames(&mut writer.lock().unwrap(), 1))
        };
        let reconciled = harness.reconcile(std::slice::from_ref(&meeting), &appending);
        assert_eq!(reconciled.live, [meeting.id]);
        assert_eq!(reconciled.failed, Vec::<Uuid>::new());
        assert_eq!(harness.state(&meeting), MeetingState::Recording);
        assert!(harness.store.asset(meeting.id).unwrap().is_none());

        // The writer stops; the next launch finds the master fresh but
        // still, and recovers it.
        drop(writer);
        let reconciled = harness.reconcile(std::slice::from_ref(&meeting), &sampled(|| {}));
        assert_eq!(reconciled.recovered, [meeting.id]);
        assert_eq!(
            harness.store.meeting(meeting.id).unwrap().unwrap().duration,
            5_280.0 / SAMPLE_RATE
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
        let result = RecordingResult {
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
        };

        let other = Store::open(harness.dir.path().join("steno.sqlite")).unwrap();
        let (release, released) = std::sync::mpsc::channel::<()>();
        let (held, holding) = std::sync::mpsc::channel();
        let holder = std::thread::spawn(move || {
            other
                .write(|_| {
                    held.send(()).unwrap();
                    let _ = released.recv();
                    Ok(())
                })
                .unwrap();
        });
        holding.recv().unwrap();
        let error = harness
            .intake()
            .complete(meeting.id, result, None)
            .await
            .unwrap_err();
        assert!(error.is_busy(), "{error}");
        drop(release);
        holder.join().unwrap();
        assert_eq!(harness.state(&meeting), MeetingState::Recording);

        let reconciled = harness.reconcile(std::slice::from_ref(&meeting), &an_hour_later());
        assert_eq!(reconciled.recovered, [meeting.id]);
        let recovered = harness.store.meeting(meeting.id).unwrap().unwrap();
        assert_eq!(recovered.duration, 0.2);
        harness.pipeline.current().wait_until_idle().await;
        assert_eq!(harness.state(&meeting), MeetingState::Ready);
    }
}
