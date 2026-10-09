//! The state machine over a [`CaptureBackend`].
//! Swift: `Sources/StenoAudio/Capture/CaptureSession.swift`.
//!
//! `Idle → Starting → Recording → Stopping → Idle`, or `Failed` when a
//! device stays lost, a write or the close fails, or a sync failed while
//! recording. Owns the sink, the processing thread, the relay, the writer
//! thread and the [`RecordingWriting`] implementation; `stop()` tears them
//! down in order (an in-flight rebuild, the backend, processing, writer,
//! files; see Threads) and returns the [`CaptureResult`]: the finished
//! [`AudioAsset`] (`Caf48kFloat32`, `sidecars_16k` filled, retention
//! `KeepForever` until the caller sets it from `Settings`) with statistics.
//!
//! No in-app playback while recording, enforced by [`Playback`]: `start`
//! takes the process's gate ([`Playback::global`]) before the backend
//! starts, which stops any playback that runs and refuses new playback,
//! and the recording keeps that hold across every rebuild until its
//! teardown is done. On macOS the call capture's tap includes Steno's own
//! process, so anything Steno played would land in the system lane.
//!
//! A device change while recording does not end the recording. The
//! backend reports it through the sink; the session stops the backend and
//! the processing thread, starts the backend again on the devices as they
//! are now (backing off on the clock between tries, as often as below),
//! fills the gap with silence through the relay so the master stays on
//! wall time, starts a processing thread built for the new latencies, and
//! keeps the sink, the relay, the writer thread, the writer and the files.
//! The state stays `Recording`; `notices` carries `DeviceChanged`,
//! `StillRestarting` and `Delivering` (below) and `DeviceResumed`. From the
//! old stream's stop until the restarted one delivers, the levels read
//! silence.
//!
//! A chosen microphone that is gone fails no restart: the live backends
//! then record the default input and say so in [`CaptureStream::input`],
//! which [`CaptureSession::stream`] hands out (see
//! [`CaptureBackend::start`]). One that is connected but does not open is
//! the session's to replace: when the start, or a rebuild's last restart,
//! fails on it, the session starts the backend once more without a UID and
//! marks that input as the fallback, so the recording ends only when the
//! default input cannot be opened either (Rust only: Swift fails the
//! start, and ends the recording after its restarts). A rebuild tries the
//! default already after its first failed restart when the stream it
//! replaces was on the fallback, or after the first restart on which the
//! graph did not run on the chosen microphone ([`CaptureError::DidNotRun`]),
//! not after the last: the gap then stays under
//! [`CaptureSession::MAXIMUM_GAP`] when that is the first restart, so the
//! restarts take nothing from the master.
//!
//! A rebuild ends the recording in `Failed(DeviceLost)` only when its
//! [`CaptureSession::RESTART_ATTEMPTS`] restarts, and the default after
//! the last, fail in a way that will not pass, or when it panics. A last
//! restart that fails with `DidNotRun` (on the default too, when there is
//! a chosen microphone to fall back from), and any last restart of a
//! rebuild that followed [`DeviceChangeReason::DeliveryStalled`] or
//! [`DeviceChangeReason::AudioServiceRestarted`] (`coreaudiod` restarted,
//! or the connection to the PipeWire daemon lost), does not end it: the
//! devices are there, or were a moment ago, and the graph may run again (a
//! source whose owner stalled and resumes, a driver that hangs for a
//! while, the audio service coming back), so the restarts go on, each with
//! the default after it, until one runs or `stop()` (Rust only). They back
//! off as [`CaptureSession::restart_backoff`] says: `RESTART_BACKOFF`
//! between the first `RESTART_ATTEMPTS`, then 2 s, then every
//! [`CaptureSession::RESTART_BACKOFF_LONGEST`] (4 s), with no limit. Once
//! the devices are back, that costs the start in flight (up to 3 s on
//! Linux), the wait and the first frame's arrival: about 8 s of audio at
//! worst, filled with silence. `notices` carries `StillRestarting` once
//! they pass `RESTART_ATTEMPTS`, for a warning, and `Delivering` once audio
//! arrives after it. Over a watched backend (below) whose streams do not
//! wait for playback, a restart that starts but whose stream offers no
//! frame within [`CaptureSession::STALL_TIMEOUT`] is stopped and counts as
//! one that did not run (on macOS and Windows a device that delivers
//! nothing still starts); its gap runs to the first frame.
//!
//! A stream a rebuild resumed on that stalls again within 10 s, or before
//! it delivered anything, continues that rebuild's streak: the next
//! rebuild counts on from its restarts, waits the backoff step it reached
//! and keeps its warning, so a stream that resumes and stalls over and
//! over backs off as restarts that fail do. The known case is a Mac call
//! capture whose output has no other client: its restarts start and wait
//! for playback (below), so until call mode runs from its start (A10,
//! #251) it resumes and stalls about every 6 s once the backoff is at its
//! longest. The log says when a streak's first restart fails and then
//! about once a minute, with the count; meanwhile the lines a try would
//! log, the session's and the backends', go to `debug` (`start_log`).
//!
//! A device that stops delivering without any notification (a driver or a
//! source's owner that hangs, a graph that stops running) is caught by the
//! session itself, on every platform: over a backend that delivers
//! continuously ([`CaptureBackend::delivers_continuously`]) a watch thread
//! samples the sink's count of frames offered every
//! [`CaptureSession::STALL_CHECK_INTERVAL`] on the clock, and a stream that
//! delivered and then offered nothing for longer than
//! [`CaptureSession::STALL_TIMEOUT`] is reported as
//! [`DeviceChangeReason::DeliveryStalled`], so the rebuild above takes over.
//! Silence, zeros included, is delivery. The gap of every rebuild then
//! runs from the last frame that thread saw arrive, not from the report, so
//! the time a backend spends coalescing a change is not lost from the
//! master either.
//!
//! Over a backend that [`CaptureBackend::waits_for_playback`] (every Mac
//! call capture: the backend cannot tell whether it has the capture
//! permission, without which its IOProc runs only while something plays)
//! a stream that delivers nothing is not stalled before the recording's
//! first frame, so it is not rebuilt over and over before anything
//! played; after it, a stream that stops is rebuilt, but a restart is not
//! held to deliver at once (the streak above backs it off) and a chosen
//! microphone is not given up for stalling, since the stall may only mean
//! playback stopped.
//! Elsewhere a chosen microphone that stalls again within 10 s of the
//! rebuild that resumed on it is replaced by the default input at the next
//! rebuild, rather than restarted to stall again.
//!
//! A chosen microphone that does not open, which the session replaced
//! with the default input, is asked for again in one of two ways, and
//! never by touching the stream that records. Over a backend that can ask
//! a microphone on a stream of its own ([`CaptureBackend::probes_inputs`]:
//! PipeWire, Bluetooth sources excepted) the watch thread asks once the
//! recording has run [`CaptureSession::CHOSEN_INPUT_RECHECK`] on the
//! default, and then twice as long after each ask, up to
//! [`CaptureSession::CHOSEN_INPUT_RECHECK_LONGEST`]; only when the chosen
//! one delivered there does it report
//! [`DeviceChangeReason::ChosenInputRecheck`], whose rebuild returns to it
//! as a device change would. An ask that finds nothing costs the
//! recording nothing. Over the Mac's and Windows' backends, which cannot,
//! it is asked for only by the rebuild of the next device change. All Rust
//! only: Swift had no watchdog.
//!
//! A recording cut short (device loss, a failed write) is finalised and
//! travels in the state: `Failed { error, recording }`. So does the whole
//! recording when the close failed (a full disk while closing) or, with
//! nothing else ending it, a sync failed while recording. `stop()` returns
//! the same result, or fails when the failure left no recording.
//!
//! A writer failure and a device loss finalise on their own threads (the
//! writer failure's and the rebuild's) and hold `Stopping` meanwhile. A
//! `stop()` that finds `Stopping` (one of those finalises, or another
//! `stop()`) waits on a condition variable until the state leaves
//! `Stopping`, then answers from the outcome: the recording `Failed`
//! carries, or `InvalidState` once the state is `Idle`. Swift's actor ran
//! the queued stop right after the finalise; here a `start()` can take the
//! lock first, and the stop then still answers with the recording that
//! finalise produced, as in Swift, and leaves the new recording running
//! (`InvalidState` only when the finalise left none, or was another
//! `stop()` that ended `Idle`). Only `stop()` waits there; a finaliser
//! never waits for a stopper, so the wait cannot deadlock, and a finaliser
//! that panics leaves `Failed` with no recording rather than `Stopping` for
//! good.
//!
//! # Threads
//!
//! Swift's actor becomes one mutex over the session state. The rule that
//! keeps it deadlock-free: **no thread is joined while the mutex is held**
//! (one bounded exception on Linux, below). The backend's producer, the
//! processing thread and the writer thread all call back into the session
//! (device changes, levels, write errors) and take the mutex to do so;
//! teardown therefore moves the recording out of the state under the
//! lock, releases it, and only then stops the threads.
//! The watch thread takes the mutex for each sample and reports through
//! the sink with it released; `finish()` raises its token and joins it,
//! lock released, before anything else. A probe it started runs on a
//! thread of its own, never with the mutex held, and is left to end on
//! its own (within the backend's start deadline) when the recording ends.
//! The rebuild runs on its own thread, takes the mutex for each step that
//! touches the state and sleeps outside it on the injected [`Clock`], with
//! a [`Cancel`] token `stop()` raises. Teardown raises that token and then
//! joins the rebuild thread, lock released, before it stops the backend: the
//! rebuild may be inside its own `backend.stop()` with the IOProc and the
//! old processing thread still running, or may just have started the
//! backend again, and the rings are cleared only once both are over. So
//! `stop()` blocks for as long as the rebuild's current step takes: its
//! `backend.stop()` of the old devices and the old processing thread's
//! stop (HAL teardown, up to hundreds of milliseconds, unbounded if the HAL
//! hangs), or a `backend.start()` and its settle (below); a restarted
//! stream's wait for its first frame gives up at once. When every
//! restart failed, the rebuild finalises the recording itself and first
//! takes its own handle out of the state, so `finish()` never joins the
//! thread it runs on.
//!
//! A stop that cuts a rebuild short loses nothing from the statistics. The
//! join returns the system-lane peak of the processing thread the rebuild
//! stopped, plus any gap silence no `resume` counted. `finish()` reads the
//! ring overruns before it clears the rings, and counts the whole frames
//! still in them (audio a restarted backend delivered before any processing
//! thread ran) as dropped on every lane.
//!
//! The one long hold is deliberate: `start` and the rebuild's `try_start`
//! keep the mutex across `backend.start()`: on macOS up to 200 ms while
//! [`NominalSampleRate::settle`](super::NominalSampleRate::settle) waits
//! for the aggregate, on Linux until PipeWire runs the first cycle (1 to
//! 2 s for a Bluetooth sink; none within 3 s fails the start), on Windows
//! until both streams have opened and started (process loopback's
//! activation included; 10 s in all at most, then the start fails). A
//! `stop()` arriving meanwhile queues behind it and then finds a started
//! backend to tear down, instead of racing a half-built one; a backend
//! never calls back into the session from `start`, so the hold cannot
//! deadlock. On Linux a failed start closes the capture's gate and joins
//! the PipeWire thread (2 s at most) with the mutex held, the one join
//! under it; that is safe because no device-change report can begin
//! before `start` took the thread's answer, so nothing that thread does
//! waits on the mutex. It can stall, though: every caller, `state()`
//! included, waits as long as `backend.start()` takes, so a HAL call that
//! hangs there freezes the session's callers with it. Restarts that go on
//! until one runs hold it for much of the time: on Linux, while a stopped
//! source's owner holds the graph up, each attempt waits out the start
//! deadline on the chosen microphone and again on the default (3 s each,
//! 7.5 s for a thread that does not answer), up to 4 s apart, each start
//! a hold of its own, so `state()`, `stream()` and `stop()` wait up to
//! one start's deadline at a time. A restarted stream's wait for its first
//! frame (`STALL_TIMEOUT` at most) and the backoff hold nothing, and
//! `stop()` cancels both.

