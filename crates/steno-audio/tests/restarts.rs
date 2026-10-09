//! A capture's restarts as one streak, over a device the test drives by
//! hand on a manual clock: how often they try, what they log, when the
//! warning comes and goes, and that a capture that comes back loses
//! nothing. Each test reads the lines the binary logs, so the tests here
//! run one at a time. Rust only.

// Test arithmetic: frame counts cast freely, and gap seconds compare
// exactly on purpose.
#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::float_cmp,
    clippy::too_many_lines
)]

use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use steno_audio::capture::{
    CaptureBackend, CaptureConfiguration, CaptureError, CaptureMode, CaptureNotice, CaptureResult,
    CaptureSession, CaptureStream, DeviceChangeReason,
};
use steno_audio::realtime::LaneFrameSink;
use steno_audio::testing::ManualClock;
use steno_audio::writer::CafFile;
use steno_audio::{Clock, SAMPLE_RATE};
use steno_core::AudioLane;
use steno_core::paths::file_url_path;
use tempfile::TempDir;
use uuid::Uuid;

const RECV: Duration = Duration::from_secs(10);
/// Frames in one pushed callback: 10 ms at 48 kHz.
const CALLBACK: usize = 480;
/// One sample of the watch thread, the step the clock moves by.
const STEP: Duration = CaptureSession::STALL_CHECK_INTERVAL;
/// How long a stream must deliver before the warning ends (the session's
/// `STALLED_AGAIN`).
const STALLED_AGAIN: Duration = Duration::from_secs(10);

/// Stands in for a watched device: every start hands the sink over and
/// delivers nothing until the test pushes a callback (`push`); a stopped
/// device takes none. Each start's reading of the manual clock is kept.
struct Pushed {
    clock: Arc<ManualClock>,
    /// The sink and the lane count while started, `None` once stopped,
    /// under one lock with `push`, so no frame lands after `stop`.
    running: Mutex<Option<(Arc<LaneFrameSink>, usize)>>,
    starts: Mutex<Vec<Duration>>,
    /// Frames the sink took, over every start.
    accepted: Mutex<usize>,
}

impl Pushed {
    fn new(clock: Arc<ManualClock>) -> Arc<Self> {
        Arc::new(Self {
            clock,
            running: Mutex::new(None),
            starts: Mutex::new(Vec::new()),
            accepted: Mutex::new(0),
        })
    }

    /// One callback on every lane, while started.
    fn push(&self) {
        let running = self.running.lock().unwrap();
        if let Some((sink, lanes)) = running.as_ref()
            && sink.begin_callback(CALLBACK)
        {
            for lane in 0..*lanes {
                sink.write_slice(lane, &[0.25; CALLBACK]);
            }
            sink.end_callback();
            *self.accepted.lock().unwrap() += CALLBACK;
        }
    }

    /// Samples the rings hold, while started.
    fn backlog(&self) -> usize {
        self.running
            .lock()
            .unwrap()
            .as_ref()
            .map_or(0, |(sink, _)| sink.available_to_read())
    }

    fn starts(&self) -> Vec<Duration> {
        self.starts.lock().unwrap().clone()
    }

    fn accepted(&self) -> usize {
        *self.accepted.lock().unwrap()
    }
}

impl CaptureBackend for Pushed {
    fn start(
        &self,
        lanes: &[AudioLane],
        _: Option<&str>,
        sink: Arc<LaneFrameSink>,
    ) -> Result<CaptureStream, CaptureError> {
        self.starts.lock().unwrap().push(self.clock.now());
        *self.running.lock().unwrap() = Some((sink, lanes.len()));
        Ok(CaptureStream::SYNTHETIC)
    }

    fn stop(&self) {
        self.running.lock().unwrap().take();
    }

    fn delivers_continuously(&self, _: &[AudioLane]) -> bool {
        true
    }
}

/// What the binary logs at `info` and above, process-wide, so a line is
/// caught whichever thread writes it; cleared by [`one_at_a_time`].
#[derive(Clone, Default)]
struct Log(Arc<Mutex<Vec<u8>>>);

