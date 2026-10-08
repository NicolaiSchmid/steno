//! The host's `Recorder` over the capture session and the Mac recording
//! intake. Swift: `apps/macos/Steno/Recording/RecordingController.swift`.
//! The calendar lookup, the auto-stop after a call ends and the detection
//! prompt are WP5's recorder policy (the plan's parity list); the status
//! carries what the capture session reports.
//!
//! Each recording has a watcher thread. A session that fails on its own (a
//! device that stayed lost, a write that failed on a full disk) is finished
//! there as Stop would finish it: the recording so far is saved and queued,
//! and the status says in plain words why it ended. Swift: the states task
//! of `RecordingController.observe`, which said "Recording failed:
//! `<error>`" and kept the device-loss warning beside it; here the one
//! error line also says the recording is saved. The watcher also keeps an
//! eye on the free space on the volumes of the recordings folder and of
//! the database, the smaller of the two: a recording warns when about half
//! an hour is left and, on Linux and Windows, does not start without room
//! and stops and is saved before the disk fills. On the Mac a low reading
//! only warns, since `statvfs` leaves out the space APFS would purge for
//! the user, and a disk that is truly full can fail the save. A volume that reports no size, or more space free than it
//! holds (some network and FUSE file systems), counts as unreadable and
//! never warns, stops or refuses a recording. Rust only: Swift had no disk
//! check.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, Weak};
use std::thread::JoinHandle;
use std::time::Duration;

use chrono::{FixedOffset, Utc};
use steno_audio::{
    CaptureConfiguration, CaptureError, CaptureNotice, CaptureSession, CaptureState,
    CaptureStatistics, CaptureStream, FRAMES_PER_SECOND, LaneLevels as AudioLevels,
};
use steno_bridge::{CaptureMode, PermissionKind, RecordingState};
use steno_core::{MeetingSource, RecordingEndReason, Store};
use steno_host::services::{LaneLevels, Permissions, Recorder, RecorderStatus, SpeechModels};
use steno_host::speech::ModelAsset;
use steno_pipeline::{LocalRecordingIntake, RecordingResult};
use steno_speech::SpeechRuntime;
use uuid::Uuid;

use crate::block_on;
use crate::pipeline::CurrentPipeline;

/// Builds a capture session for a configuration; the product passes
/// `CaptureSession::new`, tests a synthetic backend.
pub type MakeCaptureSession =
    Arc<dyn Fn(CaptureConfiguration) -> Result<CaptureSession, String> + Send + Sync>;

/// What a volume reports of its size, in bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Volume {
    /// Free for this user.
    pub(crate) available: u64,
    /// The whole volume.
    pub(crate) total: u64,
}

/// What the volume that holds a path reports; the product asks the file
/// system ([`DiskWatch::system`]), tests pass a fake.
pub(crate) type FreeSpace = Arc<dyn Fn(&Path) -> std::io::Result<Volume> + Send + Sync>;

/// How a recording watches the free space; see the module doc.
#[derive(Clone)]
pub(crate) struct DiskWatch {
    /// Reads a volume.
    pub(crate) free_space: FreeSpace,
    /// How often a recording reads it.
    pub(crate) interval: Duration,
    /// The database's folder, whose volume is read beside the recordings
    /// folder's: the save writes there too.
    pub(crate) database_folder: Option<PathBuf>,
    /// Whether a reading below [`Self::STOP_BELOW_BYTES`] refuses a start
    /// and stops a recording; when not, it only warns, and the minutes
    /// left count to a full disk. There a failed write ends the recording
    /// and Steno tries to save it, but a disk that is truly full can fail
    /// the save too; the files stay for the recovery at the next launch.
    /// Off on the Mac only ([`Self::system`]).
    pub(crate) stops: bool,
}

impl DiskWatch {
    /// Below this a recording does not start, and one in progress stops
    /// and is saved: room for the database, the processing and the system,
    /// so the save itself never meets a full disk. Linux and Windows only:
    /// on the Mac `statvfs` leaves out the space APFS would purge for the
    /// user (tens of GB with local Time Machine snapshots), so a reading
    /// below it there only warns ([`Self::stops`]); a recording refused or
    /// cut on a disk that had room would lose the meeting. Nothing keeps
    /// the save its room there yet, so a disk that is truly full can fail
    /// it. The Mac gets the floor once the `steno-macos` crate reads
    /// `NSURLVolumeAvailableCapacityForImportantUsageKey`, which counts that
    /// space (reading it takes `unsafe`, which belongs there).
    pub(crate) const STOP_BELOW_BYTES: u64 = 512 * 1024 * 1024;
    /// The recording time left above [`Self::STOP_BELOW_BYTES`] below which
    /// a recording warns.
    pub(crate) const WARN_BELOW: Duration = Duration::from_secs(30 * 60);

    /// The file system's free space (`statvfs`, `GetDiskFreeSpaceExW`),
    /// read every five seconds, on the volumes of the recordings folder and
    /// of `database_folder`; the floor stops recordings everywhere but on
    /// the Mac (see [`Self::STOP_BELOW_BYTES`]).
    #[must_use]
    pub(crate) fn system(database_folder: Option<&Path>) -> Self {
        Self {
            free_space: Arc::new(|path| {
                fs4::statvfs(path).map(|stats| Volume {
                    available: stats.available_space(),
                    total: stats.total_space(),
                })
            }),
            interval: Duration::from_secs(5),
            database_folder: database_folder.map(Path::to_path_buf),
            stops: !cfg!(target_os = "macos"),
        }
    }

    /// The bytes free for a recording into `audio_folder`: the smaller of
    /// what its volume and the database folder's report, each asked of its
    /// nearest folder that exists (the recordings folder may not exist
    /// before the first recording). A volume that cannot be read, reports
    /// no size or more free than its size is left out; `None` when none is
    /// left, which never stops a recording.
    fn free_for(&self, audio_folder: &Path) -> Option<u64> {
        std::iter::once(audio_folder)
            .chain(self.database_folder.as_deref())
            .filter_map(|folder| self.available(folder))
            .min()
    }

    /// The bytes free on the volume of `folder`, when it reports a size
    /// that holds them.
    fn available(&self, folder: &Path) -> Option<u64> {
        let existing = folder.ancestors().find(|path| path.exists())?;
        match (self.free_space)(existing) {
            Ok(volume) if volume.total > 0 && volume.available <= volume.total => {
                Some(volume.available)
            }
            Ok(volume) => {
                tracing::debug!(?volume, "the volume reports no usable size");
                None
            }
            Err(error) => {
                tracing::debug!(%error, "the free disk space could not be read");
                None
            }
        }
    }
}

/// What a recording does about the space left.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Room {
    Enough,
    /// A warning ([`low_space_warning`]) with the minutes of recording
    /// left, at least one.
    Low {
        minutes: u64,
    },
    /// Too little to go on.
    Full,
}

impl Room {
    /// The room `free` bytes leave a recording that writes
    /// `bytes_per_second`. The warning comes under
    /// [`DiskWatch::WARN_BELOW`] above the floor either way; when the floor
    /// `stops` the recording, below it there is no room and the minutes
    /// count to it, else they count to a full disk and the room stays low.
    fn of(free: u64, bytes_per_second: u64, stops: bool) -> Self {
        let rate = bytes_per_second.max(1);
        let spare = free.checked_sub(DiskWatch::STOP_BELOW_BYTES);
        if spare.is_some_and(|spare| spare / rate >= DiskWatch::WARN_BELOW.as_secs()) {
            return Room::Enough;
        }
        let left = match (spare, stops) {
            (None, true) => return Room::Full,
            (Some(spare), true) => spare,
            (_, false) => free,
        };
        Room::Low {
            minutes: (left / rate / 60).max(1),
        }
    }
}

/// The bytes a second the files of `configuration` grow by: the 48 kHz
/// Float32 master, one 16 kHz Int16 sidecar per lane, and the raw
/// microphone when kept. About 1.6 GB an hour for a call.
fn bytes_per_second(configuration: &CaptureConfiguration) -> u64 {
    let lanes = configuration.lanes().len() as u64;
    let raw = u64::from(configuration.keep_raw_mic_lane);
    (lanes + raw) * 48_000 * 4 + lanes * 16_000 * 2
}

/// The status line of a saved recording whose result carries `failure`,
/// in plain words: the error's text names paths and threads, so it goes to
/// the log instead ([`log_failure`]). A device that disappeared cut the
/// recording short whoever stopped it; a write or a close that failed did
/// so only when the session ended on its own (`failed`), and a Stop, a
/// quit or a call that ended met it in a recording that may miss its end.
fn ended_with(failure: &CaptureError, reason: &RecordingEndReason) -> &'static str {
    match (failure, reason) {
        (CaptureError::DeviceLost, _) => {
            "The recording stopped early: an audio device disappeared. What was recorded until then is saved."
        }
        (_, RecordingEndReason::Failed | RecordingEndReason::DeviceLost) => {
            "The recording stopped early: Steno could not write the recording to the disk. What was recorded until then is saved."
        }
        _ => {
            "The recording is saved, but Steno could not write all of it to the disk, so it may be incomplete."
        }
    }
}

/// A `warn` line for a recording that ended with `failure`: the meeting,
/// the reason and the failure's kind, never its text (it can hold a path).
fn log_failure(meeting_id: Uuid, failure: &CaptureError, reason: &RecordingEndReason) {
    let kind = match failure {
        CaptureError::DeviceLost => "device lost",
        CaptureError::WriterFailed(_) => "write failed",
        _ => "capture failed",
    };
    tracing::warn!(
        meeting = %meeting_id,
        reason = ?reason.kind(),
        kind,
        "the recording ended with a failure"
    );
}

/// Why a recording that `CaptureRecorder::start` refused did not start.
const NO_ROOM_TO_START: &str = "the disk is almost full. Free some space and try again.";

/// The warning of a recording with `minutes` left; a recording the floor
/// `stops` ([`DiskWatch::stops`]) is promised the stop, one it does not
/// (the Mac's) is asked for room instead.
fn low_space_warning(minutes: u64, stops: bool) -> String {
    let unit = if minutes == 1 { "minute" } else { "minutes" };
    let then = if stops {
        "Steno stops and saves the recording before the disk fills."
    } else {
        "Free some space to keep recording."
    };
    format!("The disk is almost full: about {minutes} {unit} of recording left. {then}")
}

/// The status line of a recording stopped because the disk was nearly full.
const STOPPED_FOR_SPACE: &str =
    "The disk is almost full, so Steno stopped the recording and saved it.";

struct Active {
    session: Arc<CaptureSession>,
    meeting_id: Uuid,
    mode: CaptureMode,
    /// The latest lane levels, written by the forwarding thread.
    levels: Arc<Mutex<Option<LaneLevels>>>,
    /// The forwarding thread, joined once the session is dropped; none
    /// when it could not be spawned, and the recording runs without levels.
    level_thread: Option<JoinHandle<()>>,
    /// The input recorded in place of the chosen microphone, now and
    /// earlier in the recording. Kept apart from the status's own warning
    /// so a rebuild can set and clear it; written at the start and by
    /// `notice_thread`, cleared by `clear_messages`.
    fallback: Arc<Mutex<Fallback>>,
    /// Re-reads the microphone after each rebuild, joined once the session
    /// is dropped; none when it could not be spawned, and the fallback
    /// warning stays as the start left it.
    notice_thread: Option<JoinHandle<()>>,
    /// The disk watch's warning ([`low_space_warning`]), set at the start
    /// and by the watcher, cleared by a reading with room again and by
    /// `clear_messages`. Kept apart from the status's own warning so it
    /// does not hide the fallback warning or outlive the low room. Rust
    /// only.
    disk_warning: Option<String>,
}

