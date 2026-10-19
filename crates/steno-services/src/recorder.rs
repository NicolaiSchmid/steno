//! The host's `Recorder` over the capture session and the Mac recording
//! intake. A start names the meeting after the calendar event under way
//! without ever waiting on the lookup; a per-recording watcher thread
//! finishes a session that fails on its own, warns and stops before the
//! disk fills, and follows the session's notices for missing audio.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, Weak};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use chrono::{FixedOffset, Utc};
use steno_audio::{
    CaptureConfiguration, CaptureError, CaptureNotice, CaptureSession, CaptureState,
    CaptureStatistics, CaptureStream, DeviceChangeReason, FRAMES_PER_SECOND,
    LaneLevels as AudioLevels,
};
use steno_bridge::{CaptureMode, PermissionKind, RecordingState};
use steno_core::{MeetingSource, RecordingEndReason, Store};
use steno_host::services::{
    INSTALLING_UPDATE, LaneLevels, LeftRecording, Permissions, QUITTING_REFUSAL, Recorder,
    RecorderStatus, SpeechModels, StartHold,
};
use steno_host::speech::ModelAsset;
use steno_pipeline::{Attendee, LocalRecordingIntake, LocalRecordingIntakeError, RecordingResult};
use steno_speech::SpeechRuntime;
use uuid::Uuid;

use crate::auto_stop::{AUTO_STOP_GRACE, CallWatch, Countdown, MicrophoneActivity};
use crate::block_on;
use crate::calendar::{Calendar, CalendarLookups, Lookup, NoCalendar};
use crate::pipeline::CurrentPipeline;
use crate::recovery::{RecoveryError, Unrecoverable};

/// Builds a capture session for a configuration; tests pass a synthetic
/// backend.
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

/// What the volume that holds a path reports; tests pass a fake.
pub(crate) type FreeSpace = Arc<dyn Fn(&Path) -> std::io::Result<Volume> + Send + Sync>;

/// How a recording watches the free space.
#[derive(Clone)]
pub(crate) struct DiskWatch {
    /// Reads a volume.
    pub(crate) free_space: FreeSpace,
    /// How often a recording reads it.
    pub(crate) interval: Duration,
    /// The database's folder, whose volume is read too: the save writes there.
    pub(crate) database_folder: Option<PathBuf>,
}

impl DiskWatch {
    /// Below this a recording does not start, and one in progress stops and
    /// is saved: room for the database, the processing and the system, so
    /// the save itself never meets a full disk.
    pub(crate) const STOP_BELOW_BYTES: u64 = 512 * 1024 * 1024;
    /// Below this `statvfs` reading the Mac does not start or go on,
    /// whatever space macOS says it frees for important use: `CacheDelete`
    /// purges that space asynchronously and a write never waits for it. The
    /// 384 MiB gap up to [`Self::STOP_BELOW_BYTES`] is the purge's window.
    #[cfg(any(target_os = "macos", test))]
    pub(crate) const HARD_FLOOR_BYTES: u64 = 128 * 1024 * 1024;
    /// The recording time left above [`Self::STOP_BELOW_BYTES`] below which
    /// a recording warns.
    pub(crate) const WARN_BELOW: Duration = Duration::from_secs(30 * 60);

    /// The system's free space, read every five seconds, on the volumes of
    /// the recordings folder and of `database_folder`.
    #[must_use]
    pub(crate) fn system(database_folder: Option<&Path>) -> Self {
        Self {
            free_space: system_volume(),
            interval: Duration::from_secs(5),
            database_folder: database_folder.map(Path::to_path_buf),
        }
    }

    /// The bytes free for a recording into `audio_folder`: the smaller of
    /// what its volume and the database folder's report, each asked of its
    /// nearest existing folder. Unreadable or implausible volumes are left
    /// out; `None` when none is left, which never stops a recording.
    fn free_for(&self, audio_folder: &Path) -> Option<u64> {
        std::iter::once(audio_folder)
            .chain(self.database_folder.as_deref())
            .filter_map(|folder| self.available(folder))
            .min()
    }

    /// The bytes free on the volume of `folder`, when it reports a size that
    /// holds them.
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

/// The volume that holds a path: `statvfs`, with the space free for
/// important use beside it, left out when that read fails. Each read asks
/// macOS afresh on a new `NSURL` (a reused one caches its values), so the
/// reading follows the writes.
#[cfg(target_os = "macos")]
fn system_volume() -> FreeSpace {
    let failing = Mutex::default();
    Arc::new(move |path| {
        let statvfs = statvfs(path)?;
        let important = important_use(
            path,
            steno_macos::volume::available_for_important_usage,
            &failing,
        );
        Ok(mac_volume(statvfs, important))
    })
}

/// The volume that holds a path as the system reports it.
#[cfg(not(target_os = "macos"))]
fn system_volume() -> FreeSpace {
    Arc::new(statvfs)
}

/// The space free for important use on the volume of `path`, `None` when
/// `read` fails. The first failure of a streak on a folder warns with the
/// error's kind alone (its text names the folder); `failing` holds the
/// folders whose last read failed.
#[cfg(any(target_os = "macos", test))]
fn important_use(
    path: &Path,
    read: impl FnOnce(&Path) -> std::io::Result<u64>,
    failing: &Mutex<std::collections::HashSet<PathBuf>>,
) -> Option<u64> {
    let result = read(path);
    let mut failing = failing
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    match result {
        Ok(bytes) => {
            failing.remove(path);
            Some(bytes)
        }
        Err(error) => {
            if failing.insert(path.to_path_buf()) {
                tracing::warn!(
                    kind = ?error.kind(),
                    "the space free for important use could not be read"
                );
            }
            tracing::debug!(%error, "the space free for important use could not be read");
            None
        }
    }
}

/// `statvfs` (`GetDiskFreeSpaceExW` on Windows) of the volume of `path`.
fn statvfs(path: &Path) -> std::io::Result<Volume> {
    fs4::statvfs(path).map(|stats| Volume {
        available: stats.available_space(),
        total: stats.total_space(),
    })
}

/// The Mac's reading of a volume: the larger of `statvfs` and the space
/// free for `important` use (so purgeable space counts; a 0 from a volume
/// `CacheDelete` does not manage leaves `statvfs` deciding), held to
/// `statvfs` plus the gap between [`DiskWatch::STOP_BELOW_BYTES`] and
/// [`DiskWatch::HARD_FLOOR_BYTES`].
#[cfg(any(target_os = "macos", test))]
fn mac_volume(statvfs: Volume, important: Option<u64>) -> Volume {
    let ceiling = statvfs
        .available
        .saturating_add(DiskWatch::STOP_BELOW_BYTES - DiskWatch::HARD_FLOOR_BYTES);
    Volume {
        available: important.unwrap_or(0).clamp(statvfs.available, ceiling),
        ..statvfs
    }
}

/// What a recording does about the space left.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Room {
    Enough,
    /// A warning with the minutes of recording left, at least one.
    Low {
        minutes: u64,
    },
    /// Too little to go on.
    Full,
}

impl Room {
    /// The room `free` bytes leave a recording that writes
    /// `bytes_per_second`: none below the floor, a warning under
    /// [`DiskWatch::WARN_BELOW`] above it.
    fn of(free: u64, bytes_per_second: u64) -> Self {
        let rate = bytes_per_second.max(1);
        let Some(spare) = free.checked_sub(DiskWatch::STOP_BELOW_BYTES) else {
            return Room::Full;
        };
        if spare / rate >= DiskWatch::WARN_BELOW.as_secs() {
            return Room::Enough;
        }
        Room::Low {
            minutes: (spare / rate / 60).max(1),
        }
    }
}

/// The bytes a second the files of `configuration` grow by: about 1.6 GB an
/// hour for a call.
fn bytes_per_second(configuration: &CaptureConfiguration) -> u64 {
    let lanes = configuration.lanes().len() as u64;
    let raw = u64::from(configuration.keep_raw_mic_lane);
    (lanes + raw) * 48_000 * 4 + lanes * 16_000 * 2
}

/// The status line of a saved recording whose result carries `failure`, in
/// plain words (the error's text names paths, so it goes to the log). A
/// lost device cut the recording short whoever stopped it; a failed write
/// or close only when the session ended on its own.
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

/// The kind of `failure` a `warn` line names, never its text (it can hold a
/// path).
fn failure_kind(failure: &CaptureError) -> &'static str {
    match failure {
        CaptureError::DeviceLost => "device lost",
        CaptureError::WriterFailed(_) => "write failed",
        CaptureError::RecordingExists(_) => "folder exists",
        CaptureError::InputDeviceUnavailable => "no input device",
        CaptureError::OutputDeviceUnavailable => "no output device",
        CaptureError::UnsupportedSampleRate { .. } => "sample rate",
        _ => "capture failed",
    }
}

/// A `warn` line for a recording that ended with `failure`.
fn log_failure(meeting_id: Uuid, failure: &CaptureError, reason: &RecordingEndReason) {
    tracing::warn!(
        meeting = %meeting_id,
        reason = ?reason.kind(),
        kind = failure_kind(failure),
        "the recording ended with a failure"
    );
}

/// The prefix of the status line of a start that failed; [`refused`] says
/// why.
const COULD_NOT_START: &str = "Recording could not start:";

/// Why a recording that `CaptureRecorder::start` refused did not start.
const NO_ROOM_TO_START: &str = "the disk is almost full. Free some space and try again.";

/// Why a start whose session could not be built, or whose devices did not
/// open for no stated reason, did not start.
const DEVICES_DID_NOT_OPEN: &str = "Steno could not open the audio devices.";

/// `reason` for a failed start's status line, after a `warn` line naming
/// `kind`, never the error's text.
fn refused(kind: &str, reason: impl Into<String>) -> String {
    tracing::warn!(kind, "the recording could not start");
    reason.into()
}

/// Why a capture session's start that failed with `error` did not start.
fn capture_refused(error: &CaptureError) -> String {
    let reason = match error {
        CaptureError::InputDeviceUnavailable => "no microphone is available.".to_owned(),
        CaptureError::OutputDeviceUnavailable => "no sound output is available.".to_owned(),
        CaptureError::UnsupportedSampleRate { actual } => {
            format!("the audio devices run at {actual} Hz, which Steno cannot record.")
        }
        CaptureError::DeviceLost => "an audio device disappeared.".to_owned(),
        CaptureError::WriterFailed(_) | CaptureError::RecordingExists(_) => {
            "Steno could not write to the recordings folder.".to_owned()
        }
        _ => DEVICES_DID_NOT_OPEN.to_owned(),
    };
    refused(failure_kind(error), reason)
}

/// The kind of an intake failure a `warn` line names.
fn intake_kind(error: &LocalRecordingIntakeError) -> &'static str {
    match error {
        LocalRecordingIntakeError::NotRecording(..) => "not recording",
        LocalRecordingIntakeError::Store(_) => "store",
        LocalRecordingIntakeError::Pipeline(_) => "pipeline",
    }
}

/// A `warn` line for a recording that could not be saved, naming `kind`.
fn log_not_saved(meeting_id: Uuid, kind: &str) {
    tracing::warn!(meeting = %meeting_id, kind, "the recording could not be saved");
}

/// The status line of a stop whose session could not finish the files.
const FILES_NOT_FINISHED: &str =
    "Recording could not be saved: Steno could not finish the recording's files.";

/// The status line of a stop whose meeting could not be stored.
const MEETING_NOT_STORED: &str = "Recording could not be saved: Steno could not store the meeting.";

/// Why a stop did not save its recording.
#[derive(Debug, Clone, Copy)]
enum Unsaved {
    /// The recording stays on disk and its meeting `recording`, for the next
    /// launch to recover.
    Kept,
    /// The meeting failed or moved on; the status line says how.
    Settled(&'static str),
}

impl Unsaved {
    /// The status line.
    fn message(self) -> &'static str {
        match self {
            Unsaved::Kept => KEPT_FOR_THE_NEXT_LAUNCH,
            Unsaved::Settled(message) => message,
        }
    }
}

/// What a stop says when the meeting could not be stored. A meeting another
/// process moved on is not this recorder's to keep; otherwise the meeting
/// stays `recording` for the next launch. Either way the files and the
/// recorded folder stay, so the next launch can adopt the master.
fn not_saved(meeting_id: Uuid, error: &LocalRecordingIntakeError) -> Unsaved {
    log_not_saved(meeting_id, intake_kind(error));
    if matches!(
        error,
        LocalRecordingIntakeError::NotRecording(..)
            | LocalRecordingIntakeError::Store(
                steno_core::StoreError::NotRecording(..)
                    | steno_core::StoreError::MeetingNotFound(_)
            )
    ) {
        Unsaved::Settled(MEETING_NOT_STORED)
    } else {
        Unsaved::Kept
    }
}

/// The status line of a stop whose recording is kept for the next launch.
const KEPT_FOR_THE_NEXT_LAUNCH: &str = "The recording is kept, but it could not be saved right now. \
     Steno will process it the next time it starts.";

/// The warning of a stop whose capture failed and whose files were recovered.
const RECOVERED_AFTER_A_FAILURE: &str =
    "Recording stopped because of an error; the audio up to that point was kept.";

/// The warning of a recording with `minutes` left.
fn low_space_warning(minutes: u64) -> String {
    let unit = if minutes == 1 { "minute" } else { "minutes" };
    format!(
        "The disk is almost full: about {minutes} {unit} of recording left. \
         Steno stops and saves the recording before the disk fills."
    )
}

/// The status line of a recording stopped because the disk was nearly full.
const STOPPED_FOR_SPACE: &str =
    "The disk is almost full, so Steno stopped the recording and saved it.";

struct Active {
    session: Arc<CaptureSession>,
    meeting_id: Uuid,
    /// The audio folder the capture writes into, for a recovery after a
    /// failed stop: the settings may name another by then.
    audio_folder: PathBuf,
    mode: CaptureMode,
    /// The latest lane levels, written by the forwarding thread.
    levels: Arc<Mutex<Option<LaneLevels>>>,
    /// The forwarding thread, joined once the session is dropped; `None`
    /// when it could not be spawned.
    level_thread: Option<JoinHandle<()>>,
    /// The input recorded in place of the chosen microphone, kept apart from
    /// the status's own warning so a rebuild can set and clear it.
    fallback: Arc<Mutex<Fallback>>,
    /// A device that stopped delivering, as the notices tell it.
    outage: Arc<Mutex<Outage>>,
    /// When the session began recording, for the missing-audio note.
    recording_since: Instant,
    /// Re-reads the microphone after each rebuild; `None` when it could not
    /// be spawned.
    notice_thread: Option<JoinHandle<()>>,
    /// The disk watch's warning, kept apart from the status's own warning so
    /// it neither hides the fallback warning nor outlives the low room.
    disk_warning: DiskWarning,
}

/// The disk watch's warning of one recording ([`low_space_warning`]).
struct DiskWarning {
    /// The minutes left at the latest reading; `None` with room.
    minutes: Option<u64>,
    /// The minutes left when the user dismissed the warning: it shows again
    /// only once fewer are left. A reading with room forgets it.
    dismissed: Option<u64>,
}

impl DiskWarning {
    /// The warning the status shows, if any.
    fn shown(&self) -> Option<String> {
        let minutes = self
            .minutes
            .filter(|minutes| self.dismissed.is_none_or(|dismissed| *minutes < dismissed))?;
        Some(low_space_warning(minutes))
    }

    /// After a reading that leaves `minutes`, `None` with room.
    fn set(&mut self, minutes: Option<u64>) {
        if minutes.is_none() {
            self.dismissed = None;
        }
        self.minutes = minutes;
    }

    /// The user dismissed the messages; a warning not shown stays as it was.
    fn dismiss(&mut self) {
        if self.shown().is_some() {
            self.dismissed = self.minutes;
        }
    }
}

/// The input a recording records in place of the chosen microphone.
#[derive(Default)]
struct Fallback {
    /// The input now; `None` on the chosen microphone.
    current: Option<String>,
    /// The latest fallback input, kept after a return to the chosen
    /// microphone for the note after the stop.
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

/// A device that stopped delivering in a recording.
#[derive(Default)]
struct Outage {
    /// The microphone whose stall the latest rebuild followed, when its
    /// clock drives the capture ([`microphone_is_the_master`]); `None`
    /// otherwise, as another stall says nothing of which device is silent.
    stalled: Option<String>,
    /// The restarts went on past `CaptureSession::RESTART_ATTEMPTS` and no
    /// audio arrived since; cleared by `Delivering` (not a resume, which may
    /// stall again soon) and once dismissed.
    still_trying: bool,
    /// The capture is recovering: from a `DeviceChanged` until its
    /// `DeviceResumed` (or `Delivering` while `restarting`). The call
    /// auto-stop waits meanwhile, for one grace at most, then a fresh
    /// countdown ends the call whatever the capture does
    /// ([`crate::auto_stop`]). A dismissal leaves it.
    recovering: bool,
    /// A `StillRestarting` came and no `Delivering` since, so a resume does
    /// not end the recovery. A dismissal leaves it.
    restarting: bool,
}

impl Outage {
    /// Follows `notice` for the warning and the recovery; `true` when it ends
    /// a recovery, so the auto-stop resumes.
    fn follow(&mut self, notice: &CaptureNotice) -> bool {
        match notice {
            CaptureNotice::DeviceChanged(_) => {
                self.recovering = true;
                false
            }
            CaptureNotice::StillRestarting { .. } => {
                self.still_trying = true;
                self.restarting = true;
                false
            }
            CaptureNotice::DeviceResumed { .. } => {
                !self.restarting && std::mem::take(&mut self.recovering)
            }
            CaptureNotice::Delivering => {
                self.still_trying = false;
                self.restarting = false;
                std::mem::take(&mut self.recovering)
            }
        }
    }