use std::collections::BTreeMap;
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, Weak};
use std::thread::JoinHandle;
use std::time::Duration;

use chrono::Utc;
use steno_core::paths::file_url;
use steno_core::{
    AudioAsset, AudioFormat, AudioLane, AudioRetention, EchoCanceller, RecordingLayout,
};
use uuid::Uuid;

use super::backend::{CaptureBackend, CaptureStream};
use super::configuration::{
    CaptureConfiguration, CaptureError, CaptureNotice, CaptureResult, CaptureState,
    CaptureStatistics, DeviceChangeReason, LaneLevel, LaneLevels,
};
use super::start_log::{self, start_log};
use crate::aec::SpeexEchoCanceller;
use crate::clock::{Cancel, Clock, SystemClock};
use crate::playback::{Playback, RecordingHold};
use crate::realtime::{
    FrameRelay, LaneFrameSink, LevelSlot, ProcessingConfiguration, ProcessingThread, RateConverter,
};
use crate::writer::{RecordingWriter, RecordingWriting, WriterThread};
use crate::{FRAME_SIZE, FRAMES_PER_SECOND, SAMPLE_RATE};

/// Opens the files for one recording; [`RecordingWriter::new`] in
/// production, a failure-injecting wrapper in tests.
pub type RecordingWriterFactory = Arc<
    dyn Fn(&RecordingLayout, &[AudioLane], bool) -> Result<Box<dyn RecordingWriting>, CaptureError>
        + Send
        + Sync,
>;

/// A recording session over one backend; see the module doc. Tests run
/// the same pipeline over a synthetic backend and a manual clock through
/// [`CaptureSession::with_backend`].
///
/// ```no_run
/// use steno_audio::{CaptureConfiguration, CaptureMode, CaptureSession};
///
/// let configuration = CaptureConfiguration::new(CaptureMode::Call, "/tmp/steno-audio");
/// let session = CaptureSession::new(configuration)?;
/// session.start(uuid::Uuid::new_v4())?;
/// std::thread::sleep(std::time::Duration::from_secs(2));
/// let result = session.stop()?;
/// println!("{:.1} s at {}", result.statistics.duration, result.asset.url);
/// # Ok::<(), steno_audio::CaptureError>(())
/// ```
pub struct CaptureSession {
    core: Arc<Core>,
}

struct Core {
    configuration: CaptureConfiguration,
    writer_headroom_frames: usize,
    backend: Arc<dyn CaptureBackend>,
    clock: Arc<dyn Clock>,
    make_writer: RecordingWriterFactory,
    /// Held from before the backend starts until the recording's teardown
    /// is done ([`Active::_recording_hold`]): no in-app playback while
    /// recording.
    playback: Playback,
    inner: Mutex<Inner>,
    /// Notified on every state change; `stop()` waits on it while another
    /// thread's finalise holds `Stopping`.
    state_changed: Condvar,
    /// For tests ([`CaptureSession::refuse_the_writer_failure_thread`]):
    /// a failed write finds no thread to end the recording on.
    refuse_writer_failure_thread: AtomicBool,
}

struct Inner {
    state: CaptureState,
    state_subscribers: Vec<Sender<CaptureState>>,
    level_subscribers: Vec<Sender<LaneLevels>>,
    notice_subscribers: Vec<Sender<CaptureNotice>>,
    latest_levels: Option<LaneLevels>,
    /// The one canceller every recording of the session shares, held here
    /// while no processing thread runs it.
    echo_canceller: Option<Box<dyn EchoCanceller>>,
    active: Option<Active>,
    /// Bumped per rebuild and never reset, so a rebuild thread that wakes
    /// after a stop and a new start does nothing.
    rebuild_generation: usize,
    /// Bumped by every `start` and never reset, so a `stop()` that waited
    /// and a writer failure that arrives late act on their own recording
    /// only, never on one started meanwhile.
    recordings_started: usize,
    /// The newest recording a `Failed` state carried, with the start it
    /// belongs to: the answer of a `stop()` that waited out its finalise
    /// while a `start()` took the lock first.
    finalised: Option<(usize, Box<CaptureResult>)>,
    /// Unit tests only: taken by the next `stop()` that finds `Stopping`
    /// and run once it has noted `recordings_started`, with the lock
    /// released, so a test can end the finalise and run a `start()` before
    /// that `stop()` takes the lock back. The tests built on it prove the
    /// guard after the wait, not the condition-variable wait itself.
    #[cfg(test)]
    before_stop_waits: Option<Box<dyn FnOnce() + Send>>,
}

struct Active {
    meeting_id: Uuid,
    stream: CaptureStream,
    sink: Arc<LaneFrameSink>,
    relay: Arc<FrameRelay>,
    /// `None` only between a rebuild taking the old thread and installing
    /// the new one.
    processing: Option<ProcessingThread>,
    writer_thread: Option<WriterThread>,
    levels: Arc<LevelSlot>,
    ended_on_device_loss: bool,
    /// Rebuilds that succeeded.
    device_changes: usize,
    /// Silence written across those rebuilds.
    gap_seconds: f64,
    /// The loudest system-lane sample over every processing thread
    /// replaced so far; `finish()` takes the maximum with the current one.
    system_peak_so_far: f32,
    /// The rebuild in flight, so `stop()` can abandon it and wait for it.
    rebuild: Option<Rebuild>,
    /// A change reported while that rebuild ran (the rebuilt backend's
    /// listeners are live before the gap is written); `resume` starts the
    /// next rebuild from it instead of losing it.
    pending_change: Option<DeviceChangeReason>,
    /// The ring overruns of the streams a rebuild has stopped, each counted
    /// at its own rate.
    ring_drops: RingDrops,
    /// The watch thread, over a backend that delivers continuously.
    watch: Option<Watch>,
    /// The recording's hold on [`Playback`], taken before the backend
    /// started, kept across rebuilds and dropped with `Active` at the end
    /// of `finish()`, after `backend.stop()`. `Some` from `start` on.
    _recording_hold: Option<RecordingHold>,
    /// The restarts of the latest rebuilds, counted as one run while the
    /// stream they resume on stalls again soon.
    streak: Streak,
}

/// The restarts of a rebuild, and of the rebuilds after it while the
/// stream each resumed on stalls again soon (`Core::restart_plan`): one
/// count, one backoff and one warning across them, so a stream that
/// resumes and stalls over and over backs off as restarts that fail do.
/// Rust only.
#[derive(Debug, Default)]
struct Streak {
    /// Restarts so far, the one the latest rebuild resumed on included.
    attempts: usize,
    /// `StillRestarting` went out and no `Delivering` since: the warning
    /// stands, across rebuilds, until audio arrives.
    warned: bool,
    /// When the streak's latest log line went out, on the clock; `None`
    /// until its first restart failed.
    logged_at: Option<Duration>,
}

/// Ring overruns in samples at [`SAMPLE_RATE`], per lane. The sink counts
/// them at the device's rate and keeps counting across a rebuild, so each
/// stream's share is rescaled at that stream's rate once it has stopped.
#[derive(Default)]
struct RingDrops {
    /// The sink's counts at the last fold.
    seen: BTreeMap<AudioLane, usize>,
    /// What every fold so far stands for at [`SAMPLE_RATE`].
    at_output_rate: BTreeMap<AudioLane, usize>,
}

impl RingDrops {
    /// Adds the overruns in `counts` since the last fold, counted at `rate`.
    fn fold(&mut self, counts: BTreeMap<AudioLane, usize>, rate: f64) {
        for (lane, count) in counts {
            let seen = self.seen.entry(lane).or_default();
            *self.at_output_rate.entry(lane).or_default() +=
                CaptureStream::rescaled(count.saturating_sub(*seen), rate, SAMPLE_RATE);
            *seen = count;
        }
    }
}

/// The watch thread of one recording, the token that ends it, and what it
/// keeps under the lock.
struct Watch {
    cancel: Cancel,
    thread: JoinHandle<()>,
    /// What the thread last saw of the sink.
    progress: Progress,
    /// Whether the recording may deliver nothing at all until something
    /// plays ([`CaptureBackend::waits_for_playback`]): until its first
    /// frame, a stream that delivers nothing is then not stalled.
    waits_for_playback: bool,
    /// Whether the backend asks a microphone on a stream of its own
    /// ([`CaptureBackend::probes_inputs`]): only then does the thread ask
    /// for a chosen microphone the session replaced on a timer.
    probes: bool,
    /// When to ask whether the chosen microphone delivers again, while the
    /// stream is the default input the session put in its place.
    recheck: Option<Duration>,
    /// The wait before the latest ask, kept for the whole recording.
    asked_after: Option<Duration>,
    /// When the latest rebuild resumed.
    resumed_at: Option<Duration>,
}

impl Watch {
    /// The next ask from `now`: after `CHOSEN_INPUT_RECHECK` the first
    /// time, twice the latest wait every time after, up to
    /// `CHOSEN_INPUT_RECHECK_LONGEST`. The wait keeps growing across a
    /// return to the chosen microphone, so one that delivers to a probe
    /// and then stalls again is asked for less and less often.
    fn ask_again(&mut self, now: Duration) {
        let wait = self
            .asked_after
            .map_or(CaptureSession::CHOSEN_INPUT_RECHECK, |wait| {
                (wait * 2).min(CaptureSession::CHOSEN_INPUT_RECHECK_LONGEST)
            });
        self.asked_after = Some(wait);
        self.recheck = Some(now + wait);
    }
}

/// The sink's [`LaneFrameSink::frames_offered`] as the watch thread saw it.
#[derive(Debug, Clone, Copy)]
struct Progress {
    /// The count last seen, and when it was first seen at that value: no
    /// frame reached the sink after that moment while the count stays.
    count: usize,
    at: Duration,
}

/// A rebuild thread and the token that abandons it.
struct Rebuild {
    cancel: Cancel,
    /// Returns the silence frames the rebuild wrote that no `resume`
    /// accounted, because a stop overtook it, the system-lane peak of the
    /// processing thread it stopped, which `finish()` then folds in, and
    /// the rate of a restarted stream no `resume` took, which the rings
    /// then carry.
    thread: JoinHandle<(usize, f32, Option<f64>)>,
}

/// A stream a `start` returned, and whether it is the default input the
/// session put in place of the chosen microphone (`start_on_the_default`).
struct Started {
    stream: CaptureStream,
    on_the_default: bool,
}

enum Restart {
    Started {
        started: Started,
        /// The restart that started it, 1 for the first.
        attempt: usize,
        /// When its first frame arrived, as far as the wait for it saw
        /// (else when the start returned), on the clock: the gap runs to
        /// it.
        at: Duration,
    },
    Abandoned,
    Exhausted,
}

/// How a rebuild's restarts go (`restart_backend`).
#[derive(Debug, Clone, Copy)]
struct RestartPlan {
    /// The stream replaced was the default in the chosen one's place.
    on_the_fallback: bool,
    /// Try the default before the chosen microphone.
    default_first: bool,
    /// Restart until one runs or the stop, whatever the error.
    until_it_runs: bool,
    /// A start counts only once the sink saw a frame from it, within
    /// `STALL_TIMEOUT`: over a watched backend whose streams do not wait
    /// for playback.
    awaits_delivery: bool,
    /// The restarts of the streak this rebuild continues, 0 for a new one:
    /// its first try comes after the backoff that follows them.
    continues: usize,
}

impl CaptureSession {
    /// The waits before the second, third and fourth restart after a device
    /// change: a Bluetooth device is gone for one to two seconds while it
    /// changes profile; a device replugged by hand takes longer and is a
    /// loss the user can see and restart from. Restarts that go on past
    /// these back off further ([`Self::restart_backoff`]).
    pub const RESTART_BACKOFF: [Duration; 3] = [
        Duration::from_millis(250),
        Duration::from_millis(500),
        Duration::from_secs(1),
    ];
    /// Restarts tried before the recording ends in `DeviceLost`: one more
    /// than the waits between them.
    pub const RESTART_ATTEMPTS: usize = Self::RESTART_BACKOFF.len() + 1;
    /// The longest wait between two restarts that go on past
    /// `RESTART_ATTEMPTS` ([`Self::restart_backoff`]). Four seconds: the
    /// wait is audio lost once the devices are back (with the start in
    /// flight and the first frame's arrival, about 8 s at worst), and a try
    /// holds the session's lock for up to a start's deadline (3 s on Linux),
    /// so the callers wait for less than half the time. Rust only.
    pub const RESTART_BACKOFF_LONGEST: Duration = Duration::from_secs(4);