impl Log {
    fn get() -> &'static Log {
        static LOG: OnceLock<Log> = OnceLock::new();
        LOG.get_or_init(|| {
            let log = Log::default();
            tracing::subscriber::set_global_default(
                tracing_subscriber::fmt()
                    .with_writer(log.clone())
                    .with_max_level(tracing::Level::INFO)
                    .with_ansi(false)
                    .without_time()
                    .finish(),
            )
            .expect("no other subscriber in this test binary");
            log
        })
    }

    /// The session's lines at `level` so far that contain `text`.
    fn count(&self, level: &str, text: &str) -> usize {
        String::from_utf8_lossy(&self.0.lock().unwrap())
            .lines()
            .filter(|line| {
                line.trim_start().starts_with(level)
                    && line.contains("steno_audio::capture::session")
                    && line.contains(text)
            })
            .count()
    }
}

impl std::io::Write for Log {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Log {
    type Writer = Log;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

/// Holds the binary's other tests off and clears the log.
fn one_at_a_time() -> (MutexGuard<'static, ()>, &'static Log) {
    static ONE: Mutex<()> = Mutex::new(());
    let guard = ONE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let log = Log::get();
    log.0.lock().unwrap().clear();
    (guard, log)
}

/// A recording in `mode`, started over a `Pushed` device on a manual
/// clock, with 160 s of relay: the writer runs on wall time, so a relay
/// that filled would hold the rebuild in its waits for room while the test
/// moves the clock on. The directory holds the recording's files.
fn record(mode: CaptureMode) -> (TempDir, CaptureSession, Drive) {
    let directory = tempfile::tempdir().unwrap();
    let clock = Arc::new(ManualClock::new());
    let backend = Pushed::new(clock.clone());
    let mut configuration = CaptureConfiguration::new(mode, directory.path());
    configuration.echo_cancellation = false;
    let session =
        CaptureSession::with_backend(configuration, backend.clone(), None, 16_000, clock.clone())
            .unwrap();
    session.start(Uuid::new_v4()).unwrap();
    let drive = Drive::new(clock, backend, &session);
    (directory, session, drive)
}

/// The test's side of a recording: moves the clock a `STEP` at a time once
/// every thread that sleeps on it does (the watch thread, and a rebuild's
/// while one runs, as the notices tell), so every step is the same on any
/// host, and keeps each notice with the clock reading it came at.
struct Drive {
    clock: Arc<ManualClock>,
    backend: Arc<Pushed>,
    notices: Receiver<CaptureNotice>,
    rebuilding: bool,
    seen: Vec<(Duration, CaptureNotice)>,
}

impl Drive {
    fn new(clock: Arc<ManualClock>, backend: Arc<Pushed>, session: &CaptureSession) -> Self {
        Self {
            clock,
            backend,
            notices: session.notices(),
            rebuilding: false,
            seen: Vec::new(),
        }
    }

    /// Takes the notices that came; `true` when any did.
    fn take_notices(&mut self) -> bool {
        let mut any = false;
        while let Ok(notice) = self.notices.try_recv() {
            any = true;
            match notice {
                CaptureNotice::DeviceChanged(_) => self.rebuilding = true,
                CaptureNotice::DeviceResumed { .. } => self.rebuilding = false,
                _ => {}
            }
            self.seen.push((self.clock.now(), notice));
        }
        any
    }