    /// The user dismissed the messages: the warning goes until the restarts
    /// go on once more; the recovery stands.
    fn dismiss(&mut self) {
        self.still_trying = false;
    }
}

/// Whether the microphone's clock drives a capture in `mode`, so a stall is
/// the microphone's: an in-person capture records it alone, and WASAPI
/// makes it the master whenever a lane needs it. A Mac call runs on the
/// output's clock and a Linux one on the graph's driver. This must be sure
/// before it names a device, unlike
/// [`CaptureBackend::stall_may_be_the_microphone`], which says "may"; a
/// change to either goes to the other.
///
/// [`CaptureBackend::stall_may_be_the_microphone`]: steno_audio::CaptureBackend::stall_may_be_the_microphone
fn microphone_is_the_master(mode: CaptureMode) -> bool {
    mode == CaptureMode::InPerson || cfg!(windows)
}

struct Inner {
    status: RecorderStatus,
    active: Option<Active>,
    /// Set by [`CaptureRecorder::stop_for_quit`]: no recording starts any more.
    quitting: bool,
    /// The [`Recorder::hold_starts`] holds alive: while there is one, a start
    /// is refused with [`INSTALLING_UPDATE`].
    start_holds: usize,
    /// What the launch has to say, shown whenever the recorder is idle until
    /// the user dismisses the messages.
    launch_note: Option<String>,
    /// The auto-stop after a call ends, for the recording in progress.
    calls: CallWatch,
    /// The system audio tests running ([`CaptureRecorder::run_tone_test`]).
    tone_tests: ToneTests,
}

/// The system audio tests that play their tone now.
struct ToneTests {
    running: usize,
    /// How long a start waits for the stopped tests to end.
    patience: Duration,
}

/// How long a start waits for a system audio test it stopped to end, after
/// which the recording starts all the same. The tone has ended well before
/// this; it covers a tap stuck inside the system.
const TONE_TEST_PATIENCE: Duration = Duration::from_secs(5);

/// [`CaptureRecorder`]'s [`StartHold`]: the last one dropped clears the
/// refusal's error.
struct HeldStarts(Weak<CaptureRecorder>);

impl Drop for HeldStarts {
    fn drop(&mut self) {
        let Some(recorder) = self.0.upgrade() else {
            return;
        };
        let mut inner = recorder.inner();
        inner.start_holds -= 1;
        let cleared = inner.start_holds == 0
            && inner
                .status
                .error
                .take_if(|error| *error == INSTALLING_UPDATE)
                .is_some();
        drop(inner);
        if cleared {
            recorder.notify();
        }
    }
}

/// The meeting source a capture in `mode` records.
fn source(mode: CaptureMode) -> MeetingSource {
    match mode {
        CaptureMode::Call => MeetingSource::MacCall,
        CaptureMode::InPerson => MeetingSource::MacInPerson,
    }
}

/// dBFS to the `0...1` RMS the bridge carries.
fn linear(db: f32) -> f64 {
    10f64.powf(f64::from(db) / 20.0).clamp(0.0, 1.0)
}

/// The input `stream` records in place of the chosen microphone; `None` on
/// the chosen one.
fn fallback_input(stream: Option<&CaptureStream>) -> Option<String> {
    let input = stream?.input.as_ref().filter(|input| input.is_fallback)?;
    Some(
        input
            .name
            .clone()
            .unwrap_or_else(|| "the system default microphone".to_owned()),
    )
}

/// The device `stream` records, by its microphone's name, else "the
/// microphone".
fn device_name(stream: Option<&CaptureStream>) -> String {
    stream
        .and_then(|stream| stream.input.as_ref()?.name.clone())
        .unwrap_or_else(|| "the microphone".to_owned())
}

/// The share of wall time a device clock may drift, which the missing-audio
/// note leaves out: 0.1 %, twice a cheap device's 500 ppm.
const CLOCK_DRIFT: f64 = 0.001;

/// The warning while no audio arrives and the session keeps restarting,
/// naming the device when it is known to have stalled.
fn no_audio_warning(stalled: Option<&str>) -> String {
    match stalled {
        Some(device) => format!("No audio from {device}. Still trying."),
        None => "No audio is arriving. Still trying.".to_owned(),
    }
}

/// The seconds a recording's master is missing, from its `statistics` and
/// `wall` seconds of wall time: the silence written for gaps and missed
/// cycles, plus the larger of the jumps too long to fill and the wall time
/// `duration` falls short of beyond [`CLOCK_DRIFT`] (the larger, since an
/// unfilled jump is also wall time the master lacks).
fn missing_seconds(statistics: &CaptureStatistics, wall: f64) -> f64 {
    let short = (wall - statistics.duration - wall * CLOCK_DRIFT).max(0.0);
    statistics.gap_seconds + statistics.filled_seconds + statistics.unfilled_seconds.max(short)
}

/// The note after a recording missing more than about a second; it names
/// the cause only when the latest gap followed a stall of `stalled`.
fn missing_audio_note(missing: f64, stalled: Option<&str>) -> Option<String> {
    if missing.is_nan() || missing <= 1.0 {
        return None;
    }
    let seconds = Duration::try_from_secs_f64(missing + 0.5).ok()?.as_secs();
    let missing = if seconds == 1 {
        "About 1 second of the recording is missing".to_owned()
    } else {
        format!("About {seconds} seconds of the recording are missing")
    };
    Some(match stalled {
        Some(device) => format!("{missing}: no audio arrived from {device}."),
        None => format!("{missing}."),
    })
}

/// The warning while the recording is on the fallback `input`.
fn fallback_warning(input: &str) -> String {
    format!("Recording from {input}. The microphone chosen in Settings is not available.")
}

/// The note after a recording that was on the fallback `input` at some
/// point; joined after [`recording_warning`]'s lines.
fn fallback_note(input: &str) -> String {
    format!(
        "Steno recorded from {input} while the microphone chosen in Settings was not available."
    )
}

/// The host's change hook ([`CaptureRecorder::on_change`]).
type Hook = Option<Arc<dyn Fn() + Send + Sync>>;

/// The thread that forwards the 10 Hz levels into `shared` and calls `hook`;
/// it ends with the session. `None` when it could not be spawned.
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

/// The thread that follows the session's notices and calls `hook`: it notes
/// a stalled microphone into `outage`, marks still-trying restarts, re-reads
/// the microphone into `fallback` on a resume, and resumes `recorder`'s
/// auto-stop at the end of a recovery. Lock order is the recorder's, then
/// the outage's, so it reads the session and resumes with the outage's lock
/// released. It holds `session` and `recorder` weakly, so dropping the
/// session ends it and there is no cycle. `None` when it could not be
/// spawned.
fn follow_the_notices(
    notices: Receiver<CaptureNotice>,
    mode: CaptureMode,
    fallback: Arc<Mutex<Fallback>>,
    outage: Arc<Mutex<Outage>>,
    session: Weak<CaptureSession>,
    recorder: Weak<CaptureRecorder>,
    hook: Hook,
) -> Option<JoinHandle<()>> {
    std::thread::Builder::new()
        .name("steno-notices".into())
        .spawn(move || {
            let lock_outage = || {
                outage
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
            };
            while let Ok(notice) = notices.recv() {
                let Some(session) = session.upgrade() else {
                    return;
                };
                // Begin the recovery before reading the session: that read
                // may wait behind a rebuild, and a countdown that runs out
                // meanwhile must end nothing.
                if matches!(notice, CaptureNotice::DeviceChanged(_)) {
                    lock_outage().follow(&notice);
                }
                // Then the session, with no lock held: a rebuild's start
                // holds the session's lock for seconds.
                let stalled = match notice {
                    // The stream is still the replaced one until the resume.
                    CaptureNotice::DeviceChanged(reason) => Some(
                        (reason == DeviceChangeReason::DeliveryStalled
                            && microphone_is_the_master(mode))
                        .then(|| device_name(session.stream().as_ref())),
                    ),
                    _ => None,
                };
                let resumed_on = matches!(notice, CaptureNotice::DeviceResumed { .. })
                    .then(|| fallback_input(session.stream().as_ref()));
                drop(session);
                let mut outage = lock_outage();
                let back = match stalled {
                    // A device change, followed above.
                    Some(stalled) => {
                        outage.stalled = stalled;
                        false
                    }
                    None => outage.follow(&notice),
                };
                drop(outage);
                if let Some(input) = resumed_on {
                    fallback
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .set(input);
                }
                if back && let Some(recorder) = recorder.upgrade() {
                    recorder.resume_auto_stop();
                }
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
    /// Where the known audio folders are listed ([`crate::audio_folders`]).
    support_directory: PathBuf,
    inner: Mutex<Inner>,
    /// Set by [`Recorder::refuse_starts`] at a menu Quit: a start is refused
    /// with [`QUITTING_REFUSAL`]. Outside `inner`, so the shell's main thread
    /// sets it without waiting for the status lock; `start` reads it under
    /// that lock.
    starts_refused: AtomicBool,
    /// The cancel that stops the system audio tests running, shared by every
    /// test running. Outside `inner`, so [`Recorder::refuse_starts`] raises
    /// it without the status lock: a leaf, held only to replace, clone or
    /// raise the cancel, never while waiting, and taken after `inner` when
    /// both are.
    tone_cancel: Mutex<steno_audio::clock::Cancel>,
    /// Signalled with every status change, for [`Self::settle`].
    changes: Condvar,
    /// Called after every status change so the host republishes.
    changed: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    /// How recordings watch the disk.
    disk: Mutex<DiskWatch>,
    /// The clock the auto-stop's grace runs on.
    clock: Mutex<Arc<dyn steno_audio::Clock>>,
    /// The calendar a start reads; none until the app names one.
    calendar: CalendarLookups,
    /// This recorder, for the watcher threads.
    this: Weak<CaptureRecorder>,
    /// Runs in [`Self::notify`] between the read of the state and the live
    /// flag's change, where a test checks the lock is held.
    #[cfg(test)]
    before_live: Mutex<Hook>,
}

impl CaptureRecorder {
    /// A recorder over `store` and `pipeline` that lists the audio folders
    /// it records into under `support_directory`.
    #[must_use]
    #[allow(
        clippy::too_many_arguments,
        reason = "each is a separate dependency the app's graph hands over"
    )]
    pub fn new(
        store: Arc<Store>,
        pipeline: Arc<CurrentPipeline>,
        make_session: MakeCaptureSession,
        permissions: Arc<dyn Permissions>,
        speech_models: Arc<dyn SpeechModels>,
        zone: FixedOffset,
        runtime: tokio::runtime::Handle,
        support_directory: PathBuf,
    ) -> Arc<Self> {
        Arc::new_cyclic(|this| CaptureRecorder {
            store,
            pipeline,
            make_session,
            permissions,
            speech_models,
            zone,
            runtime,
            support_directory,
            inner: Mutex::new(Inner {
                status: RecorderStatus::idle(),
                active: None,
                quitting: false,
                start_holds: 0,
                launch_note: None,
                calls: CallWatch::default(),
                tone_tests: ToneTests {
                    running: 0,
                    patience: TONE_TEST_PATIENCE,
                },
            }),
            starts_refused: AtomicBool::new(false),
            tone_cancel: Mutex::default(),
            changes: Condvar::new(),
            changed: Mutex::new(None),
            disk: Mutex::new(DiskWatch::system(None)),
            clock: Mutex::new(Arc::new(steno_audio::SystemClock::new())),
            calendar: CalendarLookups::new(Arc::new(NoCalendar)),
            this: this.clone(),
            #[cfg(test)]
            before_live: Mutex::new(None),
        })
    }

    /// Names each recording after the event `calendar` has under way when it
    /// starts, from the next start on.
    pub fn name_meetings_from(&self, calendar: Arc<dyn Calendar>) {
        self.calendar.read_from(calendar);
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

    /// Runs the auto-stop's grace on `clock` from the next countdown on.
    #[cfg(test)]
    pub(crate) fn count_down_on(&self, clock: Arc<dyn steno_audio::Clock>) {
        *self
            .clock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = clock;
    }

    fn clock(&self) -> Arc<dyn steno_audio::Clock> {
        self.clock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Whether a recording is starting, running or stopping.
    pub(crate) fn is_busy(&self) -> bool {
        self.inner().status.state != RecordingState::Idle
    }

    /// Runs `test`, the system audio test, unless a recording is starting,
    /// running or stopping, or the app quits: the test plays a tone a
    /// recording would pick up. `None` then. A start while it runs raises
    /// its `Cancel` and waits for it to return ([`Self::stop_tone_tests`]).
    pub(crate) fn run_tone_test<T>(
        &self,
        test: impl FnOnce(&steno_audio::clock::Cancel) -> T,
    ) -> Option<T> {
        /// Counts the test out, also by a panic, and wakes a waiting start.
        struct Ended<'a>(&'a CaptureRecorder);
        impl Drop for Ended<'_> {
            fn drop(&mut self) {
                self.0.inner().tone_tests.running -= 1;
                self.0.changes.notify_all();
            }
        }

        let cancel = {
            let mut inner = self.inner();
            if inner.quitting || inner.status.state != RecordingState::Idle {
                tracing::debug!(
                    "the system audio test is refused while a recording starts, runs or stops"
                );
                return None;
            }
            // Under the cancel's lock, which `refuse_starts` takes after it
            // set the flag: either the flag is seen here, or the cancel
            // handed out is raised there.
            let mut cancel = self.tone_cancel();
            if self.starts_refused.load(Ordering::Acquire) {
                tracing::debug!("the system audio test is refused: the app quits");
                return None;
            }
            if inner.tone_tests.running == 0 {
                *cancel = steno_audio::clock::Cancel::new();
            }
            inner.tone_tests.running += 1;
            cancel.clone()
        };
        let _ended = Ended(self);
        Some(test(&cancel))
    }

    /// Stops the system audio tests a start found running and waits up to
    /// [`TONE_TEST_PATIENCE`] for them, so no tone plays into the recording.
    fn stop_tone_tests(&self) {
        let inner = self.inner();
        if inner.tone_tests.running == 0 {
            return;
        }
        self.tone_cancel().cancel();
        let patience = inner.tone_tests.patience;
        let (_inner, waited) = self
            .changes
            .wait_timeout_while(inner, patience, |inner| inner.tone_tests.running > 0)
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if waited.timed_out() {
            tracing::warn!("the system audio test did not stop; the recording starts anyway");
        }
    }

    /// Another app's microphone activity while Steno records
    /// ([`crate::auto_stop`]): an `Opened` is a call and stops a countdown;
    /// a `Released` during a call arms the countdown. Ignored while idle or
    /// stopping.
    pub fn microphone_activity(&self, activity: MicrophoneActivity) {
        let mut inner = self.inner();
        let state = inner.status.state;
        match activity {
            MicrophoneActivity::Opened { app_name } => {
                if !matches!(state, RecordingState::Starting | RecordingState::Recording) {
                    return;
                }
                inner.calls.opened(app_name);
            }
            MicrophoneActivity::Released => {
                if !self.arm_auto_stop(&mut inner, CallWatch::released) {
                    return;
                }
            }
        }
        drop(inner);
        self.notify();
    }

    /// The capture is back after a device change or a restart: during a call
    /// whose app let go of the microphone, a fresh countdown replaces the
    /// one the release armed, since the detector does not report that
    /// release again. Takes the recorder's lock only to arm and waits for
    /// nothing, so the notice thread may call it. Call it holding no lock
    /// the change hook takes (the outage lock among them): the hook runs on
    /// the calling thread.
    pub fn resume_auto_stop(&self) {
        if self.arm_auto_stop(&mut self.inner(), CallWatch::resume) {
            self.notify();
        }
    }

    /// During a call, arms the auto-stop with `arm` and spawns its
    /// countdown; `true` when it armed.
    fn arm_auto_stop(
        &self,
        inner: &mut Inner,
        arm: impl FnOnce(&mut CallWatch, Duration) -> Option<Countdown>,
    ) -> bool {
        let call = inner.status.state == RecordingState::Recording
            && inner
                .active
                .as_ref()
                .is_some_and(|active| active.mode == CaptureMode::Call);
        if !call {
            return false;
        }
        let clock = self.clock();
        let Some(countdown) = arm(&mut inner.calls, clock.now()) else {
            return false;
        };
        let this = self.this.clone();
        let spawned = std::thread::Builder::new()
            .name("steno-auto-stop".into())
            .spawn(move || {
                if clock.sleep(AUTO_STOP_GRACE, &countdown.cancel)
                    && let Some(recorder) = this.upgrade()
                {
                    recorder.call_ended(countdown.number);
                }
            });
        if let Err(error) = spawned {
            // Without its countdown the recording runs until a stop.
            tracing::error!(%error, "the auto-stop could not be armed");
            inner.calls.keep_recording();
        }
        true
    }

    /// Whether the capture of the recording in progress is recovering, so
    /// the auto-stop waits. Takes the outage's lock under the recorder's.
    fn capture_recovering(inner: &Inner) -> bool {
        inner.active.as_ref().is_some_and(|active| {
            active
                .outage
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .recovering
        })
    }

    /// The auto-stop's countdown `number` ran out: the recording stops as
    /// Stop does, with the call's end reason, unless it was disarmed, the
    /// recording is stopping, or the capture is recovering (then a fresh
    /// countdown takes its place).
    fn call_ended(&self, number: u64) {
        let (active, reason) = {
            let mut inner = self.inner();
            if inner.status.state != RecordingState::Recording {
                return;
            }
            let recovering = Self::capture_recovering(&inner);
            if self.arm_auto_stop(&mut inner, |calls, now| {
                calls.postpone(number, now, recovering)
            }) {
                drop(inner);
                self.notify();
                return;
            }
            let Some(reason) = inner.calls.elapsed(number) else {
                return;
            };
            (Self::begin_stop(&mut inner), reason)
        };
        if let Some(active) = active {
            self.finish_stop(active, reason, None);
        }
    }

    /// Tells the user under the Record control that the launch adopted
    /// `count` recordings with no meeting. Shown while idle until the user
    /// dismisses the messages.
    pub(crate) fn note_adopted(&self, count: usize) {
        if count == 0 {
            return;
        }
        let note = if count == 1 {
            "Recovered a recording that was missing from your list. It is being processed."
                .to_owned()
        } else {
            format!(
                "Recovered {count} recordings that were missing from your list. They are being processed."
            )
        };
        self.inner().launch_note = Some(note);
        self.notify();
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

    /// Every change of the status ends here. The live flag is read and set
    /// under the recorder's lock, so two notifies never apply out of order:
    /// the pipelines learn whether a recording is under way, so no job runs
    /// `CoreML` beside one. The flag's send neither blocks nor takes the
    /// recorder's lock.
    fn notify(&self) {
        {
            let inner = self.inner();
            let live = inner.status.state != RecordingState::Idle;
            #[cfg(test)]
            if let Some(hook) = self
                .before_live
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone()
            {
                hook();
            }
            self.pipeline.live_recording().set(live);
        }
        self.changes.notify_all();
        if let Some(hook) = self.hook() {
            hook();
        }
    }

    /// Waits until no start or stop is in progress and hands back the lock,
    /// the recorder `Idle` or `Recording` under it.
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

    fn tone_cancel(&self) -> MutexGuard<'_, steno_audio::clock::Cancel> {
        self.tone_cancel
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn intake(&self) -> LocalRecordingIntake {
        LocalRecordingIntake::over(self.store.clone(), self.pipeline.current(), self.zone)
    }

    /// Loads the models processing needs in the background while the
    /// recording runs, only when the models of the pipeline's built engine
    /// ([`BuiltEngine`]) and the diarizer are installed, so it never
    /// downloads. Where the speech sidecar runs the engine
    /// ([`SpeechRuntime`]), only the diarizer's check runs: the child would
    /// hold its 2.2 GB through the whole recording.
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
            // One line, and the recording goes on: processing loads the
            // models again and fails its meeting retryably if they still
            // do not load.
            if let Err(failure) = warmed {
                tracing::warn!(%failure, "the warm-up failed; processing loads the models");
            }
        });
    }

    /// Notes `folder` as the one meeting `meeting_id` is recorded into,
    /// before its row is written ([`crate::audio_folders`]). A failure is
    /// logged and the recording goes on.
    fn record_audio_folder(&self, meeting_id: Uuid, folder: &Path) {
        self.remember_audio_folder(folder);
        if let Err(error) =
            crate::audio_folders::record(&self.support_directory, meeting_id, folder)
        {
            tracing::warn!(%meeting_id, "a recording's folder could not be recorded");
            tracing::debug!(%meeting_id, %error, "recording folder not written");
        }
    }

    /// The capture configuration of a recording in `mode` and its recordings
    /// folder; the error says why not, for after [`COULD_NOT_START`].
    fn configuration(&self, mode: CaptureMode) -> Result<(CaptureConfiguration, PathBuf), String> {
        let settings = self
            .store
            .settings()
            .map_err(|_| refused("settings", "Steno could not read its settings."))?;
        let audio_folder =
            steno_core::paths::file_url_path(&settings.audio_folder).ok_or_else(|| {
                refused(
                    "audio folder",
                    "the recordings folder in Settings is not a folder on this computer.",
                )
            })?;
        let audio_mode = match mode {
            CaptureMode::Call => steno_audio::CaptureMode::Call,
            CaptureMode::InPerson => steno_audio::CaptureMode::InPerson,
        };
        let mut configuration = CaptureConfiguration::new(audio_mode, &audio_folder);
        configuration
            .input_device_uid
            .clone_from(&settings.input_device_uid);
        Ok((configuration, audio_folder))
    }

    /// Starts the session and the meeting; the error says why not, for
    /// after [`COULD_NOT_START`].
    fn start_inner(&self, mode: CaptureMode) -> Result<(), String> {
        // First, so the calendar answers while the devices open.
        let lookup = self.calendar.start(Utc::now());
        let (configuration, audio_folder) = self.configuration(mode)?;
        let disk = self.disk();
        let rate = bytes_per_second(&configuration);
        let room = disk
            .free_for(&audio_folder)
            .map_or(Room::Enough, |free| Room::of(free, rate));
        if room == Room::Full {
            return Err(refused("no room", NO_ROOM_TO_START));
        }
        let session = (self.make_session)(configuration)
            .map_err(|_| refused("no session", DEVICES_DID_NOT_OPEN))?;
        let session = Arc::new(session);
        // Before the row: a kill from here on leaves a row whose master
        // recovery must find in this folder.
        let meeting_id = Uuid::new_v4();
        self.record_audio_folder(meeting_id, &audio_folder);
        let started_at = Utc::now();
        let intake = self.intake();
        if !self.begin_meeting(&intake, meeting_id, source(mode), started_at, lookup) {
            // Nothing was recorded; a later launch forgets the entry.
            let _ = self.forget_recording(meeting_id);
            return Err(refused("meeting", "Steno could not create the meeting."));
        }
        let begun = BegunMeeting {
            recorder: self,
            meeting_id,
            session: Some(session.clone()),
            folder: steno_core::RecordingLayout::new(&audio_folder, meeting_id).directory,
        };
        let levels_receiver = session.levels();
        let notices = session.notices();
        // Before the start, so a failure right after it is not missed.
        let states = session.states();
        if let Err(error) = session.start(meeting_id) {
            begun.disarm();
            let reason = capture_refused(&error);
            let _ = intake.fail(meeting_id, &format!("{COULD_NOT_START} {reason}"));
            let _ = self.forget_recording(meeting_id);
            return Err(reason);
        }
        let shared = Arc::new(Mutex::new(None::<LaneLevels>));
        // Spawned before `Recording` is visible, so the stop that takes the
        // session always takes the thread with it.
        let level_thread = forward_levels(levels_receiver, shared.clone(), self.hook());
        let mut fallback = Fallback::default();
        fallback.set(fallback_input(session.stream().as_ref()));
        let fallback = Arc::new(Mutex::new(fallback));
        let outage = Arc::new(Mutex::new(Outage::default()));
        let recording_since = Instant::now();
        let notice_thread = follow_the_notices(
            notices,
            mode,
            fallback.clone(),
            outage.clone(),
            Arc::downgrade(&session),
            self.this.clone(),
            self.hook(),
        );
        {
            let mut inner = self.inner();
            inner.status.state = RecordingState::Recording;
            inner.status.started_at = Some(started_at);
            inner.status.mode = Some(mode);
            inner.status.meeting_id = Some(meeting_id);
            inner.status.levels = None;
            inner.status.error = None;
            inner.status.warning = None;
            inner.active = Some(Active {
                session,
                meeting_id,
                audio_folder: audio_folder.clone(),
                mode,
                levels: shared,
                level_thread,
                fallback,
                outage,
                recording_since,
                notice_thread,
                disk_warning: DiskWarning {
                    minutes: match room {
                        Room::Low { minutes } => Some(minutes),
                        Room::Enough | Room::Full => None,
                    },
                    dismissed: None,
                },
            });
        }
        begun.disarm();
        // Not joined: it may be the thread that finishes the recording.
        let this = self.this.clone();
        let spawned = std::thread::Builder::new()
            .name("steno-recording-watch".into())
            .spawn(move || Self::watch(&this, meeting_id, &states, &disk, &audio_folder, rate));
        if let Err(error) = spawned {
            tracing::error!(%error, "the recording runs without its watcher");
        }
        Ok(())
    }

    /// Writes the `recording` row of `meeting_id`, named after the event the
    /// calendar's `lookup` found if it has already answered, else once it
    /// answers ([`Self::name_when_answered`]). `false` when the row could
    /// not be written. Never waits for the calendar; a wait added here would
    /// have to run on the recorder's clock so a frozen-clock test catches it.
    fn begin_meeting(
        &self,
        intake: &LocalRecordingIntake,
        meeting_id: Uuid,
        source: MeetingSource,
        started_at: chrono::DateTime<Utc>,
        lookup: Option<Lookup>,
    ) -> bool {
        let (event, pending) = match lookup.map(Lookup::take) {
            Some(Ok(event)) => (event, None),
            Some(Err(pending)) => (None, Some(pending)),
            None => (None, None),
        };
        let event = event.as_ref();
        let begun = intake.begin(
            meeting_id,
            source,
            event.map(|event| event.title.as_str()),
            event.map(|event| event.id.as_str()),
            &event.map(attendees_of).unwrap_or_default(),
            started_at,
        );
        if begun.is_err() {
            return false;
        }
        if let Some(pending) = pending {
            self.name_when_answered(pending, meeting_id);
        }
        true
    }

    /// Names the recording `meeting_id` after the event `pending` finds,
    /// under the store's rule. `meeting_id` is the start's own, so a late
    /// answer never reaches the next recording.
    fn name_when_answered(&self, pending: Lookup, meeting_id: Uuid) {
        let (store, pipeline, zone) = (self.store.clone(), self.pipeline.clone(), self.zone);
        pending.then(move |event| {
            let Some(event) = event else { return };
            let intake = LocalRecordingIntake::over(store, pipeline.current(), zone);
            let attendees = attendees_of(&event);
            match intake.name_after_event(meeting_id, &event.title, &event.id, &attendees) {
                Ok(true) => {
                    tracing::debug!(%meeting_id, "the calendar answered after the start");
                }
                Ok(false) => {
                    tracing::debug!(
                        %meeting_id,
                        "the calendar answered after the recording stopped or was renamed; \
                         its event is dropped"
                    );
                }
                Err(error) => {
                    tracing::debug!(
                        %meeting_id,
                        %error,
                        "the calendar event did not name the recording"
                    );
                }
            }
        });
    }