    /// The wait after the failed restart numbered `attempt` (1 for the
    /// first) before the next: `RESTART_BACKOFF`'s steps between the first
    /// `RESTART_ATTEMPTS`, then twice the step before, up to
    /// `RESTART_BACKOFF_LONGEST`, for as long as the restarts go on (250 ms,
    /// 500 ms, 1 s, 2 s, then 4 s every time). A streak of rebuilds counts
    /// its restarts across them (see the module doc). The longer steps are
    /// Rust only: Swift ended the recording after its attempts.
    #[must_use]
    pub fn restart_backoff(attempt: usize) -> Duration {
        let steps = Self::RESTART_BACKOFF.len();
        let step = attempt.max(1) - 1;
        if step < steps {
            return Self::RESTART_BACKOFF[step];
        }
        // The longest is reached long before the shift could overflow.
        let doublings = u32::try_from(step + 1 - steps).map_or(8, |doublings| doublings.min(8));
        (Self::RESTART_BACKOFF[steps - 1] * (1 << doublings)).min(Self::RESTART_BACKOFF_LONGEST)
    }
    /// The most silence written for one gap; a longer outage leaves the
    /// master that much short of wall time rather than filling minutes of
    /// zeros. It stays at 10 s although a stalled graph is now retried until
    /// the stop: the gap is written after the restarted backend runs, while
    /// its audio waits in the sink's 2 s rings, so it must fit the writer's
    /// relay (20 s by default) and drain well within those 2 s, or real
    /// audio would be dropped for zeros. Beyond it no audio exists to keep
    /// in place; only the times after the outage move earlier.
    pub const MAXIMUM_GAP: Duration = Duration::from_secs(10);
    /// How long a watched backend may deliver nothing before the watch
    /// thread reports [`DeviceChangeReason::DeliveryStalled`] and the
    /// rebuild takes over. Two seconds: a callback is 10 to 20 ms on every
    /// platform, and a device change that stops the callbacks is reported
    /// by its backend within that (PipeWire judges a burst of changes 2 s
    /// after its first at the latest; macOS tells a `coreaudiod` restart
    /// about 0.5 s after the service is back), so the watchdog speaks only
    /// where nothing else will, and does not rebuild while the service is
    /// still gone. Waiting costs no audio: the gap is filled from the last
    /// frame the thread saw, not from the report. Rust only.
    pub const STALL_TIMEOUT: Duration = Duration::from_secs(2);
    /// How often the watch thread samples the sink, on the clock: the
    /// resolution of a stall's start, and so of its gap. Rust only.
    pub const STALL_CHECK_INTERVAL: Duration = Duration::from_millis(100);
    /// How long a recording runs on the default input the session put in
    /// place of a chosen microphone that did not open before the session
    /// asks whether the chosen one delivers, over a backend that can ask
    /// without touching the recording
    /// ([`CaptureBackend::probes_inputs`]: Linux); elsewhere it is asked
    /// for again only by the rebuild of a device change. The live backends'
    /// own `FALLBACK_RECHECK` is another thing: how often they look for a
    /// chosen microphone that is missing, which needs no restart. Rust
    /// only.
    pub const CHOSEN_INPUT_RECHECK: Duration = Duration::from_secs(5);
    /// The longest wait between those asks: each ask doubles the wait up to
    /// this, for the whole recording, since each opens a stream of its own
    /// for up to the backend's start deadline (3 s on Linux), and a chosen
    /// microphone that delivers to an ask and then stalls again is asked
    /// for less and less often. An ask costs the recording nothing. Rust
    /// only.
    pub const CHOSEN_INPUT_RECHECK_LONGEST: Duration = Duration::from_secs(80);
    /// Frames the writer may fall behind the processing thread before
    /// frames are dropped and counted: 2000 (20 s) by default, so a disk
    /// that stalls for seconds (a slow sync, a sleeping external drive)
    /// loses nothing. The rings round up to 2^20 samples, 4 MiB per written
    /// channel, allocated at start. Rust only: Swift's relay held 2 s.
    pub const DEFAULT_WRITER_HEADROOM_FRAMES: usize = 20 * FRAMES_PER_SECOND;

    /// The production session: the live backend, Speex when the
    /// configuration cancels echo, the wall clock.
    pub fn new(configuration: CaptureConfiguration) -> Result<Self, CaptureError> {
        Self::with_backend(
            configuration,
            Arc::new(super::live::LiveCaptureBackend::new()),
            None,
            Self::DEFAULT_WRITER_HEADROOM_FRAMES,
            Arc::new(SystemClock::new()),
        )
    }

    /// `echo_canceller` `None` in `Call` with `echo_cancellation` on means
    /// [`SpeexEchoCanceller`] with the 200 ms tail; `InPerson` never
    /// cancels. `writer_headroom_frames` is the relay depth between
    /// processing and file I/O; a test that feeds audio faster than real
    /// time raises it so a slow disk in a debug build is not mistaken for
    /// a drop. `clock` paces the restart backoff and measures the gap after
    /// a device change; tests pass a `ManualClock`.
    pub fn with_backend(
        configuration: CaptureConfiguration,
        backend: Arc<dyn CaptureBackend>,
        echo_canceller: Option<Box<dyn EchoCanceller>>,
        writer_headroom_frames: usize,
        clock: Arc<dyn Clock>,
    ) -> Result<Self, CaptureError> {
        Self::with_writer_factory(
            configuration,
            backend,
            echo_canceller,
            writer_headroom_frames,
            clock,
            Arc::new(|layout, lanes, keep_raw| {
                Ok(Box::new(RecordingWriter::new(layout, lanes, keep_raw)?)
                    as Box<dyn RecordingWriting>)
            }),
        )
    }

    /// As [`Self::with_backend`], with the file writer injected too; tests wrap
    /// the real one to fail writes the way a full disk does.
    pub fn with_writer_factory(
        configuration: CaptureConfiguration,
        backend: Arc<dyn CaptureBackend>,
        echo_canceller: Option<Box<dyn EchoCanceller>>,
        writer_headroom_frames: usize,
        clock: Arc<dyn Clock>,
        make_writer: RecordingWriterFactory,
    ) -> Result<Self, CaptureError> {
        let echo_canceller = if configuration.uses_echo_cancellation() {
            match echo_canceller {
                Some(canceller) => Some(canceller),
                None => Some(Box::new(
                    SpeexEchoCanceller::new(SAMPLE_RATE, FRAME_SIZE)
                        .map_err(|e| CaptureError::BackendFailed(e.to_string()))?,
                ) as Box<dyn EchoCanceller>),
            }
        } else {
            None
        };
        Ok(Self {
            core: Arc::new(Core {
                configuration,
                writer_headroom_frames,
                backend,
                clock,
                make_writer,
                playback: Playback::global(),
                inner: Mutex::new(Inner {
                    state: CaptureState::Idle,
                    state_subscribers: Vec::new(),
                    level_subscribers: Vec::new(),
                    notice_subscribers: Vec::new(),
                    latest_levels: None,
                    echo_canceller,
                    active: None,
                    rebuild_generation: 0,
                    recordings_started: 0,
                    finalised: None,
                    #[cfg(test)]
                    before_stop_waits: None,
                }),
                state_changed: Condvar::new(),
                refuse_writer_failure_thread: AtomicBool::new(false),
            }),
        })
    }

    /// The session with its own [`Playback`] gate instead of the
    /// process's ([`Playback::global`]), for a test that must not share
    /// it. Call it before the first `start`.
    ///
    /// # Panics
    ///
    /// When the session has started a recording (its core is shared then).
    #[doc(hidden)]
    #[must_use]
    pub fn with_playback(mut self, playback: Playback) -> Self {
        Arc::get_mut(&mut self.core)
            .expect("with_playback before the first start")
            .playback = playback;
        self
    }

    /// The configuration given at construction.
    #[must_use]
    pub fn configuration(&self) -> &CaptureConfiguration {
        &self.core.configuration
    }

    /// The relay depth between processing and file I/O, in frames.
    #[must_use]
    pub fn writer_headroom_frames(&self) -> usize {
        self.core.writer_headroom_frames
    }

    /// The current state.
    #[must_use]
    pub fn state(&self) -> CaptureState {
        self.core.lock().state.clone()
    }

    /// Every state change from now on, starting with the current state.
    #[must_use]
    pub fn states(&self) -> Receiver<CaptureState> {
        let (sender, receiver) = channel();
        let mut inner = self.core.lock();
        let _ = sender.send(inner.state.clone());
        inner.state_subscribers.push(sender);
        receiver
    }

    /// Lane levels at 10 Hz while recording, starting with the latest.
    #[must_use]
    pub fn levels(&self) -> Receiver<LaneLevels> {
        let (sender, receiver) = channel();
        let mut inner = self.core.lock();
        if let Some(latest) = inner.latest_levels {
            let _ = sender.send(latest);
        }
        inner.level_subscribers.push(sender);
        receiver
    }

    /// Device changes from now on, while the state stays `Recording`.
    #[must_use]
    pub fn notices(&self) -> Receiver<CaptureNotice> {
        let (sender, receiver) = channel();
        self.core.lock().notice_subscribers.push(sender);
        receiver
    }

    /// The stream the backend opened for the current recording, the rebuilt
    /// backend's after a device change; `None` while not recording.
    #[must_use]
    pub fn stream(&self) -> Option<CaptureStream> {
        self.core
            .lock()
            .active
            .as_ref()
            .map(|active| active.stream.clone())
    }

    /// Whether a write failed in the recording in progress, which still
    /// shows `Recording`: every frame is dropped from that write on. The
    /// session ends such a recording itself, in `Failed`, on a thread of its
    /// own; when that thread could not be spawned the state stays
    /// `Recording`, and a caller that polls this stops the recording
    /// instead (`stop()` returns it with the write's failure in it). Rust
    /// only: Swift's writer-failure `Task` always ran.
    #[must_use]
    pub fn write_failed(&self) -> bool {
        let inner = self.core.lock();
        matches!(inner.state, CaptureState::Recording { .. })
            && inner
                .active
                .as_ref()
                .and_then(|active| active.writer_thread.as_ref())
                .is_some_and(WriterThread::has_failed)
    }

    /// For tests: a write that fails from now on finds no thread to end
    /// the recording on, as when the system cannot spawn one, so the state
    /// stays `Recording` and only [`Self::write_failed`] tells.
    #[doc(hidden)]
    pub fn refuse_the_writer_failure_thread(&self) {
        self.core
            .refuse_writer_failure_thread
            .store(true, Ordering::SeqCst);
    }

    /// Starts a recording for `meeting_id` in its own folder of the configured
    /// directory; `InvalidState` while one is starting, recording or stopping.
    pub fn start(&self, meeting_id: Uuid) -> Result<(), CaptureError> {
        self.core.start(meeting_id)
    }

    /// Ends the recording and returns it. A rebuild in flight is abandoned.
    /// After `Failed` returns the finalised partial recording the state
    /// carries, or fails when the failure left no recording. While a writer
    /// failure or a device loss is finalising, waits for it and answers from
    /// its outcome, the recording it finalised also when a `start()` came in
    /// first (the new recording goes on). A write or
    /// close that fails during the teardown leaves the state `Failed` with the
    /// recording that is returned, and so does a sync that failed while
    /// recording: the recording is whole, part of it may not have reached the
    /// disk, and a sync failure is reported only when nothing else ended the
    /// recording. Fails with `WriterFailed`, and leaves the state `Failed`,
    /// when the master is gone from disk or the writer thread died.
    pub fn stop(&self) -> Result<CaptureResult, CaptureError> {
        self.core.stop()
    }