/// The input a recording records in place of the chosen microphone, as
/// the warnings name it ([`fallback_input`]). Rust only.
#[derive(Default)]
struct Fallback {
    /// The input now, shown as [`fallback_warning`] while recording;
    /// `None` on the chosen microphone.
    current: Option<String>,
    /// The latest input the recording was on, kept after a return to the
    /// chosen microphone, shown as [`fallback_note`] after the stop.
    was_on: Option<String>,
}

impl Fallback {
    /// After a start or a rebuild that records on `input`.
    fn set(&mut self, input: Option<String>) {
        if input.is_some() {
            self.was_on.clone_from(&input);
        }
        self.current = input;
    }
}

struct Inner {
    status: RecorderStatus,
    active: Option<Active>,
    /// Set by [`CaptureRecorder::stop_for_quit`]: the app is ending, so
    /// no recording starts any more.
    quitting: bool,
}

/// dBFS to the `0...1` RMS the bridge carries.
fn linear(db: f32) -> f64 {
    10f64.powf(f64::from(db) / 20.0).clamp(0.0, 1.0)
}

/// The input `stream` records in place of the chosen microphone (the
/// fallback), as the warnings name it; `None` on the chosen one. Rust only:
/// Swift fails the start when the chosen microphone is missing.
fn fallback_input(stream: Option<&CaptureStream>) -> Option<String> {
    let input = stream?.input.as_ref().filter(|input| input.is_fallback)?;
    Some(
        input
            .name
            .clone()
            .unwrap_or_else(|| "the system default microphone".to_owned()),
    )
}

/// The warning while the recording is on the fallback `input`.
fn fallback_warning(input: &str) -> String {
    format!("Recording from {input}. The microphone chosen in Settings is not available.")
}

/// The note after a recording that was on the fallback `input` at some
/// point, so a recording nobody watched still says which microphone it
/// heard; joined after [`recording_warning`]'s lines.
fn fallback_note(input: &str) -> String {
    format!(
        "Steno recorded from {input} while the microphone chosen in Settings was not available."
    )
}

/// The host's change hook ([`CaptureRecorder::on_change`]).
type Hook = Option<Arc<dyn Fn() + Send + Sync>>;

/// The thread that forwards the levels, which arrive on `receiver` at
/// 10 Hz, into `shared` (`Active::levels`) and calls `hook`, so the host
/// republishes `recording`; it ends with the session. None when it could
/// not be spawned, and the recording runs without levels.
fn forward_levels(
    receiver: Receiver<AudioLevels>,
    shared: Arc<Mutex<Option<LaneLevels>>>,
    hook: Hook,
) -> Option<JoinHandle<()>> {
    std::thread::Builder::new()
        .name("steno-recording-levels".into())
        .spawn(move || {
            while let Ok(update) = receiver.recv() {
                *shared
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(levels(&update));
                if let Some(hook) = &hook {
                    hook();
                }
            }
        })
        .inspect_err(|error| tracing::error!(%error, "the recording runs without its levels"))
        .ok()
}

/// The thread that re-reads the microphone into `fallback` after each
/// rebuild ([`CaptureNotice::DeviceResumed`] on `notices`), since a rebuild
/// may record another one, and calls `hook`. It holds `session` weakly, so
/// dropping the session ends the notices and the thread. None when it
/// could not be spawned, and the fallback warning stays as the start left
/// it.
fn follow_the_microphone(
    notices: Receiver<CaptureNotice>,
    fallback: Arc<Mutex<Fallback>>,
    session: Weak<CaptureSession>,
    hook: Hook,
) -> Option<JoinHandle<()>> {
    std::thread::Builder::new()
        .name("steno-notices".into())
        .spawn(move || {
            while let Ok(notice) = notices.recv() {
                if !matches!(notice, CaptureNotice::DeviceResumed { .. }) {
                    continue;
                }
                let Some(session) = session.upgrade() else {
                    return;
                };
                fallback
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .set(fallback_input(session.stream().as_ref()));
                drop(session);
                if let Some(hook) = &hook {
                    hook();
                }
            }
        })
        .inspect_err(|error| {
            tracing::error!(%error, "the recording runs without its microphone notices");
        })
        .ok()
}

fn levels(levels: &AudioLevels) -> LaneLevels {
    LaneLevels {
        mic: linear(levels.mic.rms),
        system: levels.system.as_ref().map(|lane| linear(lane.rms)),
    }
}

pub struct CaptureRecorder {
    store: Arc<Store>,
    pipeline: Arc<CurrentPipeline>,
    make_session: MakeCaptureSession,
    permissions: Arc<dyn Permissions>,
    /// Whether the models are on disk, so a warm-up never downloads.
    speech_models: Arc<dyn SpeechModels>,
    zone: FixedOffset,
    runtime: tokio::runtime::Handle,
    inner: Mutex<Inner>,
    /// Signalled with every status change, for [`Self::settle`].
    changes: Condvar,
    /// Called after every status change so the host republishes.
    changed: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    /// How recordings watch the disk; [`DiskWatch::system`] without the
    /// database folder until the app names it, or a test's own
    /// ([`Self::watch_disk_with`]).
    disk: Mutex<DiskWatch>,
    /// This recorder, for the watcher threads.
    this: Weak<CaptureRecorder>,
}

impl CaptureRecorder {
    #[must_use]
    pub fn new(
        store: Arc<Store>,
        pipeline: Arc<CurrentPipeline>,
        make_session: MakeCaptureSession,
        permissions: Arc<dyn Permissions>,
        speech_models: Arc<dyn SpeechModels>,
        zone: FixedOffset,
        runtime: tokio::runtime::Handle,
    ) -> Arc<Self> {
        Arc::new_cyclic(|this| CaptureRecorder {
            store,
            pipeline,
            make_session,
            permissions,
            speech_models,
            zone,
            runtime,
            inner: Mutex::new(Inner {
                status: RecorderStatus::idle(),
                active: None,
                quitting: false,
            }),
            changes: Condvar::new(),
            changed: Mutex::new(None),
            disk: Mutex::new(DiskWatch::system(None)),
            this: this.clone(),
        })
    }

    /// Replaces how recordings watch the disk, for the next recording on.
    pub(crate) fn watch_disk_with(&self, disk: DiskWatch) {
        *self
            .disk
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = disk;
    }