    /// The watcher of the recording of `meeting_id`, until its session
    /// leaves `Recording` or is gone: it finishes a session that failed on
    /// its own or had a write fail, and reads the free space for `folder`
    /// against `rate` bytes a second every `disk.interval`.
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
                    match Room::of(free, rate) {
                        Room::Enough => recorder.warn(meeting_id, None),
                        Room::Low { minutes } => recorder.warn(meeting_id, Some(minutes)),
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

    /// Whether the session of `meeting_id`, still recording, had a write fail.
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

    /// Stops the recording of `meeting_id` with `reason` and `error`, when it
    /// is still the one in progress; a Stop or a quit that came first wins.
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

    /// Notes the disk watch's reading of `minutes` left, `None` with room;
    /// not once the recording is stopping.
    fn warn(&self, meeting_id: Uuid, minutes: Option<u64>) {
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
        let before = active.disk_warning.shown();
        active.disk_warning.set(minutes);
        if active.disk_warning.shown() == before {
            return;
        }
        drop(inner);
        self.notify();
    }

    /// Quitting: once a start or stop in progress has settled, a recording
    /// is stopped with the `quit` end reason and saved before this returns;
    /// a stop already under way is waited for instead, and neither retries
    /// a busy commit. No recording starts once this is called.
    pub fn stop_for_quit(&self) {
        // Before the wait, so a stop under way retries its commit no more
        // and nothing starts meanwhile.
        {
            let mut inner = self.inner();
            inner.quitting = true;
            self.tone_cancel().cancel();
        }
        let active = Self::begin_stop(&mut self.settle());
        if let Some(active) = active {
            self.finish_stop(active, RecordingEndReason::Quit, None);
        }
    }

    /// Takes the session of the recording in progress and marks the recorder
    /// `Stopping`, the auto-stop disarmed; `None` when nothing records.
    fn begin_stop(inner: &mut Inner) -> Option<Active> {
        let active = inner.active.take()?;
        inner.calls.reset();
        inner.status.state = RecordingState::Stopping;
        Some(active)
    }

    /// Stops the session `begin_stop` took and saves the recording, then
    /// leaves the recorder `Idle` with the outcome's message. `error` says
    /// why the recorder stopped on its own; else a failure the result
    /// carries does ([`ended_with`]). A panic leaves the recorder `Idle` too
    /// ([`Unwinding`]) and the row `recording` for the next launch. The
    /// recorded folder is never forgotten here.
    fn finish_stop(&self, active: Active, reason: RecordingEndReason, error: Option<String>) {
        let unwinding = Unwinding::of(self, RecordingState::Stopping, SAVE_PANICKED);
        self.notify();
        // Once the app quits, a busy commit is tried no more, so the stop
        // ends within the exit's patience.
        let intake = {
            let this = self.this.clone();
            self.intake().retrying_while(move || {
                this.upgrade()
                    .is_some_and(|recorder| !recorder.inner().quitting)
            })
        };
        let meeting_id = active.meeting_id;
        let wall = active.recording_since.elapsed().as_secs_f64();
        let outcome = match active.session.stop() {
            Ok(result) => {
                log_dropped_frames(meeting_id, &result.statistics);
                if let Some(failure) = &result.failure {
                    log_failure(meeting_id, failure, &reason);
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
                        meeting_id,
                        RecordingResult {
                            asset: result.asset,
                            duration,
                            end_reason: reason,
                        },
                        None,
                    ),
                );
                let missing = missing_seconds(&statistics, wall);
                match completed {
                    Ok(_) => Ok((recording_warning(active.mode, &statistics), missing, ended)),
                    Err(error) => Err(not_saved(meeting_id, &error)),
                }
            }
            Err(failure) => self
                .recover_failed_stop(&intake, &active, &failure, &reason)
                .map(|()| (Some(RECOVERED_AFTER_A_FAILURE.to_owned()), 0.0, error)),
        };
        // The recorded folder stays whatever the outcome: a saved commit is
        // not durable yet (`synchronous = NORMAL`), and the next launch
        // forgets the entry once a durable checkpoint has run.
        drop(active.session);
        for thread in [active.level_thread, active.notice_thread]
            .into_iter()
            .flatten()
        {
            let _ = thread.join();
        }
        // Read once the notice thread is gone, so its last rebuild counts.
        let outcome = outcome.map(|(warning, missing, ended)| {
            let stalled = active
                .outage
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .stalled
                .clone();
            let missing = missing_audio_note(missing, stalled.as_deref());
            let note = active
                .fallback
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .was_on
                .as_deref()
                .map(fallback_note);
            let warning = [warning, missing, note]
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
            Err(unsaved) => inner.status.error = Some(unsaved.message().to_owned()),
        }
        drop(inner);
        std::mem::forget(unwinding);
        self.notify();
    }

    /// A stop whose session failed and took the asset with it: the master
    /// is recovered as an interrupted recording is at launch. A master that
    /// cannot be found now, or an unreadable folder, keeps the meeting for
    /// the next launch (an unmounted volume leaves an empty folder); else
    /// the meeting fails.
    fn recover_failed_stop(
        &self,
        intake: &LocalRecordingIntake,
        active: &Active,
        failure: &CaptureError,
        reason: &RecordingEndReason,
    ) -> Result<(), Unsaved> {
        let meeting_id = active.meeting_id;
        log_failure(meeting_id, failure, reason);
        let recovered = block_on(
            &self.runtime,
            crate::recovery::recover(
                intake,
                std::slice::from_ref(&active.audio_folder),
                meeting_id,
                source(active.mode),
            ),
        );
        match recovered {
            Ok(_) => {
                tracing::warn!(%meeting_id, "a recording whose capture failed was recovered");
                Ok(())
            }
            Err(RecoveryError::NotSaved(error)) => Err(not_saved(meeting_id, &error)),
            Err(
                RecoveryError::Unreachable | RecoveryError::Unrecoverable(Unrecoverable::NoMaster),
            ) => {
                tracing::warn!(%meeting_id, "a recording whose capture failed is left for the next launch: its master cannot be found now");
                Err(Unsaved::Kept)
            }
            Err(RecoveryError::Unrecoverable(_)) => {
                log_not_saved(meeting_id, failure_kind(failure));
                let _ = intake.fail(meeting_id, FILES_NOT_FINISHED);
                Err(Unsaved::Settled(FILES_NOT_FINISHED))
            }
        }
    }

    /// The status of a recorder that records nothing, the messages kept.
    fn idle(inner: &mut Inner) {
        inner.status.state = RecordingState::Idle;
        inner.status.started_at = None;
        inner.status.mode = None;
        inner.status.meeting_id = None;
        inner.status.levels = None;
        inner.calls.reset();
    }
}

/// Held through [`CaptureRecorder::finish_stop`] in `Stopping` and through
/// a start in `Starting`: if either unwinds, the drop leaves the recorder
/// `Idle` with `error`, which `settle` would otherwise wait on forever; a
/// state the step had already left is left alone.
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

/// Held through a start from the moment its meeting is begun until it
/// records: if the start unwinds, the drop stops the session, removes the
/// meeting's folder, fails the meeting and forgets its recorded folder, so
/// nothing is left of a start the user was told did not happen.
struct BegunMeeting<'a> {
    recorder: &'a CaptureRecorder,
    meeting_id: Uuid,
    /// The session being started; `None` once the start is through.
    session: Option<Arc<CaptureSession>>,
    folder: PathBuf,
}

impl BegunMeeting<'_> {
    fn disarm(mut self) {
        self.session = None;
    }
}

impl Drop for BegunMeeting<'_> {
    fn drop(&mut self) {
        let Some(session) = self.session.take() else {
            return;
        };
        // The stop comes first: it closes the files, and Windows does not
        // delete a folder whose files are open.
        let _ = session.stop();
        let _ = std::fs::remove_dir_all(&self.folder);
        let _ = self.recorder.intake().fail(self.meeting_id, START_PANICKED);
        let _ = self.recorder.forget_recording(self.meeting_id);
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
        if status.state != RecordingState::Idle {
            status.call_app = inner.calls.call_app().map(str::to_owned);
            status.auto_stop = inner
                .calls
                .status(self.clock().now(), Self::capture_recovering(&inner));
        }
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
            let no_audio = {
                let outage = active
                    .outage
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                outage
                    .still_trying
                    .then(|| no_audio_warning(outage.stalled.as_deref()))
            };
            // Every warning that applies, the disk's first.
            status.warning = [active.disk_warning.shown(), no_audio, fallback]
                .into_iter()
                .flatten()
                .reduce(|warning, next| format!("{warning} {next}"));
        } else if status.state == RecordingState::Idle
            && let Some(note) = &inner.launch_note
        {
            status.warning = Some(match status.warning.take() {
                Some(warning) => format!("{warning} {note}"),
                None => note.clone(),
            });
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
            if self.starts_refused.load(Ordering::Acquire) {
                inner.status.error = Some(QUITTING_REFUSAL.to_owned());
                drop(inner);
                self.notify();
                return;
            }
            if inner.start_holds > 0 {
                inner.status.error = Some(INSTALLING_UPDATE.to_owned());
                drop(inner);
                self.notify();
                return;
            }
            inner.status.state = RecordingState::Starting;
            inner.status.error = None;
            inner.status.warning = None;
            inner.calls.begin(call_app);
        }
        // A meeting a panicking start had begun is failed on the way.
        let unwinding = Unwinding::of(self, RecordingState::Starting, START_PANICKED);
        self.notify();
        self.stop_tone_tests();
        match self.start_inner(mode) {
            Ok(()) => self.warm_up_if_installed(),
            Err(error) => {
                let mut inner = self.inner();
                inner.status.state = RecordingState::Idle;
                inner.status.error = Some(format!("{COULD_NOT_START} {error}"));
            }
        }
        std::mem::forget(unwinding);
        self.notify();
    }

    fn hold_starts(&self) -> StartHold {
        self.inner().start_holds += 1;
        Box::new(HeldStarts(self.this.clone()))
    }

    fn refuse_starts(&self) {
        self.starts_refused.store(true, Ordering::Release);
        // A system audio test running is stopped, not waited for.
        self.tone_cancel().cancel();
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
        self.inner().calls.keep_recording();
        self.notify();
    }

    fn clear_messages(&self) {
        let mut inner = self.inner();
        inner.status.warning = None;
        inner.status.error = None;
        inner.launch_note = None;
        if let Some(active) = inner.active.as_mut() {
            *active
                .fallback
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Fallback::default();
            active
                .outage
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .dismiss();
            active.disk_warning.dismiss();
        }
        drop(inner);
        self.notify();
    }

    fn refresh_permissions(&self) {
        let denied = denied_permissions(steno_bridge::Platform::CURRENT, &*self.permissions);
        self.inner().status.denied_permissions = denied;
        self.notify();
    }

    fn remember_audio_folder(&self, folder: &Path) {
        if let Err(error) = crate::audio_folders::remember(&self.support_directory, folder) {
            // The folder can name the user.
            tracing::warn!("an audio folder could not be added to the known folders");
            tracing::debug!(%error, "known audio folders not written");
        }
    }

    /// The folders the meeting may be in (`recovery::meeting_folders`); a
    /// master modified within
    /// [`LiveRecordingCheck::fresh_within`](crate::recovery::LiveRecordingCheck::fresh_within)
    /// counts as still written.
    fn left_recording(&self, meeting_id: Uuid) -> LeftRecording {
        let folders =
            crate::recovery::meeting_folders(&self.store, &self.support_directory, meeting_id);
        let check = crate::recovery::LiveRecordingCheck::default();
        let still_written = folders
            .iter()
            .any(|folder| check.is_fresh(&crate::recovery::master_path(folder, meeting_id)));
        LeftRecording {
            folders,
            still_written,
        }
    }

    /// On any failure the store is checkpointed durably, so the row no
    /// longer needs the entry.
    fn forget_recording(&self, meeting_id: Uuid) -> std::io::Result<Option<PathBuf>> {
        crate::audio_folders::forget(&self.support_directory, &[meeting_id])
            .map(|mut forgotten| forgotten.remove(&meeting_id))
            .inspect_err(|error| {
                // The error can name the user's folder: debug alone.
                tracing::debug!(%meeting_id, %error, "a recording folder not forgotten");
                if let Err(error) = self.store.checkpoint_durably() {
                    tracing::warn!(
                        %meeting_id,
                        "a recording's folder could not be forgotten, and its meeting's row could not be made durable"
                    );
                    tracing::debug!(%meeting_id, %error, "store not checkpointed");
                }
            })
    }

    /// When the entry cannot be written again, a durable checkpoint makes
    /// the row survive a power loss.
    fn restore_recording(&self, meeting_id: Uuid, folder: &Path) {
        let Err(error) = crate::audio_folders::record(&self.support_directory, meeting_id, folder)
        else {
            return;
        };
        // The error can name the user's folder: debug alone.
        tracing::debug!(%meeting_id, %error, "recording folder not written again");
        match self.store.checkpoint_durably() {
            Ok(()) => tracing::warn!(
                %meeting_id,
                "a meeting that was not deleted lost its recording's folder; its row was made durable instead"
            ),
            Err(error) => {
                tracing::warn!(
                    %meeting_id,
                    "a meeting that was not deleted lost its recording's folder, and its row could not be made durable"
                );
                tracing::debug!(%meeting_id, %error, "store not checkpointed");
            }
        }
    }
}

/// What a saved recording warns about, every line joined into one: frames
/// that never reached the files (in seconds of the lane that lost most,
/// rounded, so from half a second on), and a call whose system audio
/// stayed silent. The dropped frames name no cause, since the count holds
/// several. A lost device is the status's error instead ([`ended_with`]).
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

/// A `warn` line when a recording lost frames, with counts only.
fn log_dropped_frames(meeting_id: Uuid, statistics: &CaptureStatistics) {
    if statistics.dropped_frames.values().any(|frames| *frames > 0) {
        tracing::warn!(
            meeting = %meeting_id,
            dropped = ?statistics.dropped_frames,
            "the recording lost frames"
        );
    }
}

/// The attendees of `event` that become the meeting's `them`: everyone but
/// the calendar's owner.
fn attendees_of(event: &steno_core::CalendarEvent) -> Vec<Attendee> {
    event
        .attendees
        .iter()
        .filter(|attendee| !attendee.is_current_user)
        .map(|attendee| Attendee {
            display_name: attendee.name.clone(),
            email: attendee.email.clone(),
        })
        .collect()
}