    /// The sink's handler, on the session. Ignored unless recording; during
    /// a rebuild the reason is kept for `resume`; otherwise the notice goes
    /// out and the rebuild runs on its own thread so `stop()` can
    /// interleave at its sleeps. Public so a test can report a change while
    /// idle, after a stop or during a rebuild; production reaches it
    /// through the sink alone.
    pub fn device_changed(&self, reason: DeviceChangeReason) {
        self.core.device_changed(reason);
    }

    /// The far-end delay for the latencies the backend reports. The mic
    /// hears the tap's signal after the output path (output latency plus
    /// safety offset), the room and the input path (input latency plus
    /// safety offset), so the far-end is delayed by the two device paths in
    /// full and the Speex tail (200 ms) is left for the room and for what
    /// the HAL under-reports. Below one processing frame the tail absorbs
    /// the offset as well; over-delaying is the one thing the MDF filter
    /// cannot recover from, so nothing is rounded up.
    #[must_use]
    pub fn far_end_delay_frames(
        input_latency_frames: usize,
        output_latency_frames: usize,
    ) -> usize {
        let total = input_latency_frames + output_latency_frames;
        if total >= FRAME_SIZE { total } else { 0 }
    }

    /// Whole relay frames for a gap: 48 000 samples a second in frames of
    /// [`FRAME_SIZE`], rounded down.
    #[must_use]
    pub fn gap_frames(gap: Duration) -> usize {
        // A capped, positive duration in samples.
        let samples = (gap.as_secs_f64() * SAMPLE_RATE).floor() as usize;
        samples / FRAME_SIZE
    }
}

impl Drop for CaptureSession {
    /// A session dropped mid-recording still stops its threads and closes
    /// its files.
    fn drop(&mut self) {
        let recording = matches!(self.core.lock().state, CaptureState::Recording { .. });
        if recording {
            let _ = self.core.stop();
        }
    }
}

impl Core {
    /// A stream that stalls again this soon after a rebuild resumed on it
    /// continues that rebuild's streak (`Streak`), and a chosen microphone
    /// that does is replaced by the default input at the next rebuild
    /// instead of being restarted again (see `restart_backend`), except
    /// over a backend whose streams may wait for playback.
    const STALLED_AGAIN: Duration = Duration::from_secs(10);
    /// How often, at most, a streak's restarts that keep failing are
    /// logged above `debug`, with the count so far.
    const RESTART_LOG_INTERVAL: Duration = Duration::from_secs(60);

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn set_state(&self, inner: &mut Inner, state: &CaptureState) {
        if let CaptureState::Failed {
            recording: Some(recording),
            ..
        } = state
        {
            inner.finalised = Some((inner.recordings_started, recording.clone()));
        }
        inner.state = state.clone();
        inner
            .state_subscribers
            .retain(|s| s.send(state.clone()).is_ok());
        self.state_changed.notify_all();
    }

    fn publish(&self, levels: LaneLevels) {
        let mut inner = self.lock();
        inner.latest_levels = Some(levels);
        inner.level_subscribers.retain(|s| s.send(levels).is_ok());
    }

    fn emit(inner: &mut Inner, notice: CaptureNotice) {
        inner.notice_subscribers.retain(|s| s.send(notice).is_ok());
    }

    fn far_end_delay_frames(&self, stream: &CaptureStream) -> usize {
        if !self.configuration.uses_echo_cancellation() {
            return 0;
        }
        // The latencies are device frames; the canceller runs at 48 kHz.
        CaptureSession::far_end_delay_frames(
            stream.at_output_rate(stream.input_latency_frames),
            stream.at_output_rate(stream.output_latency_frames),
        )
    }

    /// Whether the relay and the writer carry the raw microphone channel.
    fn keep_raw(&self) -> bool {
        self.configuration.keep_raw_mic_lane && self.configuration.lanes().contains(&AudioLane::Mic)
    }

    /// The seconds `frames` relay frames of silence last, exact for any
    /// gap length.
    fn seconds(frames: usize) -> f64 {
        (frames * FRAME_SIZE) as f64 / SAMPLE_RATE
    }

    fn make_processing_thread(
        &self,
        sink: &Arc<LaneFrameSink>,
        relay: &Arc<FrameRelay>,
        stream: &CaptureStream,
        levels: Option<Arc<LevelSlot>>,
        echo_canceller: Option<Box<dyn EchoCanceller>>,
    ) -> ProcessingThread {
        let mut configuration =
            ProcessingConfiguration::new(&self.configuration.lanes(), echo_canceller);
        configuration.far_end_delay_frames = self.far_end_delay_frames(stream);
        // The live backends refuse a rate the converter cannot take; one
        // that reports such a rate anyway is recorded unconverted rather
        // than panicking with the session's lock held.
        configuration.device_rate = if RateConverter::supports(stream.sample_rate) {
            stream.sample_rate
        } else {
            tracing::error!(
                "the capture runs at {} Hz, which cannot be converted; recording it unconverted",
                stream.sample_rate
            );
            SAMPLE_RATE
        };
        configuration.keep_raw_mic = self.keep_raw();
        ProcessingThread::new(Arc::clone(sink), Arc::clone(relay), configuration, levels)
    }

    /// [`Self::start_recording`] under a guard: a panic on the way (a
    /// writer or a processing thread the system cannot spawn) leaves
    /// `Failed` with no recording and the backend stopped, instead of
    /// `Starting` for good, which no `start()` or `stop()` leaves. The body
    /// holds the lock throughout and drops it as it unwinds, before the
    /// guard takes it.
    ///
    /// The [`Playback`] hold is taken first, before the backend (and its
    /// tap) exists, and declared before the guard so that a failed or
    /// panicking start releases it only after the guard's `backend.stop()`.
    fn start(self: &Arc<Self>, meeting_id: Uuid) -> Result<(), CaptureError> {
        let mut hold = Some(self.playback.hold_for_recording());
        let unwinding = Unwinding::starting(self);
        let started = self.start_recording(meeting_id, &mut hold);
        unwinding.disarm();
        started
    }

    /// Moves `hold` into the recording once it runs; a start that fails
    /// leaves it for `start` to drop.
    #[allow(clippy::too_many_lines)]
    fn start_recording(
        self: &Arc<Self>,
        meeting_id: Uuid,
        hold: &mut Option<RecordingHold>,
    ) -> Result<(), CaptureError> {
        let mut inner = self.lock();
        match inner.state {
            CaptureState::Idle | CaptureState::Failed { .. } => {}
            ref other => {
                return Err(CaptureError::InvalidState(format!(
                    "start while {}",
                    other.kind()
                )));
            }
        }
        // `Starting` carries no recording: a failed start must never hand
        // out the previous meeting's files.
        self.set_state(&mut inner, &CaptureState::Starting);
        inner.recordings_started += 1;
        let recording = inner.recordings_started;
        // A new recording, possibly on other devices, starts from a cold filter.
        if let Some(canceller) = inner.echo_canceller.as_mut() {
            canceller.reset();
        }
        let lanes = self.configuration.lanes();
        let layout = RecordingLayout::new(&self.configuration.output_directory, meeting_id);
        let keep_raw = self.keep_raw();

        let writer = match (self.make_writer)(&layout, &lanes, keep_raw) {
            Ok(writer) => writer,
            Err(error) => {
                let failure = as_writer_failure(error);
                self.set_state(
                    &mut inner,
                    &CaptureState::Failed {
                        error: failure.clone(),
                        recording: None,
                    },
                );
                return Err(failure);
            }
        };

        let weak: Weak<Core> = Arc::downgrade(self);
        let sink = Arc::new(LaneFrameSink::with_handler(
            &lanes,
            SAMPLE_RATE,
            2.0,
            Box::new(move |reason| {
                if let Some(core) = weak.upgrade() {
                    core.device_changed_in(recording, reason);
                }
            }),
        ));
        let started = self
            .backend
            .start(
                &lanes,
                self.configuration.input_device_uid.as_deref(),
                Arc::clone(&sink),
            )
            .map(|stream| Started {
                stream,
                on_the_default: false,
            })
            .or_else(|error| {
                self.start_on_the_default(&sink)
                    .map(|stream| Started {
                        stream,
                        on_the_default: true,
                    })
                    .ok_or(error)
            });
        let Started {
            stream,
            on_the_default,
        } = match started {
            Ok(started) => started,
            Err(error) => {
                let mut writer = writer;
                let _ = writer.finish();
                // Only a folder this start made: never a meeting's files.
                if writer.created_directory() {
                    let _ = std::fs::remove_dir_all(&layout.directory);
                }
                self.set_state(
                    &mut inner,
                    &CaptureState::Failed {
                        error: error.clone(),
                        recording: None,
                    },
                );
                return Err(error);
            }
        };

        let relay = Arc::new(FrameRelay::new(
            lanes.len() + usize::from(keep_raw),
            FRAME_SIZE,
            self.writer_headroom_frames,
        ));
        let canceller = inner.echo_canceller.take();
        let mut processing = self.make_processing_thread(&sink, &relay, &stream, None, canceller);
        let levels = Arc::clone(processing.levels());
        let on_levels: Weak<Core> = Arc::downgrade(self);
        let on_error: Weak<Core> = Arc::downgrade(self);
        let mut writer_thread = WriterThread::new(
            Arc::clone(&relay),
            writer,
            Arc::clone(&levels),
            lanes.len(),
            keep_raw,
            Box::new(move |levels| {
                if let Some(core) = on_levels.upgrade() {
                    core.publish(levels);
                }
            }),
            Box::new(move |error| {
                // The writer thread reports this; finalising joins that
                // thread, so it runs on its own, as Swift's `Task` did.
                if let Some(core) = on_error.upgrade() {
                    let spawned = if core.refuse_writer_failure_thread.load(Ordering::SeqCst) {
                        Err(std::io::Error::other("refused for a test"))
                    } else {
                        std::thread::Builder::new()
                            .name("steno-wfail".into())
                            .spawn(move || core.writer_failed(&error, recording))
                            .map(drop)
                    };
                    // No thread to finalise on: the state stays
                    // `Recording` with nothing written, which
                    // `CaptureSession::write_failed` tells a watcher, and
                    // `stop()` returns the recording with the write's
                    // failure in it.
                    if let Err(spawn) = spawned {
                        tracing::error!(%spawn, "a write failed and the recording could not be ended");
                    }
                }
            }),
        );
        writer_thread.start();
        processing.start();

        let watch = self.backend.delivers_continuously(&lanes).then(|| {
            let probes = self.backend.probes_inputs();
            let cancel = Cancel::new();
            let token = cancel.clone();
            let core = Arc::clone(self);
            // Spawned under the lock, as a rebuild is; its first step waits
            // for the lock and finds the recording in place.
            let thread = std::thread::Builder::new()
                .name("steno-watch".into())
                .spawn(move || core.watch(recording, &token))
                .expect("spawn watch thread");
            let now = self.clock.now();
            let mut watch = Watch {
                cancel,
                thread,
                progress: Progress { count: 0, at: now },
                waits_for_playback: self.backend.waits_for_playback(&lanes),
                probes,
                recheck: None,
                asked_after: None,
                resumed_at: None,
            };
            if probes && Self::replaced_the_chosen(&stream, on_the_default) {
                watch.ask_again(now);
            }
            watch
        });
        inner.active = Some(Active {
            meeting_id,
            stream,
            sink,
            relay,
            processing: Some(processing),
            writer_thread: Some(writer_thread),
            levels,
            ended_on_device_loss: false,
            device_changes: 0,
            gap_seconds: 0.0,
            system_peak_so_far: 0.0,
            rebuild: None,
            pending_change: None,
            ring_drops: RingDrops::default(),
            watch,
            _recording_hold: hold.take(),
            streak: Streak::default(),
        });
        self.set_state(
            &mut inner,
            &CaptureState::Recording {
                started_at: Utc::now(),
            },
        );
        Ok(())
    }