    pub(crate) fn disk(&self) -> DiskWatch {
        self.disk
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// The hook the app wires to `Host::recorder_changed`.
    pub fn on_change(&self, hook: Arc<dyn Fn() + Send + Sync>) {
        *self
            .changed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(hook);
    }

    fn hook(&self) -> Hook {
        self.changed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn notify(&self) {
        self.changes.notify_all();
        if let Some(hook) = self.hook() {
            hook();
        }
    }

    /// Waits until no start or stop is in progress and hands back the
    /// lock, the recorder `Idle` or `Recording` under it. Swift:
    /// `RecordingController.awaitSettled`.
    fn settle(&self) -> MutexGuard<'_, Inner> {
        self.changes
            .wait_while(self.inner(), |inner| {
                matches!(
                    inner.status.state,
                    RecordingState::Starting | RecordingState::Stopping
                )
            })
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn inner(&self) -> MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn intake(&self) -> LocalRecordingIntake {
        LocalRecordingIntake::over(self.store.clone(), self.pipeline.current(), self.zone)
    }

    /// Loads the models processing needs in the background while the
    /// recording runs, so processing does not wait for the cold load; only
    /// when the models of the current pipeline's speech engine and the
    /// diarizer's are installed, so it never starts a download. The engine
    /// is the one the pipeline was built with ([`BuiltEngine`]), not the id
    /// stored now (Rust only: Swift asked about the stored id): after a
    /// failed reload, or an engine the Swift app saved meanwhile, the two
    /// differ. An engine in this process (`CoreML` on
    /// the Mac) is loaded with the diarizer, as Swift did. Where the speech
    /// sidecar runs the engine ([`SpeechRuntime`]), only the diarizer is:
    /// the child would hold its 2.2 GB through the whole recording, outside
    /// any job's claim, so the job starts it instead. Swift:
    /// `AppEnvironment.warmUpPipelineIfModelsInstalled`, called when a
    /// recording starts.
    ///
    /// [`BuiltEngine`]: crate::pipeline::BuiltEngine
    fn warm_up_if_installed(&self) {
        let (pipeline, engine) = self.pipeline.current_with_engine();
        if !(self.speech_models.engine_installed(&engine.engine_id)
            && self.speech_models.is_installed(ModelAsset::OfflineDiarizer))
        {
            return;
        }
        let in_process = engine.runtime == SpeechRuntime::CoreMlInProcess;
        self.runtime.spawn(async move {
            let warmed = if in_process {
                pipeline.warm_up().await
            } else {
                pipeline.warm_up_diarizer().await
            };
            if let Err(failure) = warmed {
                tracing::debug!(%failure, "warm-up failed; processing loads the models");
            }
        });
    }

    /// Starts the session and the meeting.
    fn start_inner(&self, mode: CaptureMode, call_app: Option<&str>) -> Result<(), String> {
        let settings = self.store.settings().map_err(|e| e.to_string())?;
        let audio_folder = steno_core::paths::file_url_path(&settings.audio_folder)
            .ok_or_else(|| format!("audio folder is not a file URL: {}", settings.audio_folder))?;
        let audio_mode = match mode {
            CaptureMode::Call => steno_audio::CaptureMode::Call,
            CaptureMode::InPerson => steno_audio::CaptureMode::InPerson,
        };
        let mut configuration = CaptureConfiguration::new(audio_mode, &audio_folder);
        configuration
            .input_device_uid
            .clone_from(&settings.input_device_uid);
        let disk = self.disk();
        let rate = bytes_per_second(&configuration);
        let room = disk
            .free_for(&audio_folder)
            .map_or(Room::Enough, |free| Room::of(free, rate, disk.stops));
        if room == Room::Full {
            return Err(NO_ROOM_TO_START.to_owned());
        }
        let session = Arc::new((self.make_session)(configuration)?);
        let started_at = Utc::now();
        let intake = self.intake();
        let meeting = intake
            .begin(
                match mode {
                    CaptureMode::Call => MeetingSource::MacCall,
                    CaptureMode::InPerson => MeetingSource::MacInPerson,
                },
                None,
                None,
                &[],
                started_at,
            )
            .map_err(|e| e.to_string())?;
        let levels_receiver = session.levels();
        let notices = session.notices();
        // Before the start, so a failure right after it is not missed.
        let states = session.states();
        if let Err(error) = session.start(meeting.id) {
            let _ = intake.fail(meeting.id, &format!("Recording could not start: {error}"));
            return Err(error.to_string());
        }
        let meeting_id = meeting.id;
        let shared = Arc::new(Mutex::new(None::<LaneLevels>));
        // Spawned before `Recording` is visible, so the stop that takes the
        // session always takes the thread with it.
        let level_thread = forward_levels(levels_receiver, shared.clone(), self.hook());
        let mut fallback = Fallback::default();
        fallback.set(fallback_input(session.stream().as_ref()));
        let fallback = Arc::new(Mutex::new(fallback));
        let notice_thread = follow_the_microphone(
            notices,
            fallback.clone(),
            Arc::downgrade(&session),
            self.hook(),
        );
        {
            let mut inner = self.inner();
            inner.status.state = RecordingState::Recording;
            inner.status.started_at = Some(started_at);
            inner.status.mode = Some(mode);
            inner.status.call_app = call_app.map(str::to_owned);
            inner.status.meeting_id = Some(meeting_id);
            inner.status.levels = None;
            inner.status.error = None;
            inner.status.warning = None;
            inner.active = Some(Active {
                session,
                meeting_id,
                mode,
                levels: shared,
                level_thread,
                fallback,
                notice_thread,
                disk_warning: match room {
                    Room::Low { minutes } => Some(low_space_warning(minutes, disk.stops)),
                    Room::Enough | Room::Full => None,
                },
            });
        }
        // Not joined: it may be the thread that finishes the recording,
        // and it ends on its own once the session is gone.
        let this = self.this.clone();
        let spawned = std::thread::Builder::new()
            .name("steno-recording-watch".into())
            .spawn(move || Self::watch(&this, meeting_id, &states, &disk, &audio_folder, rate));
        if let Err(error) = spawned {
            tracing::error!(%error, "the recording runs without its watcher");
        }
        Ok(())
    }

    /// The watcher of the recording of `meeting_id` (see the module doc),
    /// until its session leaves `Recording` or is gone. A `Failed` the
    /// session reached on its own finishes the recording here. Every
    /// `disk.interval` so does a write that failed without one (the session
    /// could not spawn its thread to end it), and the free space for
    /// `folder` is read against `rate` bytes a second.
    fn watch(
        this: &Weak<Self>,
        meeting_id: Uuid,
        states: &Receiver<CaptureState>,
        disk: &DiskWatch,
        folder: &Path,
        rate: u64,
    ) {
        let mut recording = false;
        loop {
            match states.recv_timeout(disk.interval) {
                Ok(CaptureState::Recording { .. }) => recording = true,
                Ok(CaptureState::Failed { error, .. }) => {
                    if let Some(recorder) = this.upgrade() {
                        let reason = match error {
                            CaptureError::DeviceLost => RecordingEndReason::DeviceLost,
                            _ => RecordingEndReason::Failed,
                        };
                        recorder.stop_recording(meeting_id, reason, None);
                    }
                    return;
                }
                Ok(CaptureState::Idle) if recording => return,
                Ok(_) => {}
                Err(RecvTimeoutError::Disconnected) => return,
                Err(RecvTimeoutError::Timeout) => {
                    let Some(recorder) = this.upgrade() else {
                        return;
                    };
                    if recorder.write_failed(meeting_id) {
                        recorder.stop_recording(meeting_id, RecordingEndReason::Failed, None);
                        return;
                    }
                    let Some(free) = disk.free_for(folder) else {
                        continue;
                    };
                    match Room::of(free, rate, disk.stops) {
                        Room::Enough => recorder.warn(meeting_id, None),
                        Room::Low { minutes } => {
                            let warning = low_space_warning(minutes, disk.stops);
                            recorder.warn(meeting_id, Some(warning));
                        }
                        Room::Full => {
                            recorder.stop_recording(
                                meeting_id,
                                RecordingEndReason::Failed,
                                Some(STOPPED_FOR_SPACE.to_owned()),
                            );
                            return;
                        }
                    }
                }
            }
        }
    }

    /// Whether the session of `meeting_id`, still the one recording, had a
    /// write fail ([`CaptureSession::write_failed`]).
    fn write_failed(&self, meeting_id: Uuid) -> bool {
        let session = self
            .inner()
            .active
            .as_ref()
            .filter(|active| active.meeting_id == meeting_id)
            .map(|active| active.session.clone());
        // Asked with the recorder's lock released.
        session.is_some_and(|session| session.write_failed())
    }

    /// Stops the recording of `meeting_id` with `reason` and `error` for the
    /// status, when it is still the one in progress; a Stop or a quit that
    /// came first wins.
    fn stop_recording(&self, meeting_id: Uuid, reason: RecordingEndReason, error: Option<String>) {
        let active = {
            let mut inner = self.inner();
            if inner
                .active
                .as_ref()
                .is_none_or(|active| active.meeting_id != meeting_id)
            {
                return;
            }
            Self::begin_stop(&mut inner)
        };
        if let Some(active) = active {
            self.finish_stop(active, reason, error);
        }
    }

    /// Shows the disk watch's `warning` while the recording of
    /// `meeting_id` runs, or clears it with `None` ([`Active::disk_warning`]);
    /// not once it is stopping, whose outcome sets the messages.
    fn warn(&self, meeting_id: Uuid, warning: Option<String>) {
        let mut inner = self.inner();
        if inner.status.state != RecordingState::Recording {
            return;
        }
        let Some(active) = inner
            .active
            .as_mut()
            .filter(|active| active.meeting_id == meeting_id)
        else {
            return;
        };
        if active.disk_warning == warning {
            return;
        }
        active.disk_warning = warning;
        drop(inner);
        self.notify();
    }

    /// Quitting: once a start or a stop in progress has settled, a
    /// recording is stopped with the `quit` end reason and saved (the
    /// asset written and the meeting enqueued) before this returns; a
    /// stop already under way is waited for instead, so its reason stands.
    /// No recording starts afterwards. Swift: `awaitSettled()` and then
    /// `stop(reason: .quit)` in `AppController.shutdown`.
    pub fn stop_for_quit(&self) {
        let active = {
            let mut inner = self.settle();
            inner.quitting = true;
            Self::begin_stop(&mut inner)
        };
        if let Some(active) = active {
            self.finish_stop(active, RecordingEndReason::Quit, None);
        }
    }

    /// Takes the session of the recording in progress and marks the
    /// recorder `Stopping`; none when nothing records.
    fn begin_stop(inner: &mut Inner) -> Option<Active> {
        let active = inner.active.take()?;
        inner.status.state = RecordingState::Stopping;
        Some(active)
    }

    /// Stops the session `begin_stop` took and saves the recording, then
    /// leaves the recorder `Idle` with the outcome's message; the host
    /// hears of `Stopping` and of `Idle`. `error` says why the recorder
    /// stopped on its own; without it, a failure the result carries (a
    /// device that stayed lost, a write or a close that failed) does
    /// ([`ended_with`]). A panic on the way leaves the recorder `Idle`
    /// too ([`Unwinding`]), so the next recording can start and a quit
    /// does not wait for good. One before or inside the save leaves the
    /// meeting's row `recording` over closed files (the session's drop
    /// closes them), which the next launch fails as interrupted, the files
    /// kept.
    fn finish_stop(&self, active: Active, reason: RecordingEndReason, error: Option<String>) {
        let unwinding = Unwinding::of(self, RecordingState::Stopping, SAVE_PANICKED);
        self.notify();
        let intake = self.intake();
        let outcome = match active.session.stop() {
            Ok(result) => {
                log_dropped_frames(active.meeting_id, &result.statistics);
                if let Some(failure) = &result.failure {
                    log_failure(active.meeting_id, failure, &reason);
                }
                let ended = error.or_else(|| {
                    result
                        .failure
                        .as_ref()
                        .map(|failure| ended_with(failure, &reason).to_owned())
                });
                let duration = result.statistics.duration;
                let statistics = result.statistics.clone();
                let completed = block_on(
                    &self.runtime,
                    intake.complete(
                        active.meeting_id,
                        RecordingResult {
                            asset: result.asset,
                            duration,
                            end_reason: reason,
                        },
                        None,
                    ),
                );
                match completed {
                    Ok(_) => Ok((recording_warning(active.mode, &statistics), ended)),
                    Err(error) => Err(format!("Recording could not be saved: {error}")),
                }
            }
            Err(error) => {
                let message = format!("Recording could not be saved: {error}");
                let _ = intake.fail(active.meeting_id, &message);
                Err(message)
            }
        };
        drop(active.session);
        if let Some(level_thread) = active.level_thread {
            let _ = level_thread.join();
        }
        if let Some(notice_thread) = active.notice_thread {
            let _ = notice_thread.join();
        }
        // Read once the notice thread is gone, so its last rebuild counts.
        let outcome = outcome.map(|(warning, ended)| {
            let note = active
                .fallback
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .was_on
                .as_deref()
                .map(fallback_note);
            let warning = [warning, note]
                .into_iter()
                .flatten()
                .reduce(|warning, note| format!("{warning} {note}"));
            (warning, ended)
        });
        let mut inner = self.inner();
        Self::idle(&mut inner);
        match outcome {
            Ok((warning, ended)) => {
                inner.status.warning = warning;
                inner.status.error = ended;
            }
            Err(error) => inner.status.error = Some(error),
        }
        drop(inner);
        std::mem::forget(unwinding);
        self.notify();
    }

    /// The status of a recorder that records nothing, the messages kept.
    fn idle(inner: &mut Inner) {
        inner.status.state = RecordingState::Idle;
        inner.status.started_at = None;
        inner.status.mode = None;
        inner.status.call_app = None;
        inner.status.meeting_id = None;
        inner.status.levels = None;
        inner.status.auto_stop = None;
    }
}

/// Held through [`CaptureRecorder::finish_stop`] in `Stopping` and through
/// a start in `Starting`: if either unwinds, the drop leaves the recorder
/// `Idle` with `error` instead of in that state for good, which `settle`
/// (a quit) would wait on forever and which no start or Stop leaves; a
/// state the step had already left is left alone. Forgotten once the
/// outcome is in the status.
struct Unwinding<'a> {
    recorder: &'a CaptureRecorder,
    holds: RecordingState,
    error: &'static str,
}

impl<'a> Unwinding<'a> {
    fn of(recorder: &'a CaptureRecorder, holds: RecordingState, error: &'static str) -> Self {
        Self {
            recorder,
            holds,
            error,
        }
    }
}

impl Drop for Unwinding<'_> {
    fn drop(&mut self) {
        let recorder = self.recorder;
        let mut inner = recorder.inner();
        if inner.status.state != self.holds {
            return;
        }
        CaptureRecorder::idle(&mut inner);
        inner.status.warning = None;
        inner.status.error = Some(self.error.to_owned());
        drop(inner);
        recorder.notify();
    }
}

/// The status line of a stop that a fault inside Steno cut short.
const SAVE_PANICKED: &str = "Steno ran into a problem while saving the recording.";

/// The status line of a start that a fault inside Steno cut short.
const START_PANICKED: &str = "Recording could not start: Steno ran into a problem.";

impl Recorder for CaptureRecorder {
    fn status(&self) -> RecorderStatus {
        let inner = self.inner();
        let mut status = inner.status.clone();
        if status.state == RecordingState::Recording
            && let Some(active) = &inner.active
        {
            status.levels = *active
                .levels
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let fallback = active
                .fallback
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .current
                .as_deref()
                .map(fallback_warning);
            // Every warning that applies, the disk's first.
            status.warning = [status.warning.take(), active.disk_warning.clone(), fallback]
                .into_iter()
                .flatten()
                .reduce(|warning, next| format!("{warning} {next}"));
        }
        status
    }

    fn start(&self, mode: CaptureMode, call_app: Option<&str>) {
        {
            // One guard for the check and the change, so two starts at once
            // cannot both begin.
            let mut inner = self.inner();
            if inner.quitting || inner.status.state != RecordingState::Idle {
                return;
            }
            inner.status.state = RecordingState::Starting;
            inner.status.error = None;
            inner.status.warning = None;
        }
        // A meeting row a panicking start had begun stays `recording`
        // until the next launch fails it as interrupted.
        let unwinding = Unwinding::of(self, RecordingState::Starting, START_PANICKED);
        self.notify();
        match self.start_inner(mode, call_app) {
            Ok(()) => self.warm_up_if_installed(),
            Err(error) => {
                let mut inner = self.inner();
                inner.status.state = RecordingState::Idle;
                inner.status.error = Some(format!("Recording could not start: {error}"));
            }
        }
        std::mem::forget(unwinding);
        self.notify();
    }

    fn stop(&self) {
        let active = Self::begin_stop(&mut self.inner());
        if let Some(active) = active {
            self.finish_stop(active, RecordingEndReason::Manual, None);
        }
    }