/// The required permissions `permissions` reports denied, among the ones
/// `platform` has: off the Mac, system audio has no switch of its own.
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
        HANG_GUARD, HeldOutages, LIVE, PATIENCE, eventually, eventually_within, fake_dependencies,
        found_within, on_own_thread, synthetic_capture, temp_store,
    };
    use steno_audio::testing::synthetic::SyntheticOptions;
    use steno_audio::writer::{LaneFrames, RecordingFiles, RecordingWriter, RecordingWriting};
    use steno_core::AudioLane;
    use steno_core::paths::file_url;
    use steno_core::testing::{FakeDiarizer, FakeSpeechEngine};
    use steno_host::fakes::{FakePermissions, FakeSpeechModels};
    use steno_speech::SpeechSettings;

    struct Harness {
        store: Arc<Store>,
        /// The bytes the disk watch reads as free.
        free: Arc<AtomicU64>,
        /// The capture sessions the recorder builds.
        capture: Arc<Mutex<MakeCaptureSession>>,
        recorder: Arc<CaptureRecorder>,
        engine: Arc<FakeSpeechEngine>,
        diarizer: Arc<FakeDiarizer>,
        /// While set, a reload fails as a build error would and the
        /// current pipeline stays.
        failing_reloads: Arc<AtomicBool>,
        /// Last, so a recorder dropped mid-recording closes its files before
        /// the folder goes.
        dir: tempfile::TempDir,
    }

    impl Drop for Harness {
        /// Waits for the pipeline's runs, so no processing file is left
        /// behind.
        fn drop(&mut self) {
            let pipeline = self.recorder.pipeline.current();
            tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(pipeline.wait_until_idle());
            });
        }
    }

    /// Where the app runs each engine id with `speech_settings`.
    fn platform_rule(
        speech_settings: SpeechSettings,
    ) -> impl Fn(&str) -> SpeechRuntime + Send + Sync + 'static {
        move |engine_id| crate::speech::engine_runtime(engine_id, &speech_settings)
    }

    /// The Mac's default rule on every platform: `parakeet-v3` in process,
    /// every other id in the sidecar.
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

    /// A recorder over `models`, the settings naming `engine_id`. Each build
    /// records the stored engine id and the runtime `runtime_of` gives it.
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
        harness_with_engine(
            models,
            engine_id,
            runtime_of,
            capture,
            FakeSpeechEngine::default(),
        )
    }

    /// [`harness_capturing`] over `engine`.
    fn harness_with_engine(
        models: Arc<dyn SpeechModels>,
        engine_id: &str,
        runtime_of: impl Fn(&str) -> SpeechRuntime + Send + Sync + 'static,
        capture: MakeCaptureSession,
        engine: FakeSpeechEngine,
    ) -> Harness {
        let (dir, store) = temp_store();
        let free = Arc::new(AtomicU64::new(u64::MAX));
        let capture_slot = Arc::new(Mutex::new(capture));
        let capture = capture_slot.clone();
        let mut settings = store.settings().unwrap();
        settings.audio_folder = file_url(&dir.path().join("audio"), true);
        engine_id.clone_into(&mut settings.speech_engine_id);
        store.save_settings(&settings).unwrap();
        let engine = Arc::new(engine);
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
            dir.path().join("support"),
        );
        recorder.watch_disk_with({
            let free = free.clone();
            disk_with(move |_| Ok(volume(free.load(Ordering::SeqCst))))
        });
        Harness {
            dir,
            store: store.clone(),
            free,
            capture: capture_slot,
            recorder,
            engine,
            diarizer,
            failing_reloads,
        }
    }

    /// A disk watch over `free_space`, read every 20 ms.
    fn disk_with(
        free_space: impl Fn(&Path) -> std::io::Result<Volume> + Send + Sync + 'static,
    ) -> DiskWatch {
        DiskWatch {
            free_space: Arc::new(free_space),
            interval: Duration::from_millis(20),
            database_folder: None,
        }
    }

    /// A volume of the largest size with `available` bytes free.
    fn volume(available: u64) -> Volume {
        Volume {
            available,
            total: u64::MAX,
        }
    }

    /// The real writer with `before` run ahead of every write; its error
    /// fails that write.
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

    /// A capture session over `backend` whose writers are [`BeforeWrite`]s.
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

    /// Capture sessions whose writer fails every write after the first
    /// `writes`, as a disk that fills does.
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

    /// Starts an in-person recording on its own thread, failing as hung
    /// after [`HANG_GUARD`], and checks that it records.
    fn start(recorder: &Arc<CaptureRecorder>) {
        try_start(recorder, CaptureMode::InPerson);
        assert_eq!(recorder.status().state, RecordingState::Recording);
    }

    /// Starts a `mode` recording on its own thread, which may refuse it.
    fn try_start(recorder: &Arc<CaptureRecorder>, mode: CaptureMode) {
        let starting = recorder.clone();
        on_own_thread(HANG_GUARD, "the start returned", move || {
            starting.start(mode, None);
        });
    }

    /// Stops the recording on its own thread and returns once it is saved.
    fn stop(recorder: &Arc<CaptureRecorder>) {
        let stopping = recorder.clone();
        on_own_thread(HANG_GUARD, "the stop returned", move || stopping.stop());
    }

    /// Quits on its own thread.
    fn quit(recorder: &Arc<CaptureRecorder>) {
        let quitting = recorder.clone();
        on_own_thread(HANG_GUARD, "quitting returned", move || {
            quitting.stop_for_quit();
        });
    }

    /// The status after a start on its own thread, which may refuse it.
    fn status_after_a_start(harness: &Harness) -> RecorderStatus {
        try_start(&harness.recorder, CaptureMode::InPerson);
        harness.recorder.status()
    }

    /// Every change in `state` holds the recorder there a while; the
    /// receiver hears of each as it begins.
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

    /// Starts a recording and waits for its warm-up to load the diarizer;
    /// whether the speech engine was loaded too.
    async fn warmed_the_engine(harness: &Harness) -> bool {
        start(&harness.recorder);
        eventually("the diarizer was loaded while recording", || {
            harness.diarizer.preparations.count() > 0
        })
        .await;
        harness.engine.preparations.count() > 0
    }

    /// With the defaults the Mac's `CoreML` engine and the diarizer load;
    /// off the Mac only the diarizer does.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_recording_start_warms_the_pipeline_up_when_the_models_are_installed() {
        let harness = harness(&[ModelAsset::ParakeetV3, ModelAsset::OfflineDiarizer]);
        assert_eq!(warmed_the_engine(&harness).await, cfg!(target_os = "macos"));
        stop(&harness.recorder);
    }

    /// With Parakeet in the sidecar, the warm-up loads the diarizer only.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_recording_start_with_speech_in_the_sidecar_warms_the_diarizer_only() {
        let harness = harness_over(
            models_with(&[ModelAsset::ParakeetV3, ModelAsset::OfflineDiarizer]),
            "parakeet-v3",
            platform_rule(crate::speech::testing::sidecar_chosen()),
        );
        assert!(!warmed_the_engine(&harness).await);
        stop(&harness.recorder);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_recording_start_never_warms_up_models_that_would_download() {
        let harness = harness(&[ModelAsset::OfflineDiarizer]);
        start(&harness.recorder);
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        assert_eq!(harness.engine.preparations.count(), 0);
        assert_eq!(harness.diarizer.preparations.count(), 0);
        stop(&harness.recorder);
    }

    /// The warm-up follows the engine a reload built: moving to the
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
        stop(&harness.recorder);
    }

    /// After a failed reload the warm-up follows the built engine, not the
    /// stored id, both for a pipeline on the sidecar and one on `CoreML`.
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
        stop(&sidecar.recorder);

        let in_process = harness_over(
            models_with(&[ModelAsset::ParakeetV3, ModelAsset::OfflineDiarizer]),
            "parakeet-v3",
            mac_rule,
        );
        in_process.failing_reloads.store(true, Ordering::SeqCst);
        assert!(in_process.save_engine("parakeet-ultra").is_err());
        assert!(warmed_the_engine(&in_process).await);
        stop(&in_process.recorder);
    }

    /// A warm-up whose model load fails gives one `warn` line; the recording
    /// goes on without an error and its stop saves it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_warm_up_that_fails_leaves_the_recording_running_and_logs_one_line() {
        let log = steno_pipeline::fixtures::CapturedLog::warnings();
        let reason = format!(
            "CoreML raised NSGenericException: Failed to allocate memory IOSurface object ({})",
            Uuid::new_v4()
        );
        let harness = harness_with_engine(
            models_with(&[ModelAsset::ParakeetV3, ModelAsset::OfflineDiarizer]),
            "parakeet-v3",
            mac_rule,
            synthetic_capture(),
            FakeSpeechEngine {
                prepare_failure: Some(reason.clone()),
                ..FakeSpeechEngine::default()
            },
        );
        start(&harness.recorder);
        let meeting_id = harness.recorder.status().meeting_id.unwrap();
        eventually("the warm-up failure was logged", || {
            log.text().contains(&reason)
        })
        .await;
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        let status = harness.recorder.status();
        assert_eq!(status.state, RecordingState::Recording);
        assert_eq!(status.meeting_id, Some(meeting_id));
        assert_eq!(status.error, None);
        assert_eq!(status.warning, None);
        assert_eq!(
            harness.diarizer.preparations.count(),
            0,
            "the warm-up stopped at the engine"
        );

        stop(&harness.recorder);
        let meeting = harness.store.meeting(meeting_id).unwrap().unwrap();
        assert_eq!(meeting.end_reason, Some(RecordingEndReason::Manual));
        assert!(meeting.duration > 0.0);
        let asset = harness.store.asset(meeting_id).unwrap().unwrap();
        let master = steno_core::paths::file_url_path(&asset.url).unwrap();
        assert!(
            std::fs::metadata(&master).unwrap().len() > 4096,
            "audio kept"
        );
        let text = log.text();
        let lines: Vec<&str> = text.lines().filter(|line| line.contains(&reason)).collect();
        assert_eq!(lines.len(), 1, "{text}");
        assert!(lines[0].contains("the warm-up failed"), "{text}");
    }

    /// A microphone input for the warning tests: `None` names none.
    fn input(name: Option<&str>, is_fallback: bool) -> steno_audio::CaptureInput {
        steno_audio::CaptureInput {
            uid: name.unwrap_or("default").to_lowercase(),
            name: name.map(str::to_owned),
            is_fallback,
        }
    }

    /// A harness whose capture records on `first` and, after a device change
    /// 1 s in, on `after`.
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
            .change_device_after(1.0)
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

    /// The recorder's warning without a leading missing-audio note, which a
    /// slow rebuild on a loaded runner can add. A stall's note names a
    /// cause, so it stays.
    fn without_the_missing_audio_note(recorder: &CaptureRecorder) -> Option<String> {
        let warning = recorder.status().warning?;
        let Some(rest) = leading_seconds(&warning)
            .and_then(|seconds| note_for_seconds(seconds, None))
            .and_then(|note| warning.strip_prefix(note.as_str()))
        else {
            return Some(warning);
        };
        Some(rest.trim_start().to_owned()).filter(|rest| !rest.is_empty())
    }

    /// A rebuild onto the default input warns, naming it, until dismissed
    /// or the recording stops; the stop joins the thread that sets it.
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
        let warning = || without_the_missing_audio_note(&harness.recorder);
        start(&harness.recorder);
        assert_eq!(warning(), None, "the chosen microphone records");
        eventually_within(HANG_GUARD, "the rebuild's fallback is named", || {
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
        stop(&harness.recorder);
        assert_eq!(warning(), None, "nothing left after the stop");
        assert_eq!(held.strong_count(), 0, "the notice thread was joined");
    }

    /// A start on the default input warns at once, names the system
    /// default when the input has no name, and notes it after the stop.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_recording_that_starts_on_the_fallback_microphone_warns_at_once() {
        let harness = harness_on_inputs(input(Some("Built-in Audio"), true), input(None, true));
        let warning = || without_the_missing_audio_note(&harness.recorder);
        start(&harness.recorder);
        assert_eq!(
            warning().as_deref(),
            Some(
                "Recording from Built-in Audio. The microphone chosen in Settings is not available."
            )
        );
        let unnamed = "Recording from the system default microphone. The microphone chosen in \
                       Settings is not available.";
        eventually_within(
            HANG_GUARD,
            "the unnamed fallback reads as the default",
            || warning().as_deref() == Some(unnamed),
        )
        .await;
        stop(&harness.recorder);
        assert_eq!(
            warning().as_deref(),
            Some(
                "Steno recorded from the system default microphone while the microphone \
                 chosen in Settings was not available."
            ),
            "the note after the stop"
        );
    }

    /// A recording back on the chosen microphone before the stop still notes
    /// the fallback.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_recording_back_on_the_chosen_microphone_still_notes_the_fallback() {
        let harness = harness_on_inputs(
            input(Some("Built-in Audio"), true),
            input(Some("USB Microphone"), false),
        );
        let warning = || without_the_missing_audio_note(&harness.recorder);
        start(&harness.recorder);
        assert!(warning().is_some(), "on the fallback at the start");
        eventually_within(HANG_GUARD, "back on the chosen microphone", || {
            warning().is_none()
        })
        .await;
        stop(&harness.recorder);
        assert_eq!(
            warning().as_deref(),
            Some(
                "Steno recorded from Built-in Audio while the microphone chosen in Settings \
                 was not available."
            )
        );
    }

    /// The note joins the other warnings after the stop.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_fallback_note_follows_the_other_warnings() {
        let harness = harness_on_inputs(
            input(Some("Built-in Audio"), true),
            input(Some("Built-in Audio"), true),
        );
        try_start(&harness.recorder, CaptureMode::Call);
        assert_eq!(harness.recorder.status().state, RecordingState::Recording);
        stop(&harness.recorder);
        assert_eq!(
            harness.recorder.status().warning.as_deref(),
            Some(
                "Steno heard nothing from the call's audio. Check the system audio permission. \
                 Steno recorded from Built-in Audio while the microphone chosen in Settings \
                 was not available."
            )
        );
    }

    /// A recorder capturing "USB Microphone" over the synthetic backend at
    /// wall-clock speed, its outage set up by `options`.
    fn stalling_harness(options: fn(SyntheticOptions) -> SyntheticOptions) -> Harness {
        stalling_harness_over(options, None)
    }

    /// [`stalling_harness`] whose outage waits until the test releases it.
    fn held_stalling_harness(
        options: fn(SyntheticOptions) -> SyntheticOptions,
    ) -> (Harness, HeldOutages) {
        let held = HeldOutages::default();
        (stalling_harness_over(options, Some(held.clone())), held)
    }

    /// [`stalling_harness`], its outages held by `held` when given.
    fn stalling_harness_over(
        options: fn(SyntheticOptions) -> SyntheticOptions,
        held: Option<HeldOutages>,
    ) -> Harness {
        let capture: MakeCaptureSession = Arc::new(move |configuration: CaptureConfiguration| {
            let tone = SyntheticOptions::tones(
                &configuration.lanes(),
                &[(AudioLane::Mixed, 440.0)],
                600.0,
            )
            .real_time(true);
            let on_usb = CaptureStream {
                input: Some(input(Some("USB Microphone"), false)),
                ..CaptureStream::SYNTHETIC
            };
            let tone = tone.stream(on_usb.clone()).stream_after_restart(on_usb);
            CaptureSession::with_backend(
                configuration,
                match &held {
                    Some(held) => held.backend(options(tone)),
                    None => Arc::new(steno_audio::testing::SyntheticCaptureBackend::new(options(
                        tone,
                    ))),
                },
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

    /// Records over `harness` until the meter shows the tone, releases the
    /// `outage`, and records until `warning` shows; checks the meter reads
    /// silence and the warning stands for `holds` more, stops, checks the
    /// note names `stalled`, and returns the seconds it says are missing.
    async fn until_still_trying(
        harness: &Harness,
        outage: &HeldOutages,
        warning: &str,
        stalled: Option<&str>,
        holds: Duration,
    ) -> u64 {
        let status = || harness.recorder.status();
        let starting = std::time::Instant::now();
        start(&harness.recorder);
        // The master holds what came before the start returned, which the
        // note's wall time leaves out.
        let start_took = starting.elapsed();
        eventually_within(HANG_GUARD, "a level from the tone", || {
            status().levels.is_some_and(|levels| levels.mic > 0.1)
        })
        .await;
        assert_eq!(status().warning, None);
        outage.release();
        until_the_warning(harness, warning).await;
        let mic = status().levels.map(|levels| levels.mic);
        assert!(
            mic.is_some_and(|mic| mic < 1e-6),
            "the meter reads silence: {mic:?}"
        );
        let held = std::time::Instant::now() + holds + start_took;
        while std::time::Instant::now() < held {
            assert_eq!(status().warning.as_deref(), Some(warning), "it stands");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        stop(&harness.recorder);
        let note = status().warning.unwrap_or_default();
        assert!(note.starts_with("About "), "only the missing audio: {note}");
        let seconds = seconds_missing(&note, stalled);
        // Before the harness waits for the pipeline's runs, so none writes
        // into the removed folder.
        eventually("the pipeline picked the saved meeting up", || {
            harness.engine.transcriptions.count() > 0
        })
        .await;
        seconds
    }

    /// The seconds the note after a stop says are missing, checking its
    /// wording and the `stalled` device.
    fn seconds_missing(note: &str, stalled: Option<&str>) -> u64 {
        let missing = note.find("About ").map_or("", |start| &note[start..]);
        let seconds = leading_seconds(missing).unwrap_or_else(|| panic!("the note: {note}"));
        assert_eq!(
            Some(missing),
            note_for_seconds(seconds, stalled).as_deref(),
            "{note}"
        );
        seconds
    }

    /// The seconds a leading missing-audio note in `note` says are missing.
    fn leading_seconds(note: &str) -> Option<u64> {
        note.strip_prefix("About ")?.split(' ').next()?.parse().ok()
    }

    fn note_for_seconds(seconds: u64, stalled: Option<&str>) -> Option<String> {
        // A gap that rounds to `seconds`, past the note's 1 s floor.
        missing_audio_note(Duration::from_secs(seconds).as_secs_f64() + 0.25, stalled)
    }

    /// Waits for the recorder to show `warning`, failing as hung after
    /// [`HANG_GUARD`]; another still-trying warning fails at once.
    async fn until_the_warning(harness: &Harness, warning: &str) {
        let deadline = std::time::Instant::now() + HANG_GUARD;
        while harness.recorder.status().warning.as_deref() != Some(warning) {
            if let Some(shown) = harness.recorder.status().warning
                && shown.ends_with("Still trying.")
            {
                assert_eq!(shown, warning, "the still-trying warning");
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the warning once the restarts pass the attempts: {:?}",
                harness.recorder.status().warning
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// Seconds of tone before the device stops delivering, or more.
    const BEFORE_THE_OUTAGE: f64 = 3.0;
    const STILL_TRYING_ON_USB: &str = "No audio from USB Microphone. Still trying.";
    const USB_MICROPHONE: &str = "USB Microphone";

    /// A device whose restarts keep failing: past `RESTART_ATTEMPTS` the
    /// recorder warns naming it, the meter reads silence, and the note after
    /// the stop says how much is missing and from which device.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_device_that_stops_delivering_is_named_while_steno_keeps_trying() {
        let (harness, outage) = held_stalling_harness(|options| {
            options
                .stall_after(BEFORE_THE_OUTAGE)
                .restarts_that_fail(usize::MAX)
                .restarts_do_not_run()
        });
        let seconds = until_still_trying(
            &harness,
            &outage,
            STILL_TRYING_ON_USB,
            Some(USB_MICROPHONE),
            Duration::ZERO,
        )
        .await;
        assert!(seconds >= 3, "{seconds} s");
    }

    /// The same warning when the restarts start but deliver nothing: about
    /// ten seconds after the stall.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn restarts_that_start_but_deliver_nothing_show_the_warning() {
        let (harness, outage) = held_stalling_harness(|options| {
            options
                .stall_after(BEFORE_THE_OUTAGE)
                .restarts_stall_after(0.0)
        });
        let seconds = until_still_trying(
            &harness,
            &outage,
            STILL_TRYING_ON_USB,
            Some(USB_MICROPHONE),
            Duration::ZERO,
        )
        .await;
        assert!(seconds >= 10, "{seconds} s");
    }

    /// A call that delivers nothing from its start warns without naming the
    /// microphone, except on Windows, where its clock drives the call.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_call_with_nothing_playing_warns_without_naming_the_microphone() {
        let harness =
            stalling_harness(|options| options.stall_after(0.0).restarts_stall_after(0.0));
        let warning = if cfg!(windows) {
            STILL_TRYING_ON_USB
        } else {
            "No audio is arriving. Still trying."
        };
        try_start(&harness.recorder, CaptureMode::Call);
        assert_eq!(harness.recorder.status().state, RecordingState::Recording);
        until_the_warning(&harness, warning).await;
        stop(&harness.recorder);
        seconds_missing(
            &harness.recorder.status().warning.unwrap_or_default(),
            cfg!(windows).then_some(USB_MICROPHONE),
        );
        eventually("the pipeline picked the saved meeting up", || {
            harness.engine.transcriptions.count() > 0
        })
        .await;
    }

    /// Restarts after a device change rather than a stall name no device.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn restarts_after_another_change_blame_no_device() {
        let (harness, outage) = held_stalling_harness(|options| {
            options
                .change_device_after(BEFORE_THE_OUTAGE)
                .restarts_that_fail(usize::MAX)
                .restarts_do_not_run()
        });
        let seconds = until_still_trying(
            &harness,
            &outage,
            "No audio is arriving. Still trying.",
            None,
            Duration::ZERO,
        )
        .await;
        assert!(seconds >= 1, "{seconds} s");
    }

    /// An ordinary device change recovers at its resume, which resumes the
    /// auto-stop once.
    #[test]
    fn a_device_change_recovers_at_its_resume() {
        let resumed = CaptureNotice::DeviceResumed {
            attempt: 1,
            gap_seconds: 0.3,
        };
        let mut outage = Outage::default();
        assert!(!outage.follow(&CaptureNotice::DeviceChanged(
            DeviceChangeReason::DefaultInputChanged
        )));
        assert!(outage.recovering);
        assert!(outage.follow(&resumed), "back at the resume");
        assert!(!outage.recovering);
        assert!(!outage.follow(&resumed), "nothing to end");
    }

    /// Restarts past the attempts recover only once audio is back
    /// (`Delivering`); a dismissal ends the warning, not the recovery.
    #[test]
    fn restarts_that_went_on_recover_only_once_audio_is_back() {
        let resumed = CaptureNotice::DeviceResumed {
            attempt: CaptureSession::RESTART_ATTEMPTS + 1,
            gap_seconds: 4.0,
        };
        let mut outage = Outage::default();
        assert!(!outage.follow(&CaptureNotice::DeviceChanged(
            DeviceChangeReason::AudioServiceRestarted
        )));
        assert!(!outage.follow(&CaptureNotice::StillRestarting {
            attempt: CaptureSession::RESTART_ATTEMPTS,
        }));
        assert!(outage.still_trying);
        outage.dismiss();
        assert!(!outage.still_trying, "the warning went");
        assert!(
            outage.recovering && outage.restarting,
            "the recovery stands"
        );
        assert!(!outage.follow(&resumed));
        assert!(!outage.follow(&CaptureNotice::DeviceChanged(
            DeviceChangeReason::DeliveryStalled
        )));
        assert!(!outage.follow(&resumed));
        assert!(outage.recovering);
        assert!(outage.follow(&CaptureNotice::Delivering), "back");
        assert!(!outage.recovering && !outage.restarting && !outage.still_trying);
        assert!(!outage.follow(&CaptureNotice::Delivering), "nothing to end");
        outage.follow(&CaptureNotice::DeviceChanged(
            DeviceChangeReason::DefaultInputChanged,
        ));
        assert!(
            outage.follow(&resumed),
            "the next change is back at its resume"
        );
    }

    /// Only a microphone whose clock drives the capture is named for a
    /// stall: an in-person capture's, and a call's on Windows alone.
    #[test]
    fn only_a_microphone_that_drives_the_capture_is_blamed_for_a_stall() {
        assert!(microphone_is_the_master(CaptureMode::InPerson));
        assert_eq!(microphone_is_the_master(CaptureMode::Call), cfg!(windows));
    }

    /// A 2 s shortfall in two hours is clock drift, no note; a real
    /// shortfall or a filled gap gives one.
    #[test]
    fn the_missing_seconds_leave_the_clock_drift_out() {
        let of = |duration: f64, gap_seconds: f64| CaptureStatistics {
            duration,
            gap_seconds,
            ..statistics()
        };
        assert_eq!(missing_seconds(&of(7198.0, 0.0), 7200.0), 0.0);
        assert_eq!(
            missing_audio_note(missing_seconds(&of(7198.0, 0.0), 7200.0), None),
            None
        );
        let short = missing_seconds(&of(7180.0, 0.0), 7200.0);
        assert!((short - 12.8).abs() < 1e-9, "{short}");
        assert_eq!(
            missing_audio_note(short, None).as_deref(),
            Some("About 13 seconds of the recording are missing.")
        );
        assert_eq!(missing_seconds(&of(60.0, 3.0), 60.0), 3.0);
    }

    /// Every kind of missing audio adds up, the unfilled jump counted once
    /// as the larger of it and the wall-time shortfall.
    #[test]
    fn a_gap_missed_cycles_and_an_unfilled_jump_add_up_in_the_note() {
        let hour = CaptureStatistics {
            duration: 3_598.5,
            gap_seconds: 0.5,
            filled_seconds: 0.25,
            unfilled_seconds: 1.5,
            ..statistics()
        };
        assert_eq!(missing_seconds(&hour, 3_600.0), 2.25);
        assert_eq!(
            missing_audio_note(missing_seconds(&hour, 3_600.0), None).as_deref(),
            Some("About 2 seconds of the recording are missing.")
        );
        let unfilled_only = CaptureStatistics {
            duration: 3_598.5,
            unfilled_seconds: 1.5,
            ..statistics()
        };
        assert_eq!(missing_seconds(&unfilled_only, 3_600.0), 1.5);
        let behind = CaptureStatistics {
            duration: 50.0,
            unfilled_seconds: 1.5,
            ..statistics()
        };
        assert!(
            (missing_seconds(&behind, 60.0) - 9.94).abs() < 1e-9,
            "a wall-time shortfall larger than the unfilled jump counts as it"
        );
    }

    /// Cycles the capture's thread missed, written as silence, count in the
    /// note: two 0.75 s misses say about 2 seconds are missing.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn missed_cycles_count_in_the_note_after_the_stop() {
        let capture: MakeCaptureSession = Arc::new(|configuration: CaptureConfiguration| {
            let options = SyntheticOptions::tones(
                &configuration.lanes(),
                &[(AudioLane::Mixed, 440.0)],
                600.0,
            )
            .real_time(true)
            .miss_at(0.5, 36_000)
            .miss_at(1.5, 36_000);
            CaptureSession::with_backend(
                configuration,
                Arc::new(steno_audio::testing::SyntheticCaptureBackend::new(options)),
                None,
                CaptureSession::DEFAULT_WRITER_HEADROOM_FRAMES,
                Arc::new(steno_audio::SystemClock::new()),
            )
            .map_err(|error| error.to_string())
        });
        let harness = harness_capturing(
            models_with(&[]),
            "parakeet-v3",
            platform_rule(SpeechSettings::default()),
            capture,
        );
        start(&harness.recorder);
        // Past both misses: 2.25 s of the producer's timeline.
        tokio::time::sleep(Duration::from_millis(3_000)).await;
        stop(&harness.recorder);
        assert_eq!(
            harness.recorder.status().warning.as_deref(),
            Some("About 2 seconds of the recording are missing.")
        );
        // Before the harness waits for the pipeline's runs, so none writes
        // into the removed folder.
        eventually("the pipeline picked the saved meeting up", || {
            harness.engine.transcriptions.count() > 0
        })
        .await;
    }

    /// The note comes from about a second on, and names a device only when
    /// the latest gap followed its stall; so does the warning.
    #[test]
    fn the_missing_audio_note_comes_from_about_a_second_on() {
        assert_eq!(missing_audio_note(1.0, Some("USB Microphone")), None);
        assert_eq!(
            missing_audio_note(1.2, Some("USB Microphone")).as_deref(),
            Some(
                "About 1 second of the recording is missing: no audio arrived from USB Microphone."
            )
        );
        assert_eq!(
            missing_audio_note(12.6, Some("the microphone")).as_deref(),
            Some(
                "About 13 seconds of the recording are missing: no audio arrived from the \
                 microphone."
            )
        );
        assert_eq!(
            missing_audio_note(12.6, None).as_deref(),
            Some("About 13 seconds of the recording are missing.")
        );
        assert_eq!(
            no_audio_warning(Some("USB Microphone")),
            "No audio from USB Microphone. Still trying."
        );
        assert_eq!(
            no_audio_warning(None),
            "No audio is arriving. Still trying."
        );
    }

    /// Quitting stops the recording with `quit` and saves it before it
    /// returns.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn quitting_stops_and_saves_the_recording_with_the_quit_reason() {
        let harness = harness(&[]);
        start(&harness.recorder);
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

    /// Every pipeline sees a recording as under way from its start to the
    /// end of its stop, so no job runs `CoreML` beside it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_pipelines_see_a_recording_from_its_start_to_the_end_of_its_stop() {
        let harness = harness(&[]);
        let live = || {
            harness
                .recorder
                .pipeline
                .current()
                .dependencies()
                .live_recording
                .is_live()
        };
        assert!(!live());
        let start_seen = held_in(&harness.recorder, RecordingState::Starting);
        let starter = harness.recorder.clone();
        let starting = std::thread::spawn(move || start(&starter));
        start_seen.recv_timeout(PATIENCE).expect("the start began");
        assert!(live(), "starting");
        starting.join().unwrap();
        assert!(live(), "recording");
        let stop_seen = held_in(&harness.recorder, RecordingState::Stopping);
        let stopper = harness.recorder.clone();
        let stopping = std::thread::spawn(move || stop(&stopper));
        stop_seen.recv_timeout(PATIENCE).expect("the stop began");
        assert!(live(), "stopping");
        stopping.join().unwrap();
        assert_eq!(harness.recorder.status().state, RecordingState::Idle);
        assert!(!live(), "idle");
    }

    /// The live flag changes under the recorder's lock at every change, so
    /// two notifies on different threads never apply out of order.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_live_flag_changes_under_the_recorders_lock() {
        let harness = harness(&[]);
        let held = Arc::new(Mutex::new(Vec::new()));
        let recorder = Arc::downgrade(&harness.recorder);
        *harness.recorder.before_live.lock().unwrap() = Some(Arc::new({
            let held = held.clone();
            move || {
                if let Some(recorder) = recorder.upgrade() {
                    // Held by this thread: the flag's change follows under it.
                    let locked = recorder.inner.try_lock().is_err();
                    held.lock().unwrap().push(locked);
                }
            }
        }));
        start(&harness.recorder);
        stop(&harness.recorder);
        let held = held.lock().unwrap();
        assert!(held.len() >= 4, "a start and a stop notify: {held:?}");
        assert!(held.iter().all(|locked| *locked), "{held:?}");
    }

    /// A Quit while a manual Stop is saving waits for that save, keeping the
    /// manual stop's reason.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn quitting_during_a_stop_waits_for_its_save() {
        let harness = harness(&[]);
        start(&harness.recorder);
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

    /// A recorder dropped mid-call with the auto-stop armed lets the
    /// countdown's thread go.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_dropped_recorder_lets_its_countdown_go() {
        let harness = harness(&[]);
        let clock = Arc::new(steno_audio::testing::ManualClock::new());
        harness.recorder.count_down_on(clock.clone());
        try_start(&harness.recorder, CaptureMode::Call);
        harness
            .recorder
            .microphone_activity(MicrophoneActivity::Opened { app_name: None });
        harness
            .recorder
            .microphone_activity(MicrophoneActivity::Released);
        assert!(clock.wait_for_sleepers(1), "the countdown sleeps");
        let recorder = Arc::downgrade(&harness.recorder);
        drop(harness);
        assert!(recorder.upgrade().is_none(), "the recorder dropped");
        assert!(clock.wait_for_sleepers(0), "its countdown let go");
    }

    /// A Quit during a start stops the recording it starts.
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

    /// Once quitting ran, a start records nothing.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn no_recording_starts_after_quitting() {
        let harness = harness(&[]);
        quit(&harness.recorder);
        assert_eq!(status_after_a_start(&harness).state, RecordingState::Idle);
        assert_eq!(harness.store.all_meetings().unwrap(), []);
    }

    /// While an install holds starts off, a start records nothing and says
    /// why; a running recording goes on. Dropping the hold clears it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_start_during_an_install_is_refused_with_a_message() {
        let harness = harness(&[]);
        let hold = harness.recorder.hold_starts();
        let status = status_after_a_start(&harness);
        assert_eq!(status.state, RecordingState::Idle);
        assert_eq!(status.error.as_deref(), Some(INSTALLING_UPDATE));
        assert_eq!(harness.store.all_meetings().unwrap(), []);

        drop(hold);
        assert_eq!(harness.recorder.status().error, None);
        assert_eq!(
            status_after_a_start(&harness).state,
            RecordingState::Recording
        );
        let hold = harness.recorder.hold_starts();
        assert_eq!(harness.recorder.status().state, RecordingState::Recording);
        drop(hold);
        quit(&harness.recorder);
        assert_eq!(harness.store.all_meetings().unwrap().len(), 1);
    }

    /// An update whose install waits on a password prompt until the test
    /// cancels it; one never cancelled never returns.
    #[derive(Default)]
    struct PasswordPromptSource {
        prompt_up: tokio::sync::Notify,
        cancel: tokio::sync::Notify,
        relaunches: AtomicUsize,
        told: Mutex<Vec<String>>,
    }

    #[async_trait::async_trait]
    impl crate::updates::UpdateSource for PasswordPromptSource {
        async fn check(&self) -> Result<Option<String>, String> {
            Ok(Some("0.12.0".into()))
        }

        async fn download(&self, _version: &str) -> Result<Vec<u8>, String> {
            Ok(vec![1, 2, 3])
        }

        async fn install(&self, _version: &str, _package: Vec<u8>) -> Result<(), String> {
            self.prompt_up.notify_one();
            self.cancel.notified().await;
            Err("User canceled.".into())
        }

        fn installer(&self) -> crate::updates::Installer {
            crate::updates::Installer::AsksForAPassword
        }

        async fn relaunch(&self) {
            self.relaunches.fetch_add(1, Ordering::SeqCst);
        }

        async fn ask(&self, _question: crate::updates::Question<'_>) -> bool {
            true
        }

        fn tell_install_failed(&self, message: &str) {
            self.told.lock().unwrap().push(message.to_owned());
        }

        fn tell_relaunch_waits(&self, version: &str, _busy: crate::updates::Busy) {
            panic!("{version} was installed");
        }

        fn announce(&self, _version: &str) {}
    }

    /// The user's yes to 0.12.0 after a check that found it, over the app's
    /// gate on the harness's recorder and pipeline.
    async fn offer_after_a_check(
        harness: &Harness,
        source: Arc<dyn crate::updates::UpdateSource>,
    ) -> tokio::task::JoinHandle<()> {
        let schedule = crate::updates::UpdateSchedule::new(crate::updates::ScheduleParts {
            source,
            preferences: Arc::new(steno_host::fakes::FakePreferences::default()),
            clock: Arc::new(steno_host::fakes::FakeClock::new(chrono::Utc::now())),
            gate: Arc::new(crate::updates::IdleGate::new(
                harness.recorder.clone(),
                harness.recorder.pipeline.clone(),
            )),
            recorder: harness.recorder.clone(),
            support_directory: harness.dir.path().to_owned(),
            managed: false,
            runtime: tokio::runtime::Handle::current(),
        });
        schedule.check_on_request().await.unwrap();
        tokio::spawn(async move { schedule.offer("0.12.0").await })
    }

    /// A Record during an install's password prompt records at once; a
    /// cancelled prompt fails the install and the recording is saved.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_record_during_a_password_prompt_records() {
        let harness = harness(&[]);
        let source = Arc::new(PasswordPromptSource::default());
        let offer = offer_after_a_check(&harness, source.clone()).await;
        source.prompt_up.notified().await;
        let status = status_after_a_start(&harness);
        assert_eq!(status.state, RecordingState::Recording);
        assert_eq!(status.error, None);

        source.cancel.notify_one();
        offer.await.unwrap();
        assert_eq!(*source.told.lock().unwrap(), ["User canceled."]);
        assert_eq!(source.relaunches.load(Ordering::SeqCst), 0);
        assert_eq!(harness.recorder.status().state, RecordingState::Recording);
        stop(&harness.recorder);
        quit(&harness.recorder);
        assert_eq!(harness.store.all_meetings().unwrap().len(), 1);
    }

    /// With a prompt nobody answers, a Record records and Stop saves it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_password_prompt_nobody_answers_never_keeps_recording_off() {
        let harness = harness(&[]);
        let source = Arc::new(PasswordPromptSource::default());
        let offer = offer_after_a_check(&harness, source.clone()).await;
        source.prompt_up.notified().await;
        assert_eq!(
            status_after_a_start(&harness).state,
            RecordingState::Recording
        );
        stop(&harness.recorder);
        assert_eq!(harness.recorder.status().error, None);
        let meetings = harness.store.all_meetings().unwrap();
        assert_eq!(meetings.len(), 1);
        assert_ne!(meetings[0].state, steno_core::MeetingState::Recording);
        assert_eq!(source.relaunches.load(Ordering::SeqCst), 0);
        offer.abort();
        quit(&harness.recorder);
    }

    /// An update installed during a recording waits: nothing downloads until
    /// the meeting is processed, then it installs and relaunches with starts
    /// held off.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_users_install_waits_for_the_recording_and_its_processing() {
        let harness = harness(&[]);
        let source = Arc::new(WaitingSource {
            store: Some(harness.store.clone()),
            recorder: Some(harness.recorder.clone()),
            ..WaitingSource::default()
        });
        start(&harness.recorder);
        let offer = offer_after_a_check(&harness, source.clone()).await;
        tokio::time::sleep(crate::updates::IDLE_POLL * 2).await;
        assert_eq!(
            *source.asked.lock().unwrap(),
            ["install 0.12.0", "after the recording"]
        );
        assert_eq!(*source.steps.lock().unwrap(), Vec::<String>::new());
        assert_eq!(harness.recorder.status().state, RecordingState::Recording);

        stop(&harness.recorder);
        tokio::time::timeout(PATIENCE + crate::updates::IDLE_POLL * 2, offer)
            .await
            .expect("the install ran once the meeting was processed")
            .unwrap();
        assert_eq!(
            *source.steps.lock().unwrap(),
            [
                "download after: ready",
                "install",
                "relaunch: a Record was refused"
            ]
        );
        assert_eq!(
            *source.asked.lock().unwrap(),
            [
                "install 0.12.0",
                "after the recording",
                "install now 0.12.0"
            ],
            "the password installer asks again once the app is idle"
        );
        assert_eq!(harness.recorder.status().error, None, "the hold is gone");
    }

    /// Answers yes, and records the meetings' states at the download and
    /// whether a Record at the relaunch was refused.
    #[derive(Default)]
    struct WaitingSource {
        store: Option<Arc<Store>>,
        recorder: Option<Arc<CaptureRecorder>>,
        asked: Mutex<Vec<String>>,
        steps: Mutex<Vec<String>>,
    }

    #[async_trait::async_trait]
    impl crate::updates::UpdateSource for WaitingSource {
        async fn check(&self) -> Result<Option<String>, String> {
            Ok(Some("0.12.0".into()))
        }

        async fn download(&self, _version: &str) -> Result<Vec<u8>, String> {
            let states: Vec<_> = self
                .store
                .as_ref()
                .unwrap()
                .all_meetings()
                .unwrap()
                .iter()
                .map(|meeting| meeting.state.kind().as_str())
                .collect();
            self.steps
                .lock()
                .unwrap()
                .push(format!("download after: {}", states.join(", ")));
            Ok(vec![1, 2, 3])
        }

        async fn install(&self, _version: &str, _package: Vec<u8>) -> Result<(), String> {
            self.steps.lock().unwrap().push("install".to_owned());
            Ok(())
        }

        fn installer(&self) -> crate::updates::Installer {
            crate::updates::Installer::AsksForAPassword
        }

        async fn relaunch(&self) {
            let recorder = self.recorder.clone().unwrap();
            let refused = tokio::task::spawn_blocking(move || {
                recorder.start(CaptureMode::InPerson, None);
                recorder.status()
            })
            .await
            .unwrap();
            assert_eq!(refused.state, RecordingState::Idle);
            assert_eq!(refused.error.as_deref(), Some(INSTALLING_UPDATE));
            self.steps
                .lock()
                .unwrap()
                .push("relaunch: a Record was refused".to_owned());
        }

        async fn ask(&self, question: crate::updates::Question<'_>) -> bool {
            use crate::updates::{Busy, Question};
            self.asked.lock().unwrap().push(match question {
                Question::Install(version) => format!("install {version}"),
                Question::AfterItEnds(Busy::Recording) => "after the recording".to_owned(),
                Question::AfterItEnds(Busy::Processing) => "after the processing".to_owned(),
                Question::InstallNow(version) => format!("install now {version}"),
            });
            true
        }

        fn tell_install_failed(&self, message: &str) {
            panic!("the install failed: {message}");
        }

        fn tell_relaunch_waits(&self, version: &str, _busy: crate::updates::Busy) {
            panic!("{version} waited to relaunch");
        }

        fn announce(&self, _version: &str) {}
    }

    /// Simultaneous starts begin one recording; stopping saves it.
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
        on_own_thread(HANG_GUARD, "every start returned", move || {
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

    /// The tray's toggle starts a call recording, the next one stops it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_toggle_starts_a_call_and_the_next_one_stops_it() {
        let harness = harness(&[]);
        let toggling = harness.recorder.clone();
        on_own_thread(HANG_GUARD, "the first toggle returned", move || {
            toggling.toggle();
        });
        let status = harness.recorder.status();
        assert_eq!(status.state, RecordingState::Recording);
        assert_eq!(status.mode, Some(CaptureMode::Call));
        let meeting_id = status.meeting_id.unwrap();
        let toggling = harness.recorder.clone();
        on_own_thread(HANG_GUARD, "the second toggle returned", move || {
            toggling.toggle();
        });
        assert_eq!(harness.recorder.status().state, RecordingState::Idle);
        assert!(harness.store.asset(meeting_id).unwrap().is_some());
    }

    /// Quit while recording, as the shell drives it: the exit waits until
    /// the recording is saved and queued.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_quit_through_the_exit_gate_exits_after_the_recording_is_saved() {
        let harness = harness(&[]);
        start(&harness.recorder);
        let meeting_id = harness.recorder.status().meeting_id.unwrap();
        let gate = crate::app::ExitGate::default();
        let (exited, exit_seen) = std::sync::mpsc::channel();
        let quitting = harness.recorder.clone();
        let (store, at_exit) = (harness.store.clone(), harness.recorder.clone());
        let held = gate.exit_requested(
            HANG_GUARD,
            move || quitting.stop_for_quit(),
            move || {
                let meeting = store.meeting(meeting_id).unwrap().unwrap();
                let saved = store.asset(meeting_id).unwrap().is_some();
                let _ = exited.send((at_exit.status().state, meeting, saved));
            },
        );
        assert!(!held, "the exit waits for the save");
        let (state, meeting, saved) =
            tokio::task::spawn_blocking(move || exit_seen.recv_timeout(HANG_GUARD))
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

    /// The configured engine decides which model counts: with another engine
    /// stored whose models are missing, the warm-up neither loads nor
    /// downloads.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_recording_start_checks_the_models_of_the_configured_engine() {
        let models_dir = tempfile::tempdir().unwrap();
        let models = Arc::new(crate::speech::testing::models_in(models_dir.path()));
        crate::speech::testing::install_coreml_parakeet(&models);
        crate::speech::testing::install_onnx_diarizer(&models);
        assert!(models.is_installed(ModelAsset::OfflineDiarizer));
        let before = crate::speech::testing::files_under(models_dir.path());

        let harness = harness_over(
            models,
            "whisperkit-large-v3-turbo",
            platform_rule(SpeechSettings::default()),
        );
        start(&harness.recorder);
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        assert_eq!(harness.engine.preparations.count(), 0);
        assert_eq!(harness.diarizer.preparations.count(), 0);
        assert_eq!(
            crate::speech::testing::files_under(models_dir.path()),
            before,
            "nothing was downloaded"
        );
        stop(&harness.recorder);
    }

    /// The synthetic capture, its writer thread panicking after the first
    /// `frames`, so `stop()` fails without an asset.
    fn dying_capture(frames: usize, died: Arc<AtomicBool>) -> MakeCaptureSession {
        Arc::new(move |configuration: CaptureConfiguration| {
            let options = crate::testing::synthetic_tone(&configuration);
            let died = died.clone();
            session_with_writes(
                configuration,
                Arc::new(steno_audio::testing::SyntheticCaptureBackend::new(options)),
                CaptureSession::DEFAULT_WRITER_HEADROOM_FRAMES,
                move || {
                    let (mut left, died) = (frames, died.clone());
                    move || {
                        if left == 0 {
                            died.store(true, Ordering::SeqCst);
                            panic!("the writer dies");
                        }
                        left -= 1;
                        Ok(())
                    }
                },
            )
        })
    }

    /// The commit tries that found the database held, and whether the one in
    /// progress may end. Each test keeps one in a static, since SQLite's busy
    /// handler captures nothing.
    struct BusyTries {
        tries: AtomicUsize,
        open: AtomicBool,
    }

    impl BusyTries {
        const fn new() -> Self {
            Self {
                tries: AtomicUsize::new(0),
                open: AtomicBool::new(false),
            }
        }

        fn tries(&self) -> usize {
            self.tries.load(Ordering::SeqCst)
        }

        /// Lets the try in progress, and every later one, end at once.
        fn open(&self) {
            self.open.store(true, Ordering::SeqCst);
        }

        /// The busy handler: counts tries (`waits` 0 on a try's first call)
        /// and keeps the try waiting until the test opens it or
        /// [`HANG_GUARD`] passes.
        fn on_busy(&self, waits: i32) -> bool {
            if waits == 0 {
                self.tries.fetch_add(1, Ordering::SeqCst);
            }
            if self.open.load(Ordering::SeqCst) {
                return false;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
            Duration::from_millis(waits.unsigned_abs().into()) < HANG_GUARD
        }
    }

    /// Gives the store `handler` as its busy handler and holds the database
    /// from another connection until the hold is dropped.
    fn held_with_counted_tries(
        harness: &Harness,
        handler: fn(i32) -> bool,
    ) -> steno_core::testing::WriteLockHold {
        harness
            .store
            .on_writer(|connection| connection.busy_handler(Some(handler)))
            .unwrap();
        steno_core::testing::WriteLockHold::new(&harness.dir.path().join("steno.sqlite"))
    }

    /// [`quit`]s on its own thread and, once the recorder has seen it, lets
    /// the commit's try end.
    async fn quit_while_busy(recorder: &Arc<CaptureRecorder>, busy: &BusyTries) {
        let quitting = recorder.clone();
        let quitter = std::thread::spawn(move || quit(&quitting));
        eventually("the quit came", || recorder.inner().quitting).await;
        busy.open();
        quitter.join().unwrap();
    }

    /// A stop for the exit tries its commit once, so quitting returns
    /// within the exit's patience, and the meeting stays `recording`.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_stop_for_the_exit_tries_its_commit_once() {
        static BUSY: BusyTries = BusyTries::new();
        let harness = harness_over(
            models_with(&[]),
            "parakeet-v3",
            platform_rule(SpeechSettings::default()),
        );
        start(&harness.recorder);
        let meeting_id = harness.recorder.status().meeting_id.unwrap();
        let hold = held_with_counted_tries(&harness, |waits| BUSY.on_busy(waits));
        quit_while_busy(&harness.recorder, &BUSY).await;
        drop(hold);
        assert_eq!(BUSY.tries(), 1, "one try");
        assert_kept(&harness, meeting_id, &harness.dir.path().join("audio"));
    }

    /// The meeting was kept for the next launch, with `folder` recorded.
    fn assert_kept(harness: &Harness, meeting_id: Uuid, folder: &Path) {
        assert_eq!(
            harness.store.meeting(meeting_id).unwrap().unwrap().state,
            steno_core::MeetingState::Recording
        );
        let recorded = crate::audio_folders::recorded(&harness.dir.path().join("support")).unwrap();
        assert_eq!(
            recorded.get(&meeting_id).map(PathBuf::as_path),
            Some(folder)
        );
        assert_eq!(
            harness.recorder.status().error.as_deref(),
            Some(KEPT_FOR_THE_NEXT_LAUNCH)
        );
    }

    /// A stop under way when the app quits tries its commit no more.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_stop_under_way_when_the_app_quits_tries_its_commit_no_more() {
        static BUSY: BusyTries = BusyTries::new();
        let harness = harness(&[]);
        start(&harness.recorder);
        let meeting_id = harness.recorder.status().meeting_id.unwrap();
        let hold = held_with_counted_tries(&harness, |waits| BUSY.on_busy(waits));
        let stop_seen = held_in(&harness.recorder, RecordingState::Stopping);
        let stopper = harness.recorder.clone();
        let manual = std::thread::spawn(move || stopper.stop());
        stop_seen.recv_timeout(PATIENCE).expect("the stop began");
        quit_while_busy(&harness.recorder, &BUSY).await;
        drop(hold);
        manual.join().unwrap();
        assert_eq!(BUSY.tries(), 1, "one try");
        assert_kept(&harness, meeting_id, &harness.dir.path().join("audio"));
    }

    /// A quit during the stop's first busy try is seen before the next try.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_quit_during_the_stop_s_first_try_tries_no_more() {
        static BUSY: BusyTries = BusyTries::new();
        let harness = harness(&[]);
        start(&harness.recorder);
        let meeting_id = harness.recorder.status().meeting_id.unwrap();
        let hold = held_with_counted_tries(&harness, |waits| BUSY.on_busy(waits));
        let stopper = harness.recorder.clone();
        let manual = std::thread::spawn(move || stopper.stop());
        eventually_within(HANG_GUARD, "the stop's first try waits", || {
            BUSY.tries() == 1
        })
        .await;
        quit_while_busy(&harness.recorder, &BUSY).await;
        drop(hold);
        manual.join().unwrap();
        assert_eq!(BUSY.tries(), 1, "one try");
        assert_kept(&harness, meeting_id, &harness.dir.path().join("audio"));
    }

    /// After a menu Quit refused the starts, no start or toggle records, and
    /// the running recording still stops with its own end reason.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_start_after_a_menu_quit_is_refused_with_a_message() {
        let harness = harness(&[]);
        start(&harness.recorder);
        let meeting_id = harness.recorder.status().meeting_id.unwrap();
        harness.recorder.refuse_starts();
        assert_eq!(harness.recorder.status().state, RecordingState::Recording);
        stop(&harness.recorder);
        let meeting = harness.store.meeting(meeting_id).unwrap().unwrap();
        assert_eq!(meeting.end_reason, Some(RecordingEndReason::Manual));

        let changes = Arc::new(AtomicUsize::new(0));
        let counted = changes.clone();
        harness.recorder.on_change(Arc::new(move || {
            counted.fetch_add(1, Ordering::SeqCst);
        }));
        for refused in [
            Box::new(|recorder: &CaptureRecorder| recorder.start(CaptureMode::InPerson, None))
                as Box<dyn Fn(&CaptureRecorder) + Send>,
            Box::new(|recorder: &CaptureRecorder| recorder.toggle()),
            Box::new(|recorder: &CaptureRecorder| recorder.start(CaptureMode::Call, Some("Call"))),
        ] {
            let before = changes.load(Ordering::SeqCst);
            let starter = harness.recorder.clone();
            on_own_thread(HANG_GUARD, "the start returned", move || {
                refused(starter.as_ref());
            });
            let status = harness.recorder.status();
            assert_eq!(status.state, RecordingState::Idle);
            assert_eq!(status.error.as_deref(), Some(QUITTING_REFUSAL));
            assert!(
                changes.load(Ordering::SeqCst) > before,
                "the host hears of the refusal"
            );
        }
        assert_eq!(
            harness.store.all_meetings().unwrap().len(),
            1,
            "no junk meeting"
        );
    }

    /// The menu Quit's refusal is not the shutdown's quit: a Stop keeps
    /// retrying its commit after it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_stop_under_way_at_a_menu_quit_still_tries_its_commit_again() {
        static BUSY: BusyTries = BusyTries::new();
        let harness = harness(&[]);
        start(&harness.recorder);
        let meeting_id = harness.recorder.status().meeting_id.unwrap();
        let hold = held_with_counted_tries(&harness, |waits| BUSY.on_busy(waits));
        let stop_seen = held_in(&harness.recorder, RecordingState::Stopping);
        let stopper = harness.recorder.clone();
        let manual = std::thread::spawn(move || stopper.stop());
        stop_seen.recv_timeout(PATIENCE).expect("the stop began");
        harness.recorder.refuse_starts();
        eventually("the first try found the store held", || BUSY.tries() == 1).await;
        // Every try from here on ends at once, still held.
        BUSY.open();
        manual.join().unwrap();
        drop(hold);
        assert!(
            BUSY.tries() > 1,
            "the stop tried its commit once, as a quit's does"
        );
        assert_kept(&harness, meeting_id, &harness.dir.path().join("audio"));
    }

    /// The launch's background half, with every master an hour old.
    fn launch(store: &Arc<Store>, pipeline: &CurrentPipeline, support: &Path) -> Vec<Uuid> {
        crate::app::reconcile_at_launch(
            store,
            pipeline,
            &crate::recovery::Interrupted::list(store, support),
            &crate::testing::an_hour_later(),
            chrono::FixedOffset::east_opt(0).unwrap(),
            &tokio::runtime::Handle::current(),
        )
    }

    /// A stop whose meeting was failed or deleted while it recorded says it
    /// was not stored and keeps the recorded folder, so the next launch
    /// adopts the deleted one's master.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_stop_whose_meeting_moved_on_while_recording_settles() {
        let changes: [fn(&Store, Uuid); 2] = [
            |store, id| {
                store
                    .set_state(
                        id,
                        steno_core::MeetingState::Failed { reason: "x".into() },
                        Utc::now(),
                    )
                    .unwrap();
            },
            |store, id| {
                store.delete_meeting_left_recording(id).unwrap();
            },
        ];
        for (change, deleted) in changes.into_iter().zip([false, true]) {
            let harness = harness(&[]);
            start(&harness.recorder);
            let meeting_id = harness.recorder.status().meeting_id.unwrap();
            change(&harness.store, meeting_id);
            stop(&harness.recorder);
            assert_eq!(
                harness.recorder.status().error.as_deref(),
                Some(MEETING_NOT_STORED)
            );
            assert_eq!(
                harness.store.meeting(meeting_id).unwrap().is_none(),
                deleted
            );
            assert!(harness.store.asset(meeting_id).unwrap().is_none());
            let support = harness.dir.path().join("support");
            assert!(
                crate::audio_folders::recorded(&support)
                    .unwrap()
                    .contains_key(&meeting_id)
            );

            let adopted = launch(&harness.store, &harness.recorder.pipeline, &support);
            assert_eq!(adopted, if deleted { vec![meeting_id] } else { vec![] });
            harness.recorder.pipeline.current().wait_until_idle().await;
            assert!(harness.store.meeting(meeting_id).unwrap().is_some());
            // The adopted one's entry goes at the launch after this one.
            assert_eq!(
                crate::audio_folders::recorded(&support)
                    .unwrap()
                    .contains_key(&meeting_id),
                deleted
            );
        }
    }

    /// A saved stop keeps the recorded folder until a durable checkpoint.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_saved_stop_keeps_its_folder_for_the_next_launch_to_forget() {
        let harness = harness(&[]);
        start(&harness.recorder);
        let meeting_id = harness.recorder.status().meeting_id.unwrap();
        let support = harness.dir.path().join("support");
        stop(&harness.recorder);
        assert!(harness.store.asset(meeting_id).unwrap().is_some());
        assert!(
            crate::audio_folders::recorded(&support)
                .unwrap()
                .contains_key(&meeting_id)
        );

        harness.recorder.pipeline.current().wait_until_idle().await;
        assert_eq!(
            launch(&harness.store, &harness.recorder.pipeline, &support),
            Vec::<Uuid>::new()
        );
        assert!(crate::audio_folders::recorded(&support).unwrap().is_empty());
    }

    /// After a power loss before a checkpoint, the next launch adopts the
    /// master, since the recorded folder stayed.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_power_loss_after_a_saved_stop_leaves_the_master_to_the_next_launch() {
        let harness = harness(&[]);
        harness.store.checkpoint_durably().unwrap();
        start(&harness.recorder);
        let meeting_id = harness.recorder.status().meeting_id.unwrap();
        stop(&harness.recorder);
        assert!(harness.store.asset(meeting_id).unwrap().is_some());
        harness.recorder.pipeline.current().wait_until_idle().await;

        let after = harness.dir.path().join("after-power-loss.sqlite");
        std::fs::copy(harness.dir.path().join("steno.sqlite"), &after).unwrap();
        let store = Arc::new(Store::open(&after).unwrap());
        assert!(store.meeting(meeting_id).unwrap().is_none());
        let pipeline = crate::testing::current_pipeline(fake_dependencies(&store, "fake-engine"));
        let support = harness.dir.path().join("support");
        assert_eq!(launch(&store, &pipeline, &support), [meeting_id]);
        pipeline.current().wait_until_idle().await;
        assert!(store.asset(meeting_id).unwrap().is_some());
    }

    /// A stop whose capture failed recovers the master and queues it as
    /// `failed`, rather than failing the meeting.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_stop_whose_capture_failed_keeps_what_was_written() {
        let died = Arc::new(AtomicBool::new(false));
        let harness = harness_capturing(
            models_with(&[]),
            "parakeet-v3",
            platform_rule(SpeechSettings::default()),
            dying_capture(20, died.clone()),
        );
        start(&harness.recorder);
        let meeting_id = harness.recorder.status().meeting_id.unwrap();
        eventually_within(HANG_GUARD, "the writer died", || {
            died.load(Ordering::SeqCst)
        })
        .await;
        stop(&harness.recorder);

        let status = harness.recorder.status();
        assert_eq!(status.state, RecordingState::Idle);
        assert_eq!(status.error, None);
        assert_eq!(status.warning.as_deref(), Some(RECOVERED_AFTER_A_FAILURE));
        let meeting = harness.store.meeting(meeting_id).unwrap().unwrap();
        assert_eq!(meeting.end_reason, Some(RecordingEndReason::Failed));
        assert!(
            !matches!(
                meeting.state.kind(),
                steno_core::MeetingStateKind::Recording | steno_core::MeetingStateKind::Failed
            ),
            "{:?}",
            meeting.state
        );
        assert_eq!(
            meeting.duration,
            9_600.0 / steno_audio::SAMPLE_RATE,
            "20 frames"
        );
        let asset = harness.store.asset(meeting_id).unwrap().unwrap();
        assert_eq!(asset.lanes, vec![steno_core::AudioLane::Mixed]);
        assert!(
            asset
                .sidecars_16k
                .contains_key(&steno_core::AudioLane::Mixed)
        );
    }

    /// A failed-capture stop whose master cannot be found now (an unmounted
    /// volume) keeps the meeting for the next launch.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_failed_stop_whose_master_cannot_be_found_is_kept() {
        for unmount_the_audio_folder in [true, false] {
            let died = Arc::new(AtomicBool::new(false));
            let harness = harness_capturing(
                models_with(&[]),
                "parakeet-v3",
                platform_rule(SpeechSettings::default()),
                dying_capture(20, died.clone()),
            );
            start(&harness.recorder);
            let meeting_id = harness.recorder.status().meeting_id.unwrap();
            eventually_within(HANG_GUARD, "the writer died", || {
                died.load(Ordering::SeqCst)
            })
            .await;
            let audio = harness.dir.path().join("audio");
            let unmounted = harness.dir.path().join("unmounted");
            let meeting_folder = steno_core::RecordingLayout::new(&audio, meeting_id).directory;
            // Tried until the dying writer closed its files (Windows).
            eventually("the folder went", || {
                if unmount_the_audio_folder {
                    std::fs::rename(&audio, &unmounted).is_ok()
                } else {
                    std::fs::remove_dir_all(&meeting_folder).is_ok()
                }
            })
            .await;
            stop(&harness.recorder);
            assert_kept(&harness, meeting_id, &audio);
        }
    }

    /// The database file of `harness`'s store.
    fn database(harness: &Harness) -> PathBuf {
        crate::testing::database_in(harness.dir.path())
    }

    /// A stop while another process holds the write lock shows `Stopping`
    /// within [`LIVE`] of the notice and
    /// [`END_TO_END`](crate::testing::END_TO_END) of the stop, with a reload
    /// in flight; the save lands within a second of the lock's release.
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn a_stop_under_a_held_lock_shows_stopping_at_once_and_saves_once_the_lock_goes() {
        let harness = harness(&[]);
        let hosted = crate::testing::hosted(&harness.store, &harness.recorder);
        start(&harness.recorder);
        let meeting_id = harness.recorder.status().meeting_id.unwrap();

        let mut held =
            crate::testing::HeldStore::with_a_write_waiting(&harness.store, &database(&harness));
        let reload = hosted.reload_in_flight();
        let since = Instant::now();
        let stopper = harness.recorder.clone();
        let stopping = std::thread::spawn(move || stopper.stop());
        let shown = hosted
            .sink
            .recording_in("stopping", since, PATIENCE)
            .expect("the stop reached the host");
        // Recorded once `recorder_changed` returned.
        let noticed = found_within(PATIENCE, "the stop's notice", || {
            hosted
                .hooks_since(since)
                .into_iter()
                .find(|call| call.state != RecordingState::Recording)
                .map(|call| call.began)
        });
        let after_notice = shown.saturating_duration_since(noticed);
        assert!(
            after_notice < LIVE,
            "Stopping reached the host {after_notice:?} after the recorder's notice"
        );
        let after_stop = shown.duration_since(since);
        assert!(
            after_stop < crate::testing::END_TO_END,
            "Stopping reached the host {after_stop:?} after the stop"
        );

        let released = held.release_after_hold();
        crate::testing::saved_within(&harness.store, meeting_id, released, Duration::from_secs(1));
        stopping.join().unwrap();
        reload.join().unwrap();
        assert_eq!(harness.recorder.status().state, RecordingState::Idle);
    }

    /// A stop during a held forced reload and a three-second shared read
    /// shows `Stopping` within 100 ms of the notice, saves within [`LIVE`],
    /// and finishes its files while both reads are still held.
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn a_stop_during_a_slow_reload_shows_stopping_at_once_and_finishes_its_files() {
        const FEED: Duration = Duration::from_millis(100);
        const SHARED_READ: Duration = Duration::from_secs(3);
        let harness = harness(&[]);
        let shown_meeting = crate::testing::shown_meeting(&harness.store);
        let pipeline = Arc::new(crate::testing::GatedFiles::default());
        let hosted =
            crate::testing::hosted_over(&harness.store, &harness.recorder, Some(pipeline.clone()));
        let own = hosted.host.own_store();
        pipeline.gate_inside_reads_on(own.clone());
        hosted.host.request_meeting(shown_meeting.id);
        assert!(pipeline.gate.reloads() > 0, "the meeting's detail is shown");
        start(&harness.recorder);
        let meeting_id = harness.recorder.status().meeting_id.unwrap();
        // Another connection, so watching the row takes neither reader.
        let watcher = Store::open_without_migrating(database(&harness)).unwrap();

        let armed = pipeline.gate.arm();
        let reload = hosted.reload_in_flight();
        assert!(
            pipeline.gate.wait_for(PATIENCE, |state| state.held == 1),
            "the reload is held with the view models and the host's reader"
        );
        // Waits for the host's reader while the reload holds it.
        let host_read = std::thread::spawn(move || own.read(|_| Ok(())).unwrap());
        let (reading, read_began) = std::sync::mpsc::channel();
        let shared = harness.store.clone();
        let shared_read = std::thread::spawn(move || {
            shared
                .read(|_| {
                    reading.send(()).unwrap();
                    std::thread::sleep(SHARED_READ);
                    Ok(())
                })
                .unwrap();
        });
        read_began.recv_timeout(PATIENCE).unwrap();

        let since = Instant::now();
        let stopper = harness.recorder.clone();
        let stopping = std::thread::spawn(move || stopper.stop());
        let shown = hosted
            .sink
            .recording_in("stopping", since, PATIENCE)
            .expect("the stop reached the host");
        let noticed = found_within(PATIENCE, "the stop's notice", || {
            hosted
                .hooks_since(since)
                .into_iter()
                .find(|call| call.state != RecordingState::Recording)
                .map(|call| call.began)
        });
        let after_notice = shown.saturating_duration_since(noticed);
        assert!(
            after_notice < FEED,
            "Stopping reached the host {after_notice:?} after the recorder's notice"
        );
        let (saved, state) = found_within(PATIENCE, "the stop's save", || {
            let state = watcher.meeting(meeting_id).unwrap().unwrap().state;
            (state != steno_core::MeetingState::Recording).then(|| (Instant::now(), state))
        });
        assert!(
            !matches!(state, steno_core::MeetingState::Failed { .. }),
            "saved, got {state:?}"
        );
        let took = saved.saturating_duration_since(noticed);
        assert!(
            took < LIVE,
            "the meeting was saved {took:?} after the recorder's notice"
        );
        found_within(HANG_GUARD, "the stop ended", || {
            stopping.is_finished().then_some(())
        });
        assert!(
            !shared_read.is_finished(),
            "the stop ended while the shared reader was held"
        );
        assert!(
            pipeline
                .gate
                .wait_for(Duration::ZERO, |state| state.held == 1)
                && !host_read.is_finished(),
            "the stop ended while the reload held the host's reader"
        );
        drop(armed);
        stopping.join().unwrap();
        reload.join().unwrap();
        host_read.join().unwrap();
        shared_read.join().unwrap();
        assert_eq!(harness.recorder.status().state, RecordingState::Idle);
    }

    /// A stop's two moves (`Stopping`, `Idle`) each ask the reload owner for
    /// a pass, and the stopped row reaches the list within [`LIVE`] of the
    /// lock's release from the owner's forced pass, not the stop's thread.
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn a_stop_asks_for_a_reload_per_move() {
        let harness = harness(&[]);
        let hosted = crate::testing::hosted(&harness.store, &harness.recorder);
        let owner = tokio::spawn(crate::app::own_the_reloads(
            hosted.host.clone(),
            Duration::from_secs(3600),
        ));
        start(&harness.recorder);
        let meeting_id = harness.recorder.status().meeting_id.unwrap();
        let mut held = crate::testing::HeldStore::lock_only(&database(&harness));
        let requests = || hosted.host.reload_request_count();
        let before = requests();

        let stopper = harness.recorder.clone();
        let stopping = std::thread::Builder::new()
            .name("the-stop".to_owned())
            .spawn(move || stopper.stop())
            .unwrap();
        found_within(PATIENCE, "Stopping asked for a reload", || {
            (requests() > before).then_some(())
        });
        assert_eq!(harness.recorder.status().state, RecordingState::Stopping);
        assert_eq!(requests(), before + 1, "one request for Stopping");
        let saved = held.release();
        found_within(HANG_GUARD, "the stop saved", || {
            stopping.is_finished().then_some(())
        });
        stopping.join().unwrap();
        let id = meeting_id.to_string();
        let (at, thread) = found_within(PATIENCE, "the stopped row listed", || {
            hosted
                .sink
                .since_on(steno_bridge::BridgeTopic::MeetingsList, saved)
                .into_iter()
                .find(|(_, list, _)| {
                    list["groups"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .any(|group| {
                            group["meetings"]
                                .as_array()
                                .into_iter()
                                .flatten()
                                .any(|row| {
                                    row["id"]
                                        .as_str()
                                        .is_some_and(|row_id| row_id.eq_ignore_ascii_case(&id))
                                        && row["state"] != "recording"
                                })
                        })
                })
                .map(|(at, _, thread)| (at, thread))
        });
        let took = at.duration_since(saved);
        assert!(
            took < LIVE,
            "the stopped row was listed {took:?} after the save"
        );
        assert_ne!(
            thread.as_deref(),
            Some("the-stop"),
            "listed from the stop's thread"
        );
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(
            requests(),
            before + 2,
            "one request for Stopping, one for Idle"
        );
        owner.abort();
    }

    /// The stop runs and saves on its own thread, so it saves even while the
    /// services runtime's only worker is blocked.
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn a_stop_saves_while_every_runtime_worker_is_blocked() {
        let harness = harness(&[]);
        start(&harness.recorder);
        let meeting_id = harness.recorder.status().meeting_id.unwrap();
        let (blocked, worker) = std::sync::mpsc::channel();
        let (unblock, unblocked) = std::sync::mpsc::channel::<()>();
        tokio::spawn(async move {
            blocked.send(()).unwrap();
            let _ = unblocked.recv_timeout(HANG_GUARD);
        });
        worker
            .recv_timeout(PATIENCE)
            .expect("the worker is blocked");

        stop(&harness.recorder);
        let state = harness.store.meeting(meeting_id).unwrap().unwrap().state;
        unblock.send(()).unwrap();
        assert!(
            !matches!(
                state,
                steno_core::MeetingState::Recording | steno_core::MeetingState::Failed { .. }
            ),
            "saved while the worker was blocked, got {state:?}"
        );
    }

    /// A start while another process holds the write lock waits out the busy
    /// timeout, then is refused with no capture and no row, while `Starting`
    /// and commands stay live.
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn a_start_under_a_held_lock_is_refused_and_the_ui_stays_live() {
        use steno_bridge::BridgeHost as _;
        let harness = harness(&[]);
        let hosted = crate::testing::hosted(&harness.store, &harness.recorder);
        let mut held = crate::testing::HeldStore::lock_only(&database(&harness));
        let since = Instant::now();
        let starter = harness.recorder.clone();
        let starting = std::thread::spawn(move || starter.start(CaptureMode::InPerson, None));
        let shown = hosted
            .sink
            .recording_in("starting", since, PATIENCE)
            .expect("the start reached the host")
            .duration_since(since);
        assert!(
            shown < LIVE,
            "Starting reached the host {shown:?} after the start"
        );
        // The start's write waits out its busy timeout meanwhile.
        std::thread::sleep(Duration::from_secs(1));
        assert!(!starting.is_finished(), "the start still waits");
        let asked = Instant::now();
        hosted.host.settings_recording_refresh_devices().unwrap();
        let answered = asked.elapsed();
        assert!(
            answered < LIVE,
            "a command took {answered:?} during the start"
        );
        let slowest = hosted
            .hooks_since(since)
            .iter()
            .map(|call| call.took)
            .max()
            .unwrap_or_default();
        assert!(slowest < LIVE, "a change's hook took {slowest:?}");

        starting.join().unwrap();
        let refused = since.elapsed();
        assert!(
            refused >= Duration::from_secs(4),
            "refused after the busy timeout, not {refused:?}"
        );
        let status = harness.recorder.status();
        assert_eq!(status.state, RecordingState::Idle);
        assert_eq!(
            status.error.as_deref(),
            Some("Recording could not start: Steno could not create the meeting.")
        );
        assert!(
            hosted
                .sink
                .recording_in("recording", since, Duration::ZERO)
                .is_none(),
            "nothing recorded"
        );
        held.release();
        assert_eq!(harness.store.meetings(10, 0).unwrap(), Vec::new());
        let audio = harness.dir.path().join("audio");
        assert!(
            std::fs::read_dir(&audio).map_or(true, |mut entries| entries.next().is_none()),
            "no capture wrote into the audio folder"
        );
    }

    /// A start whose meeting cannot be written forgets the folder it
    /// recorded first; the folder stays known.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_start_whose_meeting_cannot_be_written_forgets_its_folder() {
        let harness = harness(&[]);
        harness
            .store
            .on_writer(|connection| connection.busy_timeout(std::time::Duration::from_millis(100)))
            .unwrap();
        let hold =
            steno_core::testing::WriteLockHold::new(&harness.dir.path().join("steno.sqlite"));
        try_start(&harness.recorder, CaptureMode::InPerson);
        drop(hold);
        let status = harness.recorder.status();
        assert_eq!(status.state, RecordingState::Idle);
        assert_eq!(
            status.error.as_deref(),
            Some("Recording could not start: Steno could not create the meeting.")
        );
        assert_eq!(harness.store.meetings(10, 0).unwrap(), Vec::new());
        let support = harness.dir.path().join("support");
        assert!(crate::audio_folders::recorded(&support).unwrap().is_empty());
        assert_eq!(
            crate::audio_folders::known(&support).unwrap(),
            [harness.dir.path().join("audio")]
        );
    }

    /// The folder is recorded before the master exists; a failed session
    /// start fails the row and forgets the entry, the folder staying known.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_folder_is_recorded_before_the_master_and_forgotten_when_the_start_fails() {
        type Seen = (
            std::collections::BTreeMap<Uuid, PathBuf>,
            Vec<PathBuf>,
            Vec<Uuid>,
        );
        let seen: Arc<Mutex<Option<Seen>>> = Arc::default();
        let failing = {
            let seen = seen.clone();
            crate::testing::synthetic_capture_writing(Arc::new(move |layout, _, _| {
                let audio_folder = layout.directory.parent().unwrap();
                let root = audio_folder.parent().unwrap();
                let support = root.join("support");
                let store = Store::open(root.join("steno.sqlite")).unwrap();
                let rows = store.meetings(10, 0).unwrap();
                *seen.lock().unwrap() = Some((
                    crate::audio_folders::recorded(&support).unwrap(),
                    crate::audio_folders::known(&support).unwrap(),
                    rows.iter().map(|meeting| meeting.id).collect(),
                ));
                Err(CaptureError::WriterFailed("refused".into()))
            }))
        };
        let harness = harness_capturing(
            models_with(&[]),
            "parakeet-v3",
            platform_rule(SpeechSettings::default()),
            failing,
        );
        try_start(&harness.recorder, CaptureMode::InPerson);
        assert_eq!(harness.recorder.status().state, RecordingState::Idle);

        let audio_folder = harness.dir.path().join("audio");
        let support = harness.dir.path().join("support");
        let (recorded, known, rows) = seen.lock().unwrap().take().expect("the start tried");
        let [meeting_id] = rows[..] else {
            panic!("one row: {rows:?}");
        };
        assert_eq!(
            recorded,
            std::collections::BTreeMap::from([(meeting_id, audio_folder.clone())])
        );
        assert_eq!(known, std::slice::from_ref(&audio_folder));
        assert!(matches!(
            harness.store.meeting(meeting_id).unwrap().unwrap().state,
            steno_core::MeetingState::Failed { .. }
        ));
        assert!(crate::audio_folders::recorded(&support).unwrap().is_empty());
        assert_eq!(
            crate::audio_folders::known(&support).unwrap(),
            [audio_folder]
        );
    }

    /// A meeting left `recording` counts as still written while its master
    /// is under ten seconds old, so the host refuses to delete it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_left_recording_is_still_written_while_its_master_is_fresh() {
        let harness = harness(&[]);
        let meeting_id = Uuid::new_v4();
        let support = harness.dir.path().join("support");
        let recorded = harness.dir.path().join("elsewhere");
        let retired = harness.dir.path().join("retired");
        crate::audio_folders::record(&support, meeting_id, &recorded).unwrap();
        crate::audio_folders::remember(&support, &retired).unwrap();
        let master = crate::recovery::master_path(&recorded, meeting_id);
        std::fs::create_dir_all(master.parent().unwrap()).unwrap();
        let file = std::fs::File::create(&master).unwrap();

        let left = harness.recorder.left_recording(meeting_id);
        assert_eq!(
            left.folders,
            [
                recorded.clone(),
                harness.dir.path().join("audio"),
                retired.clone()
            ]
        );
        assert!(left.still_written);

        file.set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(3_600))
            .unwrap();
        assert!(!harness.recorder.left_recording(meeting_id).still_written);

        assert_eq!(
            harness.recorder.forget_recording(meeting_id).unwrap(),
            Some(recorded)
        );
        assert_eq!(
            harness.recorder.left_recording(meeting_id).folders,
            [harness.dir.path().join("audio"), retired]
        );
    }

    /// A delete that does not go through records the folder again; a forget
    /// that cannot write the record keeps the entry, so the delete is refused.
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_forgotten_folder_is_restored_and_a_failed_forget_keeps_it() {
        use std::os::unix::fs::PermissionsExt as _;
        let harness = harness(&[]);
        let meeting_id = Uuid::new_v4();
        let support = harness.dir.path().join("support");
        let folder = harness.dir.path().join("audio");
        crate::audio_folders::record(&support, meeting_id, &folder).unwrap();
        let forgotten = harness.recorder.forget_recording(meeting_id).unwrap();
        assert_eq!(forgotten.as_deref(), Some(folder.as_path()));
        assert!(
            harness
                .recorder
                .forget_recording(meeting_id)
                .unwrap()
                .is_none()
        );
        harness.recorder.restore_recording(meeting_id, &folder);
        let entries = crate::audio_folders::recorded(&support).unwrap();
        assert_eq!(entries.get(&meeting_id), Some(&folder));

        std::fs::set_permissions(&support, std::fs::Permissions::from_mode(0o555)).unwrap();
        let failed = harness.recorder.forget_recording(meeting_id);
        std::fs::set_permissions(&support, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(failed.is_err(), "{failed:?}");
        let entries = crate::audio_folders::recorded(&support).unwrap();
        assert_eq!(entries.get(&meeting_id), Some(&folder));
    }

    /// A restore that cannot write the record checkpoints the store durably
    /// instead (`synchronous = FULL`, a WAL restart).
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_restore_that_fails_checkpoints_the_store_durably() {
        use std::os::unix::fs::PermissionsExt as _;
        let harness = harness(&[]);
        let meeting_id = Uuid::new_v4();
        let support = harness.dir.path().join("support");
        let folder = harness.dir.path().join("audio");
        crate::audio_folders::record(&support, meeting_id, &folder).unwrap();
        let forgotten = harness.recorder.forget_recording(meeting_id).unwrap();
        assert_eq!(forgotten.as_deref(), Some(folder.as_path()));
        let levels: Arc<Mutex<Vec<i64>>> = Arc::default();
        let seen = levels.clone();
        harness.store.probe_commits(move |connection| {
            let level = connection
                .query_row("PRAGMA synchronous", [], |row| row.get(0))
                .unwrap();
            seen.lock().unwrap().push(level);
        });

        std::fs::set_permissions(&support, std::fs::Permissions::from_mode(0o555)).unwrap();
        harness.recorder.restore_recording(meeting_id, &folder);
        std::fs::set_permissions(&support, std::fs::Permissions::from_mode(0o755)).unwrap();

        let entries = crate::audio_folders::recorded(&support).unwrap();
        assert_eq!(entries.get(&meeting_id), None);
        assert_eq!(*levels.lock().unwrap(), [2]);
    }

    /// A forget that fails checkpoints the store durably, so after a power
    /// loss the row is still there.
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_failed_forget_then_a_power_loss_keeps_the_row() {
        use std::os::unix::fs::PermissionsExt as _;
        let harness = harness(&[]);
        harness.store.checkpoint_durably().unwrap();
        start(&harness.recorder);
        let meeting_id = harness.recorder.status().meeting_id.unwrap();
        stop(&harness.recorder);
        harness.recorder.pipeline.current().wait_until_idle().await;
        let support = harness.dir.path().join("support");

        std::fs::set_permissions(&support, std::fs::Permissions::from_mode(0o555)).unwrap();
        let failed = harness.recorder.forget_recording(meeting_id);
        std::fs::set_permissions(&support, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(failed.is_err(), "{failed:?}");
        // What a write that failed after its rename leaves.
        crate::audio_folders::forget(&support, &[meeting_id]).unwrap();

        let after = harness.dir.path().join("after-power-loss.sqlite");
        std::fs::copy(harness.dir.path().join("steno.sqlite"), &after).unwrap();
        let store = Arc::new(Store::open(&after).unwrap());
        assert!(store.meeting(meeting_id).unwrap().is_some());
        assert!(store.asset(meeting_id).unwrap().is_some());
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
            filled_seconds: 0.0,
            unfilled_seconds: 0.0,
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

    /// Under half a second lost is no warning.
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

    /// Lost frames leave a `warn` line with the counts per lane.
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

    /// A stop whose recording lost frames, the writer stalled behind a
    /// one-frame relay, logs them.
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
        // The first write waits for `go`, so the backend overflows the relay.
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
        start(recorder);
        let meeting_id = recorder.status().meeting_id.unwrap();
        let finished = backend.clone();
        on_own_thread(HANG_GUARD, "the backend delivered", move || {
            finished.wait_until_finished();
        });
        go.send(()).unwrap();
        stop(recorder);
        let text = log.text();
        assert!(
            text.lines()
                .any(|line| line.contains(&meeting_id.to_string())
                    && line.contains("the recording lost frames")),
            "{text}"
        );
    }

    /// Waits until the recorder is idle after a recording that ended on its
    /// own, bounded only by [`HANG_GUARD`], and returns the saved meeting.
    async fn ended_on_its_own(harness: &Harness, meeting_id: Uuid) -> steno_core::Meeting {
        eventually_within(HANG_GUARD, "the recording ended on its own", || {
            harness.recorder.status().state == RecordingState::Idle
        })
        .await;
        harness.store.meeting(meeting_id).unwrap().unwrap()
    }

    /// A disk that fills mid-recording ends it at once: saved as `failed`,
    /// queued, and the status says why.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_writer_that_fails_mid_recording_ends_and_saves_the_recording() {
        writer_fails_mid_recording(false).await;
    }

    /// The same when the session has no thread to end it on: the watcher's
    /// tick reads the failed write and stops it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_failed_write_the_session_cannot_end_is_stopped_by_the_watcher() {
        writer_fails_mid_recording(true).await;
    }

    /// A recording whose writer fails after 30 frames ends on its own;
    /// `no_thread` refuses the session its ending thread.
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
        start(&harness.recorder);
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

    /// A device lost through every restart ends the recording with
    /// `deviceLost`, saved.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_device_that_stays_lost_ends_and_saves_the_recording() {
        ends_as_a_lost_device(|options| {
            options
                .change_device_after(0.2)
                .restarts_that_fail(CaptureSession::RESTART_ATTEMPTS)
        })
        .await;
    }

    /// A rebuild that panics ends the recording as a lost device does.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_rebuild_that_panics_ends_and_saves_the_recording() {
        ends_as_a_lost_device(|options| options.change_device_after(0.2).restart_panics()).await;
    }

    /// A recording as `device` changes it ends as a lost device, saved with
    /// `deviceLost`.
    async fn ends_as_a_lost_device(device: fn(SyntheticOptions) -> SyntheticOptions) {
        let harness = harness(&[]);
        harness.capture_with(failing_capture(None, device));
        start(&harness.recorder);
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

    /// A stop that panics part way leaves the recorder idle with an error
    /// and the row `recording` for the next launch; the next recording starts.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_stop_that_panics_leaves_the_recorder_idle_with_an_error() {
        let harness = harness(&[]);
        start(&harness.recorder);
        let meeting_id = harness.recorder.status().meeting_id.unwrap();
        // First audio: real capture work.
        eventually_within(HANG_GUARD, "frames reached the files", || {
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
        let support = harness.dir.path().join("support");
        assert_eq!(
            crate::audio_folders::recorded(&support).unwrap()[&meeting_id],
            audio_folder
        );
        start(&harness.recorder);
        stop(&harness.recorder);
    }

    /// A start that panics part way leaves the recorder idle with an error.
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
        start(&harness.recorder);
        stop(&harness.recorder);
    }

    /// A start that panics after its files exist fails the meeting, forgets
    /// its folder and leaves no folder.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_start_that_panics_after_the_meeting_began_fails_it_and_leaves_no_folder() {
        let harness = harness(&[]);
        harness.capture_with(failing_capture(
            None,
            SyntheticOptions::start_panics_once_running,
        ));
        let starter = harness.recorder.clone();
        let starting = std::thread::spawn(move || starter.start(CaptureMode::InPerson, None));
        assert!(starting.join().is_err(), "the start panicked");
        let status = harness.recorder.status();
        assert_eq!(status.state, RecordingState::Idle);
        assert_eq!(status.error.as_deref(), Some(START_PANICKED));
        let meetings = harness.store.all_meetings().unwrap();
        let [meeting] = meetings.as_slice() else {
            panic!("one meeting: {meetings:?}");
        };
        assert_eq!(
            meeting.state,
            steno_core::MeetingState::Failed {
                reason: START_PANICKED.to_owned()
            }
        );
        let audio_folder =
            steno_core::paths::file_url_path(&harness.store.settings().unwrap().audio_folder)
                .unwrap();
        assert!(
            !steno_core::RecordingLayout::new(&audio_folder, meeting.id)
                .directory
                .exists()
        );
        let support = harness.dir.path().join("support");
        assert!(crate::audio_folders::recorded(&support).unwrap().is_empty());
        harness.capture_with(synthetic_capture());
        start(&harness.recorder);
        stop(&harness.recorder);
    }

    /// A lost device cut the recording short whoever stopped it; a failed
    /// write says "stopped early" only when it ended the recording.
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

    /// A capture that does not start says why in plain words; the `warn`
    /// line names the kind only.
    #[test]
    fn a_capture_that_does_not_start_says_why_in_plain_words() {
        let log = steno_pipeline::fixtures::CapturedLog::warnings();
        let path = "/home/plain-words-test/Steno: Permission denied";
        for (error, reason) in [
            (
                CaptureError::InputDeviceUnavailable,
                "no microphone is available.",
            ),
            (
                CaptureError::UnsupportedSampleRate { actual: 384_000 },
                "the audio devices run at 384000 Hz, which Steno cannot record.",
            ),
            (
                CaptureError::WriterFailed(path.to_owned()),
                "Steno could not write to the recordings folder.",
            ),
            (
                CaptureError::BackendFailed(path.to_owned()),
                DEVICES_DID_NOT_OPEN,
            ),
        ] {
            assert_eq!(capture_refused(&error), reason, "{error:?}");
        }
        let text = log.text();
        assert!(text.contains("the recording could not start"), "{text}");
        assert!(!text.contains("plain-words-test"), "{text}");
    }

    /// Without room for the save, a recording does not start.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_recording_does_not_start_on_a_full_disk() {
        let harness = harness(&[]);
        harness
            .free
            .store(DiskWatch::STOP_BELOW_BYTES - 1, Ordering::SeqCst);
        try_start(&harness.recorder, CaptureMode::Call);
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

    /// With little room the recording warns, then stops and is saved before
    /// the disk fills.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_recording_warns_when_the_disk_runs_low_and_stops_before_it_fills() {
        let harness = harness(&[]);
        // Ten minutes of an in-person recording above the floor.
        let ten_minutes = 600 * (48_000 * 4 + 16_000 * 2);
        harness
            .free
            .store(DiskWatch::STOP_BELOW_BYTES + ten_minutes, Ordering::SeqCst);
        start(&harness.recorder);
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
        start(&harness.recorder);
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
        stop(&harness.recorder);
    }

    /// A dismissed disk warning returns only once less is left; a reading
    /// with room forgets the dismissal.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_dismissed_disk_warning_returns_only_with_less_room() {
        let harness = harness(&[]);
        let minutes = |minutes: u64| DiskWatch::STOP_BELOW_BYTES + minutes * 60 * 224_000;
        let free = Arc::new(AtomicU64::new(minutes(10)));
        let reads = Arc::new(AtomicUsize::new(0));
        harness.recorder.watch_disk_with({
            let (free, reads) = (free.clone(), reads.clone());
            disk_with(move |_| {
                reads.fetch_add(1, Ordering::SeqCst);
                Ok(volume(free.load(Ordering::SeqCst)))
            })
        });
        let warning = || harness.recorder.status().warning;
        let ticks = || async {
            let at = reads.load(Ordering::SeqCst);
            eventually("the watcher read the disk", || {
                reads.load(Ordering::SeqCst) > at + 5
            })
            .await;
        };
        start(&harness.recorder);
        assert_eq!(warning(), Some(low_space_warning(10)));
        harness.recorder.clear_messages();
        ticks().await;
        assert_eq!(warning(), None, "dismissed at 10 minutes");
        free.store(minutes(5), Ordering::SeqCst);
        eventually("the warning shows again with less room", || {
            warning() == Some(low_space_warning(5))
        })
        .await;
        harness.recorder.clear_messages();
        free.store(u64::MAX, Ordering::SeqCst);
        ticks().await;
        free.store(minutes(10), Ordering::SeqCst);
        eventually("a low room after room again warns afresh", || {
            warning() == Some(low_space_warning(10))
        })
        .await;
        stop(&harness.recorder);
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
        start(&harness.recorder);
        assert_eq!(
            harness.recorder.status().warning,
            Some(format!(
                "{} Recording from Built-in Audio. The microphone chosen in Settings is not \
                 available.",
                low_space_warning(1)
            ))
        );
        stop(&harness.recorder);
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
        start(&harness.recorder);
        eventually("the space was read while recording", || {
            reads.load(Ordering::SeqCst) > 2
        })
        .await;
        assert_eq!(harness.recorder.status().state, RecordingState::Recording);
        stop(&harness.recorder);
        assert_eq!(harness.recorder.status().error, None);
    }

    /// A volume that reports no size, or more free than it holds, is
    /// unreadable: the recording runs.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_volume_that_reports_no_size_lets_the_recording_run() {
        for reported in [
            Volume {
                total: 0,
                ..volume(0)
            },
            Volume {
                total: 1,
                ..volume(2)
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
            start(&harness.recorder);
            eventually("the space was read while recording", || {
                reads.load(Ordering::SeqCst) > 2
            })
            .await;
            let status = harness.recorder.status();
            assert_eq!(status.state, RecordingState::Recording);
            assert_eq!(status.warning, None);
            stop(&harness.recorder);
            assert_eq!(harness.recorder.status().error, None);
        }
    }

    /// The free space is the smaller of the two volumes'; unreadable ones
    /// are left out.
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

    /// The minutes count to the floor, and below it there is no room.
    #[test]
    fn the_room_left_counts_from_the_floor() {
        let rate = 448_000;
        let floor = DiskWatch::STOP_BELOW_BYTES;
        assert_eq!(Room::of(0, rate), Room::Full);
        assert_eq!(Room::of(floor - 1, rate), Room::Full);
        assert_eq!(Room::of(floor + 30 * 60 * rate, rate), Room::Enough);
        assert_eq!(
            Room::of(floor + 29 * 60 * rate, rate),
            Room::Low { minutes: 29 }
        );
        assert_eq!(Room::of(floor, rate), Room::Low { minutes: 1 });
    }

    /// The warning promises the stop.
    #[test]
    fn the_low_space_warning_promises_the_stop() {
        assert_eq!(
            low_space_warning(29),
            "The disk is almost full: about 29 minutes of recording left. \
             Steno stops and saves the recording before the disk fills."
        );
        assert_eq!(
            low_space_warning(1),
            "The disk is almost full: about 1 minute of recording left. \
             Steno stops and saves the recording before the disk fills."
        );
    }

    /// The system's reading of a folder's volume holds its free space; on
    /// the Mac it lies between `statvfs` and `statvfs` plus the floor gap.
    /// Up to five tries, as a busy disk moves between reads.
    #[test]
    fn the_system_reading_of_a_folder_s_volume_is_within_its_size() {
        let folder = tempfile::tempdir().unwrap();
        let watch = DiskWatch::system(None);
        let volume = (watch.free_space)(folder.path()).unwrap();
        assert!(volume.total > 0, "{volume:?}");
        assert!(volume.available <= volume.total, "{volume:?}");
        #[cfg(target_os = "macos")]
        {
            let gap = DiskWatch::STOP_BELOW_BYTES - DiskWatch::HARD_FLOOR_BYTES;
            let slack = 128 * MIB;
            let mut tries = Vec::new();
            let held = (0..5).any(|_| {
                let before = statvfs(folder.path()).unwrap().available;
                let reading = (watch.free_space)(folder.path()).unwrap().available;
                let important =
                    steno_macos::volume::available_for_important_usage(folder.path()).unwrap();
                let after = statvfs(folder.path()).unwrap().available;
                let (low, high) = (before.min(after), before.max(after));
                tries.push(format!(
                    "the reading {reading}, statvfs {before} then {after}, important use {important}"
                ));
                let purgeable = important.saturating_sub(high).min(gap);
                high - low <= slack
                    && reading + slack >= low + purgeable
                    && reading <= high + gap + slack
            });
            assert!(held, "{tries:#?}");
        }
    }

    /// A failing important-use read warns once per streak per folder, with
    /// the error's kind only.
    #[test]
    fn a_failing_important_use_read_warns_once_per_streak_without_the_folder() {
        let failing = Mutex::default();
        let folder = Path::new("/Users/someone/Recordings");
        let other = Path::new("/Volumes/External");
        let fails = |path: &Path| {
            std::io::Result::<u64>::Err(std::io::Error::other(format!(
                "the volume of {} could not be read",
                path.display()
            )))
        };
        let (readings, logged) = crate::testing::warnings(|| {
            [
                important_use(folder, fails, &failing),
                important_use(folder, fails, &failing),
                important_use(folder, |_| Ok(7), &failing),
                important_use(folder, fails, &failing),
            ]
        });
        assert_eq!(readings, [None, None, Some(7), None]);
        assert_eq!(logged.matches("could not be read").count(), 2, "{logged}");
        assert!(logged.contains("kind=Other"), "{logged}");
        assert!(!logged.contains("someone"), "{logged}");

        let (reading, logged) = crate::testing::warnings(|| important_use(other, fails, &failing));
        assert_eq!(reading, None);
        assert_eq!(logged.matches("could not be read").count(), 1, "{logged}");
        assert!(!logged.contains("External"), "{logged}");
    }

    const MIB: u64 = 1024 * 1024;
    const GB: u64 = 1_000_000_000;

    /// The Mac reads the larger of `statvfs` and the important-use space: a
    /// volume answering 0 (a disk image) records on `statvfs`, and purgeable
    /// space counts up to the floor gap.
    #[test]
    fn the_mac_reads_the_larger_of_statvfs_and_the_space_for_important_use() {
        let rate = 448_000;
        let gap = DiskWatch::STOP_BELOW_BYTES - DiskWatch::HARD_FLOOR_BYTES;
        // As a 1 GB APFS or HFS+ image reports itself on macOS 26.
        let image = Volume {
            available: 1_050_000_000,
            total: 1_070_000_000,
        };
        assert_eq!(mac_volume(image, Some(0)), image);
        assert_eq!(mac_volume(image, None), image);
        assert_eq!(Room::of(image.available, rate), Room::Low { minutes: 19 });
        assert_eq!(mac_volume(volume(GB), Some(GB + 7)), volume(GB + 7));
        assert_eq!(
            mac_volume(volume(400 * MIB), Some(5 * GB)),
            volume(400 * MIB + gap)
        );
        assert_eq!(
            mac_volume(volume(10 * GB), Some(50 * GB)),
            volume(10 * GB + gap)
        );
    }

    /// Below the hard floor on `statvfs` there is no room, whatever macOS
    /// may purge; above it the minutes count to the hard floor.
    #[test]
    fn the_mac_has_no_room_below_the_hard_floor_whatever_important_use_says() {
        let rate = 448_000;
        let room = |statvfs: u64, important: Option<u64>| {
            Room::of(mac_volume(volume(statvfs), important).available, rate)
        };
        let (floor, hard_floor) = (DiskWatch::STOP_BELOW_BYTES, DiskWatch::HARD_FLOOR_BYTES);
        assert_eq!(room(100 * MIB, Some(5 * GB)), Room::Full);
        assert_eq!(room(hard_floor - 1, Some(u64::MAX)), Room::Full);
        assert_eq!(room(hard_floor, Some(5 * GB)), Room::Low { minutes: 1 });
        assert_eq!(room(200 * MIB, Some(50 * GB)), Room::Low { minutes: 2 });
        assert_eq!(room(floor - 1, None), Room::Full);
        assert_eq!(room(floor - 1, Some(0)), Room::Full);
        assert_eq!(room(floor, None), Room::Low { minutes: 1 });
    }

    /// On the Mac's reading a recording neither starts nor goes on at a
    /// `statvfs` of 100 MiB, though 5 GB may be purged.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_mac_s_reading_refuses_and_stops_a_recording_below_its_floors() {
        for (low, important) in [
            (100 * MIB, Some(5 * GB)),
            (DiskWatch::STOP_BELOW_BYTES - 1, None),
        ] {
            let harness = harness(&[]);
            let statvfs = Arc::new(AtomicU64::new(low));
            harness.recorder.watch_disk_with({
                let statvfs = statvfs.clone();
                disk_with(move |_| {
                    Ok(mac_volume(
                        volume(statvfs.load(Ordering::SeqCst)),
                        important,
                    ))
                })
            });
            let status = status_after_a_start(&harness);
            assert_eq!(status.state, RecordingState::Idle, "{important:?}");
            assert_eq!(
                status.error,
                Some(format!("{COULD_NOT_START} {NO_ROOM_TO_START}"))
            );
            assert_eq!(harness.store.meetings(10, 0).unwrap(), []);

            statvfs.store(5 * GB, Ordering::SeqCst);
            start(&harness.recorder);
            let meeting_id = harness.recorder.status().meeting_id.unwrap();
            statvfs.store(low, Ordering::SeqCst);
            let meeting = ended_on_its_own(&harness, meeting_id).await;
            assert_eq!(
                harness.recorder.status().error.as_deref(),
                Some(STOPPED_FOR_SPACE)
            );
            assert_eq!(meeting.end_reason, Some(RecordingEndReason::Failed));
            assert!(harness.store.asset(meeting_id).unwrap().is_some());
        }
    }

    /// A 1 GB disk image, attached for a test and detached with it.
    #[cfg(target_os = "macos")]
    struct DiskImage {
        /// Holds the image and its mount point.
        _folder: tempfile::TempDir,
        mount_point: PathBuf,
    }

    #[cfg(target_os = "macos")]
    impl DiskImage {
        /// Creates an empty image of `file_system` and attaches it hidden.
        fn attach(file_system: &str) -> Self {
            let folder = tempfile::tempdir().unwrap();
            let image = folder.path().join("probe.dmg");
            let mount_point = folder.path().join("volume");
            std::fs::create_dir(&mount_point).unwrap();
            let run = |command: &mut std::process::Command| {
                let status = command
                    .stdout(std::process::Stdio::null())
                    .status()
                    .unwrap();
                assert!(status.success(), "{command:?}: {status}");
            };
            run(std::process::Command::new("hdiutil")
                .args([
                    "create",
                    "-size",
                    "1g",
                    "-fs",
                    file_system,
                    "-volname",
                    "StenoProbe",
                ])
                .arg(&image));
            run(std::process::Command::new("hdiutil")
                .args(["attach", "-nobrowse", "-mountpoint"])
                .arg(&mount_point)
                .arg(&image));
            Self {
                _folder: folder,
                mount_point,
            }
        }
    }

    #[cfg(target_os = "macos")]
    impl Drop for DiskImage {
        fn drop(&mut self) {
            let _ = std::process::Command::new("hdiutil")
                .args(["detach", "-force"])
                .arg(&self.mount_point)
                .stdout(std::process::Stdio::null())
                .status();
        }
    }

    /// A recording onto a disk image, which reports 0 bytes for important
    /// use, starts on `statvfs`'s reading. Ignored (it runs `hdiutil`); on a
    /// Mac run
    /// `cargo test -p steno-services --lib -- --ignored --nocapture
    /// a_recording_into_a_disk_image_starts`.
    #[cfg(target_os = "macos")]
    #[ignore = "attaches disk images with hdiutil"]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_recording_into_a_disk_image_starts() {
        for file_system in ["APFS", "HFS+"] {
            let image = DiskImage::attach(file_system);
            let volume = image.mount_point.as_path();
            let important = steno_macos::volume::available_for_important_usage(volume).unwrap();
            let total = steno_macos::volume::total_capacity(volume).unwrap();
            let statvfs = statvfs(volume).unwrap();
            let disk = DiskWatch::system(Some(volume));
            let reading = (disk.free_space)(volume).unwrap();
            eprintln!(
                "{file_system}: important use {} of {}, statvfs {} of {}, the disk watch {}",
                important, total, statvfs.available, statvfs.total, reading.available,
            );
            assert!(
                reading.available.abs_diff(statvfs.available) < 16 * MIB,
                "{reading:?} against {statvfs:?}"
            );
            assert_ne!(Room::of(reading.available, 448_000), Room::Full);

            let harness = harness(&[]);
            harness.recorder.watch_disk_with(disk);
            let mut settings = harness.store.settings().unwrap();
            settings.audio_folder = file_url(&volume.join("Audio"), true);
            harness.store.save_settings(&settings).unwrap();
            start(&harness.recorder);
            let meeting_id = harness.recorder.status().meeting_id.unwrap();
            stop(&harness.recorder);
            assert_eq!(harness.recorder.status().error, None, "{file_system}");
            assert!(harness.store.asset(meeting_id).unwrap().is_some());
        }
    }

    /// A low-space warning that arrives while stopping does not show.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_disk_warning_during_a_stop_does_not_show() {
        let harness = harness(&[]);
        start(&harness.recorder);
        let meeting_id = harness.recorder.status().meeting_id.unwrap();
        let stop_seen = held_in(&harness.recorder, RecordingState::Stopping);
        let stopper = harness.recorder.clone();
        let stopping = std::thread::spawn(move || stopper.stop());
        stop_seen.recv_timeout(PATIENCE).expect("the stop began");
        harness.recorder.warn(meeting_id, Some(5));
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

    /// The event "event-42" under way at the start, with an attendee and the
    /// calendar's owner.
    fn roadmap_sync() -> steno_core::CalendarEvent {
        let now = Utc::now();
        steno_core::CalendarEvent {
            id: "event-42".to_owned(),
            title: "Roadmap sync".to_owned(),
            start: now - chrono::TimeDelta::minutes(5),
            end: now + chrono::TimeDelta::minutes(30),
            attendees: vec![
                steno_core::CalendarAttendee {
                    name: "Jérôme".to_owned(),
                    email: Some("jerome@example.com".to_owned()),
                    is_current_user: false,
                },
                steno_core::CalendarAttendee {
                    name: "Nicolai".to_owned(),
                    email: Some("nicolai@example.com".to_owned()),
                    is_current_user: true,
                },
            ],
        }
    }

    /// Waits until `meeting_id` is named after [`roadmap_sync`] with its one
    /// attendee.
    async fn until_named_after_the_event(harness: &Harness, meeting_id: Uuid) {
        eventually("the event named the recording", || {
            harness
                .store
                .meeting(meeting_id)
                .unwrap()
                .is_some_and(|meeting| meeting.calendar_event_id.is_some())
        })
        .await;
        let meeting = harness.store.meeting(meeting_id).unwrap().unwrap();
        assert_eq!(meeting.title, "Roadmap sync");
        assert_eq!(meeting.title_origin, steno_core::TitleOrigin::Calendar);
        assert_eq!(meeting.calendar_event_id.as_deref(), Some("event-42"));
        let participants = harness.store.participants(meeting_id).unwrap();
        let names: Vec<_> = participants
            .iter()
            .map(|participant| participant.display_name.as_str())
            .collect();
        assert_eq!(names, ["Jérôme"], "the owner is not an attendee row");
        assert_eq!(participants[0].role, steno_core::ParticipantRole::Them);
        assert_eq!(participants[0].email.as_deref(), Some("jerome@example.com"));
    }

    /// The event under way names the meeting and its attendees but the owner
    /// become participants, whether the answer came before the row or after.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_event_under_way_names_the_recording_and_its_attendees() {
        let harness = harness(&[]);
        let calendar = crate::calendar::testing::FakeCalendar::with(vec![roadmap_sync()]);
        harness.recorder.name_meetings_from(calendar.clone());
        start(&harness.recorder);
        let meeting_id = harness.recorder.status().meeting_id.unwrap();
        until_named_after_the_event(&harness, meeting_id).await;
        stop(&harness.recorder);
        let stopped = harness.store.meeting(meeting_id).unwrap().unwrap();
        assert_eq!(stopped.title, "Roadmap sync", "the stop keeps the name");
        assert_eq!(calendar.reads(), 1);
    }

    /// A calendar that has not answered never holds the start up; its later
    /// answer names the recording.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_slow_calendar_never_holds_a_start_up() {
        let harness = harness(&[]);
        let calendar = crate::calendar::testing::FakeCalendar::with(vec![roadmap_sync()]);
        calendar.hold();
        harness.recorder.name_meetings_from(calendar.clone());
        // Returns although the calendar is held.
        start(&harness.recorder);
        assert!(calendar.wait_for_a_held_read(PATIENCE), "the lookup ran");
        let meeting_id = harness.recorder.status().meeting_id.unwrap();
        let meeting = harness.store.meeting(meeting_id).unwrap().unwrap();
        assert_eq!(meeting.title_origin, steno_core::TitleOrigin::Default);
        assert!(meeting.title.starts_with("Meeting "), "{}", meeting.title);
        assert_eq!(meeting.calendar_event_id, None);
        assert_eq!(harness.store.participants(meeting_id).unwrap(), Vec::new());

        calendar.release();
        until_named_after_the_event(&harness, meeting_id).await;
        stop(&harness.recorder);
    }

    /// A calendar still answering the previous start is not asked again
    /// until it answers.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_calendar_that_hangs_is_read_by_one_start_at_a_time() {
        let harness = harness(&[]);
        let calendar = crate::calendar::testing::FakeCalendar::with(vec![roadmap_sync()]);
        calendar.hold();
        harness.recorder.name_meetings_from(calendar.clone());
        start(&harness.recorder);
        assert!(calendar.wait_for_a_held_read(PATIENCE));
        let first = harness.recorder.status().meeting_id.unwrap();
        stop(&harness.recorder);
        start(&harness.recorder);
        let second = harness.recorder.status().meeting_id.unwrap();
        assert_eq!(calendar.reads(), 1, "the second start did not read");
        calendar.release();
        stop(&harness.recorder);
        for meeting_id in [first, second] {
            let meeting = harness.store.meeting(meeting_id).unwrap().unwrap();
            assert_eq!(meeting.calendar_event_id, None, "stopped before the answer");
        }
        eventually("the calendar's thread ended", || {
            harness.recorder.calendar.start(Utc::now()).is_some()
        })
        .await;
    }

    /// A failing or empty calendar gives the default title, no participants.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_failing_or_empty_calendar_starts_as_before() {
        let failing = crate::calendar::testing::FakeCalendar::with(vec![roadmap_sync()]);
        failing.fail();
        let empty = crate::calendar::testing::FakeCalendar::with(Vec::new());
        for calendar in [failing, empty] {
            let harness = harness(&[]);
            harness.recorder.name_meetings_from(calendar.clone());
            start(&harness.recorder);
            let meeting_id = harness.recorder.status().meeting_id.unwrap();
            eventually("the calendar was read", || calendar.reads() == 1).await;
            stop(&harness.recorder);
            let meeting = harness.store.meeting(meeting_id).unwrap().unwrap();
            assert_eq!(meeting.title_origin, steno_core::TitleOrigin::Default);
            assert_eq!(meeting.calendar_event_id, None);
            assert_eq!(harness.store.participants(meeting_id).unwrap(), Vec::new());
        }
    }

    /// Waits until the calendar handed `count` answers over.
    async fn until_answered(harness: &Harness, count: usize) {
        eventually("the late answer was handed over", || {
            harness.recorder.calendar.answered.load(Ordering::SeqCst) == count
        })
        .await;
    }

    /// A user who renamed the recording before the calendar answered keeps
    /// the title.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_late_answer_never_overwrites_a_users_title() {
        let harness = harness(&[]);
        let calendar = crate::calendar::testing::FakeCalendar::with(vec![roadmap_sync()]);
        calendar.hold();
        harness.recorder.name_meetings_from(calendar.clone());
        start(&harness.recorder);
        assert!(calendar.wait_for_a_held_read(PATIENCE));
        let meeting_id = harness.recorder.status().meeting_id.unwrap();
        harness
            .store
            .rename(meeting_id, "Typed", Utc::now())
            .unwrap();
        calendar.release();
        until_answered(&harness, 1).await;
        let meeting = harness.store.meeting(meeting_id).unwrap().unwrap();
        assert_eq!(meeting.title, "Typed");
        assert_eq!(meeting.title_origin, steno_core::TitleOrigin::User);
        assert_eq!(meeting.calendar_event_id, None);
        assert_eq!(harness.store.participants(meeting_id).unwrap(), Vec::new());
        stop(&harness.recorder);
        let stopped = harness.store.meeting(meeting_id).unwrap().unwrap();
        assert_eq!(stopped.title, "Typed", "the stop keeps the user's title");
    }

    /// A late answer names neither the stopped recording that asked nor the
    /// next one.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_late_answer_never_touches_another_meeting() {
        let harness = harness(&[]);
        let calendar = crate::calendar::testing::FakeCalendar::with(vec![roadmap_sync()]);
        calendar.hold();
        harness.recorder.name_meetings_from(calendar.clone());
        start(&harness.recorder);
        assert!(calendar.wait_for_a_held_read(PATIENCE));
        let first = harness.recorder.status().meeting_id.unwrap();
        stop(&harness.recorder);
        start(&harness.recorder);
        let second = harness.recorder.status().meeting_id.unwrap();
        assert_ne!(first, second);
        calendar.release();
        until_answered(&harness, 1).await;
        for meeting_id in [first, second] {
            let meeting = harness.store.meeting(meeting_id).unwrap().unwrap();
            assert_eq!(meeting.title_origin, steno_core::TitleOrigin::Default);
            assert_eq!(meeting.calendar_event_id, None);
            assert_eq!(harness.store.participants(meeting_id).unwrap(), Vec::new());
        }
        assert_eq!(
            harness.recorder.status().meeting_id,
            Some(second),
            "the newer one still records"
        );
        stop(&harness.recorder);
    }

    use steno_audio::capture::permission::ProbeAnswer;
    use steno_host::services::Preferences as _;

    /// What the fake system audio test saw: its runs, and the order of its
    /// tone's end and the capture sessions.
    #[derive(Default)]
    struct ToneLog {
        runs: AtomicUsize,
        events: Mutex<Vec<&'static str>>,
    }

    impl ToneLog {
        fn note(&self, event: &'static str) {
            self.events.lock().unwrap().push(event);
        }

        fn events(&self) -> Vec<&'static str> {
            self.events.lock().unwrap().clone()
        }
    }

    /// The Mac's permissions over `harness`'s recorder with the system audio
    /// test `probe`.
    fn tone_tested(
        harness: &Harness,
        probe: impl Fn(&steno_audio::clock::Cancel, &ToneLog) -> ProbeAnswer + Send + Sync + 'static,
    ) -> (
        Arc<crate::permissions::SystemPermissions>,
        Arc<ToneLog>,
        Arc<steno_host::fakes::FakePreferences>,
    ) {
        let log = Arc::new(ToneLog::default());
        let preferences = Arc::new(steno_host::fakes::FakePreferences::default());
        let probed = log.clone();
        let permissions = Arc::new(crate::permissions::SystemPermissions::with_probe(
            Arc::new(FakePermissions::all(steno_bridge::PermissionState::Unknown)),
            preferences.clone(),
            Arc::new(move |cancel| {
                probed.runs.fetch_add(1, Ordering::SeqCst);
                probe(cancel, &probed)
            }),
            steno_bridge::Platform::Macos,
        ));
        permissions.keep_out_of_recordings(&harness.recorder);
        (permissions, log, preferences)
    }

    /// While a recording starts or runs, a system audio test request plays
    /// nothing and answers the state as it was.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_tone_test_never_plays_while_a_recording_starts_or_runs() {
        let harness = harness(&[]);
        let (permissions, log, preferences) = tone_tested(&harness, |_, _| ProbeAnswer::Heard);

        // Starting: the capture is held until the request was answered.
        let (entered, entering) = std::sync::mpsc::channel::<()>();
        let (opened, open) = std::sync::mpsc::channel::<()>();
        let (entered, open) = (Mutex::new(entered), Mutex::new(open));
        let held = synthetic_capture();
        harness.capture_with(Arc::new(move |configuration| {
            entered.lock().unwrap().send(()).unwrap();
            open.lock().unwrap().recv_timeout(HANG_GUARD).unwrap();
            held(configuration)
        }));
        let starting = harness.recorder.clone();
        let start = std::thread::spawn(move || starting.start(CaptureMode::InPerson, None));
        entering.recv_timeout(HANG_GUARD).unwrap();
        assert_eq!(harness.recorder.status().state, RecordingState::Starting);
        assert_eq!(
            permissions.request(PermissionKind::SystemAudio),
            steno_bridge::PermissionState::Unknown
        );
        opened.send(()).unwrap();
        start.join().unwrap();
        assert_eq!(harness.recorder.status().state, RecordingState::Recording);

        // Recording, after an earlier test heard the tone.
        preferences.set_flag(crate::permissions::SYSTEM_AUDIO_GRANTED_KEY, true);
        assert_eq!(
            permissions.request(PermissionKind::SystemAudio),
            steno_bridge::PermissionState::Granted
        );
        assert_eq!(log.runs.load(Ordering::SeqCst), 0, "no tone played");
        assert!(preferences.flag(crate::permissions::SYSTEM_AUDIO_GRANTED_KEY));
        stop(&harness.recorder);

        // Idle again: the test runs.
        assert_eq!(
            permissions.request(PermissionKind::SystemAudio),
            steno_bridge::PermissionState::Granted
        );
        assert_eq!(log.runs.load(Ordering::SeqCst), 1);
    }

    /// A start during the system audio test stops it and opens its devices
    /// only once it returned; the stopped test stores no answer.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_start_stops_the_tone_test_before_it_opens_a_device() {
        let harness = harness(&[]);
        let (began, beginning) = std::sync::mpsc::channel::<()>();
        let began = Mutex::new(began);
        let (permissions, log, preferences) = tone_tested(&harness, move |cancel, log| {
            began.lock().unwrap().send(()).unwrap();
            // Plays until stopped.
            if cancel.sleep(HANG_GUARD) {
                return ProbeAnswer::Silent;
            }
            log.note("the tone stopped");
            ProbeAnswer::Stopped
        });
        let capture = synthetic_capture();
        let noted = log.clone();
        harness.capture_with(Arc::new(move |configuration| {
            noted.note("a device opened");
            capture(configuration)
        }));
        let requesting = permissions.clone();
        let request = std::thread::spawn(move || requesting.request(PermissionKind::SystemAudio));
        beginning.recv_timeout(PATIENCE).unwrap();

        start(&harness.recorder);
        assert_eq!(log.events(), ["the tone stopped", "a device opened"]);
        assert_eq!(
            request.join().unwrap(),
            steno_bridge::PermissionState::Unknown
        );
        assert_eq!(
            preferences.stored_flag(crate::permissions::SYSTEM_AUDIO_GRANTED_KEY),
            None,
            "nothing found, nothing stored"
        );
        stop(&harness.recorder);
    }

    /// A test that does not stop holds a start up for
    /// [`TONE_TEST_PATIENCE`] at most.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_tone_test_that_does_not_stop_never_keeps_a_recording_from_starting() {
        let harness = harness(&[]);
        harness.recorder.inner().tone_tests.patience = Duration::ZERO;
        let (began, beginning) = std::sync::mpsc::channel::<()>();
        let (release, released) = std::sync::mpsc::channel::<()>();
        let (began, released) = (Mutex::new(began), Mutex::new(released));
        let (permissions, _, _) = tone_tested(&harness, move |_, _| {
            began.lock().unwrap().send(()).unwrap();
            released.lock().unwrap().recv_timeout(HANG_GUARD).unwrap();
            ProbeAnswer::Stopped
        });
        let requesting = permissions.clone();
        let request = std::thread::spawn(move || requesting.request(PermissionKind::SystemAudio));
        beginning.recv_timeout(PATIENCE).unwrap();
        start(&harness.recorder);
        release.send(()).unwrap();
        request.join().unwrap();
        stop(&harness.recorder);
    }

    /// A start waits for a slow-stopping tone test to return before opening
    /// a device, within the patience.
    fn waits_for_a_slow_stopping_tone_test(patience: Option<Duration>) {
        let harness = harness(&[]);
        if let Some(patience) = patience {
            harness.recorder.inner().tone_tests.patience = patience;
        }
        let (began, beginning) = std::sync::mpsc::channel::<()>();
        let (opened, open) = std::sync::mpsc::channel::<()>();
        let (began, open) = (Mutex::new(began), Mutex::new(open));
        let (permissions, log, _) = tone_tested(&harness, move |cancel, log| {
            began.lock().unwrap().send(()).unwrap();
            if cancel.sleep(HANG_GUARD) {
                return ProbeAnswer::Silent;
            }
            // Slow to stop: a device that opens now opens over the tone.
            let _ = open.lock().unwrap().recv_timeout(Duration::from_secs(1));
            log.note("the tone stopped");
            ProbeAnswer::Stopped
        });
        let capture = synthetic_capture();
        let noted = log.clone();
        let opened = Mutex::new(opened);
        harness.capture_with(Arc::new(move |configuration| {
            noted.note("a device opened");
            let _ = opened.lock().unwrap().send(());
            capture(configuration)
        }));
        let requesting = permissions.clone();
        let request = std::thread::spawn(move || requesting.request(PermissionKind::SystemAudio));
        beginning.recv_timeout(PATIENCE).unwrap();
        start(&harness.recorder);
        assert_eq!(log.events(), ["the tone stopped", "a device opened"]);
        request.join().unwrap();
        stop(&harness.recorder);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_start_waits_for_a_slow_stopping_tone_test() {
        waits_for_a_slow_stopping_tone_test(Some(HANG_GUARD));
    }

    /// The same under [`TONE_TEST_PATIENCE`], the product's bound.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_start_waits_for_a_slow_stopping_tone_test_within_the_patience() {
        waits_for_a_slow_stopping_tone_test(None);
    }

    /// A request while the recording stops plays nothing.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_tone_test_never_plays_while_a_recording_stops() {
        let harness = harness(&[]);
        let (permissions, log, _preferences) = tone_tested(&harness, |_, _| ProbeAnswer::Heard);
        start(&harness.recorder);
        let (reached, seen) = std::sync::mpsc::channel::<()>();
        let (go, gate) = std::sync::mpsc::channel::<()>();
        let (reached, gate) = (Mutex::new(reached), Mutex::new(gate));
        let watched = Arc::downgrade(&harness.recorder);
        harness.recorder.on_change(Arc::new(move || {
            if watched
                .upgrade()
                .is_some_and(|recorder| recorder.status().state == RecordingState::Stopping)
            {
                let _ = reached.lock().unwrap().send(());
                let _ = gate.lock().unwrap().recv_timeout(HANG_GUARD);
            }
        }));
        let stopper = harness.recorder.clone();
        let stopping = std::thread::spawn(move || stopper.stop());
        seen.recv_timeout(HANG_GUARD).expect("the stop began");
        assert_eq!(harness.recorder.status().state, RecordingState::Stopping);
        assert_eq!(
            permissions.request(PermissionKind::SystemAudio),
            steno_bridge::PermissionState::Unknown
        );
        assert_eq!(log.runs.load(Ordering::SeqCst), 0, "no tone while stopping");
        drop(go);
        stopping.join().unwrap();
    }

    /// A test after one a start stopped runs again; one asked while quitting
    /// never runs.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_test_after_a_stopped_one_runs_again_and_none_while_quitting() {
        let harness = harness(&[]);
        let (began, beginning) = std::sync::mpsc::channel::<()>();
        let began = Mutex::new(began);
        let calls = AtomicUsize::new(0);
        let (permissions, log, _preferences) = tone_tested(&harness, move |cancel, _| {
            if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                began.lock().unwrap().send(()).unwrap();
                if cancel.sleep(HANG_GUARD) {
                    return ProbeAnswer::Silent;
                }
                return ProbeAnswer::Stopped;
            }
            if cancel.is_cancelled() {
                ProbeAnswer::Stopped
            } else {
                ProbeAnswer::Heard
            }
        });
        let requesting = permissions.clone();
        let request = std::thread::spawn(move || requesting.request(PermissionKind::SystemAudio));
        beginning.recv_timeout(PATIENCE).unwrap();
        start(&harness.recorder);
        assert_eq!(
            request.join().unwrap(),
            steno_bridge::PermissionState::Unknown
        );
        stop(&harness.recorder);
        assert_eq!(
            permissions.request(PermissionKind::SystemAudio),
            steno_bridge::PermissionState::Granted,
            "the next test runs with a fresh cancel"
        );
        assert_eq!(log.runs.load(Ordering::SeqCst), 2);
        quit(&harness.recorder);
        assert_eq!(
            permissions.request(PermissionKind::SystemAudio),
            steno_bridge::PermissionState::Granted
        );
        assert_eq!(log.runs.load(Ordering::SeqCst), 2, "no test while quitting");
    }

    /// After the menu's Quit, a system audio test is refused too.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_tone_test_after_a_menu_quit_is_refused() {
        let harness = harness(&[]);
        let (permissions, log, _preferences) = tone_tested(&harness, |_, _| ProbeAnswer::Heard);
        harness.recorder.refuse_starts();
        assert_eq!(
            permissions.request(PermissionKind::SystemAudio),
            steno_bridge::PermissionState::Unknown
        );
        assert_eq!(log.runs.load(Ordering::SeqCst), 0, "no tone after a Quit");
    }

    /// A system audio test under way at a menu Quit or the shutdown is
    /// stopped at once, not waited for.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_tone_test_under_way_stops_at_a_menu_quit_and_at_the_shutdown() {
        for (quits, by) in [
            (
                Box::new(|recorder: &Arc<CaptureRecorder>| {
                    let refusing = recorder.clone();
                    on_own_thread(HANG_GUARD, "the refusal returned", move || {
                        refusing.refuse_starts();
                    });
                }) as Box<dyn Fn(&Arc<CaptureRecorder>)>,
                "the menu's Quit",
            ),
            (
                Box::new(quit) as Box<dyn Fn(&Arc<CaptureRecorder>)>,
                "the shutdown",
            ),
        ] {
            let harness = harness(&[]);
            let (began, beginning) = std::sync::mpsc::channel::<()>();
            let began = Mutex::new(began);
            let (permissions, log, _preferences) = tone_tested(&harness, move |cancel, log| {
                began.lock().unwrap().send(()).unwrap();
                if cancel.sleep(HANG_GUARD) {
                    return ProbeAnswer::Silent;
                }
                log.note("the tone stopped");
                ProbeAnswer::Stopped
            });
            let (answered, answer) = std::sync::mpsc::channel();
            let requesting = permissions.clone();
            std::thread::spawn(move || {
                let _ = answered.send(requesting.request(PermissionKind::SystemAudio));
            });
            beginning.recv_timeout(PATIENCE).unwrap();
            quits(&harness.recorder);
            assert_eq!(
                answer.recv_timeout(PATIENCE).expect(by),
                steno_bridge::PermissionState::Unknown,
                "{by}"
            );
            assert_eq!(log.events(), ["the tone stopped"], "{by}");
        }
    }
}