    fn stop(&self) -> Result<CaptureResult, CaptureError> {
        {
            // Another thread's finalise (see the module doc) is waited out.
            // The loop re-checks after every wakeup: a spurious one, or a
            // `Stopping` left and entered again before this thread got the
            // lock back. A poisoned lock is read as it is, so a panic
            // elsewhere does not end the wait early.
            let mut inner = self.lock();
            let recording = inner.recordings_started;
            #[cfg(test)]
            if matches!(inner.state, CaptureState::Stopping)
                && let Some(hook) = inner.before_stop_waits.take()
            {
                drop(inner);
                hook();
                inner = self.lock();
            }
            while matches!(inner.state, CaptureState::Stopping) {
                inner = self
                    .state_changed
                    .wait(inner)
                    .unwrap_or_else(PoisonError::into_inner);
            }
            // A `start()` took the lock first: the recording this stop was
            // for has ended, and the new one is not this caller's to stop.
            // The finalise it waited for answers, as it would have had this
            // stop come first (Swift's actor ran it right after).
            if inner.recordings_started != recording {
                return match &inner.finalised {
                    Some((finalised, result)) if *finalised == recording => Ok((**result).clone()),
                    _ => Err(CaptureError::InvalidState(
                        "stop after another recording started".into(),
                    )),
                };
            }
            match &inner.state {
                CaptureState::Recording { .. } => {}
                CaptureState::Failed { recording, .. } => {
                    return match recording {
                        Some(recording) => Ok((**recording).clone()),
                        None => Err(CaptureError::InvalidState(
                            "stop after a failure that left no recording".into(),
                        )),
                    };
                }
                other => {
                    return Err(CaptureError::InvalidState(format!(
                        "stop while {}",
                        other.kind()
                    )));
                }
            }
            self.set_state(&mut inner, &CaptureState::Stopping);
        }
        let unwinding = Unwinding::arm(
            self,
            CaptureError::BackendFailed("the teardown panicked".into()),
        );
        let finished = self.finish();
        // Closing the files can fail on a full disk; the master is still
        // readable to its last frame, so the result comes back with the
        // failure in it and the state carries it too.
        let state = match &finished {
            Ok(result) => match &result.failure {
                None => CaptureState::Idle,
                Some(error) => CaptureState::Failed {
                    error: error.clone(),
                    recording: Some(Box::new(result.clone())),
                },
            },
            Err(error) => CaptureState::Failed {
                error: error.clone(),
                recording: None,
            },
        };
        self.set_state(&mut self.lock(), &state);
        unwinding.disarm();
        finished
    }

    /// Rebuild abandoned and joined, backend off, rings drained, relay
    /// drained, files closed, asset built. The asset is built even when
    /// a write or closing the files failed (its paths are fixed at start
    /// and the duration is what the master holds); the failure comes back
    /// in [`CaptureResult::failure`]: a failed write or close, else the
    /// device loss that ended the recording, else a sync that failed while
    /// recording. `WriterFailed` with no asset when there is nothing to
    /// hand out: the writer thread died and took the writer with it, or
    /// the master is gone from disk (its folder deleted while recording; an
    /// unlinked file still writes and closes without an error). The caller
    /// has set the state to `Stopping`; the threads are stopped with the
    /// lock released (see the module doc).
    fn finish(&self) -> Result<CaptureResult, CaptureError> {
        let mut active = self
            .lock()
            .active
            .take()
            .ok_or_else(|| CaptureError::InvalidState("nothing to finish".into()))?;
        // The rate the rings carry: the stream's, or a restarted one's that
        // the stop overtook before its `resume`.
        let mut ring_rate = active.stream.sample_rate;
        // The watch thread waits on nothing but the lock and the clock, and
        // never finishes a recording itself, so the join cannot deadlock.
        if let Some(watch) = active.watch.take() {
            watch.cancel.cancel();
            let _ = watch.thread.join();
        }
        if let Some(rebuild) = active.rebuild.take() {
            rebuild.cancel.cancel();
            // With `active` gone every step of the rebuild gives up; what
            // it wrote of a gap before that is in the master, and the peak
            // of the processing thread it stopped comes back with it. The
            // join must come before the processing thread's stop, the
            // writer's and `clear()`; before or after `backend.stop()` is
            // the same, since a backend stop while the rebuild's own runs
            // finds the backend's state already taken and returns at once.
            let (unaccounted, peak, restarted_rate) =
                rebuild.thread.join().unwrap_or((0, 0.0, None));
            ring_rate = restarted_rate.unwrap_or(ring_rate);
            active.gap_seconds += Self::seconds(unaccounted);
            active.system_peak_so_far = active.system_peak_so_far.max(peak);
        }
        self.backend.stop();
        let mut system_peak = active.system_peak_so_far;
        if let Some(mut processing) = active.processing.take() {
            processing.stop();
            system_peak = system_peak.max(processing.system_peak());
            self.lock().echo_canceller = processing.take_echo_canceller();
        }
        let writer_lost =
            || CaptureError::WriterFailed("the writer thread ended without its files".into());
        let mut writer_thread = active.writer_thread.take().ok_or_else(writer_lost)?;
        writer_thread.stop();
        // A write that failed during this drain went to `writer_failed`,
        // which ignores it once the state is `Stopping`; it comes back
        // beside the asset instead, as a failed close does. A sync that
        // failed while recording is kept apart, behind both.
        let write_failure = writer_thread.take_error();
        let sync_failure = writer_thread.take_sync_error();
        let mut writer = writer_thread.take_writer().ok_or_else(writer_lost)?;
        // Read before `clear()`, which zeroes the ring overrun counts. Whole
        // frames still in the rings never reached the relay: a restarted
        // backend delivered them after a stop overtook its rebuild, with no
        // processing thread running yet. They count as dropped. The rings
        // hold the device's rate; the counts are 48 kHz frames.
        active
            .ring_drops
            .fold(active.sink.dropped_samples(), ring_rate);
        let undrained =
            CaptureStream::rescaled(active.sink.available_to_read(), ring_rate, SAMPLE_RATE)
                / FRAME_SIZE;
        active.sink.clear();
        let closing = writer.finish().err();
        let failure = write_failure
            .or(closing)
            .map(as_writer_failure)
            .or_else(|| {
                active
                    .ended_on_device_loss
                    .then_some(CaptureError::DeviceLost)
            })
            .or(sync_failure);
        let files = writer.files();
        if matches!(files.master.try_exists(), Ok(false)) {
            return Err(CaptureError::WriterFailed(format!(
                "{} is gone",
                files.master.display()
            )));
        }
        let lanes = self.configuration.lanes();
        let mut dropped: BTreeMap<AudioLane, usize> = BTreeMap::new();
        for (lane, samples) in &active.ring_drops.at_output_rate {
            *dropped.entry(*lane).or_default() += samples / FRAME_SIZE;
        }
        if undrained > 0 {
            for lane in &lanes {
                *dropped.entry(*lane).or_default() += undrained;
            }
        }
        for (index, frames) in active.relay.dropped_frames().into_iter().enumerate() {
            if index < lanes.len() && frames > 0 {
                *dropped.entry(lanes[index]).or_default() += frames;
            }
        }
        let statistics = CaptureStatistics {
            duration: files.duration,
            dropped_frames: dropped,
            system_lane_silent: lanes.contains(&AudioLane::System)
                && system_peak < LaneLevel::SILENT_PEAK_LINEAR,
            ended_on_device_loss: active.ended_on_device_loss,
            device_changes: active.device_changes,
            gap_seconds: active.gap_seconds,
        };
        let asset = AudioAsset {
            id: Uuid::new_v4(),
            meeting_id: active.meeting_id,
            url: file_url(&files.master, false),
            format: AudioFormat::Caf48kFloat32,
            lanes: lanes.clone(),
            sidecars_16k: files
                .sidecars_16k
                .iter()
                .map(|(lane, path)| (*lane, file_url(path, false)))
                .collect(),
            mixdown_url: None,
            retention: AudioRetention::KeepForever,
            expires_at: None,
        };
        Ok(CaptureResult {
            asset,
            statistics,
            failure,
        })
    }

    // Device changes

    fn device_changed(self: &Arc<Self>, reason: DeviceChangeReason) {
        let mut inner = self.lock();
        self.begin_rebuild(&mut inner, reason);
    }

    /// [`Self::device_changed`] for a report from the backend of
    /// `recording`: one an older recording's backend left in its handler
    /// past that recording's stop reaches nothing.
    fn device_changed_in(self: &Arc<Self>, recording: usize, reason: DeviceChangeReason) {
        let mut inner = self.lock();
        if inner.recordings_started == recording {
            self.begin_rebuild(&mut inner, reason);
        }
    }

    /// [`Self::device_changed`] under the caller's guard.
    fn begin_rebuild(self: &Arc<Self>, inner: &mut Inner, reason: DeviceChangeReason) {
        if !matches!(inner.state, CaptureState::Recording { .. }) {
            return;
        }
        let generation = inner.rebuild_generation + 1;
        let recording = inner.recordings_started;
        let Some(active) = inner.active.as_mut() else {
            return;
        };
        if active.rebuild.is_some() {
            active.pending_change = Some(reason);
            return;
        }
        let cancel = Cancel::new();
        let core = Arc::clone(self);
        let token = cancel.clone();
        // Spawned under the lock so the handle is in place before any
        // `finish()` can look for it; the thread's first step waits for
        // the lock.
        let thread = match std::thread::Builder::new()
            .name("steno-rebuild".into())
            .spawn(move || core.rebuild(generation, recording, reason, &token))
        {
            Ok(thread) => thread,
            // Never a panic here: this runs on the backend's thread (the
            // PipeWire loop's on Linux), which a panic would end without a
            // word. The recording goes on with the devices it had, and the
            // latch opens again (only a rebuild opens it otherwise), so the
            // backend's next report tries again. Not tested: the spawn
            // cannot be made to fail here without a seam on the backend's
            // thread; a lane that stops delivering after it is the stall
            // watchdog's to catch.
            Err(spawn) => {
                tracing::error!(%spawn, ?reason, "a device change could not be followed");
                active.sink.rearm_device_change();
                return;
            }
        };
        active.rebuild = Some(Rebuild { cancel, thread });
        inner.rebuild_generation = generation;
        Self::emit(inner, CaptureNotice::DeviceChanged(reason));
    }

    /// Whether `stream` is the default input the session started in place
    /// of the chosen microphone (`start_on_the_default`), which no backend
    /// watches for the chosen one coming back.
    fn replaced_the_chosen(stream: &CaptureStream, on_the_default: bool) -> bool {
        on_the_default && stream.input.as_ref().is_some_and(|input| input.is_fallback)
    }