    fn toggle(&self) {
        // Read first: a guard in the scrutinee would be held through the
        // arms, and `start` and `stop` lock again.
        let state = self.inner().status.state;
        match state {
            RecordingState::Idle => self.start(CaptureMode::Call, None),
            RecordingState::Recording => self.stop(),
            _ => {}
        }
    }

    fn keep_recording(&self) {
        self.inner().status.auto_stop = None;
        self.notify();
    }

    fn clear_messages(&self) {
        let mut inner = self.inner();
        inner.status.warning = None;
        inner.status.error = None;
        // The fallback's warning and note until the next rebuild says
        // otherwise, the disk's until the next low reading.
        if let Some(active) = inner.active.as_mut() {
            *active
                .fallback
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Fallback::default();
            active.disk_warning = None;
        }
        drop(inner);
        self.notify();
    }

    fn refresh_permissions(&self) {
        let denied = denied_permissions(steno_bridge::Platform::CURRENT, &*self.permissions);
        self.inner().status.denied_permissions = denied;
        self.notify();
    }
}

/// What a saved recording warns about, every line that applies joined
/// into one: frames that never reached the files (in seconds of the lane
/// that lost most, rounded to the nearest second, so from half a second
/// on: a lone 10 ms drift slip is not worth a warning, and the log line
/// keeps every count), and a call whose system audio stayed silent. The
/// dropped frames name no cause, since the count holds several: the relay
/// full behind a slow disk, ring overruns while the computer was too busy,
/// frames a stop left undrained and, on Windows, the slips that absorb
/// clock drift. A device that disappeared is the status's error instead
/// ([`ended_with`]). Swift:
/// `RecordingController.stop`, where a device loss replaced the silent-lane
/// line as a warning and that line reads "The system audio lane stayed
/// silent"; the joining and the dropped frames are Rust only.
fn recording_warning(mode: CaptureMode, statistics: &CaptureStatistics) -> Option<String> {
    let mut lines = Vec::new();
    let dropped = statistics
        .dropped_frames
        .values()
        .max()
        .copied()
        .unwrap_or(0);
    let seconds = (dropped + FRAMES_PER_SECOND / 2) / FRAMES_PER_SECOND;
    if seconds > 0 {
        lines.push(if seconds == 1 {
            "About 1 second of the recording is missing.".to_owned()
        } else {
            format!("About {seconds} seconds of the recording are missing.")
        });
    }
    if mode == CaptureMode::Call && statistics.system_lane_silent {
        lines.push(
            "Steno heard nothing from the call's audio. Check the system audio permission."
                .to_owned(),
        );
    }
    (!lines.is_empty()).then(|| lines.join(" "))
}

/// A `warn` line when a recording lost frames, with the meeting's id and
/// the count per lane (counts only, the log's privacy rule). Rust only.
fn log_dropped_frames(meeting_id: Uuid, statistics: &CaptureStatistics) {
    if statistics.dropped_frames.values().any(|frames| *frames > 0) {
        tracing::warn!(
            meeting = %meeting_id,
            dropped = ?statistics.dropped_frames,
            "the recording lost frames"
        );
    }
}