    /// Waits until the watch thread sleeps, and the rebuild's thread too
    /// while one runs. A notice sent before a thread slept is taken before
    /// the count is trusted.
    fn settle(&mut self) {
        let deadline = Instant::now() + RECV;
        loop {
            self.take_notices();
            let sleepers = 1 + usize::from(self.rebuilding);
            if self.clock.pending_sleepers() == sleepers && !self.take_notices() {
                return;
            }
            assert!(Instant::now() < deadline, "the session's threads sleep");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    /// `steps` steps, the device delivering one callback in each with
    /// `delivering`. Outside a rebuild a callback waits for the processing
    /// thread to read the one before, so no ring overruns on a slow host.
    fn run(&mut self, steps: usize, delivering: bool) {
        for _ in 0..steps {
            self.settle();
            if delivering {
                let deadline = Instant::now() + RECV;
                while !self.rebuilding && self.backend.backlog() > 0 {
                    assert!(Instant::now() < deadline, "the processing thread reads");
                    std::thread::sleep(Duration::from_millis(1));
                }
                self.backend.push();
            }
            self.clock.advance(STEP);
        }
        self.settle();
    }

    /// Steps, delivering with `delivering`, until `done` holds of the
    /// latest notice, within `limit` steps.
    fn run_until(
        &mut self,
        limit: usize,
        delivering: bool,
        done: impl Fn(&CaptureNotice) -> bool,
    ) -> Duration {
        let from = self.seen.len();
        for _ in 0..limit {
            self.run(1, delivering);
            if let Some((at, _)) = self.seen[from..].iter().find(|(_, notice)| done(notice)) {
                return *at;
            }
        }
        panic!(
            "no such notice within {limit} steps, at {:?}: {:?}",
            self.clock.now(),
            &self.seen[from..]
        );
    }

    fn count(&self, what: impl Fn(&CaptureNotice) -> bool) -> usize {
        self.seen.iter().filter(|(_, notice)| what(notice)).count()
    }

    /// Steps until the clock reads `until`, the device delivering one
    /// callback a step with `delivering`.
    fn run_to(&mut self, until: Duration, delivering: bool) {
        while self.clock.now() < until {
            self.run(1, delivering);
        }
    }
}

fn steps_in(duration: Duration) -> usize {
    (duration.as_millis() / STEP.as_millis()) as usize
}

fn is_resumed(notice: &CaptureNotice) -> bool {
    matches!(notice, CaptureNotice::DeviceResumed { .. })
}

fn is_still_restarting(notice: &CaptureNotice) -> bool {
    matches!(notice, CaptureNotice::StillRestarting { .. })
}

fn is_delivering(notice: &CaptureNotice) -> bool {
    *notice == CaptureNotice::Delivering
}

fn master_frames(result: &CaptureResult) -> usize {
    CafFile::read(&file_url_path(&result.asset.url).unwrap())
        .unwrap()
        .frame_count()
}

/// The session's `warn` lines over a recording that has run `until`: the
/// stall and the streak's first failure, then about one a minute.
fn assert_about_once_a_minute(log: &Log, until: Duration) {
    let warns = log.count("WARN", "");
    let minutes = until.as_secs() / 60;
    assert!(
        warns >= 2 && warns <= 2 + minutes as usize,
        "{warns} warn lines in {until:?}"
    );
}

/// Every frame the device delivered is in the master, beside the silence
/// that filled the gaps, and none was dropped.
fn assert_nothing_lost(result: &CaptureResult, backend: &Pushed) {
    assert!(
        result.statistics.dropped_frames.is_empty(),
        "nothing dropped"
    );
    assert_eq!(
        master_frames(result),
        backend.accepted() + (result.statistics.gap_seconds * SAMPLE_RATE).round() as usize,
        "every frame delivered, and the gaps"
    );
}

/// A device that resumes, delivers a moment and stalls again within 10 s,
/// over and over: its restarts are one streak across the rebuilds, counted
/// on and backed off as restarts that fail are, up to
/// `RESTART_BACKOFF_LONGEST`. Over two minutes the log says so about once a
/// minute, `StillRestarting` comes once and the warning stands through the
/// resumes, with no `Delivering` between them. Once the device delivers
/// for 10 s, `Delivering` comes once, with one `info` line, and the master
/// holds every frame it delivered.
#[test]
fn a_device_that_flaps_backs_off_logs_once_a_minute_and_warns_once() {
    let (_one, log) = one_at_a_time();
    let (_directory, session, mut drive) = record(CaptureMode::InPerson);
    drive.run(5, true);
    let end = Duration::from_secs(120);
    while drive.clock.now() < end {
        // Stalls, then a restart delivers at its first sample, and
        // delivers 0.8 s more after the resume.
        drive.run_until(steps_in(Duration::from_secs(10)), false, |notice| {
            *notice == CaptureNotice::DeviceChanged(DeviceChangeReason::DeliveryStalled)
        });
        drive.run_until(steps_in(Duration::from_secs(10)), true, is_resumed);
        drive.run(8, true);
    }
    let resumes: Vec<usize> = drive
        .seen
        .iter()
        .filter_map(|(_, notice)| match notice {
            CaptureNotice::DeviceResumed { attempt, .. } => Some(*attempt),
            _ => None,
        })
        .collect();
    assert!(resumes.len() >= 15, "it flaps: {resumes:?}");
    assert_eq!(
        resumes,
        (1..=resumes.len()).collect::<Vec<_>>(),
        "one streak, counted on across the rebuilds"
    );
    // Each restart waits the backoff after the restarts before it, from
    // the stall's report, to the step.
    let starts = drive.backend.starts();
    let stalls: Vec<Duration> = drive
        .seen
        .iter()
        .filter(|(_, notice)| matches!(notice, CaptureNotice::DeviceChanged(_)))
        .map(|(at, _)| *at)
        .collect();
    for (restart, (stalled, started)) in stalls.iter().zip(&starts[1..]).enumerate() {
        let wait = if restart == 0 {
            Duration::ZERO
        } else {
            CaptureSession::restart_backoff(restart)
        };
        let waited = started.saturating_sub(*stalled);
        assert!(
            waited >= wait && waited <= wait + STEP,
            "restart {}: {waited:?} after the stall, the backoff {wait:?}",
            restart + 1
        );
    }
    assert_eq!(
        CaptureSession::restart_backoff(resumes.len() - 1),
        CaptureSession::RESTART_BACKOFF_LONGEST,
        "the backoff reached its longest"
    );
    assert_eq!(
        drive.count(is_still_restarting),
        1,
        "the warning comes once: {:?}",
        drive.seen
    );
    assert_eq!(
        drive.count(is_delivering),
        0,
        "and stands through the resumes"
    );
    assert_about_once_a_minute(log, drive.clock.now());
    assert_eq!(log.count("INFO", "delivers again"), 0);

    // The device delivers for good: the warning ends once it has for
    // `STALLED_AGAIN`, and only then.
    drive.run_until(steps_in(Duration::from_secs(10)), false, |notice| {
        matches!(notice, CaptureNotice::DeviceChanged(_))
    });
    let resumed_at = drive.run_until(steps_in(Duration::from_secs(10)), true, is_resumed);
    let delivering = drive.run_until(steps_in(Duration::from_secs(12)), true, is_delivering);
    let after = delivering.saturating_sub(resumed_at);
    assert!(
        after >= STALLED_AGAIN && after <= STALLED_AGAIN + 2 * STEP,
        "{after:?} after the resume"
    );
    drive.run(steps_in(Duration::from_secs(5)), true);
    assert_eq!(drive.count(is_delivering), 1, "once");
    assert_eq!(log.count("INFO", "delivers again"), 1);
    let result = session.stop().unwrap();
    assert!(!result.statistics.ended_on_device_loss);
    assert_nothing_lost(&result, &drive.backend);
}

/// A Mac call capture whose silent output did not start (A10 logs it at
/// `warn`): it delivers nothing until another app plays, from its start
/// on. Held to deliver as every capture is, it is restarted on the
/// backoff, about every 6 s (a 2 s wait for a frame, then 4 s), with about
/// one log line a minute and one `StillRestarting`, and no audio exists to
/// lose meanwhile. Once something plays, the next restart delivers, the
/// silence before it fills up to `MAXIMUM_GAP`, and the master holds every
/// frame from then on; the warning ends once it has delivered for 10 s.
#[test]
fn a_call_capture_with_nothing_playing_restarts_on_the_backoff_and_loses_nothing_once_it_plays() {
    let (_one, log) = one_at_a_time();
    let (_directory, session, mut drive) = record(CaptureMode::Call);
    drive.run_to(Duration::from_secs(120), false);
    assert_eq!(
        drive.seen.first(),
        Some(&(
            CaptureSession::STALL_TIMEOUT + STEP,
            CaptureNotice::DeviceChanged(DeviceChangeReason::DeliveryStalled)
        )),
        "nothing from the start is a stall"
    );
    assert_eq!(
        drive.count(is_resumed),
        0,
        "no restart counts without a frame"
    );
    assert_eq!(drive.count(is_still_restarting), 1);
    let starts = drive.backend.starts();
    let apart: Vec<Duration> = starts
        .windows(2)
        .map(|pair| pair[1].saturating_sub(pair[0]))
        .collect();
    let longest = CaptureSession::STALL_TIMEOUT + CaptureSession::RESTART_BACKOFF_LONGEST;
    for (restart, gap) in apart.iter().enumerate().skip(1) {
        assert!(
            *gap <= longest + STEP,
            "restart {restart}: {gap:?} after the one before"
        );
    }
    let at_the_longest = &apart[CaptureSession::RESTART_ATTEMPTS + 1..];
    assert!(at_the_longest.len() >= 15, "{apart:?}");
    assert!(
        at_the_longest.iter().all(|gap| *gap >= longest),
        "about every 6 s, never more often: {apart:?}"
    );
    assert_about_once_a_minute(log, drive.clock.now());

    // Something plays from here on: the next restart delivers.
    let resumed_at = drive.run_until(steps_in(Duration::from_secs(10)), true, is_resumed);
    let gap = drive
        .seen
        .iter()
        .find_map(|(_, notice)| match notice {
            CaptureNotice::DeviceResumed { gap_seconds, .. } => Some(*gap_seconds),
            _ => None,
        })
        .unwrap();
    assert_eq!(gap, CaptureSession::MAXIMUM_GAP.as_secs_f64(), "capped");
    let delivering = drive.run_until(steps_in(Duration::from_secs(12)), true, is_delivering);
    assert!(delivering.saturating_sub(resumed_at) >= STALLED_AGAIN);
    assert_eq!(log.count("INFO", "delivers again"), 1);
    let result = session.stop().unwrap();
    assert!(!result.statistics.ended_on_device_loss);
    assert_eq!(result.statistics.device_changes, 1);
    assert_nothing_lost(&result, &drive.backend);
}

/// A warning that stands when a new streak begins (a device change after
/// restarts that went past `RESTART_ATTEMPTS` and resumed) stands into it,
/// and ends once the stream that change resumed on has delivered for 10 s,
/// with one `Delivering` and nothing after it.
#[test]
fn a_warning_that_stands_into_a_new_streak_ends_once_its_stream_delivers() {
    let (_one, _log) = one_at_a_time();
    let (_directory, session, mut drive) = record(CaptureMode::InPerson);
    drive.run(5, true);
    drive.run_until(
        steps_in(Duration::from_secs(30)),
        false,
        is_still_restarting,
    );
    drive.run_until(steps_in(Duration::from_secs(10)), true, is_resumed);
    drive.run(10, true);
    session.device_changed(DeviceChangeReason::DefaultInputChanged);
    drive.run_until(steps_in(Duration::from_secs(10)), true, |notice| {
        matches!(notice, CaptureNotice::DeviceResumed { attempt: 1, .. })
    });
    let streak = drive.seen.len();
    drive.run_until(steps_in(Duration::from_secs(12)), true, is_delivering);
    drive.run(steps_in(Duration::from_secs(5)), true);
    assert_eq!(
        drive.seen[streak..]
            .iter()
            .map(|(_, notice)| *notice)
            .collect::<Vec<_>>(),
        vec![CaptureNotice::Delivering],
        "one Delivering, and nothing after it"
    );
    let result = session.stop().unwrap();
    assert_nothing_lost(&result, &drive.backend);
}