    /// The watch thread of the start numbered `recording`, over a backend
    /// that delivers continuously: every `STALL_CHECK_INTERVAL` on the
    /// clock it samples the sink's count of frames offered, and reports
    /// through the sink, as the backend's listeners do, a stream that
    /// delivered and then stopped for longer than `STALL_TIMEOUT`
    /// (`DeliveryStalled`); it tells the first frames after restarts that
    /// sent `StillRestarting` (`Delivering`). While the stream is the
    /// default input the session put in place of the chosen microphone,
    /// over a backend that probes, it asks on a `steno-probe` thread of its
    /// own whether the chosen one delivers (`CaptureBackend::probe_input`,
    /// seconds at most, so the sampling goes on meanwhile), and only when
    /// it did reports `ChosenInputRecheck`, whose rebuild returns to it; a
    /// probe that finds nothing costs the recording nothing. Judges nothing
    /// while a rebuild runs; ends with the recording, leaving a probe still
    /// running to end on its own. Never on a real-time thread: the
    /// producer's only share is the one store per callback the count
    /// takes.
    fn watch(self: &Arc<Self>, recording: usize, cancel: &Cancel) {
        let mut probe: Option<JoinHandle<bool>> = None;
        while self
            .clock
            .sleep(CaptureSession::STALL_CHECK_INTERVAL, cancel)
        {
            // Joined only once finished, so at once.
            let delivered = probe
                .take_if(|probe| probe.is_finished())
                .is_some_and(|probe| probe.join().unwrap_or(false));
            let (report, ask) = {
                let mut inner = self.lock();
                if inner.recordings_started != recording
                    || !matches!(inner.state, CaptureState::Recording { .. })
                {
                    return;
                }
                let now = self.clock.now();
                let Some(active) = inner.active.as_mut() else {
                    return;
                };
                let Some(watched) = active.watch.as_mut() else {
                    return;
                };
                if active.rebuild.is_some() {
                    continue;
                }
                let count = active.sink.frames_offered();
                let progress = &mut watched.progress;
                let mut reason = None;
                // Audio arrives: a warning about restarts that went on is
                // over, and stays so unless they start again.
                let mut delivering = false;
                if count != progress.count {
                    progress.count = count;
                    progress.at = now;
                    delivering = std::mem::take(&mut active.streak.warned);
                } else if (count != 0 || !watched.waits_for_playback)
                    && now.saturating_sub(progress.at) > CaptureSession::STALL_TIMEOUT
                {
                    reason = Some(DeviceChangeReason::DeliveryStalled);
                }
                // A probe's answer counts only while the stream is still the
                // default in the chosen one's place.
                let replaced = watched.recheck.is_some();
                if reason.is_none() && delivered && replaced {
                    reason = Some(DeviceChangeReason::ChosenInputRecheck);
                }
                let ask = reason.is_none()
                    && probe.is_none()
                    && watched.recheck.is_some_and(|due| now >= due);
                let ask = if ask {
                    watched.ask_again(now);
                    self.configuration.input_device_uid.clone()
                } else {
                    None
                };
                let report = reason.map(|reason| (Arc::clone(&active.sink), reason));
                if delivering {
                    Self::emit(&mut inner, CaptureNotice::Delivering);
                }
                (report, ask)
            };
            if let Some(uid) = ask {
                let backend = Arc::clone(&self.backend);
                probe = std::thread::Builder::new()
                    .name("steno-probe".into())
                    .spawn(move || backend.probe_input(&uid))
                    .inspect_err(|spawn| {
                        tracing::warn!(%spawn, "the chosen microphone could not be asked for");
                    })
                    .ok();
            }
            // Outside the lock: the sink's handler takes it. A change the
            // backend reported first holds the latch, and this one is
            // dropped there.
            if let Some((sink, reason)) = report {
                sink.report_device_change(reason);
            }
        }
    }

    fn still_rebuilding(inner: &Inner, generation: usize) -> bool {
        matches!(inner.state, CaptureState::Recording { .. })
            && inner.active.as_ref().is_some_and(|a| a.rebuild.is_some())
            && inner.rebuild_generation == generation
    }

    /// [`Self::rebuild_steps`], and a panic in them ends the recording as
    /// a device that stayed lost does: saved, in `Failed(DeviceLost)`, so
    /// the recorder hears of it rather than showing `Recording` over a
    /// session that no longer writes anything. `recording` is the start
    /// the rebuild belongs to, `reason` the change it follows.
    fn rebuild(
        self: &Arc<Self>,
        generation: usize,
        recording: usize,
        reason: DeviceChangeReason,
        cancel: &Cancel,
    ) -> (usize, f32, Option<f64>) {
        // `AssertUnwindSafe` holds: every step changes `Inner` under the
        // lock in whole assignments (a poisoned lock is read as is), so
        // a panic leaves it as the last completed step did. What a panic
        // can leave half done is outside it: the old backend stopped, the
        // processing thread taken out and not replaced, a new stream not
        // handed over. `finish()` copes with each, as it does when a stop
        // cuts a rebuild short: it stops the backend again, stops a
        // processing thread only when there is one, and drains the rings.
        let steps = AssertUnwindSafe(|| self.rebuild_steps(generation, recording, reason, cancel));
        std::panic::catch_unwind(steps).unwrap_or_else(|_| {
            tracing::error!("following a device change panicked; the recording ends");
            self.device_lost(recording, generation);
            (0, 0.0, None)
        })
    }

    /// Old backend and processing thread off, then `start` again with
    /// backoff; the gap from the old stream's last frame (as the watch
    /// thread saw it; unwatched, from the moment the old backend was told
    /// to stop) is written as silence before the new processing thread
    /// starts. The sink, the relay, the writer thread and the files stay.
    /// Nothing here runs on a real-time thread. Returns the silence frames
    /// written that `resume` did not account because a stop came first, the
    /// old processing thread's system-lane peak for a `finish()` that took
    /// the recording before this thread could fold it in, and the rate of a
    /// restarted stream that stop kept from its `resume`. The meter reads
    /// silence from the old stream's stop until the restarted one delivers.
    fn rebuild_steps(
        self: &Arc<Self>,
        generation: usize,
        recording: usize,
        reason: DeviceChangeReason,
        cancel: &Cancel,
    ) -> (usize, f32, Option<f64>) {
        // The gap's stopwatch (`silent_from`) runs from before the
        // teardown: the HAL calls in `stop()` take tens to hundreds of
        // milliseconds during a device transition, and that is dead time in
        // the master too. It runs from earlier still when the watch thread
        // saw the last frame arrive before the change was reported (the
        // backends coalesce changes for 0.5 s, a stall is reported after
        // `STALL_TIMEOUT`).
        let now = self.clock.now();
        let mut silent_from = now;
        let (sink, relay, processing, plan) = {
            let mut inner = self.lock();
            if !Self::still_rebuilding(&inner, generation) {
                return (0, 0.0, None);
            }
            let Some(active) = inner.active.as_mut() else {
                return (0, 0.0, None);
            };
            if let Some(watch) = &active.watch
                && watch.progress.count == active.sink.frames_offered()
            {
                silent_from = silent_from.min(watch.progress.at);
            }
            let plan = self.restart_plan(active, reason, now);
            if plan.continues == 0 {
                active.streak.attempts = 0;
                active.streak.logged_at = None;
                if reason == DeviceChangeReason::DeliveryStalled {
                    tracing::warn!(
                        "the capture delivered nothing for over {} ms; restarting it",
                        CaptureSession::STALL_TIMEOUT.as_millis()
                    );
                }
            }
            (
                Arc::clone(&active.sink),
                Arc::clone(&active.relay),
                active.processing.take(),
                plan,
            )
        };
        // Whatever whole frames the rings hold are the old device's last
        // audio; the processing thread's stop drains them into the relay.
        self.backend.stop();
        let mut canceller = None;
        let mut peak = 0.0f32;
        if let Some(mut processing) = processing {
            processing.stop();
            peak = processing.system_peak();
            canceller = processing.take_echo_canceller();
        }
        // New devices mean a new echo path: the filter starts cold, as at start.
        if let Some(canceller) = canceller.as_mut() {
            canceller.reset();
        }
        {
            let mut inner = self.lock();
            inner.echo_canceller = canceller;
            // Gone when `finish()` took the recording meanwhile; the peak
            // then reaches it through this thread's return value. The old
            // stream delivers no more, so its overruns are counted at its
            // rate; the restarted one's come at the next fold.
            if let Some(active) = inner.active.as_mut() {
                active.system_peak_so_far = active.system_peak_so_far.max(peak);
                active
                    .ring_drops
                    .fold(sink.dropped_samples(), active.stream.sample_rate);
                // No processing thread writes the slot now; the writer
                // republishes this, so a meter does not hold the old
                // stream's last level through a stall or a long restart.
                active
                    .levels
                    .publish(LaneLevel::SILENCE, Some(LaneLevel::SILENCE));
            }
        }
        // The old backend's `stop()` lets no new report through, so the
        // latch can open now: a report from the rebuilt backend before the
        // gap is written reaches `device_changed`, which keeps it for
        // `resume`. A report the old backend left in the handler reaches
        // `device_changed` late and costs one more rebuild (see the
        // PipeWire backend's module doc).
        sink.rearm_device_change();
        let restart = self.restart_backend(&sink, plan, generation, cancel);
        let (unaccounted, restarted_rate) = match restart {
            Restart::Started {
                started,
                attempt,
                at,
            } => {
                let rate = started.stream.sample_rate;
                // The gap grows through every failed attempt and is written
                // once, in full, when a start succeeds; what the restarted
                // stream delivered after its start waits in the rings.
                let elapsed = at.saturating_sub(silent_from);
                let gap_frames =
                    CaptureSession::gap_frames(elapsed.min(CaptureSession::MAXIMUM_GAP));
                let written = self.write_silence(gap_frames, &relay, generation, cancel);
                if written == gap_frames
                    && self.relay_has_room(&sink, &relay, &started.stream, generation, cancel)
                    && self.resume(started, attempt, gap_frames, &sink, &relay, generation)
                {
                    (0, None)
                } else {
                    (written, Some(rate))
                }
            }
            Restart::Abandoned => (0, None),
            Restart::Exhausted => {
                self.device_lost(recording, generation);
                (0, None)
            }
        };
        (unaccounted, peak, restarted_rate)
    }

    /// How the rebuild that follows `reason` restarts `active`'s stream,
    /// judged at `now` (`RestartPlan`).
    fn restart_plan(
        &self,
        active: &Active,
        reason: DeviceChangeReason,
        now: Duration,
    ) -> RestartPlan {
        let on_the_fallback = active
            .stream
            .input
            .as_ref()
            .is_some_and(|input| input.is_fallback);
        let watch = active.watch.as_ref();
        // A stream that may only be waiting for playback (a Mac call capture
        // without the capture permission) stops whenever playback does: no
        // restart can make it deliver, and the microphone is not to blame.
        let waits_for_playback = watch.is_some_and(|watch| watch.waits_for_playback);
        // The stream the last rebuild resumed on stalled soon after, or
        // before it delivered anything.
        let stalled = reason == DeviceChangeReason::DeliveryStalled;
        let (soon, before_delivering) = watch
            .and_then(|watch| Some((watch.resumed_at?, watch.progress.at)))
            .map_or((false, false), |(resumed_at, delivered_at)| {
                (
                    now.saturating_sub(resumed_at) < Self::STALLED_AGAIN,
                    delivered_at <= resumed_at,
                )
            });
        // A chosen microphone the last rebuild resumed on that stalls again
        // so soon would be restarted, and stall, over and over.
        let stalled_again = stalled
            && soon
            && !on_the_fallback
            && !waits_for_playback
            && self.configuration.input_device_uid.is_some();
        RestartPlan {
            on_the_fallback,
            default_first: stalled_again,
            until_it_runs: matches!(
                reason,
                DeviceChangeReason::AudioServiceRestarted | DeviceChangeReason::DeliveryStalled
            ),
            awaits_delivery: watch.is_some() && !waits_for_playback,
            continues: if stalled && (soon || before_delivering) {
                active.streak.attempts
            } else {
                0
            },
        }
    }