/// The required permissions `permissions` reports denied, among the ones
/// `platform` has: off the Mac, system audio has no switch of its own
/// (Windows' is the microphone's), so it is never reported denied there.
fn denied_permissions(
    platform: steno_bridge::Platform,
    permissions: &dyn Permissions,
) -> Vec<PermissionKind> {
    PermissionKind::for_platform(platform)
        .iter()
        .copied()
        .filter(|kind| steno_host::services::permission_is_required(*kind))
        .filter(|kind| permissions.state(*kind) == steno_bridge::PermissionState::Denied)
        .collect()
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

    use super::*;
    use crate::app::BuildError;
    use crate::pipeline::{BuiltEngine, BuiltPipeline, MakeDependencies};
    use crate::testing::{
        PATIENCE, eventually, fake_dependencies, on_own_thread, synthetic_capture, temp_store,
    };
    use steno_audio::testing::synthetic::SyntheticOptions;
    use steno_audio::writer::{LaneFrames, RecordingFiles, RecordingWriter, RecordingWriting};
    use steno_core::AudioLane;
    use steno_core::paths::file_url;
    use steno_core::testing::{FakeDiarizer, FakeSpeechEngine};
    use steno_host::fakes::{FakePermissions, FakeSpeechModels};
    use steno_speech::SpeechSettings;

    struct Harness {
        _dir: tempfile::TempDir,
        store: Arc<Store>,
        /// The bytes the disk watch reads as free; plenty unless a test
        /// lowers it.
        free: Arc<AtomicU64>,
        /// The capture sessions the recorder builds; the synthetic tone
        /// unless a test swaps in its own.
        capture: Arc<Mutex<MakeCaptureSession>>,
        recorder: Arc<CaptureRecorder>,
        engine: Arc<FakeSpeechEngine>,
        diarizer: Arc<FakeDiarizer>,
        /// While set, a reload fails as a build error would and the
        /// current pipeline stays.
        failing_reloads: Arc<AtomicBool>,
    }

    /// Where the app runs each engine id with `speech_settings`.
    fn platform_rule(
        speech_settings: SpeechSettings,
    ) -> impl Fn(&str) -> SpeechRuntime + Send + Sync + 'static {
        move |engine_id| crate::speech::engine_runtime(engine_id, &speech_settings)
    }

    /// The Mac's default rule, on every platform: `parakeet-v3` in this
    /// process, every other id in the speech sidecar.
    fn mac_rule(engine_id: &str) -> SpeechRuntime {
        if engine_id == "parakeet-v3" {
            SpeechRuntime::CoreMlInProcess
        } else {
            SpeechRuntime::OnnxSidecar
        }
    }

    /// Fake models with `assets` on disk.
    fn models_with(assets: &[ModelAsset]) -> Arc<FakeSpeechModels> {
        let models = Arc::new(FakeSpeechModels::default());
        for asset in assets {
            models.set_installed(*asset, None);
        }
        models
    }

    /// A recorder over fake models with `installed` on disk.
    fn harness(installed: &[ModelAsset]) -> Harness {
        harness_over(
            models_with(installed),
            "parakeet-v3",
            platform_rule(SpeechSettings::default()),
        )
    }

    /// A recorder over `models`, the settings naming `engine_id`. Each
    /// build, the first and every reload, records the engine id stored
    /// at that moment and the runtime `runtime_of` gives it, as
    /// `app::pipeline_dependencies` does.
    fn harness_over(
        models: Arc<dyn SpeechModels>,
        engine_id: &str,
        runtime_of: impl Fn(&str) -> SpeechRuntime + Send + Sync + 'static,
    ) -> Harness {
        harness_capturing(models, engine_id, runtime_of, synthetic_capture())
    }

    /// [`harness_over`] recording through `capture`.
    fn harness_capturing(
        models: Arc<dyn SpeechModels>,
        engine_id: &str,
        runtime_of: impl Fn(&str) -> SpeechRuntime + Send + Sync + 'static,
        capture: MakeCaptureSession,
    ) -> Harness {
        let (dir, store) = temp_store();
        let free = Arc::new(AtomicU64::new(u64::MAX));
        let capture_slot = Arc::new(Mutex::new(capture));
        let capture = capture_slot.clone();
        let mut settings = store.settings().unwrap();
        settings.audio_folder = file_url(&dir.path().join("audio"), true);
        engine_id.clone_into(&mut settings.speech_engine_id);
        store.save_settings(&settings).unwrap();
        let engine = Arc::new(FakeSpeechEngine::default());
        let diarizer = Arc::new(FakeDiarizer::default());
        let mut dependencies = fake_dependencies(&store, "fake-engine")
            .with_speech_engine(steno_pipeline::SharedSpeechEngine::new(engine.clone()));
        dependencies.diarizer = diarizer.clone();
        let failing_reloads = Arc::new(AtomicBool::new(false));
        let make: MakeDependencies = {
            let (store, failing_reloads) = (store.clone(), failing_reloads.clone());
            Arc::new(move || {
                if failing_reloads.load(Ordering::SeqCst) {
                    return Err(BuildError::DatabaseFolder(std::io::Error::other(
                        "the build fails",
                    )));
                }
                let engine_id = store.settings()?.speech_engine_id;
                Ok(BuiltPipeline {
                    dependencies: dependencies.clone(),
                    engine: BuiltEngine {
                        runtime: runtime_of(&engine_id),
                        engine_id,
                    },
                })
            })
        };
        let pipeline = Arc::new(CurrentPipeline::new(
            make().unwrap(),
            make,
            tokio::runtime::Handle::current(),
        ));
        let recorder = CaptureRecorder::new(
            store.clone(),
            pipeline,
            Arc::new(move |configuration| (capture.lock().unwrap())(configuration)),
            Arc::new(FakePermissions::all_granted()),
            models,
            chrono::FixedOffset::east_opt(0).unwrap(),
            tokio::runtime::Handle::current(),
        );
        recorder.watch_disk_with({
            let free = free.clone();
            disk_with(move |_| Ok(volume(free.load(Ordering::SeqCst))))
        });
        Harness {
            _dir: dir,
            store: store.clone(),
            free,
            capture: capture_slot,
            recorder,
            engine,
            diarizer,
            failing_reloads,
        }
    }

    /// A disk watch over `free_space`, read every 20 ms, on the recordings
    /// folder's volume alone, whose floor stops recordings as off the Mac,
    /// on every platform.
    fn disk_with(
        free_space: impl Fn(&Path) -> std::io::Result<Volume> + Send + Sync + 'static,
    ) -> DiskWatch {
        DiskWatch {
            free_space: Arc::new(free_space),
            interval: Duration::from_millis(20),
            database_folder: None,
            stops: true,
        }
    }

    /// A volume of the largest size with `available` bytes free.
    fn volume(available: u64) -> Volume {
        Volume {
            available,
            total: u64::MAX,
        }
    }

    /// The real writer with `before` run ahead of every write; an error
    /// from it fails that write.
    struct BeforeWrite<F> {
        inner: RecordingWriter,
        before: F,
    }

    impl<F: FnMut() -> Result<(), CaptureError> + Send> RecordingWriting for BeforeWrite<F> {
        fn files(&self) -> RecordingFiles {
            self.inner.files()
        }
        fn write(&mut self, frames: &LaneFrames<'_>) -> Result<(), CaptureError> {
            (self.before)()?;
            self.inner.write(frames)
        }
        fn sync(&mut self) -> std::io::Result<()> {
            self.inner.sync()
        }
        fn finish(&mut self) -> Result<RecordingFiles, CaptureError> {
            self.inner.finish()
        }
    }

    /// A capture session over `backend` and a relay of `headroom` frames
    /// whose writers are [`BeforeWrite`]s, each with a `before` from
    /// `make_before`.
    fn session_with_writes<F>(
        configuration: CaptureConfiguration,
        backend: Arc<dyn steno_audio::CaptureBackend>,
        headroom: usize,
        make_before: impl Fn() -> F + Send + Sync + 'static,
    ) -> Result<CaptureSession, String>
    where
        F: FnMut() -> Result<(), CaptureError> + Send + 'static,
    {
        CaptureSession::with_writer_factory(
            configuration,
            backend,
            None,
            headroom,
            Arc::new(steno_audio::SystemClock::new()),
            Arc::new(move |layout, lanes, keep_raw| {
                Ok(Box::new(BeforeWrite {
                    inner: RecordingWriter::new(layout, lanes, keep_raw)?,
                    before: make_before(),
                }) as Box<dyn RecordingWriting>)
            }),
        )
        .map_err(|error| error.to_string())
    }

    /// Capture sessions over the synthetic tone as `device` changes it,
    /// whose writer fails every write after the first `writes`, as a disk
    /// that fills does.
    fn failing_capture(
        writes: Option<usize>,
        device: fn(SyntheticOptions) -> SyntheticOptions,
    ) -> MakeCaptureSession {
        Arc::new(move |configuration: CaptureConfiguration| {
            let options = device(crate::testing::synthetic_tone(&configuration));
            session_with_writes(
                configuration,
                Arc::new(steno_audio::testing::SyntheticCaptureBackend::new(options)),
                CaptureSession::DEFAULT_WRITER_HEADROOM_FRAMES,
                move || {
                    let mut left = writes;
                    move || match left.as_mut() {
                        Some(0) => {
                            Err(CaptureError::WriterFailed("No space left on device".into()))
                        }
                        Some(left) => {
                            *left -= 1;
                            Ok(())
                        }
                        None => Ok(()),
                    }
                },
            )
        })
    }

    impl Harness {
        /// Records with `make` from the next start on.
        fn capture_with(&self, make: MakeCaptureSession) {
            *self.capture.lock().unwrap() = make;
        }

        /// Stores `engine_id` as a Settings save does, then reloads.
        fn save_engine(&self, engine_id: &str) -> Result<(), BuildError> {
            let mut settings = self.store.settings().unwrap();
            engine_id.clone_into(&mut settings.speech_engine_id);
            self.store.save_settings(&settings).unwrap();
            self.recorder.pipeline.reload()
        }
    }

    async fn start(recorder: &Arc<CaptureRecorder>) {
        let starting = recorder.clone();
        tokio::task::spawn_blocking(move || starting.start(CaptureMode::InPerson, None))
            .await
            .unwrap();
        assert_eq!(recorder.status().state, RecordingState::Recording);
    }

    async fn stop(recorder: &Arc<CaptureRecorder>) {
        let stopping = recorder.clone();
        tokio::task::spawn_blocking(move || stopping.stop())
            .await
            .unwrap();
    }

    /// Quits on a thread of its own, failing the test rather than hanging
    /// it when quitting never returns.
    fn quit(recorder: &Arc<CaptureRecorder>) {
        let quitting = recorder.clone();
        on_own_thread(PATIENCE, "quitting returned", move || {
            quitting.stop_for_quit();
        });
    }

    /// Every change that finds the recorder in `state` holds it there for
    /// a while; the receiver hears of each one as it begins.
    fn held_in(
        recorder: &Arc<CaptureRecorder>,
        state: RecordingState,
    ) -> std::sync::mpsc::Receiver<()> {
        let (reached, seen) = std::sync::mpsc::channel();
        let watched = Arc::downgrade(recorder);
        recorder.on_change(Arc::new(move || {
            if watched
                .upgrade()
                .is_some_and(|recorder| recorder.status().state == state)
            {
                let _ = reached.send(());
                std::thread::sleep(std::time::Duration::from_millis(300));
            }
        }));
        seen
    }

    /// Starts a recording and waits until its warm-up has loaded the
    /// diarizer, which `warm_up` loads after the speech engine; whether
    /// the speech engine was loaded too.
    async fn warmed_the_engine(harness: &Harness) -> bool {
        start(&harness.recorder).await;
        eventually("the diarizer was loaded while recording", || {
            harness.diarizer.preparations.count() > 0
        })
        .await;
        harness.engine.preparations.count() > 0
    }

    /// With the defaults, the Mac's `CoreML` engine and the diarizer are
    /// both loaded, as Swift did; off the Mac the speech sidecar runs
    /// Parakeet, so only the diarizer is.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_recording_start_warms_the_pipeline_up_when_the_models_are_installed() {
        let harness = harness(&[ModelAsset::ParakeetV3, ModelAsset::OfflineDiarizer]);
        assert_eq!(warmed_the_engine(&harness).await, cfg!(target_os = "macos"));
        stop(&harness.recorder).await;
    }

    /// With Parakeet in the speech sidecar (here chosen on every
    /// platform), a recording's warm-up loads the diarizer only: the
    /// child would otherwise stay resident through the recording.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_recording_start_with_speech_in_the_sidecar_warms_the_diarizer_only() {
        let harness = harness_over(
            models_with(&[ModelAsset::ParakeetV3, ModelAsset::OfflineDiarizer]),
            "parakeet-v3",
            platform_rule(crate::speech::testing::sidecar_chosen()),
        );
        assert!(!warmed_the_engine(&harness).await);
        stop(&harness.recorder).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_recording_start_never_warms_up_models_that_would_download() {
        let harness = harness(&[ModelAsset::OfflineDiarizer]);
        start(&harness.recorder).await;
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        assert_eq!(harness.engine.preparations.count(), 0);
        assert_eq!(harness.diarizer.preparations.count(), 0);
        // Processing the recording loads them, as it always did.
        stop(&harness.recorder).await;
    }

    /// The warm-up follows the engine a reload built: the Mac's rule on
    /// every platform, so moving from an id the sidecar runs to the
    /// in-process `parakeet-v3` makes the recording load the engine.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_warm_up_follows_the_engine_a_reload_built() {
        let harness = harness_over(
            models_with(&[
                ModelAsset::ParakeetV3,
                ModelAsset::ParakeetUltra,
                ModelAsset::OfflineDiarizer,
            ]),
            "parakeet-ultra",
            mac_rule,
        );
        harness.save_engine("parakeet-v3").unwrap();
        assert!(warmed_the_engine(&harness).await);
        stop(&harness.recorder).await;
    }

    /// After a failed reload the pipeline keeps the engine it was built
    /// with, and so does the warm-up, whatever id the store holds now (a
    /// Swift app that saved another engine looks the same): a pipeline on
    /// the sidecar is not warmed into a child although the store names
    /// the in-process `parakeet-v3`, and a pipeline on `CoreML` still
    /// loads its engine although the store names a sidecar id. Each half
    /// installs only the built engine's model and the diarizer, so the
    /// installed check must ask about the built engine too.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn after_a_failed_reload_the_warm_up_follows_the_engine_the_pipeline_kept() {
        let sidecar = harness_over(
            models_with(&[ModelAsset::ParakeetUltra, ModelAsset::OfflineDiarizer]),
            "parakeet-ultra",
            mac_rule,
        );
        sidecar.failing_reloads.store(true, Ordering::SeqCst);
        assert!(sidecar.save_engine("parakeet-v3").is_err());
        assert!(!warmed_the_engine(&sidecar).await);
        stop(&sidecar.recorder).await;

        let in_process = harness_over(
            models_with(&[ModelAsset::ParakeetV3, ModelAsset::OfflineDiarizer]),
            "parakeet-v3",
            mac_rule,
        );
        in_process.failing_reloads.store(true, Ordering::SeqCst);
        assert!(in_process.save_engine("parakeet-ultra").is_err());
        assert!(warmed_the_engine(&in_process).await);
        stop(&in_process.recorder).await;
    }

    /// A microphone input for the warning tests: `None` names none.
    fn input(name: Option<&str>, is_fallback: bool) -> steno_audio::CaptureInput {
        steno_audio::CaptureInput {
            uid: name.unwrap_or("default").to_lowercase(),
            name: name.map(str::to_owned),
            is_fallback,
        }
    }

    /// A harness whose capture records on `first` and, after one device
    /// change 0.3 s in, on `after`.
    fn harness_on_inputs(
        first: steno_audio::CaptureInput,
        after: steno_audio::CaptureInput,
    ) -> Harness {
        let capture: MakeCaptureSession = Arc::new(move |configuration: CaptureConfiguration| {
            let stream = |input: &steno_audio::CaptureInput| CaptureStream {
                input: Some(input.clone()),
                ..CaptureStream::SYNTHETIC
            };
            let options = steno_audio::testing::synthetic::SyntheticOptions::tones(
                &configuration.lanes(),
                &[(steno_core::AudioLane::Mixed, 440.0)],
                600.0,
            )
            .real_time(true)
            .change_device_after(0.3)
            .stream(stream(&first))
            .stream_after_restart(stream(&after));
            CaptureSession::with_backend(
                configuration,
                Arc::new(steno_audio::testing::SyntheticCaptureBackend::new(options)),
                None,
                CaptureSession::DEFAULT_WRITER_HEADROOM_FRAMES,
                Arc::new(steno_audio::SystemClock::new()),
            )
            .map_err(|error| error.to_string())
        });
        harness_capturing(
            models_with(&[]),
            "parakeet-v3",
            platform_rule(SpeechSettings::default()),
            capture,
        )
    }

    /// A rebuild that records the default input in place of the chosen
    /// microphone shows a warning naming the one recorded, until it is
    /// dismissed (then no note follows the stop) or the recording stops.
    /// The stop joins the thread that sets it: the hook it calls is held
    /// up, and its handle on the input is gone once `stop` returns.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_recording_on_the_fallback_microphone_names_it_in_a_warning() {
        let harness = harness_on_inputs(
            input(Some("USB Microphone"), false),
            input(Some("Built-in Audio"), true),
        );
        harness.recorder.on_change(Arc::new(|| {
            if std::thread::current().name() == Some("steno-notices") {
                std::thread::sleep(std::time::Duration::from_millis(500));
            }
        }));
        let warning = || harness.recorder.status().warning;
        start(&harness.recorder).await;
        assert_eq!(warning(), None, "the chosen microphone records");
        eventually("the rebuild's fallback is named", || {
            warning().as_deref()
                == Some(
                    "Recording from Built-in Audio. The microphone chosen in Settings is not \
                     available.",
                )
        })
        .await;
        harness.recorder.clear_messages();
        assert_eq!(warning(), None, "dismissed");
        let held = Arc::downgrade(&harness.recorder.inner().active.as_ref().unwrap().fallback);
        stop(&harness.recorder).await;
        assert_eq!(warning(), None, "nothing left after the stop");
        assert_eq!(held.strong_count(), 0, "the notice thread was joined");
    }

    /// A recording that starts on the default input in place of the chosen
    /// microphone warns at once, names the system default when the input
    /// has no name, and says after the stop which input it recorded.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_recording_that_starts_on_the_fallback_microphone_warns_at_once() {
        let harness = harness_on_inputs(input(Some("Built-in Audio"), true), input(None, true));
        let warning = || harness.recorder.status().warning;
        start(&harness.recorder).await;
        assert_eq!(
            warning().as_deref(),
            Some(
                "Recording from Built-in Audio. The microphone chosen in Settings is not available."
            )
        );
        eventually("the unnamed fallback reads as the default", || {
            warning().as_deref()
                == Some(
                    "Recording from the system default microphone. The microphone chosen in \
                     Settings is not available.",
                )
        })
        .await;
        stop(&harness.recorder).await;
        assert_eq!(
            warning().as_deref(),
            Some(
                "Steno recorded from the system default microphone while the microphone \
                 chosen in Settings was not available."
            ),
            "the note after the stop"
        );
    }

    /// A recording that went back to the chosen microphone before the stop
    /// still says after it which input it recorded on the way.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_recording_back_on_the_chosen_microphone_still_notes_the_fallback() {
        let harness = harness_on_inputs(
            input(Some("Built-in Audio"), true),
            input(Some("USB Microphone"), false),
        );
        let warning = || harness.recorder.status().warning;
        start(&harness.recorder).await;
        assert!(warning().is_some(), "on the fallback at the start");
        eventually("back on the chosen microphone", || warning().is_none()).await;
        stop(&harness.recorder).await;
        assert_eq!(
            warning().as_deref(),
            Some(
                "Steno recorded from Built-in Audio while the microphone chosen in Settings \
                 was not available."
            )
        );
    }

    /// The note joins the other warnings after the stop: a call on the
    /// fallback whose system lane stayed silent says both.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_fallback_note_follows_the_other_warnings() {
        let harness = harness_on_inputs(
            input(Some("Built-in Audio"), true),
            input(Some("Built-in Audio"), true),
        );
        let starting = harness.recorder.clone();
        tokio::task::spawn_blocking(move || starting.start(CaptureMode::Call, None))
            .await
            .unwrap();
        assert_eq!(harness.recorder.status().state, RecordingState::Recording);
        stop(&harness.recorder).await;
        assert_eq!(
            harness.recorder.status().warning.as_deref(),
            Some(
                "Steno heard nothing from the call's audio. Check the system audio permission. \
                 Steno recorded from Built-in Audio while the microphone chosen in Settings \
                 was not available."
            )
        );
    }

    /// Quitting stops the recording with `quit` and saves it: the meeting
    /// is queued for processing when the call returns.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn quitting_stops_and_saves_the_recording_with_the_quit_reason() {
        let harness = harness(&[]);
        start(&harness.recorder).await;
        let meeting_id = harness.recorder.status().meeting_id.unwrap();
        quit(&harness.recorder);
        assert_eq!(harness.recorder.status().state, RecordingState::Idle);
        let meeting = harness.store.meeting(meeting_id).unwrap().unwrap();
        assert_eq!(meeting.end_reason, Some(RecordingEndReason::Quit));
        assert_ne!(
            meeting.state.kind(),
            steno_core::MeetingStateKind::Recording
        );
        assert!(harness.store.asset(meeting_id).unwrap().is_some());
    }

    /// A Quit while a manual Stop is still saving waits for that save:
    /// when quitting returns the asset is written and the meeting keeps the
    /// manual stop's reason, rather than the app exiting mid-save.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn quitting_during_a_stop_waits_for_its_save() {
        let harness = harness(&[]);
        start(&harness.recorder).await;
        let meeting_id = harness.recorder.status().meeting_id.unwrap();
        let stop_seen = held_in(&harness.recorder, RecordingState::Stopping);
        let stopper = harness.recorder.clone();
        let manual = std::thread::spawn(move || stopper.stop());
        stop_seen.recv_timeout(PATIENCE).expect("the stop began");
        quit(&harness.recorder);
        assert_eq!(harness.recorder.status().state, RecordingState::Idle);
        assert!(harness.store.asset(meeting_id).unwrap().is_some());
        let meeting = harness.store.meeting(meeting_id).unwrap().unwrap();
        assert_eq!(meeting.end_reason, Some(RecordingEndReason::Manual));
        manual.join().unwrap();
    }

    /// A Quit while a start is under way stops the recording it starts,
    /// rather than leaving it running into the exit.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn quitting_during_a_start_stops_the_recording_it_starts() {
        let harness = harness(&[]);
        let start_seen = held_in(&harness.recorder, RecordingState::Starting);
        let starter = harness.recorder.clone();
        let start = std::thread::spawn(move || starter.start(CaptureMode::InPerson, None));
        start_seen.recv_timeout(PATIENCE).expect("the start began");
        quit(&harness.recorder);
        start.join().unwrap();
        assert_eq!(harness.recorder.status().state, RecordingState::Idle);
        let meetings = harness.store.all_meetings().unwrap();
        assert_eq!(meetings.len(), 1);
        assert_eq!(meetings[0].end_reason, Some(RecordingEndReason::Quit));
        assert!(harness.store.asset(meetings[0].id).unwrap().is_some());
    }

    /// Once quitting ran, a start (the tray's Record while the exit waits)
    /// records nothing, so no meeting is left recording past the exit.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn no_recording_starts_after_quitting() {
        let harness = harness(&[]);
        quit(&harness.recorder);
        let starter = harness.recorder.clone();
        on_own_thread(PATIENCE, "the start returned", move || {
            starter.start(CaptureMode::InPerson, None);
        });
        assert_eq!(harness.recorder.status().state, RecordingState::Idle);
        assert_eq!(harness.store.all_meetings().unwrap(), []);
    }

    /// Starts at the same moment begin one recording between them: the
    /// others return without a session, and stopping saves that one.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn concurrent_starts_begin_one_recording() {
        let harness = harness(&[]);
        let all_ready = Arc::new(std::sync::Barrier::new(8));
        let starters: Vec<_> = (0..8)
            .map(|_| {
                let (starter, ready) = (harness.recorder.clone(), all_ready.clone());
                std::thread::spawn(move || {
                    ready.wait();
                    starter.start(CaptureMode::InPerson, None);
                })
            })
            .collect();
        on_own_thread(PATIENCE, "every start returned", move || {
            for starter in starters {
                starter.join().unwrap();
            }
        });
        assert_eq!(harness.recorder.status().state, RecordingState::Recording);
        assert_eq!(harness.store.all_meetings().unwrap().len(), 1);
        quit(&harness.recorder);
        let meetings = harness.store.all_meetings().unwrap();
        assert_eq!(meetings.len(), 1);
        assert!(harness.store.asset(meetings[0].id).unwrap().is_some());
    }

    /// The tray's Record and its Stop (`recording.toggle`): a toggle
    /// starts a call recording, the next one stops and saves it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_toggle_starts_a_call_and_the_next_one_stops_it() {
        let harness = harness(&[]);
        let toggling = harness.recorder.clone();
        on_own_thread(PATIENCE, "the first toggle returned", move || {
            toggling.toggle();
        });
        let status = harness.recorder.status();
        assert_eq!(status.state, RecordingState::Recording);
        assert_eq!(status.mode, Some(CaptureMode::Call));
        let meeting_id = status.meeting_id.unwrap();
        let toggling = harness.recorder.clone();
        on_own_thread(PATIENCE, "the second toggle returned", move || {
            toggling.toggle();
        });
        assert_eq!(harness.recorder.status().state, RecordingState::Idle);
        assert!(harness.store.asset(meeting_id).unwrap().is_some());
    }

    /// Quit while recording, as the shell's run loop drives it: the exit
    /// request is held, the recording is stopped once and saved, and only
    /// then the exit runs; by then the asset is written and the meeting is
    /// queued, and the pipeline goes on to process it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_quit_through_the_exit_gate_exits_after_the_recording_is_saved() {
        let harness = harness(&[]);
        start(&harness.recorder).await;
        let meeting_id = harness.recorder.status().meeting_id.unwrap();
        let gate = crate::app::ExitGate::default();
        let (exited, exit_seen) = std::sync::mpsc::channel();
        let quitting = harness.recorder.clone();
        let (store, at_exit) = (harness.store.clone(), harness.recorder.clone());
        let held = gate.exit_requested(
            crate::app::SHUTDOWN_PATIENCE,
            move || quitting.stop_for_quit(),
            move || {
                let meeting = store.meeting(meeting_id).unwrap().unwrap();
                let saved = store.asset(meeting_id).unwrap().is_some();
                let _ = exited.send((at_exit.status().state, meeting, saved));
            },
        );
        assert!(!held, "the exit waits for the save");
        let (state, meeting, saved) = tokio::task::spawn_blocking(move || {
            exit_seen.recv_timeout(std::time::Duration::from_secs(20))
        })
        .await
        .unwrap()
        .expect("exited after the save");
        assert_eq!(state, RecordingState::Idle);
        assert!(saved, "the asset was written before the exit");
        assert_eq!(meeting.end_reason, Some(RecordingEndReason::Quit));
        assert_ne!(
            meeting.state.kind(),
            steno_core::MeetingStateKind::Recording
        );
        eventually("the pipeline picked the saved meeting up", || {
            harness.engine.transcriptions.count() > 0
        })
        .await;
    }

    /// The configured engine decides which model counts: with the `CoreML`
    /// Parakeet and the diarizer on disk but another engine id stored
    /// (Whisper, picked in Settings or in the Swift app), the speech
    /// sidecar would load the ONNX models, which are missing, and the
    /// warm-up neither loads nor downloads.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_recording_start_checks_the_models_of_the_configured_engine() {
        let models_dir = tempfile::tempdir().unwrap();
        let models = Arc::new(crate::speech::testing::models_in(models_dir.path()));
        crate::speech::testing::install_coreml_parakeet(&models.coreml);
        crate::speech::testing::install_onnx_diarizer(&models);
        assert!(models.is_installed(ModelAsset::OfflineDiarizer));
        let before = crate::speech::testing::files_under(models_dir.path());

        let harness = harness_over(
            models,
            "whisperkit-large-v3-turbo",
            platform_rule(SpeechSettings::default()),
        );
        start(&harness.recorder).await;
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        assert_eq!(harness.engine.preparations.count(), 0);
        assert_eq!(harness.diarizer.preparations.count(), 0);
        assert_eq!(
            crate::speech::testing::files_under(models_dir.path()),
            before,
            "nothing was downloaded"
        );
        stop(&harness.recorder).await;
    }

    #[test]
    fn only_the_platform_s_own_required_permissions_are_reported_denied() {
        use steno_bridge::{PermissionState, Platform};
        let denied = FakePermissions::all(PermissionState::Denied);
        assert_eq!(
            denied_permissions(Platform::Macos, &denied),
            [PermissionKind::Microphone, PermissionKind::SystemAudio],
        );
        assert_eq!(
            denied_permissions(Platform::Windows, &denied),
            [PermissionKind::Microphone]
        );
        assert_eq!(
            denied_permissions(Platform::Linux, &denied),
            [PermissionKind::Microphone]
        );
        let system_audio =
            FakePermissions::with_states([(PermissionKind::SystemAudio, PermissionState::Denied)]);
        assert_eq!(
            denied_permissions(Platform::Linux, &system_audio),
            Vec::new()
        );
        assert_eq!(
            denied_permissions(Platform::Macos, &system_audio),
            [PermissionKind::SystemAudio]
        );
    }

    fn statistics() -> CaptureStatistics {
        CaptureStatistics {
            duration: 60.0,
            dropped_frames: std::collections::BTreeMap::new(),
            system_lane_silent: false,
            ended_on_device_loss: false,
            device_changes: 0,
            gap_seconds: 0.0,
        }
    }

    #[test]
    fn a_clean_recording_warns_about_nothing() {
        assert_eq!(recording_warning(CaptureMode::Call, &statistics()), None);
    }

    /// Lost frames are a warning, in seconds of the lane that lost most,
    /// rounded to the nearest second: 2.5 s reads 3.
    #[test]
    fn dropped_frames_warn_with_the_seconds_missing() {
        let mut dropped = statistics();
        dropped.dropped_frames.insert(AudioLane::Mic, 250);
        dropped.dropped_frames.insert(AudioLane::System, 200);
        assert_eq!(
            recording_warning(CaptureMode::Call, &dropped).as_deref(),
            Some("About 3 seconds of the recording are missing.")
        );
        dropped.dropped_frames.clear();
        dropped.dropped_frames.insert(AudioLane::Mixed, 50);
        assert_eq!(
            recording_warning(CaptureMode::InPerson, &dropped).as_deref(),
            Some("About 1 second of the recording is missing.")
        );
    }

    /// Under half a second lost is no warning: one 10 ms slip, as Windows'
    /// drift correction makes, or 490 ms.
    #[test]
    fn under_half_a_second_lost_warns_about_nothing() {
        for frames in [1, 49] {
            let mut dropped = statistics();
            dropped.dropped_frames.insert(AudioLane::Mic, frames);
            assert_eq!(recording_warning(CaptureMode::InPerson, &dropped), None);
        }
    }

    /// Every warning that applies is kept; a device loss is the error, not
    /// one of them.
    #[test]
    fn every_warning_that_applies_is_joined() {
        let mut all = statistics();
        all.ended_on_device_loss = true;
        all.system_lane_silent = true;
        all.dropped_frames.insert(AudioLane::Mic, 100);
        let warning = recording_warning(CaptureMode::Call, &all).unwrap();
        assert_eq!(
            warning,
            "About 1 second of the recording is missing. \
             Steno heard nothing from the call's audio. Check the system audio permission."
        );
        // In person there is no system lane to warn about.
        assert!(
            !recording_warning(CaptureMode::InPerson, &all)
                .unwrap()
                .contains("call's audio")
        );
    }

    /// A recording that lost frames leaves a `warn` line with the meeting
    /// and the counts per lane; a clean one leaves none.
    #[test]
    fn dropped_frames_are_logged_with_the_meeting_and_the_counts() {
        let log = steno_pipeline::fixtures::CapturedLog::warnings();
        let clean = Uuid::new_v4();
        log_dropped_frames(clean, &statistics());
        let lossy = Uuid::new_v4();
        let mut dropped = statistics();
        dropped.dropped_frames.insert(AudioLane::Mic, 250);
        dropped.dropped_frames.insert(AudioLane::System, 0);
        log_dropped_frames(lossy, &dropped);
        let text = log.text();
        assert!(!text.contains(&clean.to_string()), "{text}");
        let line = text
            .lines()
            .find(|line| line.contains(&lossy.to_string()))
            .unwrap_or_else(|| panic!("no line for the meeting: {text}"));
        assert!(line.contains("Mic: 250, System: 0"), "{line}");
    }

    /// A stop whose recording lost frames logs them with the meeting: a
    /// backend delivers two seconds at once while the writer is stalled
    /// behind a one-frame relay.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_stop_that_lost_frames_logs_them() {
        use steno_audio::testing::SyntheticCaptureBackend;

        let log = steno_pipeline::fixtures::CapturedLog::warnings();
        let harness = harness(&[]);
        let (go, stalled) = std::sync::mpsc::channel();
        let stalled = Arc::new(Mutex::new(Some(stalled)));
        let backend = Arc::new(SyntheticCaptureBackend::new(SyntheticOptions::tones(
            &[AudioLane::Mixed],
            &[(AudioLane::Mixed, 440.0)],
            2.0,
        )));
        // The first write waits for `go`, so the backend overflows a
        // one-frame relay meanwhile.
        harness.capture_with({
            let backend = backend.clone();
            Arc::new(move |configuration| {
                let stalled = stalled.clone();
                session_with_writes(configuration, backend.clone(), 1, move || {
                    let mut go = stalled.lock().unwrap().take();
                    move || {
                        if let Some(go) = go.take() {
                            let _ = go.recv_timeout(PATIENCE);
                        }
                        Ok(())
                    }
                })
            })
        });
        let recorder = &harness.recorder;
        start(recorder).await;
        let meeting_id = recorder.status().meeting_id.unwrap();
        let finished = backend.clone();
        tokio::task::spawn_blocking(move || finished.wait_until_finished())
            .await
            .unwrap();
        go.send(()).unwrap();
        stop(recorder).await;
        let text = log.text();
        assert!(
            text.lines()
                .any(|line| line.contains(&meeting_id.to_string())
                    && line.contains("the recording lost frames")),
            "{text}"
        );
    }

    /// Waits until the recorder is idle again after a recording that ended
    /// on its own, and returns the saved meeting.
    async fn ended_on_its_own(harness: &Harness, meeting_id: Uuid) -> steno_core::Meeting {
        eventually("the recording ended on its own", || {
            harness.recorder.status().state == RecordingState::Idle
        })
        .await;
        harness.store.meeting(meeting_id).unwrap().unwrap()
    }

    /// A disk that fills mid-recording fails the session's writer; the
    /// recorder hears of it at once instead of showing Recording until a
    /// Stop: the recording so far is saved with `failed` and queued, and
    /// the status says why it stopped.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_writer_that_fails_mid_recording_ends_and_saves_the_recording() {
        writer_fails_mid_recording(false).await;
    }

    /// As [`a_writer_that_fails_mid_recording_ends_and_saves_the_recording`],
    /// when the session finds no thread to end the recording on: the state
    /// stays `Recording`, and the watcher's tick reads the failed write
    /// ([`CaptureSession::write_failed`]) and stops and saves it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_failed_write_the_session_cannot_end_is_stopped_by_the_watcher() {
        writer_fails_mid_recording(true).await;
    }

    /// A recording whose writer fails after 30 frames ends on its own,
    /// saved and with the plain error line; `no_thread` refuses the
    /// session its thread to end it on.
    async fn writer_fails_mid_recording(no_thread: bool) {
        let harness = harness(&[]);
        let log = steno_pipeline::fixtures::CapturedLog::warnings();
        let make = failing_capture(Some(30), |options| options);
        harness.capture_with(Arc::new(move |configuration| {
            let session = make(configuration)?;
            if no_thread {
                session.refuse_the_writer_failure_thread();
            }
            Ok(session)
        }));
        start(&harness.recorder).await;
        let meeting_id = harness.recorder.status().meeting_id.unwrap();
        let meeting = ended_on_its_own(&harness, meeting_id).await;
        assert_eq!(
            harness.recorder.status().error.as_deref(),
            Some(
                "The recording stopped early: Steno could not write the recording to the disk. What was recorded until then is saved."
            )
        );
        // The detail goes to the log as a kind, never the error's text.
        let text = log.text();
        let line = text
            .lines()
            .find(|line| {
                line.contains(&meeting_id.to_string())
                    && line.contains("the recording ended with a failure")
            })
            .unwrap_or_else(|| panic!("no failure line: {text}"));
        assert!(line.contains("write failed"), "{line}");
        assert!(!line.contains("No space left"), "{line}");
        assert_eq!(meeting.end_reason, Some(RecordingEndReason::Failed));
        assert_ne!(
            meeting.state.kind(),
            steno_core::MeetingStateKind::Recording
        );
        let asset = harness.store.asset(meeting_id).unwrap().unwrap();
        assert!(asset.url.ends_with("recording.caf"), "{}", asset.url);
        assert!(meeting.duration > 0.0);
    }

    /// A device that stays lost through every restart ends the recording
    /// with `deviceLost`, saved, and the status says so.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_device_that_stays_lost_ends_and_saves_the_recording() {
        ends_as_a_lost_device(|options| {
            options
                .change_device_after(0.2)
                .restarts_that_fail(CaptureSession::RESTART_ATTEMPTS)
        })
        .await;
    }

    /// A rebuild that panics after a device change ends the recording as a
    /// device that stayed lost does: the recorder goes idle with its
    /// error, and the meeting is saved with `deviceLost`.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_rebuild_that_panics_ends_and_saves_the_recording() {
        ends_as_a_lost_device(|options| options.change_device_after(0.2).restart_panics()).await;
    }

    /// A recording over the synthetic tone as `device` changes it ends on
    /// its own as a lost device: the status says so, and the meeting is
    /// saved with `deviceLost`.
    async fn ends_as_a_lost_device(device: fn(SyntheticOptions) -> SyntheticOptions) {
        let harness = harness(&[]);
        harness.capture_with(failing_capture(None, device));
        start(&harness.recorder).await;
        let meeting_id = harness.recorder.status().meeting_id.unwrap();
        let meeting = ended_on_its_own(&harness, meeting_id).await;
        assert_eq!(
            harness.recorder.status().error.as_deref(),
            Some(
                "The recording stopped early: an audio device disappeared. What was recorded until then is saved."
            )
        );
        assert_eq!(meeting.end_reason, Some(RecordingEndReason::DeviceLost));
        assert!(harness.store.asset(meeting_id).unwrap().is_some());
        assert!(meeting.duration > 0.0);
    }

    /// A stop that panics part way (here the host's change hook, on the
    /// stopping thread, before the save) leaves the recorder idle with an
    /// error rather than stopping for good, and the next recording starts.
    /// The meeting's row stays `recording` for the next launch, over a
    /// master the session's drop closed.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_stop_that_panics_leaves_the_recorder_idle_with_an_error() {
        let harness = harness(&[]);
        start(&harness.recorder).await;
        let meeting_id = harness.recorder.status().meeting_id.unwrap();
        eventually("frames reached the files", || {
            harness.recorder.status().levels.is_some()
        })
        .await;
        let watched = Arc::downgrade(&harness.recorder);
        harness.recorder.on_change(Arc::new(move || {
            let stopping = watched
                .upgrade()
                .is_some_and(|recorder| recorder.status().state == RecordingState::Stopping);
            let on_the_stop = std::thread::current().name() == Some("test-stop");
            assert!(!(stopping && on_the_stop), "the change hook panics");
        }));
        let stopper = harness.recorder.clone();
        let stopping = std::thread::Builder::new()
            .name("test-stop".into())
            .spawn(move || stopper.stop())
            .unwrap();
        assert!(stopping.join().is_err(), "the stop panicked");
        let status = harness.recorder.status();
        assert_eq!(status.state, RecordingState::Idle);
        assert_eq!(status.error.as_deref(), Some(SAVE_PANICKED));
        let meeting = harness.store.meeting(meeting_id).unwrap().unwrap();
        assert_eq!(meeting.state, steno_core::MeetingState::Recording);
        let audio_folder =
            steno_core::paths::file_url_path(&harness.store.settings().unwrap().audio_folder)
                .unwrap();
        let master = steno_core::RecordingLayout::new(&audio_folder, meeting_id)
            .master(steno_core::AudioFormat::Caf48kFloat32);
        assert!(steno_audio::CafFile::read(&master).unwrap().frame_count() > 0);
        start(&harness.recorder).await;
        stop(&harness.recorder).await;
    }

    /// A start that panics part way (here the host's change hook, on the
    /// starting thread) leaves the recorder idle with an error rather than
    /// starting for good, and the next recording starts.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_start_that_panics_leaves_the_recorder_idle_with_an_error() {
        let harness = harness(&[]);
        let watched = Arc::downgrade(&harness.recorder);
        harness.recorder.on_change(Arc::new(move || {
            let starting = watched
                .upgrade()
                .is_some_and(|recorder| recorder.status().state == RecordingState::Starting);
            let on_the_start = std::thread::current().name() == Some("test-start");
            assert!(!(starting && on_the_start), "the change hook panics");
        }));
        let starter = harness.recorder.clone();
        let starting = std::thread::Builder::new()
            .name("test-start".into())
            .spawn(move || starter.start(CaptureMode::InPerson, None))
            .unwrap();
        assert!(starting.join().is_err(), "the start panicked");
        let status = harness.recorder.status();
        assert_eq!(status.state, RecordingState::Idle);
        assert_eq!(status.error.as_deref(), Some(START_PANICKED));
        start(&harness.recorder).await;
        stop(&harness.recorder).await;
    }

    /// A device that disappeared cut the recording short whoever stopped
    /// it; a failed write or close says "stopped early" only when it ended
    /// the recording, and a Stop, a quit or an ended call that met one says
    /// the saved recording may be incomplete.
    #[test]
    fn the_failure_line_follows_the_failure_and_who_stopped() {
        let write = CaptureError::WriterFailed("/Users/someone/Audio: No space left".into());
        for reason in [
            RecordingEndReason::Manual,
            RecordingEndReason::Quit,
            RecordingEndReason::CallEnded { app_name: None },
        ] {
            assert_eq!(
                ended_with(&write, &reason),
                "The recording is saved, but Steno could not write all of it to the disk, so it may be incomplete."
            );
            assert!(
                ended_with(&CaptureError::DeviceLost, &reason).contains("audio device disappeared")
            );
        }
        assert_eq!(
            ended_with(&write, &RecordingEndReason::Failed),
            "The recording stopped early: Steno could not write the recording to the disk. What was recorded until then is saved."
        );
    }

    /// Without room for the save, a recording does not start, and no
    /// meeting is written.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_recording_does_not_start_on_a_full_disk() {
        let harness = harness(&[]);
        harness
            .free
            .store(DiskWatch::STOP_BELOW_BYTES - 1, Ordering::SeqCst);
        let starting = harness.recorder.clone();
        tokio::task::spawn_blocking(move || starting.start(CaptureMode::Call, None))
            .await
            .unwrap();
        let status = harness.recorder.status();
        assert_eq!(status.state, RecordingState::Idle);
        assert_eq!(
            status.error.as_deref(),
            Some(
                "Recording could not start: the disk is almost full. Free some space and try again."
            )
        );
        assert_eq!(harness.store.meetings(10, 0).unwrap(), []);
    }

    /// With little room the recording starts and warns how long it can
    /// run; when the room runs out it stops and is saved before the disk
    /// fills.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_recording_warns_when_the_disk_runs_low_and_stops_before_it_fills() {
        let harness = harness(&[]);
        // Ten minutes of an in-person recording above the floor.
        let ten_minutes = 600 * (48_000 * 4 + 16_000 * 2);
        harness
            .free
            .store(DiskWatch::STOP_BELOW_BYTES + ten_minutes, Ordering::SeqCst);
        start(&harness.recorder).await;
        let meeting_id = harness.recorder.status().meeting_id.unwrap();
        assert_eq!(
            harness.recorder.status().warning.as_deref(),
            Some(
                "The disk is almost full: about 10 minutes of recording left. Steno stops and saves the recording before the disk fills."
            )
        );
        harness
            .free
            .store(DiskWatch::STOP_BELOW_BYTES / 2, Ordering::SeqCst);
        let meeting = ended_on_its_own(&harness, meeting_id).await;
        assert_eq!(
            harness.recorder.status().error.as_deref(),
            Some(STOPPED_FOR_SPACE)
        );
        assert_eq!(meeting.end_reason, Some(RecordingEndReason::Failed));
        assert!(harness.store.asset(meeting_id).unwrap().is_some());
    }

    /// The warning follows the room left while recording.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_disk_warning_appears_once_the_room_runs_low_while_recording() {
        let harness = harness(&[]);
        start(&harness.recorder).await;
        assert_eq!(harness.recorder.status().warning, None);
        harness.free.store(
            DiskWatch::STOP_BELOW_BYTES + 60 * (48_000 * 4 + 16_000 * 2),
            Ordering::SeqCst,
        );
        eventually("the warning shows", || {
            harness
                .recorder
                .status()
                .warning
                .is_some_and(|warning| warning.contains("about 1 minute of recording left"))
        })
        .await;
        harness.free.store(u64::MAX, Ordering::SeqCst);
        eventually("the warning goes with room again", || {
            harness.recorder.status().warning.is_none()
        })
        .await;
        stop(&harness.recorder).await;
    }

    /// The disk's warning and the fallback microphone's show side by side.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_disk_warning_does_not_hide_the_fallback_microphone() {
        let harness = harness_on_inputs(
            input(Some("Built-in Audio"), true),
            input(Some("Built-in Audio"), true),
        );
        harness.free.store(
            DiskWatch::STOP_BELOW_BYTES + 60 * (48_000 * 4 + 16_000 * 2),
            Ordering::SeqCst,
        );
        start(&harness.recorder).await;
        assert_eq!(
            harness.recorder.status().warning,
            Some(format!(
                "{} Recording from Built-in Audio. The microphone chosen in Settings is not \
                 available.",
                low_space_warning(1, true)
            ))
        );
        stop(&harness.recorder).await;
    }

    /// A free space that cannot be read never stops a recording.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_unreadable_free_space_lets_the_recording_run() {
        let harness = harness(&[]);
        let reads = Arc::new(AtomicUsize::new(0));
        harness.recorder.watch_disk_with({
            let reads = reads.clone();
            disk_with(move |_| {
                reads.fetch_add(1, Ordering::SeqCst);
                Err(std::io::Error::other("statvfs failed"))
            })
        });
        start(&harness.recorder).await;
        eventually("the space was read while recording", || {
            reads.load(Ordering::SeqCst) > 2
        })
        .await;
        assert_eq!(harness.recorder.status().state, RecordingState::Recording);
        stop(&harness.recorder).await;
        assert_eq!(harness.recorder.status().error, None);
    }

    /// A volume that reports no size (some network and FUSE file systems
    /// answer zero blocks), or more free than it holds, is unreadable: the
    /// recording starts and runs.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_volume_that_reports_no_size_lets_the_recording_run() {
        for reported in [
            Volume {
                available: 0,
                total: 0,
            },
            Volume {
                available: 2,
                total: 1,
            },
        ] {
            let harness = harness(&[]);
            let reads = Arc::new(AtomicUsize::new(0));
            harness.recorder.watch_disk_with({
                let reads = reads.clone();
                disk_with(move |_| {
                    reads.fetch_add(1, Ordering::SeqCst);
                    Ok(reported)
                })
            });
            start(&harness.recorder).await;
            eventually("the space was read while recording", || {
                reads.load(Ordering::SeqCst) > 2
            })
            .await;
            let status = harness.recorder.status();
            assert_eq!(status.state, RecordingState::Recording);
            assert_eq!(status.warning, None);
            stop(&harness.recorder).await;
            assert_eq!(harness.recorder.status().error, None);
        }
    }

    /// The free space is the smaller of the recordings folder's volume and
    /// the database folder's; one that cannot be read is left out, and with
    /// neither readable there is no number.
    #[test]
    fn the_free_space_is_the_smaller_of_the_two_volumes() {
        let audio = tempfile::tempdir().unwrap();
        let database = tempfile::tempdir().unwrap();
        let watch = |audio_free: Option<u64>, database_free: Option<u64>| {
            let audio_path = audio.path().to_path_buf();
            let mut watch = disk_with(move |path| {
                let free = if path == audio_path {
                    audio_free
                } else {
                    database_free
                };
                free.map(volume)
                    .ok_or_else(|| std::io::Error::other("statvfs failed"))
            });
            watch.database_folder = Some(database.path().to_path_buf());
            watch
        };
        // The recordings folder does not exist yet: its parent is asked.
        let recordings = audio.path().join("Audio");
        assert_eq!(watch(Some(10), Some(20)).free_for(audio.path()), Some(10));
        assert_eq!(watch(Some(30), Some(20)).free_for(audio.path()), Some(20));
        assert_eq!(watch(Some(30), Some(20)).free_for(&recordings), Some(20));
        assert_eq!(watch(None, Some(20)).free_for(audio.path()), Some(20));
        assert_eq!(watch(Some(30), None).free_for(audio.path()), Some(30));
        assert_eq!(watch(None, None).free_for(audio.path()), None);
    }

    /// Where the floor stops a recording, the minutes count to it and
    /// below it there is no room; where it does not (the Mac), the warning
    /// comes at the same point, the minutes count to a full disk, and the
    /// room never runs out.
    #[test]
    fn the_room_left_counts_from_the_floor() {
        let rate = 448_000;
        let floor = DiskWatch::STOP_BELOW_BYTES;
        assert_eq!(Room::of(0, rate, true), Room::Full);
        assert_eq!(Room::of(floor - 1, rate, true), Room::Full);
        assert_eq!(Room::of(floor + 30 * 60 * rate, rate, true), Room::Enough);
        assert_eq!(
            Room::of(floor + 29 * 60 * rate, rate, true),
            Room::Low { minutes: 29 }
        );
        assert_eq!(Room::of(floor, rate, true), Room::Low { minutes: 1 });

        assert_eq!(Room::of(floor + 30 * 60 * rate, rate, false), Room::Enough);
        assert_eq!(
            Room::of(floor + 29 * 60 * rate, rate, false),
            Room::Low {
                minutes: 29 + floor / rate / 60
            }
        );
        assert_eq!(
            Room::of(30 * 60 * rate, rate, false),
            Room::Low { minutes: 30 }
        );
        assert_eq!(Room::of(0, rate, false), Room::Low { minutes: 1 });
    }

    /// The warning promises the stop only where the floor stops the
    /// recording.
    #[test]
    fn the_low_space_warning_promises_a_stop_only_where_there_is_one() {
        assert_eq!(
            low_space_warning(29, true),
            "The disk is almost full: about 29 minutes of recording left. \
             Steno stops and saves the recording before the disk fills."
        );
        assert_eq!(
            low_space_warning(1, false),
            "The disk is almost full: about 1 minute of recording left. \
             Free some space to keep recording."
        );
    }

    /// The system's disk watch stops recordings at the floor everywhere
    /// but on the Mac, whose `statvfs` leaves out purgeable space.
    #[test]
    fn the_system_disk_watch_stops_at_the_floor_only_off_the_mac() {
        assert_eq!(DiskWatch::system(None).stops, !cfg!(target_os = "macos"));
    }

    /// Under the Mac's policy a reading below the floor neither refuses
    /// the start nor stops the recording: it starts with a warning that
    /// promises no stop, records on through many readings, and a Stop
    /// saves it as usual.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn under_the_mac_s_policy_a_reading_below_the_floor_only_warns() {
        let harness = harness(&[]);
        let reads = Arc::new(AtomicUsize::new(0));
        harness.recorder.watch_disk_with({
            let reads = reads.clone();
            DiskWatch {
                stops: false,
                ..disk_with(move |_| {
                    reads.fetch_add(1, Ordering::SeqCst);
                    Ok(volume(DiskWatch::STOP_BELOW_BYTES / 2))
                })
            }
        });
        start(&harness.recorder).await;
        let meeting_id = harness.recorder.status().meeting_id.unwrap();
        // 256 MiB at 224 000 bytes a second, to a full disk.
        assert_eq!(
            harness.recorder.status().warning.as_deref(),
            Some(
                "The disk is almost full: about 19 minutes of recording left. Free some space to keep recording."
            )
        );
        let at_start = reads.load(Ordering::SeqCst);
        eventually("the space was read while recording", || {
            reads.load(Ordering::SeqCst) > at_start + 5
        })
        .await;
        let status = harness.recorder.status();
        assert_eq!(status.state, RecordingState::Recording);
        assert_eq!(status.error, None);
        stop(&harness.recorder).await;
        assert_eq!(harness.recorder.status().error, None);
        let meeting = harness.store.meeting(meeting_id).unwrap().unwrap();
        assert_eq!(meeting.end_reason, Some(RecordingEndReason::Manual));
        assert!(harness.store.asset(meeting_id).unwrap().is_some());
    }

    /// A low-space warning that arrives while the recording is stopping
    /// does not show: the stop's outcome sets the messages.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_disk_warning_during_a_stop_does_not_show() {
        let harness = harness(&[]);
        start(&harness.recorder).await;
        let meeting_id = harness.recorder.status().meeting_id.unwrap();
        let stop_seen = held_in(&harness.recorder, RecordingState::Stopping);
        let stopper = harness.recorder.clone();
        let stopping = std::thread::spawn(move || stopper.stop());
        stop_seen.recv_timeout(PATIENCE).expect("the stop began");
        harness
            .recorder
            .warn(meeting_id, Some(low_space_warning(5, true)));
        let status = harness.recorder.status();
        assert_eq!(status.state, RecordingState::Stopping);
        assert_eq!(status.warning, None);
        stopping.join().unwrap();
        assert_eq!(harness.recorder.status().warning, None);
    }

    /// A call writes about 1.6 GB an hour, an in-person recording half of
    /// it, the raw microphone 0.7 GB more.
    #[test]
    fn a_call_writes_about_one_point_six_gigabytes_an_hour() {
        let call = CaptureConfiguration::new(steno_audio::CaptureMode::Call, "/audio");
        assert_eq!(bytes_per_second(&call) * 3600, 1_612_800_000);
        let mut in_person = CaptureConfiguration::new(steno_audio::CaptureMode::InPerson, "/audio");
        assert_eq!(bytes_per_second(&in_person), 224_000);
        in_person.keep_raw_mic_lane = true;
        assert_eq!(bytes_per_second(&in_person), 416_000);
    }
}