    /// `start` again, backing off on the clock between tries
    /// (`CaptureSession::restart_backoff`), and once more on the default
    /// input (`start_on_the_default`) after the last failure, or already
    /// after an earlier one when waiting buys nothing: the stream replaced
    /// was on the fallback (`plan.on_the_fallback`), so the default worked a
    /// moment ago, or the graph did not run on the chosen microphone
    /// ([`CaptureError::DidNotRun`]: on Linux each such attempt waits out
    /// the 3 s start deadline). That early try comes once, after the first
    /// failure that calls for it; when it fails, the restarts go on. With
    /// `plan.default_first` (a chosen microphone that stalled again soon
    /// after the last rebuild resumed on it) the default is tried before
    /// the chosen one, once. With `plan.awaits_delivery` a start whose
    /// stream delivers nothing within `STALL_TIMEOUT` is stopped and fails
    /// with `DidNotRun` (`try_start`), as on macOS and Windows a device that
    /// delivers nothing still starts. A last restart that fails with
    /// `DidNotRun` (the devices are there, the graph does not run: a source
    /// whose owner stalls), or any failure after a stall or a restart of
    /// the audio service (`plan.until_it_runs`: a driver that hangs for a
    /// while, `coreaudiod` or the PipeWire daemon coming back), is not the
    /// end: the restarts, each with the default after it, go on until one
    /// runs or the stop (Rust only), and `notices` carries
    /// `StillRestarting` once they pass `RESTART_ATTEMPTS`
    /// (`restart_failed`). A rebuild that continues a streak
    /// (`plan.continues`) counts on from its restarts and waits the
    /// backoff that follows them before its first try. Each start holds
    /// the session's mutex (see Threads) and releases it before the wait
    /// for its first frame, so while they go on the callers wait for up to
    /// a start's deadline at a time. `Started` with the attempt that
    /// succeeded, `Exhausted` when the last one and the default failed
    /// otherwise, `Abandoned` when `stop()` cancelled a sleep or the
    /// recording is gone.
    fn restart_backend(
        &self,
        sink: &Arc<LaneFrameSink>,
        plan: RestartPlan,
        generation: usize,
        cancel: &Cancel,
    ) -> Restart {
        let mut default_tried = false;
        let mut attempt = plan.continues;
        // What the restart before the next try failed with, for the log; a
        // streak's rebuild follows a stall of the stream it resumed on.
        let mut failure = "the stream it resumed on stopped delivering".to_owned();
        loop {
            if attempt > 0 {
                self.restart_failed(attempt, &failure, generation);
                // The next pass checks the rebuild is still wanted.
                if !self
                    .clock
                    .sleep(CaptureSession::restart_backoff(attempt), cancel)
                {
                    return Restart::Abandoned;
                }
            }
            attempt += 1;
            let last = attempt >= CaptureSession::RESTART_ATTEMPTS;
            if plan.default_first && !default_tried {
                default_tried = true;
                if let Ok(restart) = self.try_start(sink, true, attempt, plan, generation, cancel) {
                    return restart;
                }
            }
            let error = match self.try_start(sink, false, attempt, plan, generation, cancel) {
                Ok(restart) => return restart,
                Err(error) => error,
            };
            let early = !default_tried
                && (plan.on_the_fallback || matches!(error, CaptureError::DidNotRun(_)));
            if (early || last) && self.configuration.input_device_uid.is_some() {
                default_tried = true;
                if let Ok(restart) = self.try_start(sink, true, attempt, plan, generation, cancel) {
                    return restart;
                }
            }
            if last && !plan.until_it_runs && !matches!(error, CaptureError::DidNotRun(_)) {
                return Restart::Exhausted;
            }
            failure = error.to_string();
        }
    }

    /// The restart numbered `attempt` of the rebuild numbered `generation`
    /// failed with `failure`, and more follow. The log gets one line when
    /// the streak's first restart fails and one about every
    /// `RESTART_LOG_INTERVAL` after, with the count, and `debug` between
    /// them, while every try's own lines go to `debug` (`try_start`).
    /// `notices` carries `StillRestarting` once the streak passed
    /// `RESTART_ATTEMPTS`, unless its warning already stands.
    fn restart_failed(&self, attempt: usize, failure: &str, generation: usize) {
        let mut inner = self.lock();
        if !Self::still_rebuilding(&inner, generation) {
            return;
        }
        let now = self.clock.now();
        let Some(active) = inner.active.as_mut() else {
            return;
        };
        let streak = &mut active.streak;
        match streak.logged_at {
            None => {
                tracing::warn!("restart {attempt} of the capture failed ({failure}); trying again");
                streak.logged_at = Some(now);
            }
            Some(at) if now.saturating_sub(at) >= Self::RESTART_LOG_INTERVAL => {
                tracing::warn!(
                    "the capture is still restarting: {attempt} restarts so far, the latest \
                     failed ({failure})"
                );
                streak.logged_at = Some(now);
            }
            Some(_) => tracing::debug!("restart {attempt} of the capture failed ({failure})"),
        }
        if attempt >= CaptureSession::RESTART_ATTEMPTS && !streak.warned {
            streak.warned = true;
            Self::emit(&mut inner, CaptureNotice::StillRestarting { attempt });
        }
    }

    /// One start of a rebuild, `attempt`, on the chosen microphone or, with
    /// `on_the_default`, the default in its place (`start_on_the_default`,
    /// whose own error it logs; the caller keeps the chosen one's). Once
    /// the streak's first restart failed (`restart_failed`) the start's
    /// own lines, the session's and the backend's, go to `debug`
    /// (`start_log`). The start holds the session's mutex; with
    /// `plan.awaits_delivery` the wait for its first frame does not: the
    /// sink's count of frames offered is sampled every
    /// `STALL_CHECK_INTERVAL` on the clock, and the gap then runs to the
    /// last sample that saw nothing, not to the start's return. A stream
    /// that offered none `STALL_TIMEOUT` after its start is stopped and
    /// fails with `DidNotRun`; a change its listeners reported meanwhile
    /// is dropped with it, since the next start reads the devices as they
    /// are, and the sink's latch opens again for that start's listeners.
    /// A stop during the wait answers `Started`: the stream is running and
    /// is the stop's to tear down, so the rebuild's next steps give up on
    /// it and `finish()` stops it. `Started`, or `Abandoned` when the
    /// recording is gone; the error when the start failed or delivered
    /// nothing. Rust only.
    fn try_start(
        &self,
        sink: &Arc<LaneFrameSink>,
        on_the_default: bool,
        attempt: usize,
        plan: RestartPlan,
        generation: usize,
        cancel: &Cancel,
    ) -> Result<Restart, CaptureError> {
        let (stream, mut at, offered) = {
            let inner = self.lock();
            if !Self::still_rebuilding(&inner, generation) {
                return Ok(Restart::Abandoned);
            }
            let quiet = inner
                .active
                .as_ref()
                .is_some_and(|active| active.streak.logged_at.is_some());
            // The old stream is stopped, so the count stays until this
            // start delivers.
            let offered = sink.frames_offered();
            let stream = start_log::quietly(quiet, || {
                if on_the_default {
                    self.start_on_the_default(sink)
                        .ok_or(CaptureError::InputDeviceUnavailable)
                } else {
                    self.backend.start(
                        &self.configuration.lanes(),
                        self.configuration.input_device_uid.as_deref(),
                        Arc::clone(sink),
                    )
                }
            })?;
            (stream, self.clock.now(), offered)
        };
        let started = at;
        while plan.awaits_delivery && sink.frames_offered() == offered {
            let now = self.clock.now();
            if now.saturating_sub(started) >= CaptureSession::STALL_TIMEOUT {
                self.backend.stop();
                if let Some(active) = self.lock().active.as_mut() {
                    active.pending_change = None;
                }
                sink.rearm_device_change();
                return Err(CaptureError::DidNotRun(format!(
                    "the restarted capture delivered nothing within {} ms",
                    CaptureSession::STALL_TIMEOUT.as_millis()
                )));
            }
            at = now;
            if !self
                .clock
                .sleep(CaptureSession::STALL_CHECK_INTERVAL, cancel)
            {
                break;
            }
        }
        Ok(Restart::Started {
            started: Started {
                stream,
                on_the_default,
            },
            attempt,
            at,
        })
    }

    /// The default input in place of a chosen microphone whose `start`
    /// failed although it may be connected (a device still settling after
    /// it was plugged in, one another app holds, a link that stalls):
    /// `start` with no UID, its input marked as the fallback unless it is
    /// the chosen one after all; `None` without a chosen microphone or when
    /// this start fails too (the caller keeps the chosen one's error). A
    /// backend started without a UID watches for no chosen device, so this
    /// cannot loop. The chosen one is asked for again by the next rebuild,
    /// which comes when a default device moves or one in use goes, not when
    /// the chosen one is plugged in again, and on Linux also by the watch
    /// thread's probes (`CHOSEN_INPUT_RECHECK`), which touch nothing that
    /// records. Called with the mutex held, as every `backend.start`; its
    /// lines go to `debug` inside a quiet restart (`start_log`).
    fn start_on_the_default(&self, sink: &Arc<LaneFrameSink>) -> Option<CaptureStream> {
        let chosen = self.configuration.input_device_uid.as_deref()?;
        let mut stream = self
            .backend
            .start(&self.configuration.lanes(), None, Arc::clone(sink))
            .inspect_err(|error| {
                start_log!(warn, "the default input did not start either: {error}");
            })
            .ok()?;
        if let Some(input) = stream.input.as_mut()
            && input.uid != chosen
        {
            start_log!(
                warn,
                "the input device {chosen} did not start; recording from the default input {}",
                input.uid
            );
            input.is_fallback = true;
        }
        Some(stream)
    }

    /// The new processing thread on the kept sink and relay, built for the
    /// started stream's latencies and publishing into the shared
    /// `LevelSlot`; the statistics and the notice follow. A change reported
    /// during the rebuild starts the next one. A stream on the default in
    /// the chosen microphone's place has the watch thread ask for the
    /// chosen one again over a backend that probes
    /// (`CHOSEN_INPUT_RECHECK`). The streak counts `attempt` restarts, for
    /// a rebuild that continues it. `false` when a stop came first.
    fn resume(
        self: &Arc<Self>,
        Started {
            stream,
            on_the_default,
        }: Started,
        attempt: usize,
        gap_frames: usize,
        sink: &Arc<LaneFrameSink>,
        relay: &Arc<FrameRelay>,
        generation: usize,
    ) -> bool {
        let mut inner = self.lock();
        if !Self::still_rebuilding(&inner, generation) {
            return false;
        }
        let canceller = inner.echo_canceller.take();
        let Some(active) = inner.active.as_mut() else {
            return false;
        };
        let gap_seconds = Self::seconds(gap_frames);
        let mut processing = self.make_processing_thread(
            sink,
            relay,
            &stream,
            Some(Arc::clone(&active.levels)),
            canceller,
        );
        processing.start();
        let pending = active.pending_change.take();
        if let Some(watch) = active.watch.as_mut() {
            let now = self.clock.now();
            watch.progress.count = sink.frames_offered();
            watch.progress.at = now;
            watch.resumed_at = Some(now);
            watch.recheck = None;
            if watch.probes && Self::replaced_the_chosen(&stream, on_the_default) {
                watch.ask_again(now);
            }
        }
        active.stream = stream;
        active.processing = Some(processing);
        active.device_changes += 1;
        active.gap_seconds += gap_seconds;
        active.rebuild = None;
        active.streak.attempts = attempt;
        // Unwatched, nothing tells the first frame: the resume stands for
        // it. A watched stream's first frame ends the warning on the watch
        // thread, after this resume or never, as a stream that waits for
        // playback may resume without one.
        let delivering = active.watch.is_none() && std::mem::take(&mut active.streak.warned);
        Self::emit(
            &mut inner,
            CaptureNotice::DeviceResumed {
                attempt,
                gap_seconds,
            },
        );
        if delivering {
            Self::emit(&mut inner, CaptureNotice::Delivering);
        }
        // Must stay under this guard: released and taken again, a stop and a
        // new start could come in between and hand this recording's change
        // to the next one. No test reaches that window, so this comment is
        // what keeps the call here.
        if let Some(pending) = pending {
            self.begin_rebuild(&mut inner, pending);
        }
        true
    }

    /// Zeros in every written channel for `frames` relay frames, through
    /// the relay the writer thread keeps draining. The rings under the sink
    /// are not touched: sized before the device's rate is known, they ask
    /// for two seconds at 48 kHz, rounded up to 131 072 samples (2.7 s at
    /// 48 kHz, 0.68 s at 192 kHz), and nothing drains them while the
    /// processing thread is stopped, so a longer gap would silently shrink
    /// into `dropped_samples`. A full relay (a long gap, or a writer still
    /// behind the old producer) is waited out in 5 ms steps on the clock;
    /// `has_room` is asked first because a refused `begin_frame` counts as
    /// a dropped frame. Returns the frames written, fewer than `frames`
    /// when the rebuild was abandoned meanwhile.
    fn write_silence(
        &self,
        frames: usize,
        relay: &FrameRelay,
        generation: usize,
        cancel: &Cancel,
    ) -> usize {
        if frames == 0 {
            return 0;
        }
        let zeros = vec![0.0f32; relay.frame_size()];
        let mut written = 0;
        while written < frames {
            if !Self::still_rebuilding(&self.lock(), generation) {
                break;
            }
            if relay.has_room() && relay.begin_frame() {
                for channel in 0..relay.channels() {
                    relay.write(channel, &zeros);
                }
                relay.end_frame();
                written += 1;
            } else if !self.clock.sleep(Duration::from_millis(5), cancel) {
                break;
            }
        }
        written
    }

    /// Waits, in 5 ms steps on the clock, until the relay has room for the
    /// whole frames the rings collected while the gap was written (capped
    /// at the relay's capacity, re-read on every step): the new processing
    /// thread pushes them at once, and a relay still full of silence would
    /// refuse and count them. Returns `false` when the rebuild was
    /// abandoned meanwhile.
    fn relay_has_room(
        &self,
        sink: &LaneFrameSink,
        relay: &FrameRelay,
        stream: &CaptureStream,
        generation: usize,
        cancel: &Cancel,
    ) -> bool {
        // The rings hold the restarted device's rate.
        let backlog = || {
            (stream.at_output_rate(sink.available_to_read()) / FRAME_SIZE)
                .min(relay.capacity_frames())
        };
        while relay
            .capacity_frames()
            .saturating_sub(relay.available_frames())
            < backlog()
        {
            if !Self::still_rebuilding(&self.lock(), generation) {
                return false;
            }
            if !self.clock.sleep(Duration::from_millis(5), cancel) {
                return false;
            }
        }
        true
    }

    /// Every restart failed, or the rebuild panicked: the recording ends,
    /// finalised and carried in `Failed(DeviceLost)`. Only when the
    /// rebuild numbered `generation` of the start numbered `recording` is
    /// still the newest, checked under the same lock as the change: an
    /// older one's recording, or its place, belongs to another thread now.
    /// The generation decides only for a panic after `resume` began the
    /// next rebuild: before that no newer one can begin (`begin_rebuild`
    /// keeps a report for later while this one holds `active.rebuild`),
    /// and a stop leaves `Recording`. No test reaches it; it stays as the
    /// guard for that case.
    fn device_lost(&self, recording: usize, generation: usize) {
        {
            let mut inner = self.lock();
            if !matches!(inner.state, CaptureState::Recording { .. })
                || inner.recordings_started != recording
                || inner.rebuild_generation != generation
            {
                return;
            }
            let Some(active) = inner.active.as_mut() else {
                return;
            };
            active.ended_on_device_loss = true;
            // This runs inside the rebuild thread; `finish()` must not
            // cancel or join it, and it is over anyway.
            active.rebuild = None;
            self.set_state(&mut inner, &CaptureState::Stopping);
        }
        let unwinding = Unwinding::arm(self, CaptureError::DeviceLost);
        let state = match self.finish() {
            Ok(mut result) => CaptureState::Failed {
                error: result
                    .failure
                    .get_or_insert(CaptureError::DeviceLost)
                    .clone(),
                recording: Some(Box::new(result)),
            },
            Err(error) => CaptureState::Failed {
                error,
                recording: None,
            },
        };
        self.set_state(&mut self.lock(), &state);
        unwinding.disarm();
    }

    /// The writer thread of the start numbered `recording` failed; a
    /// failure that arrives after that recording ended is ignored.
    fn writer_failed(&self, error: &CaptureError, recording: usize) {
        {
            let mut inner = self.lock();
            if !matches!(inner.state, CaptureState::Recording { .. })
                || inner.recordings_started != recording
            {
                return;
            }
            self.set_state(&mut inner, &CaptureState::Stopping);
        }
        let failure = as_writer_failure(error.clone());
        let unwinding = Unwinding::arm(self, failure.clone());
        let result = self.finish().ok().map(|mut result| {
            result.failure = Some(failure.clone());
            Box::new(result)
        });
        let mut inner = self.lock();
        self.set_state(
            &mut inner,
            &CaptureState::Failed {
                error: failure,
                recording: result,
            },
        );
        unwinding.disarm();
    }
}

/// `error` as a `WriterFailed`: one that already is passes through, so its
/// `Display` says "writing the recording failed" once. A `RecordingExists`
/// passes through too: it wrote nothing, and its `Display` names the folder.
fn as_writer_failure(error: CaptureError) -> CaptureError {
    match error {
        CaptureError::WriterFailed(_) | CaptureError::RecordingExists(_) => error,
        other => CaptureError::WriterFailed(other.to_string()),
    }
}

/// Armed while a finalise holds `Stopping`, or a start `Starting`. If it
/// panics, the drop during the unwind sets `Failed { error, recording:
/// None }` and wakes the waiting `stop()`s, which would otherwise wait for
/// good; the next `start()` then works. A start's also stops the backend
/// it may have started. A step that returns disarms it.
struct Unwinding<'a> {
    core: &'a Core,
    error: Option<CaptureError>,
    /// A start's, which holds `Starting`; a finalise's holds `Stopping`.
    starting: bool,
}

impl<'a> Unwinding<'a> {
    fn arm(core: &'a Core, error: CaptureError) -> Self {
        Self {
            core,
            error: Some(error),
            starting: false,
        }
    }

    fn starting(core: &'a Core) -> Self {
        Self {
            core,
            error: Some(CaptureError::BackendFailed(
                "starting the recording panicked".into(),
            )),
            starting: true,
        }
    }

    fn disarm(mut self) {
        self.error = None;
    }
}

impl Drop for Unwinding<'_> {
    fn drop(&mut self) {
        if let Some(error) = self.error.take() {
            // The state is the guarded step's until the drop leaves it, so
            // the backend stops with the lock released, as everywhere.
            let holds = if self.starting {
                CaptureState::Starting
            } else {
                CaptureState::Stopping
            };
            if self.core.lock().state != holds {
                return;
            }
            if self.starting {
                self.core.backend.stop();
            }
            self.core.set_state(
                &mut self.core.lock(),
                &CaptureState::Failed {
                    error,
                    recording: None,
                },
            );
        }
    }
}

#[cfg(test)]
mod tests {
    //! The window between a finalise's wakeup and the waiting `stop()`'s
    //! next hold of the lock, forced through `before_stop_waits`: the
    //! public API in `tests/session.rs` can only race for it. Swift's actor
    //! ran the queued stop right after the finalise, so Swift has no
    //! counterpart.

    use std::path::Path;

    use steno_core::paths::file_url_path;

    use super::*;
    use crate::capture::CaptureMode;
    use crate::testing::SyntheticCaptureBackend;
    use crate::writer::{LaneFrames, RecordingFiles};

    const RECV: Duration = Duration::from_secs(10);

    /// The production writer, except that every write fails as a full disk
    /// does and `finish()` waits for `release`, which holds the writer
    /// failure's finalise in `Stopping`.
    struct FailsThenHolds {
        inner: RecordingWriter,
        release: Receiver<()>,
    }

    impl RecordingWriting for FailsThenHolds {
        fn files(&self) -> RecordingFiles {
            self.inner.files()
        }
        fn write(&mut self, _frames: &LaneFrames<'_>) -> Result<(), CaptureError> {
            Err(CaptureError::WriterFailed("DiskFull".into()))
        }
        fn sync(&mut self) -> std::io::Result<()> {
            self.inner.sync()
        }
        fn finish(&mut self) -> Result<RecordingFiles, CaptureError> {
            // Bounded, so a failed test cannot hang its session's drop.
            let _ = self.release.recv_timeout(RECV);
            self.inner.finish()
        }
    }

    /// Receives from `states` until a state matches `want`.
    fn wait_for(states: &Receiver<CaptureState>, want: impl Fn(&CaptureState) -> bool) {
        loop {
            let state = states.recv_timeout(RECV).expect("the state changes");
            if want(&state) {
                return;
            }
        }
    }

    /// The first recording's writer fails and its finalise holds
    /// `Stopping`; a `stop()` finds it there and pauses in
    /// `before_stop_waits` while the finalise ends `Failed` and, given a
    /// `second` meeting, a `start()` for it takes the lock. Returns the
    /// session, what that `stop()` returned and the first meeting.
    fn stop_that_waited(
        directory: &Path,
        second: Option<Uuid>,
    ) -> (CaptureSession, Result<CaptureResult, CaptureError>, Uuid) {
        let (release, released) = channel();
        let first_writer = Mutex::new(Some(released));
        let session = CaptureSession::with_writer_factory(
            CaptureConfiguration::new(CaptureMode::InPerson, directory),
            Arc::new(SyntheticCaptureBackend::tones(
                &[AudioLane::Mixed],
                &[(AudioLane::Mixed, 440.0)],
                0.5,
            )),
            None,
            CaptureSession::DEFAULT_WRITER_HEADROOM_FRAMES,
            Arc::new(SystemClock::new()),
            Arc::new(move |layout, lanes, keep_raw| {
                let inner = RecordingWriter::new(layout, lanes, keep_raw)?;
                Ok(match first_writer.lock().unwrap().take() {
                    Some(release) => Box::new(FailsThenHolds { inner, release }),
                    None => Box::new(inner) as Box<dyn RecordingWriting>,
                })
            }),
        )
        .unwrap();
        let states = session.states();
        let first = Uuid::new_v4();
        session.start(first).unwrap();
        wait_for(&states, |state| matches!(state, CaptureState::Stopping));

        let (paused, pause) = channel();
        let (resume, resumed) = channel::<()>();
        session.core.lock().before_stop_waits = Some(Box::new(move || {
            paused.send(()).unwrap();
            resumed.recv_timeout(RECV).unwrap();
        }));
        let stopped = std::thread::scope(|scope| {
            let stopper = scope.spawn(|| session.stop());
            pause
                .recv_timeout(RECV)
                .expect("the stop() finds the finalise in Stopping");
            release.send(()).unwrap();
            wait_for(&states, |state| {
                matches!(state, CaptureState::Failed { .. })
            });
            if let Some(second) = second {
                session.start(second).unwrap();
            }
            resume.send(()).unwrap();
            stopper.join().unwrap()
        });
        (session, stopped, first)
    }

    /// A `start()` takes the lock between the finalise's `Failed` and the
    /// waiting `stop()`: the stop answers with the recording that finalise
    /// produced, as Swift's actor did, and leaves the new recording
    /// running, instead of ending a meeting it was never asked to end.
    #[test]
    fn a_stop_that_waited_does_not_stop_the_next_recording() {
        let directory = tempfile::tempdir().unwrap();
        let second = Uuid::new_v4();
        let (session, stopped, first) = stop_that_waited(directory.path(), Some(second));
        let result = stopped.expect("the finalised recording, not InvalidState");
        assert_eq!(result.asset.meeting_id, first);
        assert!(
            file_url_path(&result.asset.url).unwrap().exists(),
            "the first meeting's master"
        );
        assert!(
            matches!(session.state(), CaptureState::Recording { .. }),
            "{:?}",
            session.state()
        );
        assert_eq!(session.stop().unwrap().asset.meeting_id, second);
        assert_eq!(session.state(), CaptureState::Idle);
    }

    /// The waiting `stop()` holds the lock again before any `start()`: it
    /// returns the recording `Failed` carries, and the next recording
    /// starts and stops on its own.
    #[test]
    fn a_stop_that_waited_returns_the_failed_recording() {
        let directory = tempfile::tempdir().unwrap();
        let (session, stopped, first) = stop_that_waited(directory.path(), None);
        let result = stopped.expect("stop() returns the finalised recording");
        assert_eq!(result.asset.meeting_id, first);
        match session.state() {
            CaptureState::Failed {
                error: CaptureError::WriterFailed(detail),
                recording,
            } => {
                assert!(detail.contains("DiskFull"), "{detail}");
                assert_eq!(recording.as_deref(), Some(&result));
            }
            other => panic!("expected Failed(WriterFailed), got {other:?}"),
        }
        let second = Uuid::new_v4();
        session.start(second).unwrap();
        assert_eq!(session.stop().unwrap().asset.meeting_id, second);
        assert_eq!(session.state(), CaptureState::Idle);
    }
}
